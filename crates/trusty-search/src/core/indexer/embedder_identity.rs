//! `CodeIndexer` access to the embedder-identity stamp (#10664).
//!
//! Why: the stamp lives in the corpus and the running identity in the
//! embedder; the indexer holds both, so the build, load and status paths
//! all ask it rather than each pairing the two.
//! What: read the running identity, compare it with the corpus stamp, log the
//! load-time result, and write the stamp after a full rebuild.
//! Test: `core::indexer::tests::embedder_identity_10664`.

use crate::core::embedder_identity::{EmbedderIdentity, IdentityCheck, StampRead};

use super::CodeIndexer;

impl CodeIndexer {
    /// The running embedder's identity, `None` when none is wired or exposed.
    pub fn active_embedder_identity(&self) -> Option<EmbedderIdentity> {
        self.embedder.as_ref().and_then(|e| e.identity())
    }

    /// Compare the corpus stamp with the running embedder (#10664).
    ///
    /// Why: criterion (2); one comparison serves the load WARN and the
    /// status field, so the two cannot disagree.
    /// What: no corpus reads as `Unknown`; a failed blocking read is
    /// `Unreadable`, never a match.
    /// Test: `an_unstamped_index_reports_unknown_and_still_loads`,
    /// `an_unreadable_stamp_reports_unreadable_not_match`.
    pub async fn embedder_identity_check(&self) -> IdentityCheck {
        let stored = match self.corpus_store() {
            None => StampRead::Absent,
            Some(corpus) => {
                tokio::task::spawn_blocking(move || corpus.read_embedder_identity_sync())
                    .await
                    .unwrap_or_else(|e| StampRead::Unreadable(format!("stamp read task: {e}")))
            }
        };
        IdentityCheck::compare(stored, self.active_embedder_identity())
    }

    /// Compare at load and log one WARN on a mismatch or unreadable stamp.
    ///
    /// What: report-only; the indexer is returned to the caller unchanged.
    /// Test: `a_mismatched_stamp_is_reported_and_the_index_still_loads`.
    pub async fn report_embedder_identity_at_load(&self) {
        self.embedder_identity_check()
            .await
            .log_at_load(&self.index_id);
    }

    /// Stamp the running identity into the corpus (#10664).
    ///
    /// What: a no-op without a corpus or an exposed identity. A write failure
    /// is logged at WARN and not propagated: the stamp is a diagnostic, and
    /// a missing one reads as `unknown`, never as a match.
    /// Test: `a_completed_reindex_stamps_the_embedder_identity`.
    pub async fn stamp_embedder_identity(&self) {
        let (Some(corpus), Some(identity)) = (self.corpus_store(), self.active_embedder_identity())
        else {
            return;
        };
        let index_id = self.index_id.clone();
        let written =
            tokio::task::spawn_blocking(move || corpus.write_embedder_identity_sync(&identity))
                .await;
        match written {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                tracing::warn!("reindex[{index_id}]: embedder identity not stamped: {e:#}")
            }
            Err(e) => tracing::warn!("reindex[{index_id}]: embedder stamp task failed: {e}"),
        }
    }
}
