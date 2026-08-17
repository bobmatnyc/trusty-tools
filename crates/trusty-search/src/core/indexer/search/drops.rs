//! Per-query accounting of the rows a search retrieved and then discarded.
//!
//! Why: the search pipeline drops candidates in four places after fusion, and
//! each drop shortened the `results` array with no count, no flag, and nothing
//! in `meta`. A `top_k: 10` request returning 3 rows was byte-identical whether
//! 3 chunks matched or 7 were deleted on the way out — which is how #2203
//! presented, as "search is broken", while the corpus was intact and readable.
//! The drops themselves are often correct; being unable to tell them apart from
//! a genuine result count is the defect.
//! What: [`SearchDrops`], a plain per-query tally threaded out of
//! [`crate::core::indexer::CodeIndexer::search_with_drops`] and published as
//! the search response's `meta.dropped` block. One field per drop site, so the
//! reason travels with the count.
//! Test: `search_handler_meta_reports_rows_dropped_when_the_corpus_has_no_matching_row`
//! and `search_handler_meta_reports_rows_dropped_by_the_mode_and_docstring_filters`
//! in `service::server::tests_search`.

/// How many candidates a single search discarded after fusion, and where.
///
/// Why: see the module docs — a caller must be able to distinguish "3 results
/// because 3 matched" from "3 results because 7 were dropped, and here is
/// why". Each field names one drop site rather than summing them, because the
/// remedy differs: `unresolved_corpus` means the corpus could not be read and
/// is a fault, while the filter counts are the requested `mode` doing its job.
/// What: a `Copy` tally, `Serialize`d straight into the response `meta` block.
/// `#[non_exhaustive]` so a future drop site is an additive change.
/// Test: see the module docs.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
#[non_exhaustive]
pub struct SearchDrops {
    /// Fused ids with no row in the corpus, dropped at materialisation.
    /// Non-zero means the durable read failed or the chunk was removed while
    /// the query ran — never a normal outcome of a healthy index.
    pub unresolved_corpus: usize,
    /// Chunks deleted because their file type is outside the requested
    /// [`crate::core::indexer::SearchMode`].
    pub mode_filtered: usize,
    /// Docstring chunks deleted by `Code` mode's chunk-type filter.
    pub docstring_filtered: usize,
    /// Chunks deleted because `exclude_archived` was set and they carry a
    /// strong archive signal.
    pub archived: usize,
    /// Chunks deleted by the HTTP layer's out-of-root post-filter (#541).
    /// Filled in by `service::server::search::search_handler`, which owns that
    /// filter; always `0` on the indexer's own return. Non-zero also sets the
    /// existing `meta.stale_index_root` boolean, which this counts.
    pub out_of_root: usize,
}

impl SearchDrops {
    /// Total rows discarded across every site.
    pub fn total(&self) -> usize {
        self.unresolved_corpus
            + self.mode_filtered
            + self.docstring_filtered
            + self.archived
            + self.out_of_root
    }
}
