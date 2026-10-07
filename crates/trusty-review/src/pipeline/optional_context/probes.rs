//! Ledger rows for the context the review gathers itself (#9194).
//!
//! Why: trusty-search, trusty-analyze and the external sources fail open, so
//! a review that got nothing from them looked like one that was never asked.
//! What: [`ContextRows`] carries the `search`, `analyze` and
//! `external_sources` rows from the calls the review already made to
//! `ContextLedger::finish`; [`cap_detail`] bounds a row's detail.
//! Test: `detail_is_bounded_and_single_line`.

use crate::models::ContextSourceRecord;

/// The rows the review's own context gathering produced (#9194).
#[allow(dead_code)] // #9194: stub; built by the runner from a later commit.
#[derive(Debug, Clone)]
pub(crate) struct ContextRows {
    /// The `search` row.
    pub(crate) search: ContextSourceRecord,
    /// The `analyze` row.
    pub(crate) analyze: ContextSourceRecord,
    /// The `external_sources` row.
    pub(crate) external: ContextSourceRecord,
}

/// `text` as a ledger detail (#9194).
///
/// Test: `detail_is_bounded_and_single_line`.
#[allow(dead_code)] // #9194: stub; bounds details from a later commit.
pub(crate) fn cap_detail(text: &str) -> String {
    text.to_string()
}
