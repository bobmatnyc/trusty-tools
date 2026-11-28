//! An incremental commit stamps the corpus's reindex stamp (#9230).
//!
//! Why: `search.project.resolve` ranks two live checkouts of one repo by
//! `_meta["reindexed_unix"]`. Only a full reindex commit wrote it (#9169), so
//! a clone kept current by the watcher, `index-file`, or the boot-reconcile
//! delta lost to a stale clone that had one full reindex.
//! What: [`CodeIndexer::stamp_incremental_commit`] writes "now" into the held
//! corpus; [`CodeIndexer::record_incremental_commit`] logs a failure of it.
//! The callers (`index_file_outcome`, `index_files_batch_inner`, and the
//! delete paths through [`CodeIndexer::purge_file_committed`],
//! [`CodeIndexer::remove_file_committed`] and
//! [`CodeIndexer::remove_chunk_ids_committed`]) stamp only after their rows
//! reached or left redb, never on a refused, failed or no-op write. A failed
//! delete is returned to the caller with memory unchanged, never retried
//! warn-only.
//! Test: `indexer::tests::incremental_stamp_9230`,
//! `service::delete_stamp_9230_tests`.

use anyhow::{Context, Result};

use super::super::{CodeIndexer, RedbChunkDelete};
use crate::core::registry::IndexId;

/// #9230: test-only fault seam. The stamp write fails for each index id listed
/// here. Keyed by index id so concurrent tests never see each other's faults.
#[cfg(test)]
pub(crate) static TEST_FAIL_STAMP: std::sync::Mutex<Vec<String>> =
    std::sync::Mutex::new(Vec::new());

impl CodeIndexer {
    /// Record in the durable corpus that an incremental write just committed.
    ///
    /// Why: see the module doc; the stamp is what the resolver reads.
    /// What: writes the current unix time into `_meta` on a blocking worker.
    /// No durable corpus is a no-op `Ok`. Callers invoke it only after the
    /// write's rows landed in redb.
    /// Test: `index_file_stamps_the_corpus_it_commits`,
    /// `a_failed_stamp_write_leaves_the_previous_stamp`.
    pub(crate) async fn stamp_incremental_commit(&self) -> Result<()> {
        let Some(corpus) = self.corpus.clone() else {
            return Ok(());
        };
        #[cfg(test)]
        if TEST_FAIL_STAMP
            .lock()
            .is_ok_and(|f| f.contains(&self.index_id))
        {
            anyhow::bail!("injected stamp write failure (#9230 test seam)");
        }
        tokio::task::spawn_blocking(move || corpus.write_reindexed_now_sync())
            .await
            .context("reindex stamp task failed")?
            .context("write the incremental-commit reindex stamp")?;
        Ok(())
    }

    /// [`Self::stamp_incremental_commit`], logging a failure at `warn`.
    ///
    /// Why: the write itself landed, so failing it would make the caller
    /// retry a write that succeeded. The stamp keeps its previous value, so
    /// the resolver reads this index as older, never as fresher than it is.
    /// What: one `warn` naming the index and `what` on failure.
    /// Test: `a_failed_stamp_write_leaves_the_previous_stamp`.
    pub(crate) async fn record_incremental_commit(&self, what: &str) {
        if let Err(e) = self.stamp_incremental_commit().await {
            tracing::warn!(
                index_id = %self.index_id,
                write = %what,
                "incremental commit landed but its reindex stamp was not written; \
                 project.resolve keeps the previous stamp (#9230): {e:#}"
            );
        }
    }

    /// [`Self::purge_file_with`], fail-closed, reporting whether it committed (#9230).
    ///
    /// Why: a committed delete stamps the corpus, and only a fail-closed
    /// delete proves the rows left redb. A failed one must stay retryable.
    /// What: purges fail-closed. A failed redb delete is an `Err` with the
    /// chunk ids, entity list and content hash still in memory; the caller
    /// must not stamp. Returns `(removed, committed)`; `committed` is
    /// `removed > 0`.
    /// Test: `reconcile_delete_stamps_only_when_committed`,
    /// `rescan_delete_stamps_only_when_committed`,
    /// `excluded_pushed_write_purge_stamps_only_when_committed`.
    pub(crate) async fn purge_file_committed(
        &self,
        index_id: &IndexId,
        rel: &str,
    ) -> Result<(usize, bool)> {
        // #9459: a delete against a corpus closed for shutdown leaves its rows
        // in redb; refuse it so the reconcile delta does not stamp HEAD.
        self.refuse_if_closed_for_shutdown()?;
        // #10738: a delete in a staged swap's detach window misses redb, and
        // the re-attached corpus brings the file back.
        self.refuse_if_corpus_detached()?;
        let removed = self.purge_file_with(index_id, rel).await?;
        Ok((removed, removed > 0))
    }

    /// [`Self::remove_file`] plus the content hash, fail-closed, reporting
    /// whether it committed (#9230, #11096).
    ///
    /// Why: `POST /indexes/{id}/remove-file` must stamp a committed delete,
    /// and must not answer success for a delete redb refused. #11096: the
    /// chunks and the hash used to leave in two transactions, so a failed
    /// second one left the chunks gone and the hash kept, and the next
    /// reindex skipped the file.
    /// What: stamps the durable graph mark when anything will leave, then
    /// [`Self::remove_file_and_hash`]. A failed delete is an `Err` with the
    /// chunks and the hash still in redb and in memory. Returns
    /// `(removed, committed)`, as [`Self::purge_file_committed`] does.
    /// Test: `remove_file_report_stamps_only_a_committed_delete`,
    /// `remove_file_fault_in_the_hash_delete_keeps_chunks_and_hash_11096`.
    pub(crate) async fn remove_file_committed(&self, file_path: &str) -> Result<(usize, bool)> {
        // #10738: a delete in a staged swap's detach window misses redb, and
        // the re-attached corpus brings the file back.
        self.refuse_if_corpus_detached()?;
        let ids = self.file_chunk_ids(file_path).await?;
        // #8959: as `remove_file_with`, the durable mark precedes the removal.
        let _graph_write = if !ids.is_empty() || self.entities.read().await.contains_key(file_path)
        {
            Some(self.begin_graph_write().await?)
        } else {
            None
        };
        let index_id = IndexId::new(self.index_id.as_str());
        self.remove_file_and_hash(&index_id, file_path, &ids)
            .await?;
        Ok((ids.len(), !ids.is_empty()))
    }

    /// Remove `rel`'s chunk `ids` and its content hash as one durable commit.
    ///
    /// Why (#11096): a file's chunks and its hash must leave together. Chunks
    /// gone with the hash kept make the next reindex skip the file, so it is
    /// lost; a hash gone with the chunks kept leaves orphaned search hits.
    /// What: one redb write transaction deletes the chunk rows and the hash
    /// row. Only after it commits do the ids leave memory, the hash leave the
    /// in-process cache and the entity list go. A failed transaction is an
    /// `Err` with every store unchanged. The caller holds the graph guard.
    /// Test: `remove_file_fault_in_the_hash_delete_keeps_chunks_and_hash_11096`,
    /// `remove_file_drops_chunk_rows_and_hash_row_together_11096`.
    pub(crate) async fn remove_file_and_hash(
        &self,
        index_id: &IndexId,
        rel: &str,
        ids: &[String],
    ) -> Result<()> {
        {
            // #10209: a rehydrate that read these rows must not publish them
            // back after they leave, as in `remove_superseded_chunks`.
            let _fence = self.rehydrate_write_fence().await;
            // #11096: one transaction for the chunk rows and the hash row.
            self.try_delete_file_rows_from_redb(ids, rel).await?;
            if !ids.is_empty() {
                self.drop_chunk_ids_from_memory(ids).await;
            }
        }
        crate::service::reindex::hash::hashes_for(index_id).remove(std::path::Path::new(rel));
        self.remove_file_entities(rel).await;
        Ok(())
    }

    /// Drop chunk `ids` from every store; `Ok(true)` when they left redb (#9230).
    ///
    /// Why: the watcher's delete must stamp the corpus when it committed, and
    /// keep the ids for a retry when redb refused.
    /// What: removes fail-closed. A failed redb delete is an `Err` with the
    /// ids still in memory. Empty `ids` is `Ok(false)`.
    /// Test: `watcher_delete_stamps_only_when_committed`,
    /// `watcher_edit_delete_stamps_only_when_committed`.
    pub(crate) async fn remove_chunk_ids_committed(&self, ids: &[String]) -> Result<bool> {
        if ids.is_empty() {
            return Ok(false);
        }
        // #10738: a removal in a staged swap's detach window misses redb, and
        // the re-attached corpus brings the file back; refuse before memory
        // changes so the watcher keeps the ids for a retry.
        self.refuse_if_corpus_detached()?;
        self.remove_chunks_from_stores(ids, RedbChunkDelete::FailClosed)
            .await?;
        Ok(true)
    }
}
