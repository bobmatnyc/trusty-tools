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
//! [`CodeIndexer::refuse_if_corpus_detached`] is the same refusal for a
//! staged reindex swap's detach window (#10738).
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

    /// `Err` while a staged reindex swap has the corpus detached (#10738).
    ///
    /// Why: the abort and the promotion take the corpus out, then delete,
    /// rename and reopen files with no indexer lock held. A write in that span
    /// landed in memory only, `commit_corpus_to_redb` reported the missing
    /// corpus as success, and the put was acknowledged and then lost.
    /// What: `Err` when a corpus was wired once, none is wired now, and the
    /// index is not quarantined (the #4122 refusal covers that state, and
    /// counts it). The state lasts until the re-attach or the quarantine; a
    /// run that dies inside the window leaves it until the next attach.
    /// Test: `a_put_during_the_abort_detach_window_is_refused_or_lands_in_live`,
    /// `a_put_during_the_promotion_detach_window_is_refused_or_raises_the_generation`,
    /// `an_abort_whose_live_reopen_fails_quarantines_and_raises_the_generation`,
    /// `a_panicked_staged_run_is_covered_by_the_next_attach`.
    pub(crate) fn refuse_if_corpus_detached(&self) -> anyhow::Result<()> {
        // #10738: the #7920 snapshot-writer predicate, applied to puts.
        if self.corpus_ever_wired && self.corpus.is_none() && !self.corpus_open_failed {
            anyhow::bail!(
                "index '{}' has its durable corpus detached by a staged reindex swap: \
                 this write would persist nothing, so it was refused; retry once the \
                 swap settles (#10738)",
                self.index_id
            );
        }
        Ok(())
    }
}
