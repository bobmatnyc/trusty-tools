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
//! reached or left redb, never on a refused, failed or no-op write.
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

    /// [`Self::purge_file`] that reports whether its delete committed (#9230).
    ///
    /// Why: a committed delete stamps the corpus, and only a fail-closed
    /// delete proves the rows left redb. Boot reconcile and the rescan sweep
    /// keep their warn-only tolerance of a failed redb delete.
    /// What: purges fail-closed. On an error it logs, then runs the warn-only
    /// [`Self::purge_file`], whose count or error the caller handles as it
    /// always has. Returns `(removed, committed)`; `committed` is true only
    /// when the fail-closed purge removed rows.
    /// Test: `reconcile_delete_stamps_only_when_committed`,
    /// `rescan_delete_stamps_only_when_committed`,
    /// `excluded_pushed_write_purge_stamps_only_when_committed`.
    pub(crate) async fn purge_file_committed(
        &self,
        index_id: &IndexId,
        rel: &str,
    ) -> Result<(usize, bool)> {
        match self
            .purge_file_with(index_id, rel, RedbChunkDelete::FailClosed)
            .await
        {
            Ok(removed) => Ok((removed, removed > 0)),
            Err(e) => {
                tracing::warn!(
                    index_id = %self.index_id,
                    file = %rel,
                    "fail-closed purge failed; purging warn-only, without a \
                     reindex stamp (#9230): {e:#}"
                );
                Ok((self.purge_file(index_id, rel).await?, false))
            }
        }
    }

    /// [`Self::remove_file`] that reports whether its delete committed (#9230).
    ///
    /// Why: `POST /indexes/{id}/remove-file` must stamp a committed delete and
    /// keep its warn-only tolerance of a failed redb delete.
    /// What: removes fail-closed. On an error it logs, then runs the warn-only
    /// [`Self::remove_file`], whose count or error the caller handles as it
    /// always has. Returns `(removed, committed)`, as
    /// [`Self::purge_file_committed`] does.
    /// Test: `remove_file_report_stamps_only_a_committed_delete`.
    pub(crate) async fn remove_file_committed(&self, file_path: &str) -> Result<(usize, bool)> {
        match self
            .remove_file_with(file_path, RedbChunkDelete::FailClosed)
            .await
        {
            Ok(removed) => Ok((removed, removed > 0)),
            Err(e) => {
                tracing::warn!(
                    index_id = %self.index_id,
                    file = %file_path,
                    "fail-closed remove failed; removing warn-only, without a \
                     reindex stamp (#9230): {e:#}"
                );
                Ok((self.remove_file(file_path).await?, false))
            }
        }
    }

    /// Drop chunk `ids` from every store; `true` when they left redb (#9230).
    ///
    /// Why: the watcher's delete must stamp the corpus when it committed, and
    /// keep its old tolerance (drop from memory, log) when redb refused.
    /// What: removes fail-closed. On an error it logs and removes warn-only,
    /// as the watcher always did. Empty `ids` is `false`.
    /// Test: `watcher_delete_stamps_only_when_committed`.
    pub(crate) async fn remove_chunk_ids_committed(&self, ids: &[String]) -> bool {
        if ids.is_empty() {
            return false;
        }
        let Err(e) = self
            .remove_chunks_from_stores(ids, RedbChunkDelete::FailClosed)
            .await
        else {
            return true;
        };
        tracing::warn!(
            index_id = %self.index_id,
            "fail-closed chunk delete failed; removing warn-only, without a \
             reindex stamp (#9230): {e:#}"
        );
        if let Err(e) = self
            .remove_chunks_from_stores(ids, RedbChunkDelete::WarnOnly)
            .await
        {
            tracing::warn!(index_id = %self.index_id, "warn-only chunk delete failed: {e:#}");
        }
        false
    }
}
