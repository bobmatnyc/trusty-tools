//! Replacing a file's prior chunks on a single-file write (#8959).
//!
//! Why: `index_file` upserts by chunk id, and an id embeds the chunk's line
//! span or symbol name, so an edit that shifted lines or renamed a symbol left
//! the old-id chunks searchable beside the new ones. Removing them must hold
//! up under three failure shapes: a failed redb delete (the old ids came back
//! at the next boot), a retry after that failure (the ids were already gone
//! from memory, so the retry never looked for them), and two writes to the
//! same path racing (each removed only the ids it saw, so both versions
//! stayed).
//! What: the superseded-id computation; a removal that deletes from redb
//! first and fails the write on a redb error with memory untouched; and a
//! striped per-path lock that `index_file` holds from the superseded-id
//! snapshot through the commit.
//! Test: `index_file_replaces_a_files_prior_chunks`,
//! `a_failed_superseded_delete_fails_the_write_and_stays_retryable`,
//! `a_rewrite_after_a_failed_delete_leaves_no_old_ids_after_reopen`,
//! `a_failed_tombstone_delete_fails_the_write_and_leaves_no_old_ids_after_reopen`,
//! `a_failed_sops_purge_fails_the_write_and_leaves_no_plaintext_after_reopen` and
//! `concurrent_writes_to_one_path_keep_only_the_last_version` in
//! `indexer::tests::file_lifecycle_8959`.

use anyhow::Result;

use crate::core::chunker::RawChunk;

use super::super::CodeIndexer;

/// #8959: test-only fault seam. A redb chunk delete fails for each index id
/// listed here. Keyed by index id so concurrent tests never see each other's
/// faults.
#[cfg(test)]
pub(crate) static TEST_FAIL_CHUNK_DELETE: std::sync::Mutex<Vec<String>> =
    std::sync::Mutex::new(Vec::new());

/// How a file removal treats a failed redb chunk delete.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RedbChunkDelete {
    /// Drop from memory, then delete from redb and only log a failure.
    /// #9212: test-only; every production delete fails closed.
    #[cfg(test)]
    WarnOnly,
    /// #8959: delete from redb first; a failure is returned with memory
    /// untouched, so a retry finds the same ids.
    FailClosed,
}

/// Number of lock stripes; two paths sharing a stripe only serialize.
const PATH_LOCK_STRIPES: usize = 64;

/// Striped per-path write locks for `index_file` (#8959).
///
/// Why: a write snapshots the path's ids before its embed await, so two
/// concurrent writes to one path each removed only the ids they saw and both
/// versions stayed indexed.
/// What: a fixed array of async mutexes indexed by a hash of the path. A
/// stripe is held for the whole write, so same-path writes run one at a time
/// and the later one sees the earlier one's ids.
#[derive(Debug)]
pub(crate) struct PathWriteLocks {
    stripes: Vec<tokio::sync::Mutex<()>>,
    /// #10738: path writes finished, counted as each guard drops.
    released: std::sync::atomic::AtomicU64,
}

impl Default for PathWriteLocks {
    fn default() -> Self {
        Self {
            stripes: (0..PATH_LOCK_STRIPES)
                .map(|_| tokio::sync::Mutex::new(()))
                .collect(),
            released: std::sync::atomic::AtomicU64::new(0),
        }
    }
}

impl PathWriteLocks {
    /// Lock the stripe that `path` hashes to.
    pub(crate) async fn lock(&self, path: &str) -> PathWriteGuard<'_> {
        use std::hash::{Hash as _, Hasher as _};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        path.hash(&mut hasher);
        let stripe = usize::try_from(hasher.finish() % PATH_LOCK_STRIPES as u64).unwrap_or(0);
        PathWriteGuard {
            _stripe: self.stripes[stripe].lock().await,
            released: &self.released,
        }
    }
}

/// The guard [`CodeIndexer::lock_path_for_write`] returns (#10738).
///
/// Why: an incremental reindex copies the live corpus, then swaps staging in;
/// a path write that lands between the two is missing from staging (#839).
/// What: holds the path's stripe; on drop, counts one finished path write, so
/// the swap can tell whether one finished after its copy began.
/// Test: `a_write_in_the_carryover_window_raises_the_generation`.
pub(crate) struct PathWriteGuard<'a> {
    _stripe: tokio::sync::MutexGuard<'a, ()>,
    released: &'a std::sync::atomic::AtomicU64,
}

impl Drop for PathWriteGuard<'_> {
    fn drop(&mut self) {
        // #10738: counted while the stripe is still held, before the caller's
        // indexer guard can drop, so a swap under the write lock sees it.
        self.released
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }
}

impl CodeIndexer {
    /// Take `path`'s write lock: the one every incremental write to it holds.
    ///
    /// Why (#10738): the put core keeps the path's content hash in step with
    /// its chunks, so it holds this lock across forget, write and record.
    /// What: locks the stripe `path` hashes to; not reentrant, so the holder
    /// writes through [`Self::index_file_outcome_path_locked`].
    /// Test: `concurrent_puts_to_one_path_store_the_hash_of_the_committed_chunks`.
    pub(crate) async fn lock_path_for_write(&self, path: &str) -> PathWriteGuard<'_> {
        self.path_write_locks.lock(path).await
    }

    /// Path writes finished on this indexer since it was built (#10738).
    ///
    /// Why: every path write holds the indexer's read guard for its whole call,
    /// so a reader of this under the write guard sees every finished write.
    /// Test: `a_write_in_the_carryover_window_raises_the_generation`.
    pub(crate) fn path_writes_finished(&self) -> u64 {
        self.path_write_locks
            .released
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Chunk ids the corpus holds for `file_path` that `fresh` does not reuse.
    ///
    /// What: the file's current ids minus `fresh`'s ids. `file_chunk_ids`
    /// reads redb when the map is still evicted (#10209), and
    /// [`Self::remove_superseded_chunks`] leaves memory untouched when the
    /// redb delete fails, so a retry finds the same ids. A failed redb read
    /// is an `Err`.
    pub(super) async fn superseded_chunk_ids(
        &self,
        file_path: &str,
        fresh: &[RawChunk],
    ) -> Result<Vec<String>> {
        let keep: std::collections::HashSet<&str> = fresh.iter().map(|c| c.id.as_str()).collect();
        Ok(self
            .file_chunk_ids(file_path)
            .await?
            .into_iter()
            .filter(|id| !keep.contains(id.as_str()))
            .collect())
    }

    /// Remove every chunk id from the HNSW store, corpus, embedding cache,
    /// and BM25 index.
    ///
    /// Why: shared by every chunk-delete path. Each lock is acquired once
    /// for the whole batch to bound write-lock contention.
    /// What: `FailClosed` deletes from redb first and returns its failure with
    /// memory untouched (#8959). The test-only `WarnOnly` drops the ids from
    /// memory, then deletes them from redb and only logs a failure.
    /// Test: `test_remove_chunk_ids_removes_from_results`;
    /// `FailClosed` by `a_failed_tombstone_delete_fails_the_write_and_leaves_no_old_ids_after_reopen`.
    pub(crate) async fn remove_chunks_from_stores(
        &self,
        ids: &[String],
        mode: RedbChunkDelete,
    ) -> Result<()> {
        match mode {
            #[cfg(test)]
            RedbChunkDelete::WarnOnly => {
                let _fence = self.rehydrate_write_fence().await; // #10209
                self.drop_chunk_ids_from_memory(ids).await;
                // Issue #28: mirror the deletion into the durable redb corpus.
                self.delete_chunks_from_redb(ids).await;
                Ok(())
            }
            // #8959: redb first, so a failure leaves the ids for a retry.
            RedbChunkDelete::FailClosed => self.remove_superseded_chunks(ids).await,
        }
    }

    /// Drop superseded ids from redb, then the corpus map, BM25 and HNSW.
    ///
    /// Why: a delete that only warned let `index_file` commit and answer `Ok`
    /// while redb kept the old rows, which the next boot loaded back.
    /// What: the redb delete runs first and its failure is returned before
    /// any in-memory structure changes.
    pub(super) async fn remove_superseded_chunks(&self, ids: &[String]) -> Result<()> {
        if ids.is_empty() {
            return Ok(());
        }
        // #10209: a rehydrate that read these rows must not publish them
        // back after they leave, so the delete waits for it to finish.
        let _fence = self.rehydrate_write_fence().await;
        self.try_delete_chunks_from_redb(ids).await?;
        self.drop_chunk_ids_from_memory(ids).await;
        Ok(())
    }

    /// Remove one file's chunks, entity list and content hash, redb first, in
    /// ONE redb write transaction (#11087).
    ///
    /// Why: the chunks and the hash left redb in separate transactions, so a
    /// process exit between them committed a hash with no chunks, and the
    /// next reindex hash-skipped the file forever.
    /// What: under the rehydrate fence, deletes `ids`, the entity row and the
    /// hash row of `file_path` in one transaction. A failure is an `Err` with
    /// every store unchanged. On success it drops the ids, the entity list and
    /// the in-process hash from memory. No corpus wired: memory only.
    /// Test: `a_term_between_a_batch_delete_and_insert_leaves_the_file_served_after_resume`;
    /// the refused arm by `a_refused_prune_delete_keeps_the_file_and_withholds_the_stamps`.
    pub(crate) async fn remove_file_rows_and_hash(
        &self,
        file_path: &str,
        ids: &[String],
    ) -> Result<()> {
        let _fence = self.rehydrate_write_fence().await; // #10209
        #[cfg(test)]
        if TEST_FAIL_CHUNK_DELETE
            .lock()
            .is_ok_and(|f| f.contains(&self.index_id))
        {
            anyhow::bail!("injected redb chunk delete failure (#8959 test seam)");
        }
        if let Some(corpus) = self.corpus.clone() {
            let (ids, file) = (ids.to_vec(), vec![file_path.to_string()]);
            tokio::task::spawn_blocking(move || {
                corpus.delete_chunk_entity_and_hash_rows(&ids, &file, &file)
            })
            .await
            .map_err(|e| anyhow::anyhow!("redb file-row delete task failed: {e}"))?
            .map_err(|e| e.context("redb file-row delete"))?;
        }
        self.drop_chunk_ids_from_memory(ids).await;
        self.entities.write().await.remove(file_path);
        let id = crate::core::registry::IndexId::new(self.index_id.as_str());
        crate::service::reindex::hash::hashes_for(&id).remove(std::path::Path::new(file_path));
        Ok(())
    }

    /// Delete chunk ids from the durable redb corpus, reporting failure.
    ///
    /// What: `CorpusStore::delete_chunks` on a blocking worker. No corpus
    /// wired, or no ids, is `Ok`.
    pub(crate) async fn try_delete_chunks_from_redb(&self, ids: &[String]) -> Result<()> {
        #[cfg(test)]
        if TEST_FAIL_CHUNK_DELETE
            .lock()
            .is_ok_and(|f| f.contains(&self.index_id))
        {
            anyhow::bail!("injected redb chunk delete failure (#8959 test seam)");
        }
        let Some(corpus) = self.corpus.clone() else {
            return Ok(());
        };
        if ids.is_empty() {
            return Ok(());
        }
        let ids = ids.to_vec();
        tokio::task::spawn_blocking(move || corpus.delete_chunks(&ids))
            .await
            .map_err(|e| anyhow::anyhow!("redb chunk delete task failed: {e}"))?
            .map_err(|e| e.context("redb chunk delete"))
    }

    /// Delete a file's chunk `ids` and its file-hash row in one redb commit.
    ///
    /// What: `CorpusStore::delete_file_rows` on a blocking worker. Runs with
    /// empty `ids` too, so the hash row still goes. No corpus wired is `Ok`.
    /// The #8959 chunk-delete seam refuses before redb; the #9236 forget seam
    /// fails inside the transaction, before its commit.
    pub(crate) async fn try_delete_file_rows_from_redb(
        &self,
        ids: &[String],
        file: &str,
    ) -> Result<()> {
        #[cfg(test)]
        if TEST_FAIL_CHUNK_DELETE
            .lock()
            .is_ok_and(|f| f.contains(&self.index_id))
        {
            anyhow::bail!("injected redb chunk delete failure (#8959 test seam)");
        }
        let Some(corpus) = self.corpus.clone() else {
            return Ok(());
        };
        #[cfg(test)]
        let fail_in_txn = crate::service::reindex::hash::TEST_FAIL_FORGET_HASH
            .lock()
            .is_ok_and(|f| f.contains(&self.index_id));
        let (ids, file) = (ids.to_vec(), file.to_string());
        // #11096: chunks and hash leave in one transaction, or neither does.
        tokio::task::spawn_blocking(move || {
            #[cfg(test)]
            if fail_in_txn {
                return corpus.delete_file_rows_failing_before_commit(&ids, &file);
            }
            corpus.delete_file_rows(&ids, &file)
        })
        .await
        .map_err(|e| anyhow::anyhow!("redb file rows delete task failed: {e}"))?
        .map_err(|e| e.context("redb file rows delete"))
    }
}
