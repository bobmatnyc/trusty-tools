//! The per-review recorder behind `ReviewOutcome::context_sources` (#9192).
//!
//! Why: a review that asked for no new input must build nothing, so the
//! recorder is a no-op unless the request turned it on.
//! What: [`ContextLedger`] collects [`ContextSourceRecord`]s in order.
//! Test: `context_sources_absent_unless_requested`.

use super::{OptionalContextRequest, probes::ContextRows};
use crate::{models::ContextSourceRecord, pipeline::context_gate::GateFacts};

/// Collects one record per optional source, when enabled.
#[derive(Debug, Default)]
pub(crate) struct ContextLedger {
    enabled: bool,
    records: Vec<ContextSourceRecord>,
}

impl ContextLedger {
    /// A ledger that records only when `enabled`.
    pub(crate) fn new(enabled: bool) -> Self {
        Self {
            enabled,
            records: Vec::new(),
        }
    }

    /// Whether records are kept.
    pub(crate) fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Keep `record` when enabled; drop it otherwise.
    pub(crate) fn push(&mut self, record: ContextSourceRecord) {
        if self.enabled {
            self.records.push(record);
        }
    }

    /// Add the review's own rows and complete the ledger (#9194).
    ///
    /// Test: `ledger_fills_not_requested_rows`, `finish_does_not_duplicate_a_row`.
    #[allow(dead_code)] // #9194: stub; completes the ledger from a later commit.
    pub(crate) fn finish(
        &mut self,
        _request: &OptionalContextRequest,
        _rows: ContextRows,
        _facts: &GateFacts,
    ) {
    }

    /// The records, in the order they were pushed.
    pub(crate) fn into_records(self) -> Vec<ContextSourceRecord> {
        self.records
    }
}
