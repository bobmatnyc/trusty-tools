//! The indexer half of closing a corpus for a normal stop (#9459).
//!
//! Why: the shutdown close takes the corpus out of every indexer no reindex
//! holds. The boot and cold-load reconcile delta runs detached and takes no
//! index permit, so its writes can still reach the indexer after the take.
//! With no corpus they landed in memory only and answered `Ok`, and
//! `apply_delta` then persisted the new HEAD SHA, so the next boot never
//! retried the delta.
//! What: [`CodeIndexer::take_corpus_for_shutdown`] sets the flag and takes the
//! corpus under the caller's write lock; [`CodeIndexer::refuse_if_closed_for_shutdown`]
//! is the refusal `index_file` and `purge_file_committed` run first.
//! Test: `a_reconcile_delta_after_the_close_fails_and_does_not_stamp` in
//! `service::shutdown_close::tests`.

use std::sync::Arc;

use super::CodeIndexer;
use crate::core::corpus::CorpusStore;

impl CodeIndexer {
    /// Mark this indexer closed for shutdown and take its corpus out.
    ///
    /// Why: no write may answer `Ok` once the corpus it would persist to is
    /// gone. Setting the flag under the same `&mut self` borrow as the take
    /// means no reader sees the corpus missing without the flag set.
    /// What: sets `closed_for_shutdown`, then `Option::take` on `corpus`.
    /// The flag is never cleared; the process is about to exit.
    /// Test: `a_reconcile_delta_after_the_close_fails_and_does_not_stamp`.
    pub(crate) fn take_corpus_for_shutdown(&mut self) -> Option<Arc<CorpusStore>> {
        self.closed_for_shutdown = true;
        self.corpus.take()
    }

    /// `Err` once [`Self::take_corpus_for_shutdown`] has run.
    pub(crate) fn refuse_if_closed_for_shutdown(&self) -> anyhow::Result<()> {
        if self.closed_for_shutdown {
            anyhow::bail!(
                "index '{}' is closed for shutdown: its corpus was closed, so this \
                 write would persist nothing (#9459)",
                self.index_id
            );
        }
        Ok(())
    }
}
