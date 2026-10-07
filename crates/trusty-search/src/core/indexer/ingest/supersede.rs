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
}

impl Default for PathWriteLocks {
    fn default() -> Self {
        Self {
            stripes: (0..PATH_LOCK_STRIPES)
                .map(|_| tokio::sync::Mutex::new(()))
                .collect(),
        }
    }
}

impl PathWriteLocks {
    /// Lock the stripe that `path` hashes to.
    pub(crate) async fn lock(&self, path: &str) -> tokio::sync::MutexGuard<'_, ()> {
        use std::hash::{Hash as _, Hasher as _};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        path.hash(&mut hasher);
        let stripe = usize::try_from(hasher.finish() % PATH_LOCK_STRIPES as u64).unwrap_or(0);
        self.stripes[stripe].lock().await
    }
}

impl CodeIndexer {
    /// Chunk ids the corpus holds for `file_path` that `fresh` does not reuse.
    ///
    /// What: the file's current ids minus `fresh`'s ids. The in-memory map
    /// mirrors redb here: `chunk_ids_for_file` rehydrates an evicted map from
    /// redb, and [`Self::remove_superseded_chunks`] leaves memory untouched
    /// when the redb delete fails, so a retry finds the same ids.
    pub(super) async fn superseded_chunk_ids(
        &self,
        file_path: &str,
        fresh: &[RawChunk],
    ) -> Vec<String> {
        let keep: std::collections::HashSet<&str> = fresh.iter().map(|c| c.id.as_str()).collect();
        self.chunk_ids_for_file(file_path)
            .await
            .into_iter()
            .filter(|id| !keep.contains(id.as_str()))
            .collect()
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
        self.try_delete_chunks_from_redb(ids).await?;
        self.drop_chunk_ids_from_memory(ids).await;
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
}
