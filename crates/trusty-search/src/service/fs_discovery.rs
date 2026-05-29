//! Filesystem-based discovery of colocated `.trusty-search/` index directories.
//!
//! Why: with colocated storage each index lives at `<root>/.trusty-search/`
//! instead of the global data directory, so the daemon cannot discover indexes
//! by reading a single `indexes.toml`. Instead it scans each registered root
//! (from `roots.toml`) for `.trusty-search/` directories, including
//! NESTED ones (a project index and its sub-indexes are all found — pairs with
//! the directory-structure nesting from issue #404).
//!
//! What: `scan_roots_for_colocated_indexes` takes a list of tracked project
//! roots and returns one `ColocatedIndexEntry` for each `.trusty-search/` dir
//! found, whether at the root itself or recursively inside it.
//!
//! Test: `discovery_finds_root_and_nested`, `discovery_skips_missing_root`,
//! `discovery_dedupes_by_root_path`.

use std::path::{Path, PathBuf};

use crate::service::colocated_storage::COLOCATED_DIR_NAME;

/// A discovered colocated index: the root path that owns the `.trusty-search/`
/// directory and the index id derived from it.
///
/// Why: pairs the root path (used to open redb/usearch) with a stable index id
/// (derived from the canonical path) so the IndexRegistry can register it.
/// What: `root_path` is the directory that CONTAINS `.trusty-search/`; `id` is
/// a stable, human-readable string derived from the canonical path.
/// Test: `discovery_finds_root_and_nested` checks both fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColocatedIndexEntry {
    /// The project root that owns the `.trusty-search/` directory.
    pub root_path: PathBuf,
    /// Stable index id derived from the canonical root path.
    pub id: String,
}

/// Derive a stable, filesystem-safe index id from a canonical root path.
///
/// Why: index ids must be stable across daemon restarts (they are the key in
/// the in-memory DashMap and in HNSW snapshots). Using the canonical path
/// (symlink-resolved) means two aliases to the same directory produce the same
/// id. Characters that would be unsafe as filesystem path components are
/// replaced with `_` to keep the id readable in logs.
/// What: strips the leading separator, replaces `/`, `\`, `:`, space and
/// control characters with `_`, and truncates to 200 chars.
/// Test: `id_from_path_is_stable_and_safe`.
pub fn id_from_path(path: &Path) -> String {
    let raw = path
        .to_string_lossy()
        .trim_start_matches('/')
        .trim_start_matches('\\')
        .to_string();
    let safe: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect();
    // Truncate to a safe limit so the id can be used in log lines without flooding.
    safe.chars().take(200).collect()
}

/// Scan a list of tracked project roots for `.trusty-search/` directories,
/// recursively descending into subdirectories.
///
/// Why: a root may contain nested sub-projects (issue #404 directory-structure
/// nesting). Recursive scanning ensures both the root's own `.trusty-search/`
/// and any nested `.trusty-search/` dirs are discovered in a single pass.
///
/// What: for each root in `tracked_roots`:
/// - Skip roots that do not exist on disk (log at debug).
/// - Walk the directory tree up to `max_depth` levels deep.
/// - For each entry named `.trusty-search` that is a directory, emit a
///   `ColocatedIndexEntry` whose `root_path` is the parent of `.trusty-search`.
/// - Prune the recursion into `.trusty-search/` itself and into `.git/` to
///   avoid scanning inside binary/VCS artefact directories.
///
/// Duplicates (same canonical root_path from two different registered roots,
/// e.g. via a symlink) are deduplicated by root_path.
///
/// Test: `discovery_finds_root_and_nested`, `discovery_skips_missing_root`.
pub fn scan_roots_for_colocated_indexes(
    tracked_roots: &[PathBuf],
    max_depth: usize,
) -> Vec<ColocatedIndexEntry> {
    let mut seen: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
    let mut results = Vec::new();

    for root in tracked_roots {
        let canonical_root = match root.canonicalize() {
            Ok(c) => c,
            Err(e) => {
                tracing::debug!(
                    "fs_discovery: skipping root {} (canonicalize: {e})",
                    root.display()
                );
                continue;
            }
        };

        scan_dir_recursive(
            &canonical_root,
            &canonical_root,
            0,
            max_depth,
            &mut seen,
            &mut results,
        );
    }

    results
}

/// Recursive helper that descends into `dir` looking for `.trusty-search/`
/// subdirectories.
///
/// Why: depth-first scan with pruning on `.trusty-search/` and `.git/` keeps
/// the scan fast on large project trees.
/// What: checks if `dir` itself contains a `.trusty-search/` child (emitting
/// an entry if so), then descends into each non-pruned subdirectory.
/// Test: covered by `discovery_finds_root_and_nested`.
fn scan_dir_recursive(
    dir: &Path,
    original_root: &Path,
    depth: usize,
    max_depth: usize,
    seen: &mut std::collections::HashSet<PathBuf>,
    results: &mut Vec<ColocatedIndexEntry>,
) {
    // Check if this dir has a .trusty-search child.
    let ts_dir = dir.join(COLOCATED_DIR_NAME);
    if ts_dir.exists() && ts_dir.is_dir() {
        // The root_path of the index is `dir` (the parent of .trusty-search/).
        let root_path = dir.to_path_buf();
        if seen.insert(root_path.clone()) {
            let id = id_from_path(&root_path);
            tracing::debug!(
                "fs_discovery: found colocated index at {} (id={id})",
                root_path.display()
            );
            results.push(ColocatedIndexEntry { root_path, id });
        }
    }

    if depth >= max_depth {
        return;
    }

    // Descend into subdirectories, pruning known-bad dirs.
    let read_dir = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(e) => {
            tracing::debug!(
                "fs_discovery: cannot read dir {} ({e}) — skipping",
                dir.display()
            );
            return;
        }
    };

    for entry in read_dir.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        // Prune: do not descend into .trusty-search/ itself, .git/, or any
        // hidden dir other than known project markers — keeps the scan cheap.
        if name == COLOCATED_DIR_NAME || name == ".git" || name == "node_modules" {
            continue;
        }
        // Only scan subdirectories that are children of the original tracked
        // root, not symlinks that escape it.
        let canonical_path = match path.canonicalize() {
            Ok(c) => c,
            Err(_) => continue,
        };
        if !canonical_path.starts_with(original_root) {
            continue;
        }
        scan_dir_recursive(
            &canonical_path,
            original_root,
            depth + 1,
            max_depth,
            seen,
            results,
        );
    }
}

/// Default recursion depth for `scan_roots_for_colocated_indexes`.
///
/// Why: scanning infinitely deep would be slow and dangerous on large project
/// trees. 5 levels is deep enough for a typical monorepo layout
/// (`root/services/api/frontend/src`).
pub const DEFAULT_SCAN_DEPTH: usize = 5;

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn make_colocated(root: &Path) {
        let ts = root.join(".trusty-search");
        fs::create_dir_all(&ts).unwrap();
    }

    #[test]
    fn id_from_path_is_stable_and_safe() {
        // Why: the id must be the same for the same path across restarts and
        // must not contain characters that would break log lines or shell use.
        let id = id_from_path(Path::new("/Users/bob/Projects/my-project"));
        assert!(!id.contains('/'), "id must not contain /");
        assert!(!id.contains(' '), "id must not contain spaces");
        // Calling twice must give the same result.
        assert_eq!(
            id,
            id_from_path(Path::new("/Users/bob/Projects/my-project"))
        );
    }

    #[test]
    fn discovery_finds_root_and_nested() {
        // Why: a root with a .trusty-search/ at the top level AND another one
        // in a nested project must both be discovered in a single scan.
        let tmp = tempdir().unwrap();
        let root = tmp.path().to_path_buf();

        // Top-level colocated index.
        make_colocated(&root);

        // Nested project.
        let nested = root.join("services").join("api");
        fs::create_dir_all(&nested).unwrap();
        make_colocated(&nested);

        let roots = vec![root.clone()];
        let found = scan_roots_for_colocated_indexes(&roots, DEFAULT_SCAN_DEPTH);

        let root_paths: Vec<_> = found.iter().map(|e| &e.root_path).collect();
        let canon_root = root.canonicalize().unwrap();
        let canon_nested = nested.canonicalize().unwrap();

        assert!(
            root_paths.contains(&&canon_root),
            "top-level root must be found; got: {root_paths:?}"
        );
        assert!(
            root_paths.contains(&&canon_nested),
            "nested project must be found; got: {root_paths:?}"
        );
        assert_eq!(found.len(), 2, "exactly two indexes must be found");
    }

    #[test]
    fn discovery_skips_missing_root() {
        // Why: a tracked root that has been deleted must not cause a panic or
        // error — just a debug-level log and a skip.
        let nonexistent = PathBuf::from("/tmp/trusty-test-definitely-does-not-exist-xyz123");
        let found = scan_roots_for_colocated_indexes(&[nonexistent], DEFAULT_SCAN_DEPTH);
        assert!(found.is_empty(), "missing root must produce no results");
    }

    #[test]
    fn discovery_dedupes_by_root_path() {
        // Why: if two entries in tracked_roots resolve to the same canonical
        // path (e.g. via a symlink), only one index entry must be emitted.
        let tmp = tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        make_colocated(&root);

        // Pass the same root twice.
        let roots = vec![root.clone(), root.clone()];
        let found = scan_roots_for_colocated_indexes(&roots, DEFAULT_SCAN_DEPTH);
        assert_eq!(
            found.len(),
            1,
            "duplicate root must not produce duplicate entries"
        );
    }

    #[test]
    fn discovery_does_not_descend_into_trusty_search() {
        // Why: the scanner must not treat a sub-dir inside .trusty-search/ as
        // a project root, even if it happens to contain another .trusty-search/.
        let tmp = tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        make_colocated(&root);

        // Create a .trusty-search inside the .trusty-search dir (edge case).
        let nested_ts = root.join(".trusty-search").join(".trusty-search");
        fs::create_dir_all(&nested_ts).unwrap();

        let found = scan_roots_for_colocated_indexes(&[root], DEFAULT_SCAN_DEPTH);
        assert_eq!(
            found.len(),
            1,
            "inner .trusty-search must not be discovered"
        );
    }
}
