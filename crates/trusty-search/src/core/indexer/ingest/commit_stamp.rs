//! An incremental commit stamps the corpus's reindex stamp (#9230).
//!
//! Why: `search.project.resolve` ranks two live checkouts of one repo by
//! `_meta["reindexed_unix"]`. Only a full reindex commit wrote it (#9169), so
//! a clone kept current by the watcher, `index-file`, or the boot-reconcile
//! delta lost to a stale clone that had one full reindex.
//! What: [`CodeIndexer::stamp_incremental_commit`] writes "now" into the held
//! corpus; [`CodeIndexer::record_incremental_commit`] logs a failure of it.
//! The callers (`index_file_outcome`, `index_files_batch_inner`) stamp only
//! after their rows reached redb, never on a refused, failed or no-op write.
//! Test: `indexer::tests::incremental_stamp_9230`.

use anyhow::{Context, Result};

use super::super::CodeIndexer;

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
    pub(super) async fn record_incremental_commit(&self, what: &str) {
        if let Err(e) = self.stamp_incremental_commit().await {
            tracing::warn!(
                index_id = %self.index_id,
                write = %what,
                "incremental commit landed but its reindex stamp was not written; \
                 project.resolve keeps the previous stamp (#9230): {e:#}"
            );
        }
    }
}
