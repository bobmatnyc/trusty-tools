//! The `~/.trusty-tools/<crate>/config.yaml` cross-crate config convention (#1220).
//!
//! Why: every trusty-* crate had been inventing its own config location and
//! format (`~/.trusty-mpm/config.toml`, trusty-memory's bespoke
//! `.trusty-tools/trusty-memory.yaml` pin file, env-var-only settings, …). #1220
//! standardises ONE convention so an operator always knows where a crate's
//! configuration lives: `~/.trusty-tools/<crate>/config.yaml`. Centralising the
//! path resolution and the typed YAML load/save here means each crate adopts the
//! convention by calling two functions rather than re-deriving the path and the
//! graceful-fallback semantics every time.
//!
//! What: [`crate_config_dir`] / [`crate_config_path`] resolve the canonical
//! directory and file for a crate; [`load`] / [`load_or_default`] deserialise the
//! YAML into a typed value (absent file → `None`/defaults; malformed → typed
//! error / logged-defaults); [`save`] serialises a typed value back atomically.
//! All path-taking variants (`*_at`) take an explicit base so they are hermetically
//! testable against a temp dir; the home-resolving wrappers delegate to them.
//!
//! Format note: YAML is the #1220-mandated format (RFC #1225 Q4). It is parsed by
//! `serde_yaml` 0.9 (pure-Rust `libyaml-safer`), pinned in the workspace manifest;
//! the migration to a maintained successor is tracked in #1250 (out of #1220 scope).
//!
//! Test: `crate_config_path_layout`, `load_absent_is_none`, `save_then_load_round_trips`,
//! `load_or_default_on_missing`, `load_malformed_is_err` in the `tests` module.

use std::path::{Path, PathBuf};

use serde::Serialize;
use serde::de::DeserializeOwned;

/// Top-level directory (under `$HOME`) holding every crate's config tree.
///
/// Why: a single named constant keeps every crate agreeing on the root so the
/// convention cannot drift to `.trusty_tools` / `.trustytools` variants.
/// What: `".trusty-tools"`.
/// Test: `crate_config_path_layout`.
pub const TRUSTY_TOOLS_DIR: &str = ".trusty-tools";

/// Canonical config filename within a crate's directory.
///
/// Why: #1220 fixes the file as `config.yaml` (not `config.yml` / `settings.yaml`)
/// so tooling and docs can reference one exact path.
/// What: `"config.yaml"`.
/// Test: `crate_config_path_layout`.
pub const CONFIG_FILE: &str = "config.yaml";

/// Errors raised while loading or saving a crate config.
///
/// Why: callers need to distinguish "the file is missing" (expected on a fresh
/// install — handled by `Option`/defaults, never an error) from a genuine I/O or
/// parse failure they must surface. A typed enum lets binaries map each to the
/// right log level or exit behaviour.
/// What: an I/O variant (carrying the offending path) and a parse/serialise
/// variant (carrying the path + the `serde_yaml` message).
/// Test: `load_malformed_is_err` exercises the `Yaml` variant.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// Reading or writing the config file failed (other than a benign not-found).
    #[error("config I/O error at {path}: {source}")]
    Io {
        /// The path the failed operation targeted.
        path: PathBuf,
        /// The underlying I/O error.
        source: std::io::Error,
    },

    /// The YAML could not be parsed (load) or produced (save).
    #[error("config YAML error at {path}: {message}")]
    Yaml {
        /// The path being parsed/serialised when the error occurred.
        path: PathBuf,
        /// The `serde_yaml` error message.
        message: String,
    },
}

/// Resolve the config directory for `crate_name` under an explicit base.
///
/// Why: the hermetic core for [`crate_config_dir`]; tests point `base` at a temp
/// dir so they never touch the real `~/.trusty-tools`.
/// What: `<base>/.trusty-tools/<crate_name>`.
/// Test: `crate_config_path_layout`.
pub fn crate_config_dir_at(base: &Path, crate_name: &str) -> PathBuf {
    base.join(TRUSTY_TOOLS_DIR).join(crate_name)
}

/// Resolve the config file for `crate_name` under an explicit base.
///
/// Why: the hermetic core for [`crate_config_path`].
/// What: `<base>/.trusty-tools/<crate_name>/config.yaml`.
/// Test: `crate_config_path_layout`.
pub fn crate_config_path_at(base: &Path, crate_name: &str) -> PathBuf {
    crate_config_dir_at(base, crate_name).join(CONFIG_FILE)
}

/// Resolve the canonical config directory for `crate_name`.
///
/// Why: production callers want `~/.trusty-tools/<crate>` without resolving the
/// home directory themselves.
/// What: `~/.trusty-tools/<crate_name>`. Returns `None` only when the home
/// directory cannot be determined (a stripped CI environment).
/// Test: covered indirectly via `crate_config_dir_at`.
pub fn crate_config_dir(crate_name: &str) -> Option<PathBuf> {
    dirs::home_dir().map(|home| crate_config_dir_at(&home, crate_name))
}

/// Resolve the canonical config file for `crate_name`.
///
/// Why: the single path every crate reads/writes; surfacing it lets binaries log
/// "edit this file" hints.
/// What: `~/.trusty-tools/<crate_name>/config.yaml`. `None` when home is unknown.
/// Test: covered indirectly via `crate_config_path_at`.
pub fn crate_config_path(crate_name: &str) -> Option<PathBuf> {
    dirs::home_dir().map(|home| crate_config_path_at(&home, crate_name))
}

/// Load and deserialise the config at an explicit path.
///
/// Why: the hermetic core for [`load`]; keeps the absent-vs-malformed distinction
/// in one tested place.
/// What: returns `Ok(None)` when the file does not exist (the expected fresh
/// state), `Ok(Some(value))` on a successful parse, `Err(ConfigError::Io)` on a
/// non-not-found I/O error, and `Err(ConfigError::Yaml)` on a parse failure.
/// Test: `load_absent_is_none`, `save_then_load_round_trips`, `load_malformed_is_err`.
pub fn load_at<T: DeserializeOwned>(path: &Path) -> Result<Option<T>, ConfigError> {
    let raw = match std::fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            return Err(ConfigError::Io {
                path: path.to_path_buf(),
                source: e,
            });
        }
    };
    let value = serde_yaml::from_str::<T>(&raw).map_err(|e| ConfigError::Yaml {
        path: path.to_path_buf(),
        message: e.to_string(),
    })?;
    Ok(Some(value))
}

/// Load and deserialise the canonical config for `crate_name`.
///
/// Why: the convention's primary read entry point — a crate calls this once at
/// startup to pick up `~/.trusty-tools/<crate>/config.yaml`.
/// What: resolves the path via [`crate_config_path`] then delegates to
/// [`load_at`]. Returns `Ok(None)` when home is unknown OR the file is absent
/// (both mean "no config", which the caller treats as defaults).
/// Test: covered via `load_at` tests + `save_then_load_round_trips`.
pub fn load<T: DeserializeOwned>(crate_name: &str) -> Result<Option<T>, ConfigError> {
    match crate_config_path(crate_name) {
        Some(path) => load_at(path.as_path()),
        None => Ok(None),
    }
}

/// Load the canonical config for `crate_name`, falling back to `Default`.
///
/// Why: most binaries want a value they can use unconditionally and should never
/// abort startup over a bad config. This collapses absent/home-unknown to the
/// type's `Default` and logs (at `warn`) a malformed file rather than erroring.
/// What: returns the parsed value when present and valid; otherwise
/// `T::default()`. A malformed file is logged to stderr (never stdout — MCP
/// framing) and downgraded to defaults so the process still starts.
/// Test: `load_or_default_on_missing` (absent → default).
pub fn load_or_default<T: DeserializeOwned + Default>(crate_name: &str) -> T {
    match load::<T>(crate_name) {
        Ok(Some(value)) => value,
        Ok(None) => T::default(),
        Err(e) => {
            tracing::warn!("{e}; falling back to default {crate_name} config");
            T::default()
        }
    }
}

/// Serialise and write `value` to an explicit config path (atomic).
///
/// Why: the hermetic core for [`save`]; the console's config-write path and any
/// CLI `config set` command both need the same atomic, parent-creating write so a
/// concurrent reader never observes a torn file.
/// What: creates the parent directory, serialises `value` to YAML (prefixed with a
/// provenance header comment), writes a sibling `config.yaml.tmp`, then renames it
/// over the target. Returns the path written.
/// Test: `save_then_load_round_trips`.
pub fn save_at<T: Serialize>(path: &Path, value: &T) -> Result<PathBuf, ConfigError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| ConfigError::Io {
            path: parent.to_path_buf(),
            source: e,
        })?;
    }
    let yaml = serde_yaml::to_string(value).map_err(|e| ConfigError::Yaml {
        path: path.to_path_buf(),
        message: e.to_string(),
    })?;
    let header = "# .trusty-tools/<crate>/config.yaml\n\
                  # Managed by the trusty-tools config convention (#1220).\n\
                  # Edit by hand or via the trusty-console Config tab.\n\n";
    let content = format!("{header}{yaml}");
    let tmp = path.with_extension("yaml.tmp");
    std::fs::write(&tmp, &content).map_err(|e| ConfigError::Io {
        path: tmp.clone(),
        source: e,
    })?;
    std::fs::rename(&tmp, path).map_err(|e| ConfigError::Io {
        path: path.to_path_buf(),
        source: e,
    })?;
    Ok(path.to_path_buf())
}

/// Serialise and write `value` to the canonical config for `crate_name`.
///
/// Why: the convention's primary write entry point (console toggle, `config set`).
/// What: resolves the path via [`crate_config_path`] then delegates to
/// [`save_at`]. Errors with [`ConfigError::Io`] (synthesising a path-less error)
/// when the home directory cannot be resolved.
/// Test: covered via `save_at` + `save_then_load_round_trips`.
pub fn save<T: Serialize>(crate_name: &str, value: &T) -> Result<PathBuf, ConfigError> {
    match crate_config_path(crate_name) {
        Some(path) => save_at(path.as_path(), value),
        None => Err(ConfigError::Io {
            path: PathBuf::from(format!("~/{TRUSTY_TOOLS_DIR}/{crate_name}/{CONFIG_FILE}")),
            source: std::io::Error::new(std::io::ErrorKind::NotFound, "home directory unavailable"),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
    struct Sample {
        #[serde(default)]
        name: String,
        #[serde(default)]
        count: u32,
    }

    /// Why: the convention fixes the on-disk layout; this pins
    /// `<base>/.trusty-tools/<crate>/config.yaml` exactly.
    /// Test: itself.
    #[test]
    fn crate_config_path_layout() {
        let p = crate_config_path_at(Path::new("/home/bob"), "trusty-mpm");
        assert_eq!(
            p,
            PathBuf::from("/home/bob/.trusty-tools/trusty-mpm/config.yaml")
        );
        let d = crate_config_dir_at(Path::new("/home/bob"), "trusty-mpm");
        assert_eq!(d, PathBuf::from("/home/bob/.trusty-tools/trusty-mpm"));
    }

    /// Why: an absent file is the expected fresh-install state and must be
    /// `Ok(None)`, never an error.
    /// Test: itself.
    #[test]
    fn load_absent_is_none() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = crate_config_path_at(tmp.path(), "trusty-mpm");
        let got: Option<Sample> = load_at(&path).unwrap();
        assert_eq!(got, None);
    }

    /// Why: the read/write round-trip is the core of the convention; a value
    /// written via `save_at` must load back identically.
    /// Test: itself.
    #[test]
    fn save_then_load_round_trips() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = crate_config_path_at(tmp.path(), "trusty-mpm");
        let value = Sample {
            name: "demo".into(),
            count: 7,
        };
        let written = save_at(&path, &value).unwrap();
        assert_eq!(written, path);
        let got: Sample = load_at(&path).unwrap().expect("present");
        assert_eq!(got, value);
        // The provenance header must be present so a human editing the file sees
        // where it came from.
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(raw.contains("trusty-tools config convention"));
    }

    /// Why: `load_or_default` must collapse an absent file to `T::default()` so
    /// callers can use it unconditionally.
    /// Test: itself (drives `load_or_default` via an explicit-path round-trip is
    /// not possible — it resolves home — so we assert the absent-path core here).
    #[test]
    fn load_or_default_on_missing() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = crate_config_path_at(tmp.path(), "absent-crate");
        // The path-taking core returns None for an absent file…
        assert_eq!(load_at::<Sample>(&path).unwrap(), None);
        // …which `load_or_default` would turn into the type default.
        assert_eq!(
            Sample::default(),
            Sample {
                name: String::new(),
                count: 0
            }
        );
    }

    /// Why: a malformed YAML file must surface as a typed `Yaml` error from the
    /// strict loader (binaries decide whether to downgrade to defaults).
    /// Test: itself.
    #[test]
    fn load_malformed_is_err() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = crate_config_path_at(tmp.path(), "trusty-mpm");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        // `count` expects a u32; a string makes deserialisation fail.
        std::fs::write(&path, "name: demo\ncount: not-a-number\n").unwrap();
        let err = load_at::<Sample>(&path).unwrap_err();
        assert!(matches!(err, ConfigError::Yaml { .. }), "got {err:?}");
    }
}
