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
    DEFAULT_MAX_FILE_BYTES, DEFAULT_MAX_WIRE_BYTES, DestinationScheme, DestinationUri, Level,
    LogSource,
};

use super::TrustyToolsConfig;

/// Default interval between drain passes, in seconds.
///
/// 15 minutes, per the #6533 design brief. Log bytes are a diagnostic aid, not
/// a live telemetry stream, so a slower cadence trades freshness for a smaller
/// object-store bill and fewer wakeups on a laptop.
pub const DEFAULT_INTERVAL_SECS: u64 = 900;

/// Directory under `~/.trusty-mpm/` holding the drain's manifest cache, the
/// persisted session id, and the last-run status the doctor row reads.
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

    /// GitHub login to upload under. `None` → resolved once via `gh api user`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub github_id: Option<String>,

    /// Session segment of the key layout. `None` → the persisted per-install id.
    ///
    /// See `daemon::log_drain::resolve_session_id` for why the daemon's session
    /// is per-INSTALL rather than per-boot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,

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
/// `include` the globs relative to it, `level` the minimum line level kept.
/// Test: `tests::resolve_uses_configured_sources`,
/// `tests::resolve_rejects_a_source_with_no_root`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct LogDrainSourceConfig {
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

/// A validated, runnable drain plan.
///
/// Why: the scheduler and the doctor row both need the same resolved answer,
/// and re-deriving it in two places is how the two disagree.
/// What: everything `run_once` needs except the identity, which the scheduler
/// resolves at tick time.
/// Test: `tests::resolve_fills_defaults`.
///
/// Deliberately not `PartialEq`: `LogSource` (a `trusty-common` type) is not,
/// and tests assert on the individual fields they care about rather than on
/// whole-plan equality.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct ResolvedLogDrain {
    /// The parsed destination.
    pub destination: DestinationUri,
    /// The destination as the operator wrote it, for log and doctor messages.
    pub destination_display: String,
    /// How long the scheduler sleeps between passes.
    pub interval: Duration,
    /// Plaintext source ceiling handed to `DrainConfig`.
    pub max_file_bytes: u64,
    /// Compressed-body ceiling handed to `DrainConfig` (#6547).
    pub max_wire_bytes: u64,
    /// Extra literal secrets to scrub.
    pub secrets: Vec<String>,
    /// Operator-pinned GitHub login, when one was configured.
    pub github_id: Option<String>,
    /// Operator-pinned session segment, when one was configured.
    pub session_id: Option<String>,
    /// The directories to collect.
    pub sources: Vec<LogSource>,
}

impl ResolvedLogDrain {
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
/// Test: `tests::resolve_disabled_when_section_absent`,
/// `tests::resolve_fills_defaults`,
/// `tests::resolve_rejects_a_malformed_destination`,
/// `tests::resolve_validates_even_while_disabled`.
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

    let sources = resolve_sources(&section.sources, home)?;

    let Some((destination_display, destination)) = destination else {
        // Reached only when `destination` is absent: a present-but-malformed
        // one already returned above, disabled or not.
        return if enabled {
            Err(LogDrainConfigError::MissingDestination)
        } else {
            Ok(LogDrainSetting::Disabled)
        };
    };

    if !enabled {
        return Ok(LogDrainSetting::Disabled);
    }

    Ok(LogDrainSetting::Enabled(Box::new(ResolvedLogDrain {
        destination,
        destination_display,
        interval: Duration::from_secs(interval_secs),
        max_file_bytes,
        max_wire_bytes,
        secrets: section.secrets.clone(),
        github_id: non_empty(section.github_id.as_deref()),
        session_id: non_empty(section.session_id.as_deref()),
        sources,
    })))
}

/// Validate the `sources[]` list, or supply the built-in daemon source.
///
/// Test: `tests::resolve_fills_defaults`, `tests::resolve_uses_configured_sources`.
fn resolve_sources(
    configured: &[LogDrainSourceConfig],
    home: &Path,
) -> Result<Vec<LogSource>, LogDrainConfigError> {
    if configured.is_empty() {
        return Ok(vec![LogSource {
            crate_name: super::CRATE_NAME.to_string(),
            root: home.join(".trusty-mpm").join(DAEMON_LOG_SUBDIR),
            include: DEFAULT_INCLUDE.iter().map(|s| (*s).to_string()).collect(),
            level_filter: Some(Level::Info),
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
            Ok(LogSource {
                crate_name,
                root: expand_home(&root, home),
                include,
                level_filter,
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
