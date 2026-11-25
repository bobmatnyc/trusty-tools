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

use std::sync::atomic::{AtomicBool, Ordering};
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
    /// Also `Err` while [`Self::swap_abandoned_flag`] is set: a staged run
    /// died mid-run with staging attached and its guard's rollback is running.
    /// Test: `a_put_during_the_abort_detach_window_is_refused_or_lands_in_live`,
    /// `a_put_during_the_promotion_detach_window_is_refused_or_raises_the_generation`,
    /// `an_abort_whose_live_reopen_fails_quarantines_and_raises_the_generation`,
    /// `a_panicked_staged_run_is_covered_by_the_next_attach`,
    /// `a_staged_run_dropped_mid_batch_refuses_or_rolls_back`, and for the
    /// delete paths `a_delete_during_the_abort_detach_window_is_refused_or_stays_deleted`
    /// and `a_watcher_delete_during_the_abort_detach_window_does_not_resurrect`,
    /// and for the rescan batch
    /// `a_rescan_batch_during_the_abort_detach_window_is_refused_or_lands_in_live`.
    pub(crate) fn refuse_if_corpus_detached(&self) -> anyhow::Result<()> {
        // #10738: a staged run died mid-run and its rollback has not settled;
        // the attached corpus is the staging one that rollback discards.
        if self.swap_abandoned.load(Ordering::Acquire) {
            anyhow::bail!(
                "index '{}' is rolling back a staged reindex that died mid-run: this \
                 write would land in the staging corpus the rollback discards, so it \
                 was refused; retry once the rollback settles (#10738)",
                self.index_id
            );
        }
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

    /// The flag that makes [`Self::refuse_if_corpus_detached`] refuse while a
    /// dead staged run is rolled back (#10738).
    ///
    /// Why: the run's termination guard sets it from `Drop`, where it cannot
    /// take the indexer lock, so it holds this shared handle instead.
    /// What: a clone of the indexer's `swap_abandoned` flag. The guard sets it
    /// before it spawns the rollback; the rollback clears it once live is
    /// re-attached or the index is quarantined.
    /// Test: `a_staged_run_dropped_mid_batch_refuses_or_rolls_back`.
    pub(crate) fn swap_abandoned_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.swap_abandoned)
    }
}
