//! Keeping a write and a corpus rehydrate from interleaving (#10209).
//!
//! Why: a rehydrate reads the whole corpus from redb, then publishes what it
//! read into BM25, the chunk map and the entity map. A write that landed
//! between that read and that publish was overwritten by the older scanned
//! text, and a delete in the same window came back. A removal during a long
//! rehydrate also read the still-empty chunk map, deleted nothing, and
//! answered `Ok(0)`.
//! What: [`CodeIndexer::rehydrate_write_fence`], which every write holds
//! across its store mutations while a scan-then-publish holds the write side
//! of the same lock; and [`CodeIndexer::file_chunk_ids`], which reads a
//! file's ids from redb when the map is evicted. A write that arrives during
//! a rehydrate waits for it and then applies, so it is never lost.
//! Test: `indexer::rehydrate_writes_tests`.

use std::sync::atomic::Ordering;

use anyhow::{Context, Result};
use tokio::sync::RwLockReadGuard;

use super::CodeIndexer;
use crate::core::path_selection::PathSelection;

/// #10209: test-only fault seam. The redb read in
/// [`CodeIndexer::file_chunk_ids`] fails for each index id listed here.
#[cfg(test)]
pub(crate) static TEST_FAIL_FILE_ID_READ: std::sync::Mutex<Vec<String>> =
    std::sync::Mutex::new(Vec::new());

/// Test-only: one notification per indexer, fired when a write finds the
/// fence held and has to wait for it. Keyed by the indexer's
/// `rehydrate_inflight` address, as the other rehydrate hooks are.
#[cfg(test)]
static TEST_FENCE_WAITS: std::sync::Mutex<Vec<(usize, std::sync::Arc<tokio::sync::Notify>)>> =
    std::sync::Mutex::new(Vec::new());

#[cfg(test)]
fn fence_waits() -> std::sync::MutexGuard<'static, Vec<(usize, std::sync::Arc<tokio::sync::Notify>)>>
{
    TEST_FENCE_WAITS.lock().unwrap_or_else(|e| e.into_inner())
}

/// Test-only: reports when a write of ONE indexer waits on the fence.
///
/// Why: the regression tests release a paused rehydrate only once the write
/// under test is either finished (pre-fix) or blocked on the fence (fixed),
/// with no sleep deciding which.
/// Test: `indexer::rehydrate_writes_tests`.
#[cfg(test)]
pub(crate) struct FenceWaitWatch {
    key: usize,
    notify: std::sync::Arc<tokio::sync::Notify>,
}

#[cfg(test)]
impl FenceWaitWatch {
    /// Start watching `indexer`'s fence.
    pub(crate) fn arm(indexer: &CodeIndexer) -> Self {
        let key = std::sync::Arc::as_ptr(&indexer.rehydrate_inflight) as usize;
        let notify = std::sync::Arc::new(tokio::sync::Notify::new());
        let mut waits = fence_waits();
        waits.retain(|(k, _)| *k != key);
        waits.push((key, std::sync::Arc::clone(&notify)));
        Self { key, notify }
    }

    /// Resolves once a write has found the fence held.
    pub(crate) async fn contended(&self) {
        self.notify.notified().await;
    }
}

#[cfg(test)]
impl Drop for FenceWaitWatch {
    fn drop(&mut self) {
        fence_waits().retain(|(k, _)| *k != self.key);
    }
}

impl CodeIndexer {
    /// Hold this across a write's store mutations (#10209).
    ///
    /// Why: a rehydrate holds the write side of `corpus_scan_gate` from its
    /// redb read to the end of its publish, so a write holding the read side
    /// lands wholly before the read (and the scan sees it) or wholly after
    /// the publish (and overwrites it). It never lands in between.
    /// What: the read side of `corpus_scan_gate`. Writes share it with each
    /// other. Never acquire it twice in one call chain: the lock is fair, so
    /// a second read queued behind a waiting rehydrate deadlocks. Never hold
    /// it while waiting on a rehydrate (`ensure_*_loaded`).
    /// Test: `an_id_update_during_a_rehydrate_survives_its_commit`,
    /// `a_remove_file_during_a_rehydrate_is_not_resurrected`.
    pub(super) async fn rehydrate_write_fence(&self) -> RwLockReadGuard<'_, ()> {
        #[cfg(test)]
        match self.corpus_scan_gate.try_read() {
            Ok(guard) => return guard,
            Err(_) => {
                let key = std::sync::Arc::as_ptr(&self.rehydrate_inflight) as usize;
                if let Some((_, n)) = fence_waits().iter().find(|(k, _)| *k == key) {
                    n.notify_one();
                }
            }
        }
        self.corpus_scan_gate.read().await
    }

    /// Chunk ids the corpus holds for `file_path`.
    ///
    /// Why: the corpus is the authority on which of a file's chunks exist.
    /// The removal paths, `index_file`'s superseded-id pass and
    /// `service::watch_loop`'s partial-commit arm (#100) act on this set. An
    /// evicted map holds none of them, so a removal during a long rehydrate
    /// deleted nothing and answered `Ok(0)` (#10209).
    /// What: rehydrates an idle-evicted map, then reads the file's ids from
    /// the map. When the map is still evicted, adds the ids redb holds for
    /// the file. The flag is read under the map's guard: a clear flag there
    /// means the map is a whole view. A failed redb read is an `Err`, never
    /// an empty set.
    /// Test: `a_remove_file_during_a_rehydrate_is_not_resurrected`,
    /// `a_failed_durable_id_read_fails_the_remove_and_a_retry_removes_the_file`;
    /// the warm path by `partial_commit_at_cap_then_delete_leaves_no_orphan_chunks`.
    pub(crate) async fn file_chunk_ids(&self, file_path: &str) -> Result<Vec<String>> {
        self.ensure_chunks_loaded().await;
        let (mut ids, evicted) = {
            let chunks = self.chunks.read().await;
            let ids: Vec<String> = chunks
                .values()
                .filter(|c| c.file == file_path)
                .map(|c| c.id.clone())
                .collect();
            (ids, self.chunks_evicted.load(Ordering::Relaxed))
        };
        // #10209: an evicted map is not the corpus; ask redb.
        let Some(corpus) = self.corpus.clone().filter(|_| evicted) else {
            return Ok(ids);
        };
        #[cfg(test)]
        if TEST_FAIL_FILE_ID_READ
            .lock()
            .is_ok_and(|f| f.contains(&self.index_id))
        {
            anyhow::bail!("injected durable chunk-id read failure (#10209 test seam)");
        }
        let sel = PathSelection::Files(std::collections::BTreeSet::from([file_path.to_string()]));
        let rows = tokio::task::spawn_blocking(move || corpus.rows_under(&sel))
            .await
            .context("durable chunk-id read task failed")?
            .with_context(|| {
                format!(
                    "index '{}': read the chunk ids of '{file_path}' from the corpus",
                    self.index_id
                )
            })?;
        ids.extend(rows.chunks.into_iter().map(|(id, _)| id));
        ids.sort();
        ids.dedup();
        Ok(ids)
    }

    /// [`Self::file_chunk_ids`] for tests, which assert on the set directly.
    #[cfg(test)]
    pub(crate) async fn chunk_ids_for_file(&self, file_path: &str) -> Vec<String> {
        self.file_chunk_ids(file_path)
            .await
            .expect("read the file's chunk ids")
    }
}
