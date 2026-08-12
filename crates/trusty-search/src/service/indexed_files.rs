//! Service-level mapping from `PathBuf` → chunk IDs currently in the index.
//!
//! Why: `CodeIndexer` exposes `add_chunk` / `remove_chunk` keyed by chunk ID,
//! but the [`crate::service::watcher::FileWatcher`] only knows file paths. We must
//! remember which chunk IDs a given file produced so a subsequent
//! `WatchEvent::Removed` can drop them. Tracking this in core would couple
//! the indexer to filesystem semantics; keeping it at the service layer keeps
//! `trusty-search-core` filesystem-agnostic.
//!
//! What: A thread-safe `HashMap<PathBuf, Vec<String>>` behind an `RwLock`.
//! Insert is `O(chunks)`; lookup is `O(1)`; takes ownership of the chunk-id
//! list on removal so the caller can iterate without holding the lock.
//!
//! Test: covered indirectly via [`crate::service::watch_loop`] integration tests.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::sync::RwLock;

/// Maps each indexed file path to the chunk IDs it produced. Cloning is cheap
/// (just bumps the `Arc`); the same handle is shared across the watch loop
/// and any other caller that needs to surrender chunks for a given file.
#[derive(Clone, Default)]
pub struct IndexedFiles {
    inner: Arc<RwLock<HashMap<PathBuf, Vec<String>>>>,
}

impl IndexedFiles {
    /// Construct an empty tracker.
    pub fn new() -> Self {
        Self::default()
    }

    /// Record that `path` currently owns `chunk_ids`. Replaces any prior list
    /// — callers should remove the old chunks first if they want them dropped.
    pub async fn record(&self, path: PathBuf, chunk_ids: Vec<String>) {
        self.inner.write().await.insert(path, chunk_ids);
    }

    /// Remove the mapping for `path`, returning the chunk IDs that were
    /// associated with it (if any). The returned Vec can be used to drive
    /// per-chunk removal from the indexer.
    pub async fn take(&self, path: &Path) -> Option<Vec<String>> {
        self.inner.write().await.remove(path)
    }

    /// Snapshot of every file path currently tracked.
    ///
    /// Why: the dropped-event reconcile in [`crate::service::watch_rescan`] must
    /// find files that were deleted while the OS event queue was overflowing.
    /// Those deletions are unrecoverable from the event stream, so the only way
    /// to spot them is to compare what we believe is indexed against what the
    /// walker still finds on disk.
    /// What: clones the key set under a read lock so the caller can iterate
    /// without holding it (removal takes the write lock).
    /// Test: `rescan_reconcile_drops_files_deleted_during_the_gap` in
    /// `crate::service::watch_rescan_tests`.
    pub async fn paths(&self) -> Vec<PathBuf> {
        self.inner.read().await.keys().cloned().collect()
    }

    /// Number of distinct files currently tracked. Test-only accessor.
    #[cfg(test)]
    pub async fn len(&self) -> usize {
        self.inner.read().await.len()
    }

    /// Whether no files are tracked. Test-only accessor.
    #[cfg(test)]
    pub async fn is_empty(&self) -> bool {
        self.inner.read().await.is_empty()
    }
}
