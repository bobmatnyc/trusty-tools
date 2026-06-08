//! Per-project configuration parsed from `<cwd>/.trusty-search.yaml`.
//!
//! Why: a committed `.trusty-search.yaml` dotfile lets teammates share a
//! stable index `name` and extra `exclude` patterns without retyping them on
//! every `trusty-search index` invocation. CLI flags always win over the file.
//!
//! **Note on `path:`:** the `path:` field is preserved in the struct and
//! deserialised for backward-compatibility with existing config files, but it
//! is intentionally NOT consumed by `commands::index::handle_index` for root
//! selection. The registered root is always the directory the user explicitly
//! pointed at (or the CWD) — never a subdirectory narrowed by a committed
//! `path: app` entry. See `commands::index` for the design rationale.
//!
//! What: [`ProjectConfig`] is a thin all-optional struct. [`ProjectConfig::load`]
//! reads `.trusty-search.yaml` from a directory, returning `Ok(None)` when the
//! file is simply absent (the common case) and `Err` only when the file exists
//! but is malformed — so callers can cleanly distinguish "no config, use
//! defaults" from "config present but broken, fail loudly".
//!
//! This is intentionally separate from [`super::repo_config::RepoConfig`]
//! (`trusty-search.yaml`, no leading dot), which declares *multiple* named
//! index slices for polyrepos. `.trusty-search.yaml` is the single-index
//! convenience config for the common one-project-one-index case.
//!
//! Test: see the `#[cfg(test)]` block — `test_load_absent`,
//! `test_load_name_only`, `test_load_full`, `test_load_malformed`.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Dotfile name auto-detected in the current working directory by
/// `trusty-search index`.
pub const PROJECT_CONFIG_FILENAME: &str = ".trusty-search.yaml";

/// Per-project `index` defaults loaded from `.trusty-search.yaml`.
///
/// Why: every field is optional so a partial config (e.g. just `name:`) is
/// valid — missing fields fall back to the built-in `trusty-search index`
/// defaults, and any field can still be overridden by a CLI flag.
/// What: `name` overrides the directory-basename index name; `exclude` supplies
/// extra glob patterns layered on top of `.gitignore` and the built-in skip
/// list; `path` is parsed for backward-compatibility but is no longer consumed
/// for root selection (see module-level doc comment).
/// Test: round-tripped and field-checked in this module's `#[cfg(test)]` block.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ProjectConfig {
    /// Index name. Overrides the directory-basename default when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,

    /// **Deprecated — no longer used for root or crawl selection.**
    ///
    /// Previously: subdirectory to index, resolved relative to the config
    /// file's directory. This field is still parsed so existing YAML files
    /// continue to deserialise without error, but `commands::index` does not
    /// consume it; the registered root is always the CLI-supplied directory or
    /// the CWD. Remove this field from new config files.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,

    /// Extra glob exclude patterns layered on top of `.gitignore` and the
    /// built-in skip list. Absent → no extra excludes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exclude: Option<Vec<String>>,
}

impl ProjectConfig {
    /// Load `<dir>/.trusty-search.yaml`.
    ///
    /// Why: the `index` CLI handler needs to distinguish three states cleanly:
    /// file absent (`Ok(None)` → use defaults), file present and valid
    /// (`Ok(Some(_))` → merge values), and file present but malformed
    /// (`Err(_)` → abort with a clear message rather than silently ignoring a
    /// typo'd config).
    /// What: `stat` → `read_to_string` → `serde_yml::from_str`. Missing file is
    /// not an error. Read and parse failures are surfaced as `anyhow::Error`
    /// with the offending path included for context.
    /// Test: `test_load_absent`, `test_load_name_only`, `test_load_full`,
    /// `test_load_malformed`.
    pub fn load(dir: &Path) -> anyhow::Result<Option<Self>> {
        let path = dir.join(PROJECT_CONFIG_FILENAME);
        if !path.exists() {
            return Ok(None);
        }
        let raw = std::fs::read_to_string(&path)
            .map_err(|e| anyhow::anyhow!("failed to read {}: {e}", path.display()))?;
        let cfg: Self = serde_yml::from_str(&raw)
            .map_err(|e| anyhow::anyhow!("failed to parse {}: {e}", path.display()))?;
        Ok(Some(cfg))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    /// Absent file is the common case and must be a non-error `None` so the
    /// caller falls back to built-in defaults.
    #[test]
    fn test_load_absent() {
        let tmp = tempdir().unwrap();
        let res = ProjectConfig::load(tmp.path()).unwrap();
        assert!(res.is_none(), "missing config file must return Ok(None)");
    }

    /// A config with only `name:` is valid; `path` and `exclude` stay `None`.
    #[test]
    fn test_load_name_only() {
        let tmp = tempdir().unwrap();
        fs::write(tmp.path().join(PROJECT_CONFIG_FILENAME), "name: foo\n").unwrap();
        let cfg = ProjectConfig::load(tmp.path())
            .unwrap()
            .expect("config present");
        assert_eq!(cfg.name.as_deref(), Some("foo"));
        assert!(cfg.path.is_none());
        assert!(cfg.exclude.is_none());
    }

    /// All three fields parse into the expected values. The `path` field is
    /// still deserialised correctly for backward-compatibility even though
    /// `commands::index` no longer uses it for root selection.
    #[test]
    fn test_load_full() {
        let tmp = tempdir().unwrap();
        fs::write(
            tmp.path().join(PROJECT_CONFIG_FILENAME),
            r#"
name: cto
path: app
exclude:
  - data/
  - docs/
  - "*.db"
"#,
        )
        .unwrap();
        let cfg = ProjectConfig::load(tmp.path())
            .unwrap()
            .expect("config present");
        assert_eq!(cfg.name.as_deref(), Some("cto"));
        assert_eq!(cfg.path, Some(PathBuf::from("app")));
        assert_eq!(
            cfg.exclude,
            Some(vec![
                "data/".to_string(),
                "docs/".to_string(),
                "*.db".to_string(),
            ])
        );
    }

    /// Malformed YAML must return `Err`, never panic and never silently
    /// degrade to `None`.
    #[test]
    fn test_load_malformed() {
        let tmp = tempdir().unwrap();
        fs::write(
            tmp.path().join(PROJECT_CONFIG_FILENAME),
            "name: [unclosed\n  : :",
        )
        .unwrap();
        let res = ProjectConfig::load(tmp.path());
        assert!(res.is_err(), "malformed yaml must return Err, not panic");
    }
}
