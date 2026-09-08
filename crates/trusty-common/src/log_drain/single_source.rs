//! The single-source `[log_drain]` shape shared by every daemon that drains
//! exactly one log directory to one destination (#6537, Phase 5 of epic
//! #6533; consolidated here in the same epic's code-review fix round).
//!
//! Why: `trusty-agents` and `trusty-code` each built their own copy of this
//! shape — an 8-field config section, a resolver that validates it into a
//! runnable plan, and a scheduler tick that runs one pass — because
//! trusty-mpm's own Phase 3 drain (#6657) groups several projects under one
//! `sources[]` list, which neither single-project daemon needs. The two
//! copies diverged only cosmetically (crate name, include glob, config file
//! format, wording), so per this workspace's common-entry-point rule this
//! module is the ONE implementation; each consumer keeps only its
//! file-format adapter (a TOML section in trusty-agents, a standalone YAML
//! file in trusty-code — both a bare type alias to [`SingleSourceSection`],
//! since the two formats serialize the same fields) plus a thin
//! `resolve_log_drain`/`tick` wrapper naming its own crate, include glob, and
//! state directory. trusty-mpm's own multi-source `sources[]` resolver is
//! NOT migrated here — see epic #6533's follow-up.
//! What: [`SingleSourceSection`] is the serde-agnostic 8-field shape;
//! [`resolve_single_source`] validates it into a [`LogDrainSetting`];
//! [`run_tick`] runs one pass of an already-resolved section. The local
//! idempotency-manifest-cache directory is a `state_dir: &Path` PARAMETER of
//! [`run_tick`] rather than resolved internally with `dirs::home_dir()` (the
//! shape both per-crate originals used) — every test drives it with a
//! tempdir, and only each consumer's production `spawn()` call site supplies
//! the real path.
//! Test: `tests`.

use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::github_path::derive_remote_repo;
use crate::log_drain::{
    DEFAULT_MAX_FILE_BYTES, DEFAULT_MAX_WIRE_BYTES, DestinationUri, DrainConfig, DrainTarget,
    Level, LogSource, ObjectStoreDestination, run_once,
};

/// Default interval between drain passes, in seconds — 15 minutes, matching
/// trusty-mpm's own default.
pub const DEFAULT_INTERVAL_SECS: u64 = 900;

/// The on-disk section shape: a TOML table in trusty-agents' own config, or
/// the whole of trusty-code's standalone `log_drain.yaml`. Every field
/// optional, same names as trusty-mpm's `log_drain:` section
/// (`trusty_mpm::core::trusty_tools_config::log_drain::LogDrainConfig`).
///
/// Why: modelled once so every single-source consumer's config file
/// round-trips through the same struct rather than through
/// independently-maintained copies that could silently drift apart.
/// Test: each consumer's own format round-trip test (`toml_round_trips_every_field`
/// in trusty-agents, `yaml_round_trips_every_field` in trusty-code), plus
/// this module's own `tests`.
// Deliberately NOT `#[non_exhaustive]`: unlike `ResolvedLogDrain` (built only
// inside this module), this is an on-disk config shape every consumer crate
// constructs directly in its own tests via struct-update syntax
// (`LogDrainConfig { enabled: Some(true), ..LogDrainConfig::default() }`) —
// `#[non_exhaustive]` forbids exactly that from outside this crate. A new
// field stays additive regardless, since every field is `Option`/`Vec` with
// `#[serde(default)]` and the struct derives `Default`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SingleSourceSection {
    /// Whether the scheduler runs at all. `None`/`Some(false)` → disabled.
    #[serde(default)]
    pub enabled: Option<bool>,
    /// Destination URI — `s3://bucket/prefix` or `file:///abs/path`.
    #[serde(default)]
    pub destination: Option<String>,
    /// Seconds between passes. `None` → [`DEFAULT_INTERVAL_SECS`].
    #[serde(default)]
    pub interval_secs: Option<u64>,
    /// Plaintext source ceiling. `None` → the collector's own default.
    #[serde(default)]
    pub max_file_bytes: Option<u64>,
    /// Compressed-body ceiling. `None` → the collector's own default.
    #[serde(default)]
    pub max_wire_bytes: Option<u64>,
    /// Extra literal strings scrubbed from every body before upload.
    #[serde(default)]
    pub secrets: Vec<String>,
    /// Repository owner, when the daemon has no bound project (or its root
    /// has no git origin).
    #[serde(default)]
    pub owner: Option<String>,
    /// Project name, paired with [`SingleSourceSection::owner`].
    #[serde(default)]
    pub project: Option<String>,
}

/// Every way a [`SingleSourceSection`] can be wrong.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum SingleSourceError {
    /// Enabled with no `destination`.
    #[error("log_drain.enabled is true but log_drain.destination is unset")]
    MissingDestination,
    /// `destination` did not parse as a supported URI.
    #[error("log_drain.destination is invalid: {reason}")]
    Destination {
        /// The parser's own message.
        reason: String,
    },
    /// A numeric knob was set to zero.
    #[error("log_drain.{field} must be greater than zero")]
    NonPositive {
        /// `interval_secs`, `max_file_bytes`, or `max_wire_bytes`.
        field: &'static str,
    },
    /// Neither `owner`/`project` nor the bound project's git origin resolved
    /// an identity.
    #[error(
        "log_drain has no owner/project: {reason} — set owner/project in the \
         log-drain config, or run this daemon inside a git checkout whose \
         origin names the project"
    )]
    MissingIdentity {
        /// What the identity probe found.
        reason: String,
    },
}

/// What the config says the drain should do.
#[derive(Debug, Clone)]
pub enum LogDrainSetting {
    /// No section enabled. The scheduler does not spawn a pass.
    Disabled,
    /// A validated, runnable plan.
    Enabled(Box<ResolvedLogDrain>),
}

/// A validated, runnable drain plan — everything [`run_tick`] needs except
/// the destination connection, which it makes at tick time.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct ResolvedLogDrain {
    /// The parsed destination.
    pub destination: DestinationUri,
    /// The destination as the operator wrote it, for log messages.
    pub destination_display: String,
    /// The `<owner>/<project>` every key in this pass sits under.
    pub target: DrainTarget,
    /// The daemon's own log directory — the only source it drains.
    pub source: LogSource,
    /// How long the scheduler sleeps between passes.
    pub interval: Duration,
    /// Plaintext source ceiling handed to `DrainConfig`.
    pub max_file_bytes: u64,
    /// Compressed-body ceiling handed to `DrainConfig`.
    pub max_wire_bytes: u64,
    /// Extra literal secrets to scrub.
    pub secrets: Vec<String>,
}

/// Resolve the section into a runnable plan, or refuse.
///
/// `log_root` is the daemon's own log directory (tests pass a tempdir).
/// `project_root` is the current project's root — `None` when the daemon
/// serves no particular project. `crate_name` and `default_include` are the
/// consumer's own [`LogSource::crate_name`] and include glob (e.g.
/// `"trusty-agents"`/`"daemon-*.log*"` or `"trusty-code"`/`"tcode.log*"`).
///
/// # Errors
/// See [`SingleSourceError`].
pub fn resolve_single_source(
    section: &SingleSourceSection,
    log_root: &Path,
    project_root: Option<&Path>,
    crate_name: &str,
    default_include: &str,
) -> Result<LogDrainSetting, SingleSourceError> {
    let enabled = section.enabled.unwrap_or(false);

    let destination = match section.destination.as_deref().map(str::trim) {
        Some(raw) if !raw.is_empty() => Some((
            raw.to_string(),
            DestinationUri::parse(raw).map_err(|e| SingleSourceError::Destination {
                reason: e.to_string(),
            })?,
        )),
        _ => None,
    };

    let interval_secs = section.interval_secs.unwrap_or(DEFAULT_INTERVAL_SECS);
    if interval_secs == 0 {
        return Err(SingleSourceError::NonPositive {
            field: "interval_secs",
        });
    }
    let max_file_bytes = section.max_file_bytes.unwrap_or(DEFAULT_MAX_FILE_BYTES);
    if max_file_bytes == 0 {
        return Err(SingleSourceError::NonPositive {
            field: "max_file_bytes",
        });
    }
    let max_wire_bytes = section.max_wire_bytes.unwrap_or(DEFAULT_MAX_WIRE_BYTES);
    if max_wire_bytes == 0 {
        return Err(SingleSourceError::NonPositive {
            field: "max_wire_bytes",
        });
    }

    if !enabled {
        return Ok(LogDrainSetting::Disabled);
    }

    let Some((display, uri)) = destination else {
        return Err(SingleSourceError::MissingDestination);
    };

    let target = resolve_identity(section, project_root)?;

    let source = LogSource {
        crate_name: crate_name.to_string(),
        root: log_root.to_path_buf(),
        include: vec![default_include.to_string()],
        level_filter: Some(Level::Info),
    };

    Ok(LogDrainSetting::Enabled(Box::new(ResolvedLogDrain {
        destination: uri,
        destination_display: display,
        target,
        source,
        interval: Duration::from_secs(interval_secs),
        max_file_bytes,
        max_wire_bytes,
        secrets: section.secrets.clone(),
    })))
}

/// Order: the section's own `owner`/`project`, then the git origin of the
/// bound project. Half an identity in config is refused, never filled from
/// git — mixing an explicit owner with a guessed project (or vice versa)
/// files logs under a key nobody asked for.
fn resolve_identity(
    section: &SingleSourceSection,
    project_root: Option<&Path>,
) -> Result<DrainTarget, SingleSourceError> {
    match (
        non_empty(section.owner.as_deref()),
        non_empty(section.project.as_deref()),
    ) {
        (Some(owner), Some(project)) => return Ok(DrainTarget { owner, project }),
        (Some(_), None) | (None, Some(_)) => {
            return Err(SingleSourceError::MissingIdentity {
                reason: "`owner` is set but `project` is not (or vice versa)".to_string(),
            });
        }
        (None, None) => {}
    }
    let Some(root) = project_root else {
        return Err(SingleSourceError::MissingIdentity {
            reason: "no project is bound to this daemon".to_string(),
        });
    };
    derive_remote_repo(root)
        .map(|remote| DrainTarget {
            owner: remote.owner,
            project: remote.repo,
        })
        .map_err(|e| SingleSourceError::MissingIdentity {
            reason: e.to_string(),
        })
}

fn non_empty(raw: Option<&str>) -> Option<String> {
    raw.map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// What one pass did, in the three states an operator distinguishes — mirrors
/// trusty-mpm's own `daemon::log_drain::DrainOutcome`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DrainOutcome {
    /// Every collected file either uploaded or was provably already there.
    Success,
    /// The drain is configured off; nothing was attempted.
    SkippedDisabled,
    /// The pass errored, or completed with at least one per-file failure.
    Failed,
}

/// One full pass over an already-loaded section: resolve, connect, `run_once`.
///
/// Why: the two per-crate originals resolved their own local
/// manifest-cache directory inline (`dirs::home_dir().join(".trusty-agents")`,
/// `private_state_dir()`), which meant the "enabled tick" test wrote real
/// files under a developer's actual home directory (#6537 code-review fix
/// round). Taking `state_dir` as a parameter means every test drives it with
/// a tempdir, and only the production `spawn()` call site in each consumer
/// supplies the real path.
/// What: [`resolve_single_source`], then — when enabled — connects the
/// destination and runs [`run_once`] against it, logging the verdict.
/// Returns the outcome and the interval the NEXT sleep should use.
/// Test: `tests`.
pub async fn run_tick(
    section: &SingleSourceSection,
    log_root: &Path,
    project_root: Option<&Path>,
    state_dir: &Path,
    crate_name: &str,
    default_include: &str,
) -> (DrainOutcome, Duration) {
    let default_interval = Duration::from_secs(DEFAULT_INTERVAL_SECS);
    match resolve_single_source(section, log_root, project_root, crate_name, default_include) {
        Err(e) => {
            warn!("log_drain: {e}");
            (DrainOutcome::Failed, default_interval)
        }
        Ok(LogDrainSetting::Disabled) => (DrainOutcome::SkippedDisabled, default_interval),
        Ok(LogDrainSetting::Enabled(plan)) => {
            let interval = plan.interval;
            let dest = match ObjectStoreDestination::connect(&plan.destination).await {
                Ok(dest) => dest,
                Err(e) => {
                    warn!(
                        destination = %plan.destination_display,
                        "log_drain: cannot reach destination: {e}"
                    );
                    return (DrainOutcome::Failed, interval);
                }
            };
            let drain_cfg = DrainConfig::new(state_dir.to_path_buf())
                .with_secrets(plan.secrets.clone())
                .with_max_file_bytes(plan.max_file_bytes)
                .with_max_wire_bytes(plan.max_wire_bytes);
            let outcome = match run_once(
                &drain_cfg,
                &dest,
                &plan.target,
                std::slice::from_ref(&plan.source),
            )
            .await
            {
                Ok(report) if report.errors.is_empty() => {
                    info!(
                        uploaded = report.uploaded,
                        skipped_unchanged = report.skipped_unchanged,
                        "log_drain: pass complete"
                    );
                    DrainOutcome::Success
                }
                Ok(report) => {
                    warn!(
                        errors = report.errors.len(),
                        first = ?report.errors.first(),
                        "log_drain: pass completed with per-file errors"
                    );
                    DrainOutcome::Failed
                }
                Err(e) => {
                    warn!("log_drain: run_once failed: {e}");
                    DrainOutcome::Failed
                }
            };
            (outcome, interval)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_CRATE: &str = "test-crate";
    const TEST_INCLUDE: &str = "test.log*";

    fn base_section() -> SingleSourceSection {
        SingleSourceSection::default()
    }

    #[test]
    fn absent_section_is_disabled() {
        let dir = tempfile::tempdir().expect("tempdir");
        let setting =
            resolve_single_source(&base_section(), dir.path(), None, TEST_CRATE, TEST_INCLUDE)
                .expect("resolve");
        assert!(matches!(setting, LogDrainSetting::Disabled));
    }

    #[test]
    fn validates_a_malformed_destination_even_while_disabled() {
        let dir = tempfile::tempdir().expect("tempdir");
        let section = SingleSourceSection {
            destination: Some("ftp://nope".to_string()),
            ..base_section()
        };
        let err = resolve_single_source(&section, dir.path(), None, TEST_CRATE, TEST_INCLUDE)
            .unwrap_err();
        assert!(matches!(err, SingleSourceError::Destination { .. }));
    }

    #[test]
    fn rejects_a_zero_interval() {
        let dir = tempfile::tempdir().expect("tempdir");
        let section = SingleSourceSection {
            interval_secs: Some(0),
            ..base_section()
        };
        let err = resolve_single_source(&section, dir.path(), None, TEST_CRATE, TEST_INCLUDE)
            .unwrap_err();
        assert_eq!(
            err,
            SingleSourceError::NonPositive {
                field: "interval_secs"
            }
        );
    }

    #[test]
    fn enabled_with_no_destination_is_missing_destination() {
        let dir = tempfile::tempdir().expect("tempdir");
        let section = SingleSourceSection {
            enabled: Some(true),
            ..base_section()
        };
        let err = resolve_single_source(&section, dir.path(), None, TEST_CRATE, TEST_INCLUDE)
            .unwrap_err();
        assert_eq!(err, SingleSourceError::MissingDestination);
    }

    #[test]
    fn enabled_with_no_identity_and_no_project_is_missing_identity() {
        let dir = tempfile::tempdir().expect("tempdir");
        let dest_dir = tempfile::tempdir().expect("tempdir");
        let section = SingleSourceSection {
            enabled: Some(true),
            destination: Some(format!("file://{}", dest_dir.path().display())),
            ..base_section()
        };
        let err = resolve_single_source(&section, dir.path(), None, TEST_CRATE, TEST_INCLUDE)
            .unwrap_err();
        assert!(matches!(err, SingleSourceError::MissingIdentity { .. }));
    }

    #[test]
    fn enabled_with_explicit_owner_and_project_resolves() {
        let dir = tempfile::tempdir().expect("tempdir");
        let dest_dir = tempfile::tempdir().expect("tempdir");
        let section = SingleSourceSection {
            enabled: Some(true),
            destination: Some(format!("file://{}", dest_dir.path().display())),
            owner: Some("acme".to_string()),
            project: Some("widgets".to_string()),
            ..base_section()
        };
        let setting = resolve_single_source(&section, dir.path(), None, TEST_CRATE, TEST_INCLUDE)
            .expect("resolve");
        let LogDrainSetting::Enabled(plan) = setting else {
            panic!("expected Enabled");
        };
        assert_eq!(plan.target.owner, "acme");
        assert_eq!(plan.target.project, "widgets");
        assert_eq!(plan.source.crate_name, TEST_CRATE);
        assert_eq!(plan.source.root, dir.path());
    }

    #[test]
    fn half_an_identity_is_refused_not_filled_from_git() {
        let dir = tempfile::tempdir().expect("tempdir");
        let dest_dir = tempfile::tempdir().expect("tempdir");
        let section = SingleSourceSection {
            enabled: Some(true),
            destination: Some(format!("file://{}", dest_dir.path().display())),
            owner: Some("acme".to_string()),
            ..base_section()
        };
        let err = resolve_single_source(&section, dir.path(), None, TEST_CRATE, TEST_INCLUDE)
            .unwrap_err();
        assert!(matches!(err, SingleSourceError::MissingIdentity { .. }));
    }

    /// Disabled section: `run_tick` never touches the destination or the
    /// network.
    #[tokio::test]
    async fn opt_out_skips_the_pass() {
        let log_root = tempfile::tempdir().expect("tempdir");
        std::fs::write(log_root.path().join("test.log"), "hello\n").expect("seed log");
        let state_dir = tempfile::tempdir().expect("tempdir");

        let (outcome, interval) = run_tick(
            &SingleSourceSection::default(),
            log_root.path(),
            None,
            state_dir.path(),
            TEST_CRATE,
            TEST_INCLUDE,
        )
        .await;
        assert_eq!(outcome, DrainOutcome::SkippedDisabled);
        assert_eq!(interval, Duration::from_secs(DEFAULT_INTERVAL_SECS));
    }

    /// An enabled plan against a `file://` destination uploads the seeded log
    /// on the first tick, into the INJECTED `state_dir` — never a real home
    /// directory (#6537 code-review fix round) — and a second tick is
    /// idempotent.
    #[tokio::test]
    async fn an_enabled_tick_uploads_into_the_injected_state_dir_and_is_idempotent() {
        let log_root = tempfile::tempdir().expect("tempdir");
        std::fs::write(log_root.path().join("test.log"), "hello\n").expect("seed log");
        let dest_dir = tempfile::tempdir().expect("tempdir");
        let state_dir = tempfile::tempdir().expect("tempdir");

        let section = SingleSourceSection {
            enabled: Some(true),
            destination: Some(format!("file://{}", dest_dir.path().display())),
            owner: Some("acme".to_string()),
            project: Some("widgets".to_string()),
            ..SingleSourceSection::default()
        };

        let (first, _) = run_tick(
            &section,
            log_root.path(),
            None,
            state_dir.path(),
            TEST_CRATE,
            TEST_INCLUDE,
        )
        .await;
        assert_eq!(first, DrainOutcome::Success);
        let uploaded = std::fs::read_dir(dest_dir.path().join("acme").join("widgets"))
            .expect("read the target's key prefix")
            .count();
        assert!(
            uploaded > 0,
            "run_once must have written under acme/widgets"
        );
        // The manifest cache landed under the INJECTED state_dir, proving no
        // fallback to a real home directory was ever consulted.
        assert!(
            std::fs::read_dir(state_dir.path())
                .expect("read state_dir")
                .count()
                > 0,
            "the manifest cache must land under the injected state_dir"
        );

        let (second, _) = run_tick(
            &section,
            log_root.path(),
            None,
            state_dir.path(),
            TEST_CRATE,
            TEST_INCLUDE,
        )
        .await;
        assert_eq!(
            second,
            DrainOutcome::Success,
            "an unchanged file must still report success on the next tick"
        );
    }

    /// `run_tick` reports `Failed`, never a panic, when the destination
    /// cannot be reached.
    #[tokio::test]
    async fn an_unreachable_destination_reports_failed() {
        let log_root = tempfile::tempdir().expect("tempdir");
        let state_dir = tempfile::tempdir().expect("tempdir");
        let blocker = tempfile::NamedTempFile::new().expect("tempfile");
        let section = SingleSourceSection {
            enabled: Some(true),
            destination: Some(format!(
                "file://{}",
                blocker.path().join("nested").display()
            )),
            owner: Some("acme".to_string()),
            project: Some("widgets".to_string()),
            ..SingleSourceSection::default()
        };

        let (outcome, _) = run_tick(
            &section,
            log_root.path(),
            None,
            state_dir.path(),
            TEST_CRATE,
            TEST_INCLUDE,
        )
        .await;
        assert_eq!(outcome, DrainOutcome::Failed);
    }

    /// A malformed section (bad destination) reports `Failed` rather than
    /// panicking the loop.
    #[tokio::test]
    async fn a_config_error_reports_failed() {
        let log_root = tempfile::tempdir().expect("tempdir");
        let state_dir = tempfile::tempdir().expect("tempdir");
        let section = SingleSourceSection {
            destination: Some("ftp://nope".to_string()),
            ..SingleSourceSection::default()
        };
        let (outcome, interval) = run_tick(
            &section,
            log_root.path(),
            None,
            state_dir.path(),
            TEST_CRATE,
            TEST_INCLUDE,
        )
        .await;
        assert_eq!(outcome, DrainOutcome::Failed);
        assert_eq!(interval, Duration::from_secs(DEFAULT_INTERVAL_SECS));
    }
}
