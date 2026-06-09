#[test]
fn test_query_classifier_smoke() {
    use trusty_search::core::classifier::{QueryClassifier, QueryIntent};
    assert_eq!(
        QueryClassifier::classify("fn authenticate"),
        QueryIntent::Definition
    );
    assert_eq!(
        QueryClassifier::classify("how does auth work"),
        QueryIntent::Conceptual
    );
}

#[test]
fn test_bm25_smoke() {
    use trusty_search::core::bm25::Bm25Index;
    let mut idx = Bm25Index::new();
    idx.add_document(0, "rust async tokio search");
    idx.add_document(1, "python django web framework");
    let s = idx.score("rust tokio", 0);
    assert!(s > 0.0);
}

// ── Bm25Index boundary / property tests ──────────────────────────────────────
//
// `core::bm25` is a pure re-export (`BM25Index as Bm25Index`), so the unit
// tests that exercise its constructor surface live here in the integration
// harness, which already imports via `trusty_search::core::bm25::Bm25Index`.

/// Default top-k cap used by the boundary tests below.
const TOP_K: usize = 10;

/// Empty corpus: `score_query_all` must return an empty list — there are no
/// documents to rank.
#[test]
fn bm25_empty_corpus_returns_no_results() {
    use trusty_search::core::bm25::Bm25Index;
    let idx = Bm25Index::new();
    let results = idx.score_query_all("rust tokio", TOP_K);
    assert!(
        results.is_empty(),
        "empty corpus must produce zero results, got {results:?}"
    );
}

/// Single-document corpus: querying a term that appears in the document must
/// return that document with a positive score.
#[test]
fn bm25_single_doc_matching_term_scores_positive() {
    use trusty_search::core::bm25::Bm25Index;
    let mut idx = Bm25Index::new();
    idx.upsert_document("doc-a", "rust async programming tokio runtime");
    let results = idx.score_query_all("rust async", TOP_K);
    assert!(
        !results.is_empty(),
        "a query matching the only document must return at least one result"
    );
    let (doc_id, score) = &results[0];
    assert_eq!(doc_id, "doc-a");
    assert!(*score > 0.0, "matching document must have a positive score");
}

/// Score monotonicity: a document with more occurrences of the query term
/// should score greater than or equal to one with fewer occurrences.
/// Verifies that BM25 term-frequency weighting is preserved through the
/// re-export boundary.
///
/// Scores are looked up by document id rather than by position so the test
/// does not depend on tie-break sort order.
#[test]
fn bm25_higher_term_frequency_scores_higher_or_equal() {
    use trusty_search::core::bm25::Bm25Index;
    let mut idx = Bm25Index::new();
    // "search" appears once in doc-low, five times in doc-high.
    idx.upsert_document("doc-low", "search is useful");
    idx.upsert_document("doc-high", "search search search search search engine");

    let results = idx.score_query_all("search", TOP_K);
    assert_eq!(results.len(), 2, "both documents contain the query term");

    // Look up each document's score by id — no sort-order assumption.
    let score_high = results
        .iter()
        .find(|(id, _)| id == "doc-high")
        .map(|(_, s)| *s)
        .expect("doc-high must be present in results");
    let score_low = results
        .iter()
        .find(|(id, _)| id == "doc-low")
        .map(|(_, s)| *s)
        .expect("doc-low must be present in results");

    assert!(
        score_high >= score_low,
        "higher TF should score >= lower TF: doc-high={score_high}, doc-low={score_low}"
    );
}

#[test]
fn test_chunker_smoke() {
    use trusty_search::core::chunker::chunk_text;
    let content = "fn foo() {}\nfn bar() {}\n";
    let chunks = chunk_text("test.rs", content, 150, 50);
    assert!(!chunks.is_empty());
    assert!(chunks[0].id.starts_with("test.rs:"));
}

#[test]
fn test_index_registry() {
    use std::sync::Arc;
    use tokio::sync::RwLock;
    use trusty_search::core::{
        indexer::CodeIndexer,
        registry::{IndexHandle, IndexId, IndexRegistry},
    };

    let registry = IndexRegistry::new();
    let id = IndexId::new("test-project");
    let indexer = CodeIndexer::new("test-project", "/tmp/test");
    registry.register(IndexHandle::bare(
        id.clone(),
        Arc::new(RwLock::new(indexer)),
        "/tmp/test".into(),
    ));
    assert!(registry.get(&id).is_some());
    assert_eq!(registry.len(), 1);
}
