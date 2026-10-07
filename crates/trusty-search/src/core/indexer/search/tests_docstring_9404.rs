//! `Code` mode's docstring filter must not empty a page it could have filled
//! (#9404).
//!
//! Why: the filter ran after `materialize_search_results` cut the fused list to
//! `top_k`, so a `BugDebt` query whose `top_k` best candidates were all
//! docstring chunks answered `results: []` while matching code sat deeper in
//! the oversampled candidate set.
//! What: drives `search_with_drops` end to end on a BM25-only indexer (no
//! embedder, so MMR is the identity and the ranking is deterministic) with a
//! pinned lexical stage.
//! Test: this module.

use crate::core::chunker::{ChunkType, RawChunk};
use crate::core::classifier::{QueryClassifier, QueryIntent};
use crate::core::indexer::{CodeIndexer, SearchMode, SearchQuery, SearchStage};

/// `BugDebt` via "error" — the intent `effective_mode` leaves on `Code`.
const QUERY: &str = "send room list CORS error";

fn raw(id: &str, content: String, chunk_type: ChunkType) -> RawChunk {
    RawChunk {
        id: format!("{id}:1:2"),
        file: id.to_string(),
        start_line: 1,
        end_line: 2,
        content,
        function_name: None,
        language: Some("rust".to_string()),
        chunk_type,
        calls: Vec::new(),
        inherits_from: Vec::new(),
        chunk_depth: 0,
        parent_chunk_id: None,
        child_chunk_ids: Vec::new(),
        nlp_keywords: Vec::new(),
        nlp_code_refs: Vec::new(),
        virtual_terms: Vec::new(),
    }
}

/// A docstring that repeats every query term, so BM25 ranks it first.
fn docstring(i: usize) -> RawChunk {
    raw(
        &format!("src/doc_{i}.rs"),
        format!("/// CORS error {i}: send the room list. Room list CORS error on send."),
        ChunkType::Docstring,
    )
}

/// Code that shares only the word "room" with the query.
fn code(i: usize) -> RawChunk {
    raw(
        &format!("src/handler_{i}.rs"),
        format!("fn handler_{i}(req: Request) -> Response {{ respond(load(req)) }} // room {i}"),
        ChunkType::Code,
    )
}

fn query(top_k: usize, mode: SearchMode) -> SearchQuery {
    SearchQuery {
        text: QUERY.to_string(),
        top_k,
        expand_graph: false,
        compact: false,
        mode,
        stage: Some(SearchStage::Lexical),
        ..Default::default()
    }
}

async fn indexer(chunks: Vec<RawChunk>) -> CodeIndexer {
    assert_eq!(QueryClassifier::classify(QUERY), QueryIntent::BugDebt);
    let idx = CodeIndexer::new("ds-9404", "/tmp/ds-9404");
    for c in chunks {
        idx.add_chunk(c).await.expect("add chunk");
    }
    idx
}

/// #9404: five docstrings outrank four code chunks; with `top_k: 3` every slot
/// the cut kept was a docstring, so the page came back empty. The code chunks
/// deeper in the candidate set must fill it, capped at `top_k`, in rank order.
#[tokio::test]
async fn bugdebt_query_backfills_code_past_a_docstring_filled_top_k() {
    let mut chunks: Vec<RawChunk> = (0..5).map(docstring).collect();
    chunks.extend((0..4).map(code));
    let idx = indexer(chunks).await;

    // Fixture shape: unfiltered, the top 3 are all docstrings.
    let unfiltered = idx
        .search(&query(3, SearchMode::All))
        .await
        .expect("search");
    assert!(
        unfiltered.iter().all(|c| c.id.starts_with("src/doc_")),
        "precondition: docstrings fill the unfiltered top_k: {unfiltered:?}"
    );

    let (results, dropped) = idx
        .search_with_drops(&query(3, SearchMode::Code))
        .await
        .expect("search");
    let ids: Vec<&str> = results.iter().map(|c| c.id.as_str()).collect();
    assert_eq!(results.len(), 3, "the page is filled to top_k: {ids:?}");
    assert!(
        results.iter().all(|c| c.id.starts_with("src/handler_")),
        "only code chunks survive Code mode: {ids:?}"
    );
    assert!(
        results.windows(2).all(|w| w[0].score >= w[1].score),
        "fused rank order is kept: {results:?}"
    );
    assert_eq!(
        dropped.docstring_filtered, 5,
        "every docstring ranked ahead of a returned row was withheld"
    );
}

/// #9404: when every candidate is a docstring, the filter would turn a matching
/// index into an empty success. The docstrings come back instead, in rank
/// order, and nothing is reported as withheld.
#[tokio::test]
async fn docstring_only_matches_return_the_docstrings_not_an_empty_page() {
    let unrelated = raw(
        "src/other.rs",
        "fn unrelated() -> bool { true }".to_string(),
        ChunkType::Code,
    );
    let idx = indexer(vec![docstring(0), docstring(1), unrelated]).await;

    let (results, dropped) = idx
        .search_with_drops(&query(3, SearchMode::Code))
        .await
        .expect("search");
    let ids: Vec<&str> = results.iter().map(|c| c.id.as_str()).collect();
    assert_eq!(ids.len(), 2, "both docstring matches are returned: {ids:?}");
    assert!(
        ids.iter().all(|id| id.starts_with("src/doc_")),
        "only the matching docstrings come back: {ids:?}"
    );
    assert_eq!(
        dropped.docstring_filtered, 0,
        "nothing was withheld: {dropped:?}"
    );
}
