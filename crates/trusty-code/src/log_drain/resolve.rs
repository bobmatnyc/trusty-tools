//! Turn [`super::LogDrainConfig`] into a runnable plan, or refuse (#6537).
//!
//! Why: `trusty_common::log_drain::run_once` demands a parsed destination, a
//! validated identity, and a `LogSource`. This is the fallible translation,
//! scoped to tcode's single-project shape — see the module docs on
//! `super` for why this differs from trusty-mpm's `sources[]` grouping.
//! What: [`resolve_log_drain`] validates the section whenever it is present
//! (even while disabled — a destination typo must surface before the
//! operator flips `enabled: true`), then resolves identity ONLY for an
//! enabled plan, since that step reads git.
//! Test: `tests`.

use std::path::Path;
use std::time::Duration;

use trusty_common::github_path::derive_remote_repo;
use trusty_common::log_drain::{
    DEFAULT_MAX_FILE_BYTES, DEFAULT_MAX_WIRE_BYTES, DestinationUri, DrainTarget, Level, LogSource,
};

use super::config::LogDrainConfig;

/// Default interval between drain passes, in seconds — 15 minutes, matching
/// trusty-mpm's own default (`trusty_mpm::core::trusty_tools_config::log_drain::DEFAULT_INTERVAL_SECS`).
pub const DEFAULT_INTERVAL_SECS: u64 = 900;

/// Include glob for tcode's own rotating file log (`crate::logging::init_tracing_with_file_log`).
const DEFAULT_INCLUDE: &str = "tcode.log*";

/// Producing crate name, as it appears in every drained object's key.
const CRATE_NAME: &str = "trusty-code";

/// Every way the `log_drain.yaml` file can be wrong.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum LogDrainConfigError {
    /// `enabled: true` with no `destination`.
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
        "log_drain has no owner/project: {reason} — set `owner:`/`project:` in \
         ~/.trusty-code/log_drain.yaml, or run `tcode serve` inside a git \
         checkout whose origin names the project"
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

/// A validated, runnable drain plan — everything `run_once` needs except the
/// destination connection, which the scheduler makes at tick time.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct ResolvedLogDrain {
    /// The parsed destination.
    pub destination: DestinationUri,
    /// The destination as the operator wrote it, for log messages.
    pub destination_display: String,
    /// The `<owner>/<project>` every key in this pass sits under.
    pub target: DrainTarget,
    /// tcode's own file log — the only source this daemon drains.
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

/// Resolve the config into a runnable plan, or refuse.
///
/// `log_root` is `crate::logging::file_log_dir()` in production; tests pass a
/// tempdir. `project_root` is `binding.root()` — `None` for a projectless
/// `tcode serve`.
///
/// # Errors
/// See [`LogDrainConfigError`].
pub fn resolve_log_drain(
    cfg: &LogDrainConfig,
    log_root: &Path,
    project_root: Option<&Path>,
) -> Result<LogDrainSetting, LogDrainConfigError> {
    let enabled = cfg.enabled.unwrap_or(false);

    let destination = match cfg.destination.as_deref().map(str::trim) {
        Some(raw) if !raw.is_empty() => Some((
            raw.to_string(),
            DestinationUri::parse(raw).map_err(|e| LogDrainConfigError::Destination {
                reason: e.to_string(),
            })?,
        )),
        _ => None,
    };

    let interval_secs = cfg.interval_secs.unwrap_or(DEFAULT_INTERVAL_SECS);
    if interval_secs == 0 {
        return Err(LogDrainConfigError::NonPositive {
            field: "interval_secs",
        });
    }
    let max_file_bytes = cfg.max_file_bytes.unwrap_or(DEFAULT_MAX_FILE_BYTES);
    if max_file_bytes == 0 {
        return Err(LogDrainConfigError::NonPositive {
            field: "max_file_bytes",
        });
    }
    let max_wire_bytes = cfg.max_wire_bytes.unwrap_or(DEFAULT_MAX_WIRE_BYTES);
    if max_wire_bytes == 0 {
        return Err(LogDrainConfigError::NonPositive {
            field: "max_wire_bytes",
        });
    }

    if !enabled {
        return Ok(LogDrainSetting::Disabled);
    }

    let Some((display, uri)) = destination else {
        return Err(LogDrainConfigError::MissingDestination);
    };

    let target = resolve_identity(cfg, project_root)?;

    let source = LogSource {
        crate_name: CRATE_NAME.to_string(),
        root: log_root.to_path_buf(),
        include: vec![DEFAULT_INCLUDE.to_string()],
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
        secrets: cfg.secrets.clone(),
    })))
}

/// Order: the config's own `owner`/`project`, then the git origin of the
/// bound project. Half an identity in config is refused, never filled from
/// git — mixing an explicit owner with a guessed project (or vice versa)
/// files logs under a key nobody asked for.
fn resolve_identity(
    cfg: &LogDrainConfig,
    project_root: Option<&Path>,
) -> Result<DrainTarget, LogDrainConfigError> {
    match (
        non_empty(cfg.owner.as_deref()),
        non_empty(cfg.project.as_deref()),
    ) {
        (Some(owner), Some(project)) => return Ok(DrainTarget { owner, project }),
        (Some(_), None) | (None, Some(_)) => {
            return Err(LogDrainConfigError::MissingIdentity {
                reason: "`owner` is set but `project` is not (or vice versa)".to_string(),
            });
        }
        (None, None) => {}
    }
    let Some(root) = project_root else {
        return Err(LogDrainConfigError::MissingIdentity {
            reason: "no project is bound to this tcode daemon".to_string(),
        });
    };
    derive_remote_repo(root)
        .map(|remote| DrainTarget {
            owner: remote.owner,
            project: remote.repo,
        })
        .map_err(|e| LogDrainConfigError::MissingIdentity {
            reason: e.to_string(),
        })
}

fn non_empty(raw: Option<&str>) -> Option<String> {
    raw.map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_cfg() -> LogDrainConfig {
        LogDrainConfig::default()
    }

    #[test]
    fn absent_section_is_disabled() {
        let dir = tempfile::tempdir().expect("tempdir");
        let setting = resolve_log_drain(&base_cfg(), dir.path(), None).expect("resolve");
        assert!(matches!(setting, LogDrainSetting::Disabled));
    }

    #[test]
    fn validates_a_malformed_destination_even_while_disabled() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cfg = LogDrainConfig {
            destination: Some("ftp://nope".to_string()),
            ..base_cfg()
        };
        let err = resolve_log_drain(&cfg, dir.path(), None).unwrap_err();
        assert!(matches!(err, LogDrainConfigError::Destination { .. }));
    }

    #[test]
    fn rejects_a_zero_interval() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cfg = LogDrainConfig {
            interval_secs: Some(0),
            ..base_cfg()
        };
        let err = resolve_log_drain(&cfg, dir.path(), None).unwrap_err();
        assert_eq!(
            err,
            LogDrainConfigError::NonPositive {
                field: "interval_secs"
            }
        );
    }

    #[test]
    fn enabled_with_no_destination_is_missing_destination() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cfg = LogDrainConfig {
            enabled: Some(true),
            ..base_cfg()
        };
        let err = resolve_log_drain(&cfg, dir.path(), None).unwrap_err();
        assert_eq!(err, LogDrainConfigError::MissingDestination);
    }

    #[test]
    fn enabled_with_no_identity_and_no_project_is_missing_identity() {
        let dir = tempfile::tempdir().expect("tempdir");
        let dest_dir = tempfile::tempdir().expect("tempdir");
        let cfg = LogDrainConfig {
            enabled: Some(true),
            destination: Some(format!("file://{}", dest_dir.path().display())),
            ..base_cfg()
        };
        let err = resolve_log_drain(&cfg, dir.path(), None).unwrap_err();
        assert!(matches!(err, LogDrainConfigError::MissingIdentity { .. }));
    }

    #[test]
    fn enabled_with_explicit_owner_and_project_resolves() {
        let dir = tempfile::tempdir().expect("tempdir");
        let dest_dir = tempfile::tempdir().expect("tempdir");
        let cfg = LogDrainConfig {
            enabled: Some(true),
            destination: Some(format!("file://{}", dest_dir.path().display())),
            owner: Some("acme".to_string()),
            project: Some("widgets".to_string()),
            ..base_cfg()
        };
        let setting = resolve_log_drain(&cfg, dir.path(), None).expect("resolve");
        let LogDrainSetting::Enabled(plan) = setting else {
            panic!("expected Enabled");
        };
        assert_eq!(plan.target.owner, "acme");
        assert_eq!(plan.target.project, "widgets");
        assert_eq!(plan.source.crate_name, CRATE_NAME);
        assert_eq!(plan.source.root, dir.path());
    }

    #[test]
    fn half_an_identity_is_refused_not_filled_from_git() {
        let dir = tempfile::tempdir().expect("tempdir");
        let dest_dir = tempfile::tempdir().expect("tempdir");
        let cfg = LogDrainConfig {
            enabled: Some(true),
            destination: Some(format!("file://{}", dest_dir.path().display())),
            owner: Some("acme".to_string()),
            ..base_cfg()
        };
        let err = resolve_log_drain(&cfg, dir.path(), None).unwrap_err();
        assert!(matches!(err, LogDrainConfigError::MissingIdentity { .. }));
    }
}
