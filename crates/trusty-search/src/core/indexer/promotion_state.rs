//! The deferred-promotion record: why a staged reindex did not land, kept
//! where `GET /indexes/:id/status` can report it (#7991).
//!
//! Why: the promotion gate refuses when the live `index.redb` is held by
//! another opener, and a refusal that is only a `false` return is invisible.
//! `finish_teardown::resolve_corpus_swap` used that `false` solely to skip the
//! #7004 reconciliation, so the run still pushed `ReindexStatus::Complete`, the
//! indexer kept serving the staging corpus, `chunk_count` looked healthy and
//! `search_health` answered OK — while the live corpus on disk stayed at the
//! pre-reindex state and every byte of the run was discarded at the next
//! restart. The deferral has to be reportable for the same reason the
//! migration fault does: the state is wrong and nothing else says so.
//!
//! What: [`PromotionDeferred`], recorded through `&CodeIndexer` (interior
//! mutability, matching `MigrationFaultRecord`) by
//! `service::reindex::corpus_swap::commit_staged_corpus_swap`, cleared by the
//! same function when a promotion does land. Read by
//! `service::server::status` as `promotion_deferred` and by
//! `finish::finish_reindex`, which turns it into
//! `ReindexStatus::PromotionDeferred` instead of `Complete`.
//!
//! Test: `super::super::super::service::reindex::live_corpus_lock_tests`.

use std::sync::Mutex;

use super::CodeIndexer;

/// A staged corpus promotion that was refused rather than attempted (#7991).
///
/// Why: a caller must be able to tell "this reindex landed" from "this reindex
/// ran and was thrown away", and an operator must learn it without reading the
/// daemon log.
/// What: the reason the gate refused and the RFC 3339 instant it did.
/// Test: `a_deferred_promotion_is_reported_in_status_and_is_not_complete`.
#[derive(Debug, Clone)]
pub struct PromotionDeferred {
    /// Why the promotion was refused, in one line.
    pub reason: String,
    /// When the refusal was recorded, RFC 3339.
    pub at: String,
}

/// Shared cell holding the last deferred promotion for one index.
///
/// Why/What: see the module doc. One cell, not a map: there is exactly one
/// promotion gate, so a later deferral genuinely supersedes an earlier one.
/// Test: `a_deferred_promotion_is_reported_in_status_and_is_not_complete`.
#[derive(Debug, Default)]
pub(crate) struct PromotionDeferralRecord {
    last: Mutex<Option<PromotionDeferred>>,
}

impl PromotionDeferralRecord {
    /// Poison-tolerant lock; every critical section is a single move.
    fn lock(&self) -> std::sync::MutexGuard<'_, Option<PromotionDeferred>> {
        self.last.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl CodeIndexer {
    /// Record that a staged promotion was refused (#7991).
    pub(crate) fn record_promotion_deferred(&self, reason: impl Into<String>) {
        *self.promotion_deferral.lock() = Some(PromotionDeferred {
            reason: reason.into(),
            at: chrono::Utc::now().to_rfc3339(),
        });
    }

    /// Clear the record once a promotion actually landed (#7991).
    ///
    /// Why: the deferral describes one run. A later run that promoted is proof
    /// the blocking opener is gone, so leaving the record would report a
    /// healthy index as deferred forever.
    pub(crate) fn clear_promotion_deferred(&self) {
        *self.promotion_deferral.lock() = None;
    }

    /// The outstanding deferral, or `None` when the last promotion landed.
    pub fn promotion_deferred(&self) -> Option<PromotionDeferred> {
        self.promotion_deferral.lock().clone()
    }
}
