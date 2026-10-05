//! The filesystem side of the project resolver (#9169): root classification,
//! corpus recency (reindex stamp, then mtime), query canonicalisation and the
//! git-identity fallback.
//!
//! Why: the resolver runs in the free lane with no deadline, so one dead
//! network or `/Volumes` root must not stall it. Every disk touch goes through
//! [`Disk`], and [`super::resolve`] calls it only for the matched group and the
//! nearest candidates — never for every registered root.
//! What: [`LiveDisk`] is the production [`Disk`]; [`classify_root_kind`] is the
//! ruling-f7 root classifier it uses.
//! Test: `project_resolve_tests.rs`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};

use trusty_common::workspace_layout::WorktreeDirNames;

use super::{Candidate, RootKind};
use crate::core::registry::IndexHandle;
use crate::service::storage_layout::{StorageLayout, REDB_FILE};

/// How long a stamp read waits for a resident index's indexer lock before it
/// gives up and the resolver falls back to the mtime.
const RESIDENT_LOCK_WAIT: Duration = Duration::from_secs(2);

/// Every filesystem read the resolver makes, so tests can count or fake them.
pub trait Disk {
    /// How ruling f7 classifies `c`'s root.
    fn kind(&self, c: &Candidate) -> RootKind;
    /// When a reindex last committed `c`'s corpus; `None` when unstamped or
    /// unreadable.
    fn reindexed_unix(&self, c: &Candidate) -> Option<u64>;
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
    /// The loaded indexes by id. A resident corpus is read through its open
    /// store, never reopened.
    pub resident: HashMap<String, Arc<IndexHandle>>,
    /// The daemon runtime, used from a blocking thread to take the indexer
    /// lock and the per-path corpus open gate. `None` reads no stamp.
    pub runtime: Option<tokio::runtime::Handle>,
}

impl LiveDisk {
    /// The index's `index.redb`, found through the registry's own layout
    /// without creating anything; `None` for a corpus on `/Volumes`.
    fn corpus_path(&self, c: &Candidate) -> Option<PathBuf> {
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
        (!on_external_volume(&corpus)).then_some(corpus)
    }
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

    /// Why: ruling f7's "most recently indexed" is the reindex stamp a commit
    /// writes into the corpus `_meta` (#9169); the mtime cannot say it, since
    /// redb rewrites the file on every open.
    /// What: a resident index is read through the store its indexer holds,
    /// after waiting at most [`RESIDENT_LOCK_WAIT`] for the indexer lock. Any
    /// other index is opened read-only inside `open_serialized`, the same
    /// per-path gate the daemon's own loads take, so a load waits for this
    /// read rather than failing on its lock, and the read never changes the
    /// file. Nothing is held past the call. `/Volumes` corpora are not read.
    /// Every failure is `None`, and the resolver falls back to the mtime.
    /// Test: `a_reload_of_the_older_corpus_does_not_make_it_the_newest`.
    fn reindexed_unix(&self, c: &Candidate) -> Option<u64> {
        let runtime = self.runtime.as_ref()?;
        let read = if let Some(handle) = self.resident.get(&c.index_id) {
            let store = runtime.block_on(async {
                let indexer = tokio::time::timeout(RESIDENT_LOCK_WAIT, handle.indexer.read())
                    .await
                    .ok()?;
                indexer.corpus_store()
            })?;
            store.read_reindexed_unix_sync()
        } else {
            let corpus = self.corpus_path(c)?;
            let path = corpus.clone();
            runtime.block_on(crate::core::corpus::open_serialized(&corpus, move || {
                crate::core::corpus::read_reindexed_unix_at(&path)
            }))
        };
        read.inspect_err(|e| {
            tracing::debug!(index_id = %c.index_id, "project.resolve: no reindex stamp: {e:#}");
        })
        .ok()
        .flatten()
    }

    /// Why: the fallback recency for a corpus with no reindex stamp.
    /// What: the mtime of the index's `index.redb`. redb rewrites the file
    /// when it is opened, so the value is "last written or loaded". A
    /// corpus on `/Volumes` is not read.
    /// Test: `the_most_recently_written_corpus_wins_between_two_main_checkouts`.
    fn corpus_modified_unix(&self, c: &Candidate) -> Option<u64> {
        let corpus = self.corpus_path(c)?;
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
