//! Global user configuration for trusty-search.
//!
//! Why: provides a user-editable source of truth for which projects are indexed,
//!      taking priority over auto-discovery so power users have full control.
//!      Lives at `~/.config/trusty-search/config.yaml` so it follows the same
//!      XDG-style convention used by other developer tools and survives daemon
//!      restarts independently of the in-memory registry.
//! What: defines `GlobalConfig` and `CollectionConfig` plus YAML load/save and
//!       upsert/remove helpers used by `index`, `index remove`, and the
//!       auto-discovery scanner.
//! Test: unit tests cover load/save round-trip, missing file returns default,
//!       malformed YAML errors, upsert by name, and remove by path.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Configuration for the `trusty-search prune` command (issue #1782).
///
/// Why: idle index pruning is a destructive operation, so it defaults to
///      report-only (dry-run). Every deletion requires the explicit `--apply`
///      flag on the CLI.
/// What: holds `enabled` (future daemon sweep gate), `max_idle_days` (retention
///       threshold), and `protected_indexes` (ids that are never pruned).
///       All fields have serde defaults so existing config files that predate
///       this struct keep loading without any YAML change.
/// Test: `prune_config_defaults` and `prune_config_roundtrip` in this module.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AutoPruneConfig {
    /// Controls the future daemon-side scheduled sweep. When `false` (the
    /// default) the daemon will not automatically prune idle indexes. Has no
    /// effect on the manual `trusty-search prune --apply` command — the CLI
    /// `--apply` flag is its own opt-in and does NOT check this field.
    #[serde(default)]
    pub enabled: bool,

    /// Days of query-idle time before an index becomes eligible for pruning.
    /// Default 30. An index last searched more than this many days ago is
    /// listed as eligible. Indexes with no query timestamp at all (never
    /// searched since the daemon started tracking) are NOT eligible — they
    /// are treated as "unknown" to avoid surprising deletes on fresh installs.
    #[serde(default = "default_max_idle_days")]
    pub max_idle_days: u32,

    /// Index ids that must NEVER be pruned, even with `--apply`. Exact match
    /// against the index id string (same as the `id` field in `indexes.toml`).
    #[serde(default)]
    pub protected_indexes: Vec<String>,
}

fn default_max_idle_days() -> u32 {
    30
}

impl Default for AutoPruneConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            max_idle_days: default_max_idle_days(),
            protected_indexes: Vec::new(),
        }
    }
}

/// Top-level configuration document.
///
/// Why: keeps two distinct kinds of user state side-by-side: directories to scan
///      for auto-discovery (`scan_paths`) and explicit per-collection settings
///      (`collections`). Auto-discovery and the CLI `index remove` subcommand
///      both round-trip through this struct.
/// What: a serde-friendly shape that mirrors the YAML file exactly. All fields
///       default to empty so a partially-populated file (or a missing file) is
///       valid.
/// Test: see `config_default_is_empty`, `load_returns_default_when_missing`,
///       and `roundtrip_preserves_fields`.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct GlobalConfig {
    /// Directories whose immediate subdirectories should be probed for
    /// Claude Code / git projects. When empty, the auto-discoverer falls back
    /// to a built-in default list (`~/Projects`, `~/code`, `~/src`).
    #[serde(default)]
    pub scan_paths: Vec<PathBuf>,

    /// Explicit collection registrations. Each entry corresponds to one daemon
    /// index. The CLI registers entries here when the user runs `index` and
    /// removes them when the user runs `index remove`.
    #[serde(default)]
    pub collections: Vec<CollectionConfig>,

    /// Idle-index pruning configuration (issue #1782). All fields default to
    /// safe values (`enabled = false`, `max_idle_days = 30`), so existing
    /// config files that do not contain an `auto_prune:` key continue to
    /// load without error — they simply see the report-only defaults.
    #[serde(default)]
    pub auto_prune: AutoPruneConfig,
}

/// One explicit collection entry — a named index pointing at a directory.
///
/// Why: gives users a place to declare per-project knobs (`extensions`,
///      `exclude`, `domain_terms`) that the daemon should preserve across
///      restarts. Mirrors the fields the daemon already understands via
///      `POST /indexes`, so the CLI can later push them straight through.
/// What: thin serde record. `name` and `path` are required; the filter lists
///       all default to empty.
/// Test: `roundtrip_preserves_fields` covers full serialisation;
///       `upsert_replaces_by_name` and `remove_matches_by_canonical_path`
///       exercise the helpers below.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CollectionConfig {
    /// Stable index name (matches the daemon's `IndexId`).
    pub name: String,

    /// Absolute directory the index covers.
    pub path: PathBuf,

    /// Allow-list of file extensions (without leading `.`). Empty = all.
    #[serde(default)]
    pub extensions: Vec<String>,

    /// Additional glob patterns to skip during indexing.
    #[serde(default)]
    pub exclude: Vec<String>,

    /// Domain-specific terms surfaced to the query classifier.
    #[serde(default)]
    pub domain_terms: Vec<String>,
}

impl GlobalConfig {
    /// Path to the YAML config file.
    ///
    /// Why: anchors the config under `~/.config/trusty-search/` (or the XDG
    ///      equivalent resolved by the `dirs` crate) so it stays separate from
    ///      the daemon's runtime state under `~/.trusty-search/`.
    /// What: returns `<config_dir>/trusty-search/config.yaml`, falling back to
    ///       a process-relative path when no home directory can be resolved
    ///       (rare; CI containers and friends).
    /// Test: covered by `config_path_ends_with_expected_segments`.
    pub fn config_path() -> PathBuf {
        match dirs::config_dir() {
            Some(base) => base.join("trusty-search").join("config.yaml"),
            None => PathBuf::from("trusty-search-config.yaml"),
        }
    }

    /// Load the config from disk.
    ///
    /// Why: every call site (CLI, auto-discovery, future MCP tool) needs the
    ///      same "missing file is OK, malformed file is fatal" semantics so a
    ///      stray typo never silently degrades to defaults.
    /// What: reads `config_path()` and parses it as YAML. Returns
    ///       `GlobalConfig::default()` when the file does not exist. Surfaces
    ///       I/O and parse errors via `anyhow::Context`.
    /// Test: `load_returns_default_when_missing`, `load_errors_on_malformed`.
    pub fn load() -> Result<Self> {
        let path = Self::config_path();
        Self::load_from(&path)
    }

    /// Variant of [`Self::load`] that reads an explicit path. Used by tests so
    /// they can write a tempdir-scoped file without mutating the user's real
    /// `~/.config/trusty-search/config.yaml`.
    pub fn load_from(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("could not read {}", path.display()))?;
        if raw.trim().is_empty() {
            return Ok(Self::default());
        }
        let cfg: Self = serde_yml::from_str(&raw)
            .with_context(|| format!("could not parse {} as YAML", path.display()))?;
        Ok(cfg)
    }

    /// Persist the config to disk.
    ///
    /// Why: the CLI mutates the file when the user adds/removes collections,
    ///      and a half-written file would corrupt subsequent loads.
    /// What: writes YAML to a sibling `.tmp` file then renames over the target
    ///       so readers either see the old file or the new file, never a
    ///       partial one. Creates the parent directory if needed.
    /// Test: `roundtrip_preserves_fields`, `save_creates_parent_dir`.
    pub fn save(&self) -> Result<()> {
        let path = Self::config_path();
        self.save_to(&path)
    }

    /// Variant of [`Self::save`] that writes to an explicit path. Used by tests.
    pub fn save_to(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("could not create {}", parent.display()))?;
        }
        let yaml = serde_yml::to_string(self).context("could not serialise config as YAML")?;
        let tmp = path.with_extension("yaml.tmp");
        std::fs::write(&tmp, yaml).with_context(|| format!("could not write {}", tmp.display()))?;
        std::fs::rename(&tmp, path)
            .with_context(|| format!("could not rename {} to {}", tmp.display(), path.display()))?;
        Ok(())
    }

    /// Insert or replace a collection, matched by `name`.
    ///
    /// Why: the CLI registers a collection by running `trusty-search index`;
    ///      re-running it with the same name must update the existing entry
    ///      rather than appending a duplicate.
    /// What: linear scan over `collections` (the list is short — one entry per
    ///       project), replaces in place when `name` matches, otherwise pushes.
    /// Test: `upsert_replaces_by_name`, `upsert_appends_when_absent`.
    pub fn upsert_collection(&mut self, col: CollectionConfig) {
        if let Some(slot) = self.collections.iter_mut().find(|c| c.name == col.name) {
            *slot = col;
        } else {
            self.collections.push(col);
        }
    }

    /// Remove the first collection whose `path` matches `path` (after
    /// canonicalisation).
    ///
    /// Why: `trusty-search index remove [PATH]` resolves the target via the
    ///      CWD walk, then asks the global config to drop the matching entry.
    ///      Comparing canonical paths makes the lookup robust against trailing
    ///      slashes and relative-vs-absolute drift.
    /// What: canonicalises both the input and each stored path (best-effort —
    ///       paths that no longer exist fall back to a literal equality
    ///       check), returns the removed entry on success.
    /// Test: `remove_matches_by_canonical_path`,
    ///       `remove_returns_none_for_unknown_path`.
    pub fn remove_collection_by_path(&mut self, path: &Path) -> Option<CollectionConfig> {
        let target = canonicalise(path);
        let idx = self
            .collections
            .iter()
            .position(|c| canonicalise(&c.path) == target)?;
        Some(self.collections.remove(idx))
    }
}

/// Best-effort canonicalisation.
///
/// Why: comparing user-supplied paths against stored ones is brittle without
///      normalisation — `~/Projects/foo`, `./foo`, and `/Users/me/Projects/foo`
///      should all match the same entry. `std::fs::canonicalize` covers most
///      of that, but it fails on non-existent paths (e.g. a project the user
///      already deleted), so we fall back to the original path in that case.
/// What: returns the canonical form when available, otherwise the input as-is.
/// Test: covered transitively by `remove_matches_by_canonical_path`.
fn canonicalise(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn unique_tmp(label: &str) -> PathBuf {
        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let p = std::env::temp_dir().join(format!("trusty-config-{label}-{pid}-{nanos}"));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn config_default_is_empty() {
        let cfg = GlobalConfig::default();
        assert!(cfg.scan_paths.is_empty());
        assert!(cfg.collections.is_empty());
    }

    #[test]
    fn config_path_ends_with_expected_segments() {
        let p = GlobalConfig::config_path();
        let s = p.to_string_lossy();
        assert!(
            s.ends_with("trusty-search/config.yaml") || s.ends_with("trusty-search-config.yaml"),
            "unexpected config path: {s}"
        );
    }

    #[test]
    fn load_returns_default_when_missing() {
        let dir = unique_tmp("missing");
        let path = dir.join("does-not-exist.yaml");
        let cfg = GlobalConfig::load_from(&path).unwrap();
        assert_eq!(cfg, GlobalConfig::default());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_errors_on_malformed() {
        let dir = unique_tmp("malformed");
        let path = dir.join("config.yaml");
        fs::write(&path, "not: : : valid").unwrap();
        let err = GlobalConfig::load_from(&path).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("YAML") || msg.contains("yaml"), "msg={msg}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn roundtrip_preserves_fields() {
        let dir = unique_tmp("roundtrip");
        let path = dir.join("config.yaml");
        let cfg = GlobalConfig {
            scan_paths: vec![PathBuf::from("/tmp/projects")],
            collections: vec![CollectionConfig {
                name: "myproj".into(),
                path: PathBuf::from("/tmp/projects/myproj"),
                extensions: vec!["rs".into(), "toml".into()],
                exclude: vec!["target/".into()],
                domain_terms: vec!["embedding".into()],
            }],
            ..Default::default()
        };
        cfg.save_to(&path).unwrap();
        let loaded = GlobalConfig::load_from(&path).unwrap();
        assert_eq!(cfg, loaded);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_creates_parent_dir() {
        let dir = unique_tmp("parent");
        let path = dir.join("nested").join("inner").join("config.yaml");
        GlobalConfig::default().save_to(&path).unwrap();
        assert!(path.exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn upsert_replaces_by_name() {
        let mut cfg = GlobalConfig::default();
        cfg.upsert_collection(CollectionConfig {
            name: "a".into(),
            path: PathBuf::from("/old"),
            extensions: vec![],
            exclude: vec![],
            domain_terms: vec![],
        });
        cfg.upsert_collection(CollectionConfig {
            name: "a".into(),
            path: PathBuf::from("/new"),
            extensions: vec!["rs".into()],
            exclude: vec![],
            domain_terms: vec![],
        });
        assert_eq!(cfg.collections.len(), 1);
        assert_eq!(cfg.collections[0].path, PathBuf::from("/new"));
        assert_eq!(cfg.collections[0].extensions, vec!["rs".to_string()]);
    }

    #[test]
    fn upsert_appends_when_absent() {
        let mut cfg = GlobalConfig::default();
        cfg.upsert_collection(CollectionConfig {
            name: "a".into(),
            path: PathBuf::from("/a"),
            extensions: vec![],
            exclude: vec![],
            domain_terms: vec![],
        });
        cfg.upsert_collection(CollectionConfig {
            name: "b".into(),
            path: PathBuf::from("/b"),
            extensions: vec![],
            exclude: vec![],
            domain_terms: vec![],
        });
        assert_eq!(cfg.collections.len(), 2);
    }

    #[test]
    fn remove_matches_by_canonical_path() {
        let dir = unique_tmp("remove");
        let project = dir.join("proj");
        fs::create_dir_all(&project).unwrap();
        let mut cfg = GlobalConfig::default();
        cfg.upsert_collection(CollectionConfig {
            name: "proj".into(),
            path: project.clone(),
            extensions: vec![],
            exclude: vec![],
            domain_terms: vec![],
        });
        let removed = cfg.remove_collection_by_path(&project);
        assert!(removed.is_some());
        assert_eq!(removed.unwrap().name, "proj");
        assert!(cfg.collections.is_empty());
        let _ = fs::remove_dir_all(&dir);
    }

    /// Why: pins the default values for `AutoPruneConfig` so a missing
    ///      `auto_prune:` key in `config.yaml` always loads as report-only.
    /// What: constructs a default and asserts each field.
    /// Test: this test.
    #[test]
    fn prune_config_defaults() {
        let cfg = AutoPruneConfig::default();
        assert!(!cfg.enabled, "default must be disabled (report-only)");
        assert_eq!(cfg.max_idle_days, 30);
        assert!(cfg.protected_indexes.is_empty());
    }

    /// Why: `GlobalConfig` with an `auto_prune:` block must survive a
    ///      save/load round-trip without data loss.
    /// What: serialises a `GlobalConfig` with a non-default `auto_prune` block
    ///       and asserts it loads back with the same values.
    /// Test: this test.
    #[test]
    fn prune_config_roundtrip() {
        let dir = unique_tmp("prune-rt");
        let path = dir.join("config.yaml");
        let cfg = GlobalConfig {
            auto_prune: AutoPruneConfig {
                enabled: true,
                max_idle_days: 14,
                protected_indexes: vec!["trusty-tools".into()],
            },
            ..Default::default()
        };
        cfg.save_to(&path).unwrap();
        let loaded = GlobalConfig::load_from(&path).unwrap();
        assert_eq!(cfg, loaded);
        let _ = fs::remove_dir_all(&dir);
    }

    /// Why: existing `config.yaml` files without `auto_prune:` must keep
    ///      loading correctly after the field is added to `GlobalConfig`.
    /// What: write a YAML file with no `auto_prune` key, load it, assert the
    ///       field defaults to `AutoPruneConfig::default()`.
    /// Test: this test.
    #[test]
    fn prune_config_missing_key_loads_as_default() {
        let dir = unique_tmp("prune-missing");
        let path = dir.join("config.yaml");
        fs::write(&path, "scan_paths: []\ncollections: []\n").unwrap();
        let cfg = GlobalConfig::load_from(&path).unwrap();
        assert_eq!(cfg.auto_prune, AutoPruneConfig::default());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn remove_returns_none_for_unknown_path() {
        let mut cfg = GlobalConfig::default();
        cfg.upsert_collection(CollectionConfig {
            name: "a".into(),
            path: PathBuf::from("/a"),
            extensions: vec![],
            exclude: vec![],
            domain_terms: vec![],
        });
        assert!(cfg
            .remove_collection_by_path(Path::new("/nowhere"))
            .is_none());
        assert_eq!(cfg.collections.len(), 1);
    }
}
