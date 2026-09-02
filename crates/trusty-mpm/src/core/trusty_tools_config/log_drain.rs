//! The `log_drain:` config section and its resolution (#6535, Phase 3 of #6533).
//!
//! Why: `trusty_common::log_drain::run_once` demands a parsed destination, a
//! validated identity, and a concrete `LogSource` list. Something has to turn
//! an operator's YAML into those, and that translation is where a typo becomes
//! either a clear refusal or a daemon that quietly uploads nothing. Split into
//! its own sibling module for the same reason `untracked_sync` is: the parent
//! `trusty_tools_config.rs` is already near the 500-SLOC production cap.
//!
//! What: [`LogDrainConfig`] is the on-disk shape, [`resolve_log_drain`] is the
//! fallible translation into [`ResolvedLogDrain`], and [`LogDrainSetting`]
//! distinguishes "the operator turned it off" from "here is a runnable plan".
//!
//! Test: `tests` submodule.
//!
//! # Malformed is an error, never a silent skip
//!
//! The surrounding [`TrustyToolsConfig`] parse is deliberately LENIENT — an
//! unrecognised key is warned about and dropped rather than discarding the whole
//! file (see `core::config_keys`). That leniency stops at this section's
//! CONTENT. A destination URI that does not parse, a zero interval, a source
//! with no root: each is a [`LogDrainConfigError`], the scheduler refuses to
//! start, and the `log_drain` doctor row reports `Fail`. A drain that silently
//! skipped a malformed destination would look identical to one that had nothing
//! to upload, which is the failure mode this section exists to avoid.
//!
//! Validation runs whenever the section is PRESENT, including while
//! `enabled: false`. Finding the typo before the operator flips the switch is
//! the whole point.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use trusty_common::log_drain::{
    DEFAULT_MAX_FILE_BYTES, DEFAULT_MAX_WIRE_BYTES, DestinationScheme, DestinationUri, DrainTarget,
    Level, LogSource,
};

use super::TrustyToolsConfig;

mod plan;

pub use plan::DisabledSource;

/// Default interval between drain passes, in seconds.
///
/// 15 minutes, per the #6533 design brief. Log bytes are a diagnostic aid, not
/// a live telemetry stream, so a slower cadence trades freshness for a smaller
/// object-store bill and fewer wakeups on a laptop.
pub const DEFAULT_INTERVAL_SECS: u64 = 900;

/// Directory under `~/.trusty-mpm/` holding the drain's manifest cache and the
/// last-run status the doctor row reads.
pub const STATE_SUBDIR: &str = "log-drain";

/// Directory the trusty-mpm daemon's own rotating file log is written to,
/// relative to `~/.trusty-mpm/` (`bin/tm/main.rs` creates it at startup).
const DAEMON_LOG_SUBDIR: &str = "logs";

/// Include globs for the default source when the operator lists none.
///
/// `tracing_appender::rolling::daily` writes `trusty-mpm.log.YYYY-MM-DD`, so
/// the pattern has to match a dated suffix rather than a bare `.log`.
const DEFAULT_INCLUDE: &[&str] = &["trusty-mpm.log*"];

/// The `log_drain:` section of `~/.trusty-tools/trusty-mpm/config.yaml`.
///
/// Why: every knob the epic named — where logs go, how often, what to scrub,
/// what to collect — has to be declarative, because the drain runs inside a
/// daemon nobody re-invokes by hand.
/// What: every field is optional so an absent section means "disabled" and a
/// partial section means "defaults for the rest". `sources` empty means the
/// built-in daemon log source (see [`resolve_log_drain`]).
/// Test: `tests::log_drain_config_yaml_round_trip`,
/// `tests::resolve_disabled_when_section_absent`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
// #6535: `#[non_exhaustive]` for the same reason `TrustyToolsConfig` carries it
// — a new public field on a constructible all-public struct is a semver major.
#[non_exhaustive]
pub struct LogDrainConfig {
    /// Whether the scheduler runs at all. `None`/`Some(false)` → disabled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,

    /// Destination URI — `s3://bucket/prefix` or `file:///abs/path`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub destination: Option<String>,

    /// Seconds between passes. `None` → [`DEFAULT_INTERVAL_SECS`]. Zero is an
    /// error, never a busy loop.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interval_secs: Option<u64>,

    /// Plaintext source ceiling. `None` → [`DEFAULT_MAX_FILE_BYTES`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_file_bytes: Option<u64>,

    /// Compressed-body ceiling (#6547). `None` → [`DEFAULT_MAX_WIRE_BYTES`].
    ///
    /// The collector streams, so the source size no longer bounds memory; the
    /// gzip body handed to `put` does. This is the knob for that bound.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_wire_bytes: Option<u64>,

    /// Extra literal strings scrubbed from every body before upload.
    ///
    /// Additive to whatever the caller already passes. `scrub_secrets` removes
    /// values it is GIVEN — it does not detect secret-shaped text — so this is
    /// the only way an operator names a site-specific token.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub secrets: Vec<String>,

    /// Fallback repository owner for a source that resolves none (#6657).
    ///
    /// Consulted only after a source's own `owner`/`project` and the git
    /// `origin` of its root, so a source that sits inside a checkout keeps that
    /// checkout's identity. It exists for roots no repo owns — the daemon's own
    /// `~/.trusty-mpm/logs` among them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,

    /// Fallback project name, paired with [`LogDrainConfig::owner`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,

    /// Directories to collect. Empty → the built-in daemon log source.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<LogDrainSourceConfig>,
}

/// One `log_drain.sources[]` entry.
///
/// Why: the epic wants trusty-code and trusty-agents drained too (#6537), and
/// each writes somewhere different. Naming sources in config means adopting a
/// new producer needs no code change here.
/// What: `crate_name` becomes a key segment, `root` is the directory walked,
/// `include` the globs relative to it, `level` the minimum line level kept, and
/// `destination` the object store this source alone goes to (#6657).
/// Test: `tests::resolve_uses_configured_sources`,
/// `tests::resolve_rejects_a_source_with_no_root`,
/// `tests::resolve_groups_sources_by_destination`,
/// `tests::resolve_skips_a_disabled_source`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct LogDrainSourceConfig {
    /// Whether this source drains at all. `None` → it does (#6657).
    ///
    /// `enabled: false` opts one project out without deleting its entry, and
    /// `tm doctor` still lists it so an operator can see the drain is off for
    /// that project rather than misconfigured.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,

    /// Producing crate, e.g. `trusty-mpm`. Required.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub crate_name: Option<String>,
    /// Directory to walk. Required; a leading `~` is expanded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root: Option<String>,
    /// Globs matched against paths relative to `root`. Empty → `**/*`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub include: Vec<String>,
    /// Minimum line level to upload — `trace`/`debug`/`info`/`warn`/`error`.
    /// `None` → every line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub level: Option<String>,
    /// Where THIS source's logs go. `None` → the section's `destination`.
    ///
    /// #6657: one host drains several projects, and a project's logs can be
    /// required to land in a specific AWS account. Overriding per source is how
    /// that requirement is expressed without splitting the daemon. A source
    /// whose override cannot be reached is SKIPPED, never rerouted to the
    /// section default — see [`resolve_log_drain`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub destination: Option<String>,

    /// Repository owner for this source's key prefix (#6657).
    ///
    /// Set it only when `root` is not inside a git checkout, or its `origin`
    /// does not name the project the logs belong to. `owner` and `project` are
    /// set together; one without the other is an error, because half an
    /// identity cannot make a key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,

    /// Project name for this source's key prefix, paired with `owner` (#6657).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
}

/// Every way the `log_drain:` section can be wrong.
///
/// Why: each variant names the field an operator has to edit. A single opaque
/// "bad log_drain config" would leave them diffing YAML against prose.
/// What: a `thiserror` enum; `#[non_exhaustive]` so a later phase can add a
/// variant without a breaking change.
/// Test: `tests::resolve_rejects_a_malformed_destination`,
/// `tests::resolve_rejects_a_zero_interval`,
/// `tests::resolve_rejects_an_unknown_level`.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum LogDrainConfigError {
    /// `enabled: true` with no `destination`.
    #[error("log_drain.enabled is true but log_drain.destination is unset")]
    MissingDestination,

    /// `destination` did not parse as a supported URI.
    #[error("log_drain.destination is invalid: {reason}")]
    Destination {
        /// The parser's own message, which names the URI and what was wrong.
        reason: String,
    },

    /// A numeric knob was set to zero.
    #[error("log_drain.{field} must be greater than zero")]
    NonPositive {
        /// `interval_secs`, `max_file_bytes`, or `max_wire_bytes`.
        field: &'static str,
    },

    /// A `sources[]` entry omitted a required field.
    #[error("log_drain.sources[{index}].{field} is required")]
    SourceField {
        /// Position of the offending entry.
        index: usize,
        /// `crate_name` or `root`.
        field: &'static str,
    },

    /// A `sources[].destination` override did not parse (#6657).
    #[error("log_drain.sources[{index}].destination is invalid: {reason}")]
    SourceDestination {
        /// Position of the offending entry.
        index: usize,
        /// The parser's own message, which names the URI and what was wrong.
        reason: String,
    },

    /// A source whose `owner`/`project` could not be resolved (#6657).
    ///
    /// Fail-closed: the drain refuses the whole plan rather than uploading one
    /// project's logs under a guessed key. The message names the source, its
    /// root, and the two ways to fix it.
    #[error(
        "log_drain.sources[{index}] ({crate_name}, root {root}) has no owner/project: \
         {reason} — set `owner:` and `project:` on that source, or point `root:` \
         inside a git checkout whose `origin` names the project"
    )]
    SourceIdentity {
        /// Position of the offending entry.
        index: usize,
        /// The entry's `crate_name`, so the operator can find it by name.
        crate_name: String,
        /// The root that was probed.
        root: String,
        /// What the probe found, from `trusty_common::github_path`.
        reason: String,
    },

    /// A `sources[].level` value that is not a level name.
    #[error(
        "log_drain.sources[{index}].level `{value}` is not a level — \
         expected one of trace, debug, info, warn, error"
    )]
    SourceLevel {
        /// Position of the offending entry.
        index: usize,
        /// What the operator wrote.
        value: String,
    },
}

/// One drain pass: an object store, a project, and the sources feeding it.
///
/// Why: the scheduler runs one `run_once` per entry, because `run_once` takes
/// exactly one destination and exactly one [`DrainTarget`]. Grouping here
/// rather than in the scheduler means the doctor row and the scheduler read the
/// same grouping, and a source can never be counted against a destination it
/// was not configured for.
/// What: `sources` is non-empty by construction — a pass nothing points at is
/// not in the list at all. Two sources share an entry only when they resolved
/// to the same destination AND the same `owner/project` (#6657), because those
/// two together are what the object keys and the manifest are namespaced by.
/// Test: `tests::resolve_groups_sources_by_destination`,
/// `tests::resolve_splits_one_destination_by_project`.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct ResolvedDrainDestination {
    /// The parsed destination.
    pub destination: DestinationUri,
    /// The destination as the operator wrote it, for log and doctor messages.
    pub destination_display: String,
    /// The `<owner>/<project>` every key in this pass sits under.
    pub target: DrainTarget,
    /// The directories collected for this destination. Never empty.
    pub sources: Vec<LogSource>,
}

impl ResolvedDrainDestination {
    /// The destination's scheme, as the doctor row reports it.
    pub fn scheme(&self) -> &'static str {
        match self.destination.scheme() {
            DestinationScheme::S3 => "s3",
            DestinationScheme::File => "file",
            // `DestinationScheme` is `#[non_exhaustive]` and reserves `gs`/`az`
            // for a later phase; `DestinationUri::parse` refuses both today, so
            // this arm is unreachable rather than a silent mislabel.
            _ => "other",
        }
    }
}

/// A validated, runnable drain plan.
///
/// Why: the scheduler and the doctor row both need the same resolved answer,
/// and re-deriving it in two places is how the two disagree.
/// What: everything `run_once` needs except the identity, which the scheduler
/// resolves at tick time. `destinations` is non-empty for any enabled plan; the
/// knobs beside it are section-wide and apply to every pass.
/// Test: `tests::resolve_fills_defaults`.
///
/// Deliberately not `PartialEq`: `LogSource` (a `trusty-common` type) is not,
/// and tests assert on the individual fields they care about rather than on
/// whole-plan equality.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct ResolvedLogDrain {
    /// Every pass this plan runs, in config order (#6657).
    pub destinations: Vec<ResolvedDrainDestination>,
    /// Sources the operator opted out with `enabled: false` (#6657).
    ///
    /// Carried rather than dropped so `tm doctor` can say the project is off on
    /// purpose. Nothing in the scheduler reads it.
    pub disabled: Vec<DisabledSource>,
    /// How long the scheduler sleeps between passes.
    pub interval: Duration,
    /// Plaintext source ceiling handed to `DrainConfig`.
    pub max_file_bytes: u64,
    /// Compressed-body ceiling handed to `DrainConfig` (#6547).
    pub max_wire_bytes: u64,
    /// Extra literal secrets to scrub.
    pub secrets: Vec<String>,
}

/// What the config says the drain should do.
///
/// Why: "disabled" and "failed" are different doctor rows and different
/// scheduler behaviour, so they are different values rather than an
/// `Option` the caller has to interpret.
/// Test: `tests::resolve_disabled_when_section_absent`,
/// `tests::resolve_validates_even_while_disabled`.
#[derive(Debug, Clone)]
pub enum LogDrainSetting {
    /// No section, or `enabled` is not `true`. The scheduler does not spawn.
    Disabled,
    /// A validated plan. Boxed because it is much larger than the other arm.
    Enabled(Box<ResolvedLogDrain>),
}

/// Turn the `log_drain:` section into a runnable plan, or refuse.
///
/// Why: one fallible translation, called by both the scheduler and the doctor
/// row, so an operator's typo produces the same verdict in both places.
/// What: validates the section whenever it is present — including while
/// disabled — then returns [`LogDrainSetting::Disabled`] unless `enabled` is
/// `Some(true)`. `home` supplies the `~` expansion and the built-in source
/// root, so tests need no real home directory.
///
/// Defaults applied: [`DEFAULT_INTERVAL_SECS`], [`DEFAULT_MAX_FILE_BYTES`], and
/// — when `sources` is empty — one `trusty-mpm` source over
/// `<home>/.trusty-mpm/logs` filtered at INFO, which is the file appender
/// `bin/tm/main.rs` installs for the daemon.
///
/// # Per-source destinations and projects (#6657)
///
/// A source's own `destination` wins over the section's; a source that names
/// none inherits the section default. Each source also resolves an
/// `<owner>/<project>` — its own `owner:`/`project:`, else the git `origin` of
/// its `root`, else the section's `owner:`/`project:`. A source that resolves
/// none is [`LogDrainConfigError::SourceIdentity`], never a placeholder key.
///
/// Sources are then GROUPED by the destination AND project they resolved to, in
/// first-appearance order, and the scheduler runs one pass per group.
/// `destination` at the section level is required only when at least one source
/// still needs it — a config where every source names its own is complete
/// without one. A source with `enabled: false` is left out of every group and
/// recorded in [`ResolvedLogDrain::disabled`].
///
/// Grouping is by the PARSED destination, so `s3://b/p` and `s3://b/p/` are one
/// group, while two identities against one bucket (`?profile=`) are two. That
/// asymmetry matches `DestinationUri::cache_namespace`, which the manifest
/// cache is keyed by (#6548) — so each group's pass reads and writes its own
/// record with no forking of that fix.
///
/// Identity resolution runs only for an ENABLED plan: it reads git, and a
/// checkout that has lost its origin is an environment fault rather than the
/// config typo the disabled-path validation exists to catch.
///
/// Test: `tests::resolve_disabled_when_section_absent`,
/// `tests::resolve_fills_defaults`,
/// `tests::resolve_rejects_a_malformed_destination`,
/// `tests::resolve_validates_even_while_disabled`,
/// `tests::resolve_groups_sources_by_destination`,
/// `tests::resolve_reads_owner_and_project_from_the_git_origin`,
/// `tests::resolve_refuses_a_source_with_no_resolvable_identity`.
///
/// # Errors
/// Any [`LogDrainConfigError`]. The caller must not fall back to a default
/// plan: see the module docs.
pub fn resolve_log_drain(
    config: &TrustyToolsConfig,
    home: &Path,
) -> Result<LogDrainSetting, LogDrainConfigError> {
    let Some(section) = config.log_drain.as_ref() else {
        return Ok(LogDrainSetting::Disabled);
    };
    let enabled = section.enabled.unwrap_or(false);

    let destination = match section.destination.as_deref().map(str::trim) {
        Some(raw) if !raw.is_empty() => Some((
            raw.to_string(),
            DestinationUri::parse(raw).map_err(|e| LogDrainConfigError::Destination {
                reason: e.to_string(),
            })?,
        )),
        _ => None,
    };

    let interval_secs = section.interval_secs.unwrap_or(DEFAULT_INTERVAL_SECS);
    if interval_secs == 0 {
        return Err(LogDrainConfigError::NonPositive {
            field: "interval_secs",
        });
    }
    let max_file_bytes = section.max_file_bytes.unwrap_or(DEFAULT_MAX_FILE_BYTES);
    if max_file_bytes == 0 {
        return Err(LogDrainConfigError::NonPositive {
            field: "max_file_bytes",
        });
    }
    // #6547: a zero wire cap would skip every file with a recorded decision,
    // which reads exactly like a working drain with nothing to send.
    let max_wire_bytes = section.max_wire_bytes.unwrap_or(DEFAULT_MAX_WIRE_BYTES);
    if max_wire_bytes == 0 {
        return Err(LogDrainConfigError::NonPositive {
            field: "max_wire_bytes",
        });
    }

    let prepared = resolve_sources(&section.sources, home)?;

    if !enabled {
        return Ok(LogDrainSetting::Disabled);
    }

    let fallback_identity = match (
        non_empty(section.owner.as_deref()),
        non_empty(section.project.as_deref()),
    ) {
        (Some(owner), Some(project)) => Some(DrainTarget { owner, project }),
        _ => None,
    };
    let (destinations, disabled) = plan::build(prepared, destination, fallback_identity.as_ref())?;

    Ok(LogDrainSetting::Enabled(Box::new(ResolvedLogDrain {
        destinations,
        disabled,
        interval: Duration::from_secs(interval_secs),
        max_file_bytes,
        max_wire_bytes,
        secrets: section.secrets.clone(),
    })))
}

/// A destination as the operator wrote it, beside its parsed form.
type NamedDestination = (String, DestinationUri);

/// One validated `sources[]` entry, before its identity is resolved.
///
/// Why: validation is pure and runs even while the drain is disabled, but
/// identity resolution reads git and runs only for an enabled plan. This is
/// what the first stage hands the second.
struct PreparedSource {
    /// Position in `sources[]`, for error messages.
    index: usize,
    /// `enabled: false` keeps the entry out of every pass (#6657).
    enabled: bool,
    /// The collector's view of this entry.
    source: LogSource,
    /// The entry's own `destination`, when it named one.
    destination: Option<NamedDestination>,
    /// The entry's own `owner`/`project`, when it named both.
    identity: Option<DrainTarget>,
}

/// Validate the `sources[]` list, or supply the built-in daemon source.
///
/// Each entry's `destination` override is parsed here so a typo is refused
/// while the drain is still disabled, exactly as the section's own is. Nothing
/// here touches git: identity resolution belongs to [`plan::build`], which runs
/// only for an enabled plan.
///
/// Test: `tests::resolve_fills_defaults`, `tests::resolve_uses_configured_sources`,
/// `tests::resolve_rejects_a_malformed_source_destination`.
fn resolve_sources(
    configured: &[LogDrainSourceConfig],
    home: &Path,
) -> Result<Vec<PreparedSource>, LogDrainConfigError> {
    if configured.is_empty() {
        return Ok(vec![PreparedSource {
            index: 0,
            enabled: true,
            source: LogSource {
                crate_name: super::CRATE_NAME.to_string(),
                root: home.join(".trusty-mpm").join(DAEMON_LOG_SUBDIR),
                include: DEFAULT_INCLUDE.iter().map(|s| (*s).to_string()).collect(),
                level_filter: Some(Level::Info),
            },
            destination: None,
            identity: None,
        }]);
    }

    configured
        .iter()
        .enumerate()
        .map(|(index, entry)| {
            let crate_name =
                non_empty(entry.crate_name.as_deref()).ok_or(LogDrainConfigError::SourceField {
                    index,
                    field: "crate_name",
                })?;
            let root =
                non_empty(entry.root.as_deref()).ok_or(LogDrainConfigError::SourceField {
                    index,
                    field: "root",
                })?;
            let level_filter = match entry.level.as_deref().map(str::trim) {
                None => None,
                Some("") => None,
                Some(raw) => {
                    Some(
                        parse_level(raw).ok_or_else(|| LogDrainConfigError::SourceLevel {
                            index,
                            value: raw.to_string(),
                        })?,
                    )
                }
            };
            let include = if entry.include.is_empty() {
                vec!["**/*".to_string()]
            } else {
                entry.include.clone()
            };
            let over = match non_empty(entry.destination.as_deref()) {
                None => None,
                Some(raw) => {
                    let uri = DestinationUri::parse(&raw).map_err(|e| {
                        LogDrainConfigError::SourceDestination {
                            index,
                            reason: e.to_string(),
                        }
                    })?;
                    Some((raw, uri))
                }
            };
            let identity = match (
                non_empty(entry.owner.as_deref()),
                non_empty(entry.project.as_deref()),
            ) {
                (None, None) => None,
                (Some(owner), Some(project)) => Some(DrainTarget { owner, project }),
                // Half an identity cannot make a key, and filling the other
                // half from git would silently mix two projects' logs.
                (owner, _) => {
                    return Err(LogDrainConfigError::SourceIdentity {
                        index,
                        crate_name,
                        root,
                        reason: format!(
                            "`{}` is set but `{}` is not",
                            if owner.is_some() { "owner" } else { "project" },
                            if owner.is_some() { "project" } else { "owner" },
                        ),
                    });
                }
            };
            Ok(PreparedSource {
                index,
                enabled: entry.enabled.unwrap_or(true),
                source: LogSource {
                    crate_name,
                    root: expand_home(&root, home),
                    include,
                    level_filter,
                },
                destination: over,
                identity,
            })
        })
        .collect()
}

/// Map a level NAME to the drain's own [`Level`].
///
/// Deliberately hand-written rather than reusing the collector's parser: that
/// one reads level tokens out of already-written log TEXT and is uppercase-only
/// and private. An operator writes `info` in YAML.
/// Test: `tests::resolve_uses_configured_sources`, `tests::resolve_rejects_an_unknown_level`.
fn parse_level(raw: &str) -> Option<Level> {
    match raw.to_ascii_lowercase().as_str() {
        "trace" => Some(Level::Trace),
        "debug" => Some(Level::Debug),
        "info" => Some(Level::Info),
        "warn" | "warning" => Some(Level::Warn),
        "error" => Some(Level::Error),
        _ => None,
    }
}

/// Trim, then discard an empty string.
fn non_empty(raw: Option<&str>) -> Option<String> {
    raw.map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// Expand a leading `~` against `home`, mirroring `workspace_root`'s handling.
fn expand_home(raw: &str, home: &Path) -> PathBuf {
    match raw.strip_prefix("~/") {
        Some(rest) => home.join(rest),
        None if raw == "~" => home.to_path_buf(),
        None => PathBuf::from(raw),
    }
}

#[cfg(test)]
mod tests;
