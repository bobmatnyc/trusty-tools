//! `Code` mode's docstring and file-type filters, and the `exclude_archived`
//! filter, must not empty a page they could have filled (#9404).
//!
//! Why: these filters ran after `materialize_search_results` cut the fused
//! list to `top_k`, so a `BugDebt` query whose `top_k` best candidates were
//! all docstrings, doc files, or archived chunks answered `results: []` while
//! matching code sat deeper in the oversampled candidate set.
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

/// A `.markdown` doc that repeats every query term. `Code` mode's file-type
/// filter rejects it, but `doc_score_penalty` does not classify the extension,
/// so its score is not penalised and it outranks the code.
fn markdown_doc(i: usize) -> RawChunk {
    raw(
        &format!("docs/guide_{i}.markdown"),
        format!("CORS error {i}: send the room list. Room list CORS error on send."),
        ChunkType::Code,
    )
}

/// #9404 follow-up: the file-type filter also ran after the `top_k` cut. Five
/// doc files outrank four code chunks, so with `top_k: 3` every kept slot held
/// a doc file and the page came back empty. The code deeper in the candidate
/// set must fill it, capped at `top_k`, in rank order.
#[tokio::test]
async fn bugdebt_query_backfills_code_past_doc_files_filling_top_k() {
    let mut chunks: Vec<RawChunk> = (0..5).map(markdown_doc).collect();
    chunks.extend((0..4).map(code));
    let idx = indexer(chunks).await;

    // Fixture shape: unfiltered, the top 3 are all doc files.
    let unfiltered = idx
        .search(&query(3, SearchMode::All))
        .await
        .expect("search");
    assert!(
        unfiltered.iter().all(|c| c.id.starts_with("docs/guide_")),
        "precondition: doc files fill the unfiltered top_k: {unfiltered:?}"
    );

    let (results, dropped) = idx
        .search_with_drops(&query(3, SearchMode::Code))
        .await
        .expect("search");
    let ids: Vec<&str> = results.iter().map(|c| c.id.as_str()).collect();
    assert_eq!(results.len(), 3, "the page is filled to top_k: {ids:?}");
    assert!(
        results.iter().all(|c| c.id.starts_with("src/handler_")),
        "only source files survive Code mode: {ids:?}"
    );
    assert!(
        results.windows(2).all(|w| w[0].score >= w[1].score),
        "fused rank order is kept: {results:?}"
    );
    assert_eq!(
        dropped.mode_filtered, 5,
        "every doc file ranked ahead of a returned row was withheld"
    );
}

/// #9404 follow-up: when only doc files match, `Code` mode returns an empty
/// page. Unlike the docstring filter, the file-type filter is the mode itself,
/// so a doc file never stands in for code; `mode_filtered` counts every doc
/// file withheld, so the empty page is not an unexplained miss.
#[tokio::test]
async fn doc_file_only_matches_return_an_empty_page_with_every_row_counted() {
    let unrelated = raw(
        "src/other.rs",
        "fn unrelated() -> bool { true }".to_string(),
        ChunkType::Code,
    );
    let mut chunks: Vec<RawChunk> = (0..5).map(markdown_doc).collect();
    chunks.push(unrelated);
    let idx = indexer(chunks).await;

    let (results, dropped) = idx
        .search_with_drops(&query(3, SearchMode::Code))
        .await
        .expect("search");
    assert!(results.is_empty(), "no doc file stands in: {results:?}");
    assert_eq!(
        dropped.mode_filtered, 5,
        "every withheld doc file is counted: {dropped:?}"
    );
}

/// A chunk under a `legacy/` path — a strong archive signal — that repeats
/// every query term, so it outranks the code in the fused order.
fn archived(i: usize) -> RawChunk {
    raw(
        &format!("src/legacy/send_{i}.rs"),
        format!("fn send_room_list_{i}() {{ cors_error() }} // CORS error: send the room list"),
        ChunkType::Code,
    )
}

fn excluding_archived(top_k: usize) -> SearchQuery {
    SearchQuery {
        exclude_archived: true,
        ..query(top_k, SearchMode::All)
    }
}

/// #9404 follow-up: the `exclude_archived` retain also ran after the `top_k`
/// cut. Five archived chunks outrank four code chunks, so with `top_k: 3`
/// every kept slot held an archived chunk and the page came back empty. The
/// code deeper in the candidate set must fill it, capped at `top_k`.
#[tokio::test]
async fn exclude_archived_backfills_code_past_archived_chunks_filling_top_k() {
    let mut chunks: Vec<RawChunk> = (0..5).map(archived).collect();
    chunks.extend((0..4).map(code));
    let idx = indexer(chunks).await;

    // Fixture shape: without the flag, the top 3 are all archived chunks.
    let unfiltered = idx
        .search(&query(3, SearchMode::All))
        .await
        .expect("search");
    assert!(
        unfiltered.iter().all(|c| c.id.starts_with("src/legacy/")),
        "precondition: archived chunks fill the unfiltered top_k: {unfiltered:?}"
    );

    let (results, dropped) = idx
        .search_with_drops(&excluding_archived(3))
        .await
        .expect("search");
    let ids: Vec<&str> = results.iter().map(|c| c.id.as_str()).collect();
    assert_eq!(results.len(), 3, "the page is filled to top_k: {ids:?}");
    assert!(
        results.iter().all(|c| c.id.starts_with("src/handler_")),
        "no archived chunk is returned: {ids:?}"
    );
    assert!(
        results.windows(2).all(|w| w[0].score >= w[1].score),
        "the page is sorted by score: {results:?}"
    );
    assert_eq!(
        dropped.archived, 5,
        "every archived chunk ranked ahead of a returned row was withheld"
    );
}

/// #9404 follow-up: when only archived chunks match, `exclude_archived`
/// returns an empty page. The caller asked for no archived code, so none
/// stands in; `archived` counts every chunk withheld, not only the `top_k`
/// the cut used to keep.
#[tokio::test]
async fn archived_only_matches_return_an_empty_page_with_every_row_counted() {
    let unrelated = raw(
        "src/other.rs",
        "fn unrelated() -> bool { true }".to_string(),
        ChunkType::Code,
    );
    let mut chunks: Vec<RawChunk> = (0..5).map(archived).collect();
    chunks.push(unrelated);
    let idx = indexer(chunks).await;

    let (results, dropped) = idx
        .search_with_drops(&excluding_archived(3))
        .await
        .expect("search");
    assert!(
        results.is_empty(),
        "no archived chunk stands in: {results:?}"
    );
    assert_eq!(
        dropped.archived, 5,
        "every withheld archived chunk is counted: {dropped:?}"
    );
}
