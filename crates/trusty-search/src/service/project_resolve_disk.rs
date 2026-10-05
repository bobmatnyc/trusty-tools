//! The filesystem side of the project resolver (#9169): root classification,
//! corpus recency, query canonicalisation and the git-identity fallback.
//!
//! Why: the resolver runs in the free lane with no deadline, so one dead
//! network or `/Volumes` root must not stall it. Every disk touch goes through
//! [`Disk`], and [`super::resolve`] calls it only for the matched group and the
//! nearest candidates — never for every registered root.
//! What: [`LiveDisk`] is the production [`Disk`]; [`classify_root_kind`] is the
//! ruling-f7 root classifier it uses.
//! Test: `project_resolve_tests.rs`.

use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use trusty_common::workspace_layout::WorktreeDirNames;

use super::{Candidate, RootKind};
use crate::service::storage_layout::{StorageLayout, REDB_FILE};

/// Every filesystem read the resolver makes, so tests can count or fake them.
pub trait Disk {
    /// How ruling f7 classifies `c`'s root.
    fn kind(&self, c: &Candidate) -> RootKind;
    /// When `c`'s index was last written, as a unix time; `None` when unknown.
    fn corpus_modified_unix(&self, c: &Candidate) -> Option<u64>;
    /// The query path, canonicalised; `None` when that is not possible.
    fn canonicalize(&self, path: &Path) -> Option<PathBuf>;
    /// The canonical repo identity of a path outside every registered root.
    fn derive_identity(&self, path: &Path) -> Option<String>;
}

/// The production [`Disk`]: real `stat`s, with `/Volumes` roots left alone.
pub struct LiveDisk {
    /// The worktree base directory names [`classify_root_kind`] matches.
    pub names: WorktreeDirNames,
}

/// True for a path on an external volume, which is never `stat`ed: a
/// mounted-but-TCC-blocked volume can hang the call (same rule as
/// `orphan_reaper::is_reapable_orphan`).
fn on_external_volume(path: &Path) -> bool {
    path.starts_with("/Volumes")
}

impl Disk for LiveDisk {
    fn kind(&self, c: &Candidate) -> RootKind {
        classify_root_kind(&c.root_path, &self.names)
    }

    /// Why: ruling f7's "most recently indexed" needs a value every real index
    /// has. `PersistedIndex::last_indexed_unix` has no production writer
    /// (#4391), so it is `None` on every live row.
    /// What: the mtime of the index's redb corpus (`index.redb`), found through
    /// the registry's own layout (colocated or data dir) without creating
    /// anything. A reindex writes it batch by batch; redb also rewrites its
    /// header when the daemon opens it, so the value is "last written or
    /// loaded". A colocated corpus on `/Volumes` is not read.
    /// Test: `the_most_recently_written_corpus_wins_between_two_main_checkouts`.
    fn corpus_modified_unix(&self, c: &Candidate) -> Option<u64> {
        if c.colocated && on_external_volume(&c.root_path) {
            return None;
        }
        let layout = if c.colocated {
            StorageLayout::Colocated
        } else {
            StorageLayout::DataDir
        };
        let corpus = layout
            .existing_file(&c.index_id, &c.root_path, REDB_FILE)
            .ok()
            .flatten()?;
        let modified = std::fs::metadata(corpus).ok()?.modified().ok()?;
        modified
            .duration_since(UNIX_EPOCH)
            .ok()
            .map(|d| d.as_secs())
    }

    fn canonicalize(&self, path: &Path) -> Option<PathBuf> {
        if on_external_volume(path) {
            return None;
        }
        std::fs::canonicalize(path).ok()
    }

    fn derive_identity(&self, path: &Path) -> Option<String> {
        if on_external_volume(path) {
            return None;
        }
        trusty_common::repo_identity::RepoIdentity::derive(path).map(|id| id.canonical())
    }
}

/// Classify `root` for ruling f7.
///
/// What: a path component naming a worktree base (`.worktrees`, the configured
/// base, or `.claude/worktrees`) is a worktree whether or not it still exists;
/// a `/Volumes` root is indeterminate and never `stat`ed; otherwise a missing
/// root is orphaned, a `.git` file marks a linked worktree, a `.git` directory
/// a main checkout, and no `.git` at all a subdirectory (or plain) checkout.
/// Test: `classify_root_kind_reads_the_git_entry_and_the_worktree_base`,
/// `a_volumes_root_is_indeterminate_and_never_probed_for_another_project`.
pub fn classify_root_kind(root: &Path, worktree_names: &WorktreeDirNames) -> RootKind {
    let mut previous: Option<&std::ffi::OsStr> = None;
    for component in root.components() {
        let name = component.as_os_str();
        let claude_worktree = previous == Some(std::ffi::OsStr::new(".claude"))
            && name == std::ffi::OsStr::new("worktrees");
        if claude_worktree || name.to_str().is_some_and(|n| worktree_names.matches(n)) {
            return RootKind::Worktree;
        }
        previous = Some(name);
    }
    // #9169: a dead external volume must not hang the free lane.
    if on_external_volume(root) {
        return RootKind::Indeterminate;
    }
    // One `stat` of `.git` answers both questions; only a miss needs the root.
    match std::fs::metadata(root.join(".git")) {
        Ok(meta) if meta.is_file() => RootKind::Worktree,
        Ok(meta) if meta.is_dir() => RootKind::MainCheckout,
        _ if matches!(
            std::fs::symlink_metadata(root),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound
        ) =>
        {
            RootKind::Orphaned
        }
        _ => RootKind::Checkout,
    }
}
