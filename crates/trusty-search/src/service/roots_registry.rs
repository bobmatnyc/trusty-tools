//! Tracked-roots registry: a lightweight TOML file listing every project root
//! that has been registered for colocated `.trusty-search/` storage.
//!
//! Why: colocated storage scatters index data across project trees instead of
//! centralising it in a single data directory. The daemon must still know which
//! roots to scan at startup (without crawling the entire filesystem). The roots
//! registry (`<data_dir>/roots.toml`) holds that set. It is intentionally
//! minimal — just absolute `PathBuf` entries — because all per-index metadata
//! lives inside the per-project `.trusty-search/` directory (discovered at scan
//! time, not here).
//!
//! What: functions to load/save/upsert/remove tracked project roots, stored as
//! a TOML file with a single `[[root]]` array. The file is written atomically
//! (write-tmp + rename) so crashes mid-write never produce a partial file.
//!
//! Test: `roots_roundtrip`, `roots_upsert_dedupes`, `roots_remove_idempotent`.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::service::persistence::data_dir;

/// One entry in the roots registry.
///
/// Why: wrapping `PathBuf` in a struct enables the `[[root]]` TOML
/// array-of-tables syntax and leaves room for future per-root metadata
/// (e.g. scan-depth overrides) without breaking the file format.
/// What: currently only stores the absolute `path` of the project root.
/// Test: serialised/deserialised in `roots_roundtrip`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TrackedRoot {
    /// Absolute canonical path to the project root.
    pub path: PathBuf,
}

/// TOML wrapper holding the `[[root]]` array.
///
/// Why: required by TOML's array-of-tables syntax — the array must be a field
/// on a top-level struct, not the root value.
/// What: `[[root]]` → `roots` vec of `TrackedRoot`.
/// Test: round-trip through `toml::to_string_pretty` / `toml::from_str`.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
struct RootsFile {
    #[serde(default, rename = "root")]
    roots: Vec<TrackedRoot>,
}

/// Resolve the path to the roots registry file.
///
/// Why: centralise the file-name decision so all callers agree on `roots.toml`
/// inside the platform data dir.
/// What: returns `<data_dir>/roots.toml`.
/// Test: path-injectable variant used in unit tests to avoid touching real data dir.
pub fn roots_toml_path() -> Result<PathBuf> {
    Ok(data_dir()?.join("roots.toml"))
}

/// Load the tracked-roots list. Missing file → empty list (first-run case).
///
/// Why: treat a missing file as "no roots registered yet" rather than an error,
/// matching the same convention as `load_index_registry`.
/// What: reads the TOML file, returns parsed entries. Corrupted file logs a
/// warning and returns empty.
/// Test: `roots_roundtrip`.
pub fn load_roots() -> Result<Vec<TrackedRoot>> {
    load_roots_at(&roots_toml_path()?)
}

/// Path-injectable variant for tests.
pub(crate) fn load_roots_at(path: &Path) -> Result<Vec<TrackedRoot>> {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e).context("read roots.toml"),
    };
    match toml::from_str::<RootsFile>(&content) {
        Ok(f) => Ok(f.roots),
        Err(e) => {
            tracing::warn!(
                "roots.toml at {} is corrupt ({e}); starting with empty roots list",
                path.display()
            );
            Ok(Vec::new())
        }
    }
}

/// Persist the roots list atomically.
///
/// Why: write-tmp + rename ensures crash mid-write never corrupts the file.
/// What: serialises to TOML, writes to a sibling `.tmp` file, renames.
/// Test: `roots_roundtrip`.
pub fn save_roots(roots: &[TrackedRoot]) -> Result<()> {
    save_roots_at(&roots_toml_path()?, roots)
}

/// Path-injectable variant for tests.
pub(crate) fn save_roots_at(path: &Path, roots: &[TrackedRoot]) -> Result<()> {
    let file = RootsFile {
        roots: roots.to_vec(),
    };
    let serialised = toml::to_string_pretty(&file).context("serialise roots.toml")?;
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, &serialised).context("write roots.toml.tmp")?;
    std::fs::rename(&tmp, path).context("rename roots.toml.tmp → roots.toml")?;
    Ok(())
}

/// Register `root` as a tracked project root. Idempotent — adding the same
/// path twice does not produce a duplicate entry.
///
/// Why: called by `trusty-search index <path>` so the daemon knows to scan
/// this root for `.trusty-search/` directories on the next startup.
/// What: load → dedupe by path → save. Cheap; the file is tiny.
/// Test: `roots_upsert_dedupes`.
pub fn upsert_root(root: PathBuf) -> Result<()> {
    upsert_root_at(&roots_toml_path()?, root)
}

/// Path-injectable variant for tests.
pub(crate) fn upsert_root_at(path: &Path, root: PathBuf) -> Result<()> {
    let mut roots = load_roots_at(path)?;
    if roots.iter().any(|r| r.path == root) {
        return Ok(());
    }
    roots.push(TrackedRoot { path: root });
    save_roots_at(path, &roots)
}

/// Remove a root from the tracked list. No-op when the path is absent.
///
/// Why: called by `trusty-search index remove` and `trusty-search migrate
/// storage` after relocating a legacy index so the old path is not re-scanned.
/// What: load → retain(path != root) → save when anything changed.
/// Test: `roots_remove_idempotent`.
pub fn remove_root(root: &Path) -> Result<()> {
    remove_root_at(&roots_toml_path()?, root)
}

/// Path-injectable variant for tests.
pub(crate) fn remove_root_at(path: &Path, root: &Path) -> Result<()> {
    let mut roots = load_roots_at(path)?;
    let before = roots.len();
    roots.retain(|r| r.path != root);
    if roots.len() == before {
        return Ok(());
    }
    save_roots_at(path, &roots)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    #[test]
    fn roots_roundtrip() {
        // Why: serialise → deserialise must be lossless, and the file must
        // contain the canonical `[[root]]` sections humans can read/edit.
        let tmp = NamedTempFile::new().unwrap();
        let path = tmp.path().to_path_buf();

        let roots = vec![
            TrackedRoot {
                path: PathBuf::from("/projects/alpha"),
            },
            TrackedRoot {
                path: PathBuf::from("/projects/beta"),
            },
        ];
        save_roots_at(&path, &roots).unwrap();
        let loaded = load_roots_at(&path).unwrap();
        assert_eq!(loaded, roots);

        // The TOML file must contain [[root]] array-of-tables syntax.
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(
            content.contains("[[root]]"),
            "roots.toml must use [[root]] syntax; got: {content}"
        );
    }

    #[test]
    fn roots_upsert_dedupes() {
        // Why: calling `upsert_root` twice with the same path must not produce
        // duplicate entries — the roots list is a set, not a bag.
        let tmp = NamedTempFile::new().unwrap();
        let path = tmp.path().to_path_buf();

        upsert_root_at(&path, PathBuf::from("/projects/alpha")).unwrap();
        upsert_root_at(&path, PathBuf::from("/projects/alpha")).unwrap();
        let loaded = load_roots_at(&path).unwrap();
        assert_eq!(loaded.len(), 1, "duplicate insert must be a no-op");
        assert_eq!(loaded[0].path, PathBuf::from("/projects/alpha"));
    }

    #[test]
    fn roots_remove_idempotent() {
        // Why: removing a path that is not in the list must be a silent no-op.
        let tmp = NamedTempFile::new().unwrap();
        let path = tmp.path().to_path_buf();

        upsert_root_at(&path, PathBuf::from("/projects/alpha")).unwrap();
        upsert_root_at(&path, PathBuf::from("/projects/beta")).unwrap();
        assert_eq!(load_roots_at(&path).unwrap().len(), 2);

        // Remove one that exists.
        remove_root_at(&path, Path::new("/projects/alpha")).unwrap();
        let after = load_roots_at(&path).unwrap();
        assert_eq!(after.len(), 1);
        assert_eq!(after[0].path, PathBuf::from("/projects/beta"));

        // Remove the same one again — must be a no-op.
        remove_root_at(&path, Path::new("/projects/alpha")).unwrap();
        assert_eq!(load_roots_at(&path).unwrap().len(), 1);

        // Remove a path that was never there.
        remove_root_at(&path, Path::new("/projects/gamma")).unwrap();
        assert_eq!(load_roots_at(&path).unwrap().len(), 1);
    }

    #[test]
    fn missing_roots_file_returns_empty() {
        // Why: a missing `roots.toml` (first run) must be treated as "no roots
        // yet", not an error — same convention as `load_index_registry`.
        let tmp_dir = tempfile::tempdir().unwrap();
        let nonexistent = tmp_dir.path().join("roots.toml");
        let roots = load_roots_at(&nonexistent).unwrap();
        assert!(roots.is_empty());
    }
}
