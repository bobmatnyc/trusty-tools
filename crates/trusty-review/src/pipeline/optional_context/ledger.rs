//! The per-review recorder behind `ReviewOutcome::context_sources` (#9192).
//!
//! Why: a review that asked for no new input must build nothing, so the
//! recorder is a no-op unless the request turned it on.
//! What: [`ContextLedger`] collects [`ContextSourceRecord`]s; #9194:
//! [`ContextLedger::finish`] adds the review's own rows and completes the
//! set in canonical order.
//! Test: `context_sources_absent_unless_requested`, `ledger_fills_not_requested_rows`.

use super::{
    OptionalContextRequest,
    probes::{ContextRows, cap_detail},
};
use crate::{
    models::{ContextSourceRecord, SourceState},
    pipeline::context_gate::GateFacts,
};

/// The row order when reporting is on (#9194, plan §2.1).
const CANONICAL: [&str; 10] = [
    "pr_body",
    "caller_context",
    "issues",
    "spec_docs",
    "claude_md",
    "changed_files", // #9195 ruling Q6
    "search",
    "analyze",
    "symbol_context", // #9196 ruling Q7
    "external_sources",
];

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
    /// Why: AC2 asks for every source when reporting is on, so `absent`
    /// always means "asked for and empty"; a dependency the gate degraded is
    /// `unavailable` with the gate's reason even when hits arrived (ruling Q5).
    /// What: when enabled, sets the `search`, `analyze` and
    /// `external_sources` rows from `rows`, replacing any of the same name,
    /// with `facts` overlaid. Then each input row still missing is keyed on
    /// `request` (amendment 1): asked for, it reads `unavailable`, "row not
    /// recorded"; not asked for, `not_requested` with the flag that is off.
    /// `caller_context` is always recorded when enabled. Rows end in
    /// canonical order. A second call replaces rows by name (P5). Every row
    /// and item detail then passes through [`cap_detail`].
    /// Test: `ledger_fills_not_requested_rows`, `finish_caps_every_row_and_item_detail`,
    /// `a_requested_row_that_was_not_recorded_is_unavailable`,
    /// `finish_does_not_duplicate_a_row`, `a_gate_fact_marks_its_row_unavailable`,
    /// `a_disabled_ledger_finishes_empty`.
    pub(crate) fn finish(
        &mut self,
        request: &OptionalContextRequest,
        rows: ContextRows,
        facts: &GateFacts,
    ) {
        if !self.enabled {
            return;
        }
        self.upsert(overlay(rows.search, facts.search.as_deref()));
        self.upsert(overlay(rows.analyze, facts.analyze.as_deref()));
        self.upsert(rows.external);
        let inputs = [
            ("pr_body", request.include_pr_body, "include_pr_body is off"),
            ("caller_context", true, ""),
            (
                "issues",
                request.issue_docs.is_some(),
                "no issue_docs were sent",
            ),
            ("spec_docs", request.spec_docs, "spec_docs is off"),
            ("claude_md", request.claude_md, "claude_md is off"),
            (
                "changed_files",
                request.changed_files,
                "changed_files is off",
            ), // #9195
            (
                "symbol_context",
                request.symbol_context,
                "symbol_context is off",
            ), // #9196
        ];
        for (source, asked, off) in inputs {
            if self.records.iter().any(|r| r.source == source) {
                continue;
            }
            self.records.push(if asked {
                ContextSourceRecord::new(source, SourceState::Unavailable)
                    .with_detail("row not recorded")
            } else {
                ContextSourceRecord::new(source, SourceState::NotRequested).with_detail(off)
            });
        }
        // Stable: a row outside the canonical set keeps its place at the end.
        self.records.sort_by_key(|r| {
            CANONICAL
                .iter()
                .position(|c| *c == r.source)
                .unwrap_or(CANONICAL.len())
        });
        // #9194: one choke point, so no producer's detail skips `cap_detail`.
        for row in &mut self.records {
            capped(&mut row.detail);
            row.items.iter_mut().for_each(|i| capped(&mut i.detail));
        }
    }

    /// Replace the record of the same source, or append `record`.
    fn upsert(&mut self, record: ContextSourceRecord) {
        match self.records.iter_mut().find(|r| r.source == record.source) {
            Some(slot) => *slot = record,
            None => self.records.push(record),
        }
    }

    /// The records, in the order they were pushed.
    pub(crate) fn into_records(self) -> Vec<ContextSourceRecord> {
        self.records
    }
}

/// `detail` passed through [`cap_detail`], when there is one.
fn capped(detail: &mut Option<String>) {
    if let Some(text) = detail {
        *text = cap_detail(text);
    }
}

/// `row`, `unavailable` with the gate's `reason` when the gate named it.
fn overlay(row: ContextSourceRecord, reason: Option<&str>) -> ContextSourceRecord {
    match reason {
        Some(reason) => {
            let mut row = row.with_detail(&cap_detail(reason));
            row.state = SourceState::Unavailable;
            row
        }
        None => row,
    }
}

#[cfg(test)]
#[path = "ledger_tests.rs"]
pub(crate) mod tests;
