//! Find kuzu-memory stores on disk (#277).
//!
//! Why: the owner asked for auto-discovery, and a store lives wherever a
//! project was initialised — `<project>/.kuzu-memory/memories.db`, anywhere
//! under the user's home directory.
//! What: [`discover`] walks each root breadth-first to a depth limit, the way
//! `find <root> -maxdepth N -name .kuzu-memory` would. It never follows a
//! symlink, so a link loop cannot hang it, and it reports every unreadable
//! directory with the reason instead of failing the walk.
//! Test: `discovery_finds_nested_stores_and_reports_unreadable`,
//! `discovery_does_not_follow_symlink_loops`, `resolve_from_accepts_dir_or_db`.

use std::collections::{BTreeSet, VecDeque};
use std::path::{Path, PathBuf};

use super::KuzuImportError;

/// The directory kuzu-memory creates in each project.
pub const STORE_DIR_NAME: &str = ".kuzu-memory";
/// The KuzuDB database inside [`STORE_DIR_NAME`].
pub const STORE_DB_NAME: &str = "memories.db";

/// Directory names the walk never descends into: large trees that cannot
/// hold a project's store, so skipping them keeps a `$HOME` walk fast.
const PRUNED: &[&str] = &[
    ".git",
    "node_modules",
    "target",
    ".venv",
    "venv",
    "__pycache__",
    ".cargo",
    ".rustup",
    ".Trash",
    "Library",
];

/// One kuzu-memory store found on disk.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct DiscoveredStore {
    /// The `.kuzu-memory` directory.
    pub dir: PathBuf,
    /// The `memories.db` database inside it.
    pub db: PathBuf,
}

impl DiscoveredStore {
    /// The project directory that owns the store (the parent of `dir`).
    pub fn project_dir(&self) -> &Path {
        self.dir.parent().unwrap_or(&self.dir)
    }
}

/// A path the walk could not use, with the reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkippedPath {
    pub path: PathBuf,
    pub reason: String,
}

/// Everything one discovery pass found.
#[derive(Debug, Default)]
pub struct Discovery {
    pub stores: Vec<DiscoveredStore>,
    pub skipped: Vec<SkippedPath>,
}

/// Walk `roots` to `max_depth` and collect every kuzu-memory store.
///
/// Why: a store can sit in any project under `$HOME`; the operator should not
/// have to list them.
/// What: breadth-first over each root. An entry at depth `d` (the root's
/// children are depth 1) is examined when `d <= max_depth`; a directory is
/// entered only when `d < max_depth`. Symlinks are never followed, directory
/// names in [`PRUNED`] are never entered, and a `.kuzu-memory` directory is
/// never walked into. A `.kuzu-memory` directory with no `memories.db` is
/// reported as skipped. Stores are de-duplicated by canonical path, so
/// overlapping roots report each store once, and returned in sorted order.
/// Test: `discovery_finds_nested_stores_and_reports_unreadable`,
/// `discovery_does_not_follow_symlink_loops`.
pub fn discover(roots: &[PathBuf], max_depth: usize) -> Discovery {
    let mut found = BTreeSet::new();
    let mut skipped = Vec::new();
    for root in roots {
        walk_root(root, max_depth, &mut found, &mut skipped);
    }
    Discovery {
        stores: found.into_iter().collect(),
        skipped,
    }
}

fn walk_root(
    root: &Path,
    max_depth: usize,
    found: &mut BTreeSet<DiscoveredStore>,
    skipped: &mut Vec<SkippedPath>,
) {
    let mut queue = VecDeque::from([(root.to_path_buf(), 0usize)]);
    while let Some((dir, depth)) = queue.pop_front() {
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(e) => {
                skipped.push(SkippedPath {
                    path: dir,
                    reason: format!("unreadable: {e}"),
                });
                continue;
            }
        };
        for entry in entries.flatten() {
            // `DirEntry::file_type` does not follow symlinks, so a link to a
            // directory reports as a symlink and is never entered.
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if !kind.is_dir() {
                continue;
            }
            let path = entry.path();
            let name = entry.file_name();
            if name == STORE_DIR_NAME {
                match store_in_dir(&path) {
                    Some(store) => {
                        found.insert(store);
                    }
                    None => skipped.push(SkippedPath {
                        path,
                        reason: format!("no {STORE_DB_NAME} inside"),
                    }),
                }
                continue;
            }
            let child_depth = depth + 1;
            if child_depth < max_depth && !PRUNED.iter().any(|p| name == *p) {
                queue.push_back((path, child_depth));
            }
        }
    }
}

/// The store rooted at a `.kuzu-memory` directory, when its database exists.
fn store_in_dir(dir: &Path) -> Option<DiscoveredStore> {
    let db = dir.join(STORE_DB_NAME);
    if !db.exists() {
        return None;
    }
    let dir = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
    let db = dir.join(STORE_DB_NAME);
    Some(DiscoveredStore { dir, db })
}

/// Resolve a `--from` argument to a store.
///
/// Why: operators point at whichever they have to hand — the `.kuzu-memory`
/// directory or the `memories.db` inside it.
/// What: a directory must contain `memories.db`; a path named `memories.db`
/// resolves to its parent directory. Anything else is
/// [`KuzuImportError::NotAStore`].
/// Test: `resolve_from_accepts_dir_or_db`.
pub fn resolve_from(from: &Path) -> Result<DiscoveredStore, KuzuImportError> {
    if from.file_name().is_some_and(|n| n == STORE_DB_NAME) && from.exists() {
        if let Some(parent) = from.parent() {
            if let Some(store) = store_in_dir(parent) {
                return Ok(store);
            }
        }
    }
    if from.is_dir() {
        if let Some(store) = store_in_dir(from) {
            return Ok(store);
        }
    }
    Err(KuzuImportError::NotAStore(from.to_path_buf()))
}
