//! Overwrite guard for the legacy `chunks.json` snapshot (#7920).
//!
//! Why: the #1711 guard refuses to write an empty HNSW index over a populated
//! snapshot, but `chunks.json` had no equivalent. A shutdown flush resolves its
//! target path from disk at shutdown time, so an indexer whose in-memory corpus
//! was loaded from one location (or never loaded at all) could write that
//! empty or foreign state over a populated snapshot it had never read — the
//! 39-byte `{"version":1,"chunks":[],"entities":[]}` file in #7920.
//! What: [`SnapshotGuard`] remembers every snapshot path this indexer loaded or
//! wrote, and refuses a write when the file on disk holds chunks AND either the
//! in-memory corpus is empty or the path is not one this indexer owns. Every
//! refusal is counted and returned as [`SnapshotOverwriteRefused`].
//! Test: `core::indexer::tests::snapshot_guard_7920`.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use super::CodeIndexer;

/// A `chunks.json` write refused because it would destroy a populated snapshot.
///
/// Why: callers must be able to tell a refusal from an I/O failure, and a test
/// must be able to assert the refusal arm without matching log text.
/// What: carries the target, what the file holds, the in-memory chunk count,
/// and the reason the write was refused.
#[derive(Debug, thiserror::Error)]
#[error(
    "refusing to overwrite populated chunk snapshot {} ({on_disk}) with {in_memory} \
     in-memory chunk(s): {reason}. The on-disk snapshot is preserved (#7920)",
    path.display()
)]
pub struct SnapshotOverwriteRefused {
    pub path: PathBuf,
    pub on_disk: String,
    pub in_memory: usize,
    pub reason: &'static str,
}

/// What the file at a snapshot path currently holds.
enum OnDisk {
    /// Missing or zero bytes — nothing to destroy.
    Empty,
    Chunks(usize),
    /// Present but not a readable snapshot; treated as populated.
    Unreadable(u64),
}

/// Minimal parse shape: counts chunk entries without materialising them.
#[derive(serde::Deserialize)]
struct CountedSnapshot {
    #[serde(default)]
    chunks: Vec<serde::de::IgnoredAny>,
}

fn inspect(path: &Path) -> OnDisk {
    let len = match std::fs::metadata(path) {
        Ok(m) => m.len(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return OnDisk::Empty,
        Err(_) => return OnDisk::Unreadable(0),
    };
    if len == 0 {
        return OnDisk::Empty;
    }
    match std::fs::read(path) {
        Ok(bytes) => match serde_json::from_slice::<CountedSnapshot>(&bytes) {
            Ok(c) if c.chunks.is_empty() => OnDisk::Empty,
            Ok(c) => OnDisk::Chunks(c.chunks.len()),
            Err(_) => OnDisk::Unreadable(len),
        },
        Err(_) => OnDisk::Unreadable(len),
    }
}

/// Per-indexer ownership record and refusal counter for `chunks.json` writes.
///
/// Why: see the module doc. Shared behind an `Arc` so the detached
/// incremental-persist task applies the same decision as the shutdown flush.
/// What: `owned` holds paths this indexer loaded or wrote; `refused` counts
/// refusals for the lifetime of the indexer.
#[derive(Debug, Default)]
pub(crate) struct SnapshotGuard {
    owned: Mutex<HashSet<PathBuf>>,
    refused: AtomicU64,
}

impl SnapshotGuard {
    /// Record that this indexer's in-memory corpus now describes `path`.
    pub(crate) fn mark_owned(&self, path: &Path) {
        self.owned
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(path.to_path_buf());
    }

    fn owns(&self, path: &Path) -> bool {
        self.owned
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains(path)
    }

    /// Decide whether `in_memory` chunks may replace the snapshot at `path`.
    ///
    /// Why: see the module doc.
    /// What: allows the write when this indexer owns `path` and holds at least
    /// one chunk, or when the file holds nothing. Otherwise counts the refusal,
    /// logs it at ERROR, and returns [`SnapshotOverwriteRefused`]. Reads the
    /// file only on the refusal-candidate path.
    /// Test: `shutdown_flush_refuses_empty_corpus_over_populated_chunks_json`,
    /// `shutdown_flush_refuses_foreign_partial_corpus`,
    /// `chunks_added_after_load_are_persisted`.
    pub(crate) fn check_overwrite(
        &self,
        index_id: &str,
        path: &Path,
        in_memory: usize,
    ) -> Result<(), SnapshotOverwriteRefused> {
        let owned = self.owns(path);
        if owned && in_memory > 0 {
            return Ok(());
        }
        let on_disk = match inspect(path) {
            OnDisk::Empty => return Ok(()),
            OnDisk::Chunks(n) => format!("{n} chunk(s) on disk"),
            OnDisk::Unreadable(bytes) => format!("unreadable, {bytes} bytes on disk"),
        };
        let reason = if in_memory == 0 {
            "the in-memory corpus is empty"
        } else {
            "this indexer never loaded or wrote that file, so its in-memory corpus \
             describes a different location"
        };
        let refused = self.refused.fetch_add(1, Ordering::Relaxed) + 1;
        let err = SnapshotOverwriteRefused {
            path: path.to_path_buf(),
            on_disk,
            in_memory,
            reason,
        };
        tracing::error!(
            index_id = %index_id,
            refused_snapshot_overwrites = refused,
            "index '{index_id}': {err}. Move the file aside if it is genuinely stale."
        );
        Err(err)
    }

    pub(crate) fn refused(&self) -> u64 {
        self.refused.load(Ordering::Relaxed)
    }
}

impl CodeIndexer {
    /// Number of `chunks.json` writes refused by the #7920 overwrite guard.
    pub fn refused_snapshot_overwrites(&self) -> u64 {
        self.snapshot_guard.refused()
    }
}
