//! The `~/.trusty-code/log_drain.yaml` config file (#6537).
//!
//! Why: `tcode` has no crate-wide settings file today, so the drain gets its
//! own small one rather than inventing a general config system this ticket
//! does not need. Field names match trusty-mpm's `log_drain:` section
//! (`trusty_mpm::core::trusty_tools_config::log_drain::LogDrainConfig`) so an
//! operator who already knows that shape needs nothing new.
//! What: [`LogDrainConfig`] is the on-disk shape; [`load_config`] reads it,
//! defaulting to a disabled section when the file is absent. A present but
//! malformed file is an error, never a silent default — matching trusty-mpm's
//! "malformed is an error" convention (`resolve::resolve_log_drain` applies
//! the same rule to the section's CONTENT).
//! Test: `tests`.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The `log_drain.yaml` schema. Every field optional, same names as
/// trusty-mpm's `log_drain:` section.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct LogDrainConfig {
    /// Whether the scheduler runs at all. `None`/`Some(false)` → disabled.
    #[serde(default)]
    pub enabled: Option<bool>,
    /// Destination URI — `s3://bucket/prefix` or `file:///abs/path`.
    #[serde(default)]
    pub destination: Option<String>,
    /// Seconds between passes. `None` → [`super::resolve::DEFAULT_INTERVAL_SECS`].
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
    /// Repository owner, when `tcode serve` runs with no bound project (or
    /// its root has no git origin).
    #[serde(default)]
    pub owner: Option<String>,
    /// Project name, paired with [`LogDrainConfig::owner`].
    #[serde(default)]
    pub project: Option<String>,
}

/// Path of the config file: `~/.trusty-code/log_drain.yaml`.
pub fn config_path() -> PathBuf {
    crate::paths::private_state::private_state_dir().join("log_drain.yaml")
}

/// Load the config file, defaulting to a disabled section when absent.
///
/// # Errors
/// A present file that fails to parse, formatted with its path so the
/// operator can find the typo.
pub fn load_config() -> Result<LogDrainConfig, String> {
    load_from_path(&config_path())
}

/// The read-and-parse logic, over an explicit path — the test seam.
fn load_from_path(path: &Path) -> Result<LogDrainConfig, String> {
    match std::fs::read_to_string(path) {
        Ok(raw) => serde_yaml::from_str(&raw).map_err(|e| format!("{}: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(LogDrainConfig::default()),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// No file at all reads as a valid, disabled config — not an error.
    ///
    /// Why: a fresh `tcode` install must never fail to start because it has
    /// never seen this file.
    #[test]
    fn missing_file_defaults_to_disabled() {
        let dir = tempfile::tempdir().expect("tempdir");
        let missing = dir.path().join("log_drain.yaml");
        assert_eq!(load_from_path(&missing), Ok(LogDrainConfig::default()));
    }

    /// A present, malformed file is an error naming its own path.
    #[test]
    fn present_malformed_file_is_an_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("log_drain.yaml");
        std::fs::write(&path, "enabled: [this is not a bool").expect("write");
        let result = load_from_path(&path);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("log_drain.yaml"));
    }

    /// A present, well-formed file loads its fields.
    #[test]
    fn present_valid_file_loads() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("log_drain.yaml");
        std::fs::write(&path, "enabled: true\ndestination: s3://bucket/prefix\n").expect("write");
        let cfg = load_from_path(&path).expect("load");
        assert_eq!(cfg.enabled, Some(true));
        assert_eq!(cfg.destination.as_deref(), Some("s3://bucket/prefix"));
    }

    /// A well-formed file round-trips every field.
    #[test]
    fn yaml_round_trips_every_field() {
        let cfg = LogDrainConfig {
            enabled: Some(true),
            destination: Some("s3://bucket/prefix".to_string()),
            interval_secs: Some(300),
            max_file_bytes: Some(1024),
            max_wire_bytes: Some(2048),
            secrets: vec!["shh".to_string()],
            owner: Some("acme".to_string()),
            project: Some("widgets".to_string()),
        };
        let yaml = serde_yaml::to_string(&cfg).expect("serialize");
        let back: LogDrainConfig = serde_yaml::from_str(&yaml).expect("deserialize");
        assert_eq!(cfg, back);
    }

    /// Malformed YAML is a hard error, never a silent default.
    #[test]
    fn malformed_yaml_is_an_error() {
        let malformed = "enabled: [this is not a bool";
        let result: Result<LogDrainConfig, _> = serde_yaml::from_str(malformed);
        assert!(result.is_err());
    }

    /// An empty document parses to every field defaulted (absent section
    /// content, same as `LogDrainConfig::default()`).
    #[test]
    fn empty_document_defaults_every_field() {
        let cfg: LogDrainConfig = serde_yaml::from_str("{}").expect("empty doc parses");
        assert_eq!(cfg, LogDrainConfig::default());
    }
}
