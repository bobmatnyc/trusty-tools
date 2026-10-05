//! BM25 lexical index for the code corpus — trusty-search's domain wrapper
//! around the shared scorer in `trusty_common::bm25` (issues #156, #5828).
//!
//! Why: the BM25 implementation lives in `trusty-common` so trusty-search and
//! trusty-memory score identically. The two crates diverge on everything
//! *around* the scorer, though: trusty-search keys documents by chunk id and
//! rebuilds the index from redb/usearch, while trusty-memory keeps per-palace
//! snapshots on disk (`trusty_memory::bm25_index::PalaceBm25Index`). Memory
//! already owned a domain type over the shared core; search only re-exported
//! it, so search-specific behaviour had nowhere to go except inside the shared
//! implementation that memory also depends on.
//!
//! What: [`CodeBm25Index`] is a newtype over `trusty_common::bm25::BM25Index`
//! exposing exactly the operations the code indexer performs — insert/replace a
//! chunk, drop a chunk, score a query, and report corpus size. Every method
//! delegates straight through, so scoring, tokenization, and the per-document
//! term cap are byte-for-byte the shared core's.
//!
//! #5828 removed the historic `pub use trusty_common::bm25::{tokenize,
//! BM25Index as Bm25Index}` this module used to be. It left `core::bm25`
//! exporting two names for BM25 — the shared scorer under an alias, and now a
//! domain type — when trusty-search itself called neither the alias nor
//! `tokenize` anywhere. A caller that wants the raw scorer should depend on
//! `trusty-common` and name it, which is what trusty-memory already does.
//!
//! Test: `wrapper_upsert_then_score_finds_the_document`,
//! `wrapper_remove_document_drops_it_from_scoring`,
//! `wrapper_len_and_is_empty_track_the_corpus`, and
//! `wrapper_upsert_reporting_accepts_a_normal_document` in this file; the
//! indexer paths are covered by `test_remove_chunk_removes_from_results`,
//! `test_persist_and_load_chunks`, and the BM25 lane tests in
//! `core::indexer::search::lanes_tests`.

use trusty_common::bm25::BM25Index;

/// The lexical half of a code index — a BM25 corpus keyed by chunk id.
///
/// Why: trusty-search's BM25 concerns are its own. Documents are chunks, ids
/// are `path:start:end` strings, and the index is rebuilt from redb rather than
/// loaded from a snapshot. Owning a type here means a search-specific change —
/// a different document-text policy, per-chunk bookkeeping, instrumentation —
/// lands in trusty-search instead of in the scorer that trusty-memory shares.
///
/// What: a newtype over `trusty_common::bm25::BM25Index`. It adds no state and
/// no behaviour; each method forwards to the inner index unchanged.
///
/// Test: the `tests` module at the bottom of this file.
#[non_exhaustive]
pub struct CodeBm25Index {
    inner: BM25Index,
}

impl CodeBm25Index {
    /// Construct an empty index with the shared scorer's default parameters.
    pub fn new() -> Self {
        // #5828: delegate rather than reimplement — k1/b stay the shared core's.
        Self {
            inner: BM25Index::new(),
        }
    }

    /// Number of live documents in the corpus.
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    /// Whether the corpus holds no documents.
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// The corpus cap the shared core enforces on new documents (#9235).
    pub fn corpus_cap() -> usize {
        trusty_common::bm25::effective_corpus_cap()
    }

    /// Insert or replace the document for `chunk_id`.
    ///
    /// Why: the ingest, commit, and persist-restore paths all re-index a chunk
    /// by id; replacing in place keeps the corpus free of stale postings when a
    /// file changes.
    /// What: forwards to `BM25Index::upsert_document`, which removes any
    /// existing slot for the id before adding the new tokens.
    /// Test: `wrapper_upsert_then_score_finds_the_document`.
    pub fn upsert_document(&mut self, chunk_id: &str, text: &str) {
        self.inner.upsert_document(chunk_id, text);
    }

    /// Insert or replace `chunk_id`, reporting whether the document was kept.
    ///
    /// Why: the idle-eviction rehydrate loop needs a per-rebuild dropped count
    /// rather than the process-wide log-once that `upsert_document` emits.
    /// What: forwards to `BM25Index::upsert_document_reporting`; `false` means
    /// the document exceeded the shared per-document term cap and was dropped.
    /// Test: `wrapper_upsert_reporting_accepts_a_normal_document`.
    pub fn upsert_document_reporting(&mut self, chunk_id: &str, text: &str) -> bool {
        self.inner.upsert_document_reporting(chunk_id, text)
    }

    /// Drop `chunk_id` from the corpus.
    ///
    /// Why: `remove_file` and `remove_chunk` must evict lexical postings along
    /// with the chunk itself, or a deleted chunk keeps ranking.
    /// What: forwards to `BM25Index::remove_document`; unknown ids are a no-op.
    /// Test: `wrapper_remove_document_drops_it_from_scoring`.
    pub fn remove_document(&mut self, chunk_id: &str) {
        self.inner.remove_document(chunk_id);
    }

    /// Score `query` against the whole corpus, returning the top `top_k`
    /// `(chunk_id, score)` pairs.
    pub fn score_query_all(&self, query: &str, top_k: usize) -> Vec<(String, f32)> {
        self.inner.score_query_all(query, top_k)
    }

    /// Score `query`, keeping only chunk ids `filter` accepts.
    ///
    /// Why: path-prefix search must apply the filter BEFORE `top_k` truncation,
    /// or a match beyond the cap is lost. That ordering is the shared core's;
    /// this method only forwards to it.
    /// What: forwards to `BM25Index::score_query_all_with_filter`.
    /// Test: `core::indexer::tests::path_filter_search::
    /// test_path_prefix_filter_recovers_bm25_match_beyond_want`.
    pub fn score_query_all_with_filter(
        &self,
        query: &str,
        top_k: usize,
        filter: &dyn Fn(&str) -> bool,
    ) -> Vec<(String, f32)> {
        self.inner.score_query_all_with_filter(query, top_k, filter)
    }

    /// Chunk ids whose postings carry every term in `terms`.
    ///
    /// Why: the #7675 exact-match floor verifies a literal against candidates
    /// rather than scanning every chunk's content. Forwarding keeps the
    /// intersection in the shared scorer that owns the inverted index.
    /// What: forwards to `BM25Index::docs_containing_all`; `None` means no
    /// candidate set is derivable (no terms), an empty `Vec` means no chunk
    /// carries all of them.
    /// Test: `core::indexer::tests::exact_match_floor::
    /// the_postings_candidate_path_and_the_full_scan_agree`.
    pub fn docs_containing_all(&self, terms: &[String]) -> Option<Vec<&str>> {
        self.inner.docs_containing_all(terms)
    }
}

impl Default for CodeBm25Index {
    fn default() -> Self {
        Self::new()
    }
}

/// How much of the resident corpus the BM25 lane is missing (#9235).
///
/// Why: the chunk cap (`TRUSTY_MAX_CHUNKS`) exceeds the BM25 corpus cap at the
/// top of every memory tier, so a large index can hold chunks that BM25
/// silently refused. `bm25_lane_degraded` means "not converged yet"; this is
/// the separate, steady-state "converged but truncated" signal.
/// What: `docs_dropped = chunk_count - bm25_len`, saturating at 0;
/// `truncated` is `docs_dropped > 0`; `corpus_cap` is the cap in force.
/// Test: `truncation_saturates_and_flags_only_a_shortfall`,
/// `every_tier_reports_truncation_when_its_chunk_cap_exceeds_its_bm25_cap`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct Bm25Truncation {
    /// `true` when at least one resident chunk is absent from BM25.
    pub truncated: bool,
    /// Resident chunks absent from BM25.
    pub docs_dropped: u64,
    /// The BM25 corpus cap upserts enforce right now.
    pub corpus_cap: u64,
}

impl Bm25Truncation {
    /// Build the report from a chunk count, a BM25 live-doc count and a cap.
    pub fn from_counts(chunk_count: usize, bm25_len: usize, corpus_cap: usize) -> Self {
        let docs_dropped = chunk_count.saturating_sub(bm25_len) as u64;
        Self {
            truncated: docs_dropped > 0,
            docs_dropped,
            corpus_cap: corpus_cap as u64,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Bm25Truncation, CodeBm25Index};

    /// A chunk indexed through the wrapper must be findable by one of its
    /// terms, with a positive score — the delegation carries scoring through.
    #[test]
    fn wrapper_upsert_then_score_finds_the_document() {
        let mut idx = CodeBm25Index::new();
        idx.upsert_document("a.rs:1:1", "fn authenticate user token");
        idx.upsert_document("b.rs:1:1", "fn render template html");

        let hits = idx.score_query_all("authenticate", 5);
        let (id, score) = hits
            .iter()
            .find(|(id, _)| id == "a.rs:1:1")
            .expect("the matching chunk must rank");
        assert_eq!(id, "a.rs:1:1");
        assert!(*score > 0.0, "a matching chunk must score above zero");
    }

    /// Removal must evict the postings, not merely the id — the removed chunk
    /// disappears from scoring while its sibling still ranks.
    #[test]
    fn wrapper_remove_document_drops_it_from_scoring() {
        let mut idx = CodeBm25Index::new();
        idx.upsert_document("a.rs:1:1", "fn authenticate user token");
        idx.upsert_document("b.rs:1:1", "fn authenticate session cookie");

        idx.remove_document("a.rs:1:1");

        let hits = idx.score_query_all("authenticate", 5);
        assert!(
            !hits.iter().any(|(id, _)| id == "a.rs:1:1"),
            "removed chunk still ranks: {hits:?}"
        );
        assert!(
            hits.iter().any(|(id, _)| id == "b.rs:1:1"),
            "removal evicted the wrong chunk: {hits:?}"
        );
    }

    /// `len` / `is_empty` must track upserts and removals, since the BM25 lane
    /// and the idle-eviction path both branch on emptiness.
    #[test]
    fn wrapper_len_and_is_empty_track_the_corpus() {
        let mut idx = CodeBm25Index::new();
        assert!(idx.is_empty());
        assert_eq!(idx.len(), 0);

        idx.upsert_document("a.rs:1:1", "fn authenticate");
        assert!(!idx.is_empty());
        assert_eq!(idx.len(), 1);

        // Re-upserting the same id replaces rather than appends.
        idx.upsert_document("a.rs:1:1", "fn authenticate again");
        assert_eq!(idx.len(), 1);

        idx.remove_document("a.rs:1:1");
        assert!(idx.is_empty());
    }

    /// The reporting variant returns `true` for a document within the shared
    /// per-document term cap, and indexes it identically to `upsert_document`.
    #[test]
    fn wrapper_upsert_reporting_accepts_a_normal_document() {
        let mut idx = CodeBm25Index::new();
        assert!(idx.upsert_document_reporting("a.rs:1:1", "fn authenticate user"));
        assert_eq!(idx.len(), 1);
        assert!(idx
            .score_query_all("authenticate", 5)
            .iter()
            .any(|(id, _)| id == "a.rs:1:1"));
    }

    /// #9235: a shortfall reports truncation and its size; an equal or larger
    /// BM25 count (mid-rehydrate, before the chunk map fills) saturates to 0.
    #[test]
    fn truncation_saturates_and_flags_only_a_shortfall() {
        let over = Bm25Truncation::from_counts(250_000, 200_000, 200_000);
        assert!(over.truncated);
        assert_eq!(over.docs_dropped, 50_000);
        assert_eq!(over.corpus_cap, 200_000);

        let under = Bm25Truncation::from_counts(10, 10, 200_000);
        assert!(!under.truncated);
        assert_eq!(under.docs_dropped, 0);

        let ahead = Bm25Truncation::from_counts(0, 10, 200_000);
        assert!(!ahead.truncated);
        assert_eq!(ahead.docs_dropped, 0);
    }
}
