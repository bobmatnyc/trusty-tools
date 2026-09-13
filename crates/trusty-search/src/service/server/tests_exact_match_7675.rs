//! `meta.exact_match_floor` reporting for the search handler (#7675).
//!
//! Why: the floor changes which chunk ranks first, so a caller must be able to
//! see that it was a literal occurrence and not a semantic guess — without
//! re-running the query against `search_lexical` to find out.
//! What: asserts the two `meta` keys through the handler's JSON body, for a
//! literal query that the floor answers and for a conceptual one it declines.
//! Both assert through the response body rather than the Rust API, so they
//! compile against the pre-fix commit and fail on the assertion — `meta`
//! carried neither key.
//! Test: this module.

use super::*;
use axum::Json;

/// Build a single-index daemon state holding `chunks`, ready for the handler.
async fn state_with(
    chunks: &[(&str, &str, &str)],
) -> (
    Arc<SearchAppState>,
    tempfile::TempDir,
    Arc<dyn crate::core::embed::Embedder>,
) {
    use crate::core::embed::{Embedder, MockEmbedder};
    use crate::core::indexer::CodeIndexer;
    use crate::core::registry::{IndexHandle, IndexId, IndexRegistry, StageStatus};
    use crate::core::store::{UsearchStore, VectorStore};

    let tmp = tempfile::tempdir().expect("tempdir");
    let dim = 16;
    let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder::new(dim));
    let store: Arc<dyn VectorStore> = Arc::new(UsearchStore::new(dim).expect("usearch"));
    let indexer = CodeIndexer::new("exact-7675", tmp.path())
        .with_components(Arc::clone(&embedder), Arc::clone(&store));
    for (id, file, content) in chunks {
        indexer
            .add_chunk(super::tests_dropped_results::drop_test_chunk(
                id,
                file,
                content,
                crate::core::chunker::ChunkType::Code,
            ))
            .await
            .expect("add chunk");
    }
    let registry = IndexRegistry::new();
    let handle = IndexHandle::bare(
        IndexId::new("exact-7675"),
        Arc::new(tokio::sync::RwLock::new(indexer)),
        tmp.path().to_path_buf(),
    );
    let stages = Arc::clone(&handle.stages);
    registry.register(handle);
    {
        let mut s = stages.write().await;
        s.lexical.status = StageStatus::Ready;
        s.semantic.status = StageStatus::Ready;
    }
    let state = Arc::new(SearchAppState::new(registry));
    state.install_embedder(Arc::clone(&embedder)).await;
    (state, tmp, embedder)
}

async fn meta_for(state: &Arc<SearchAppState>, text: &str) -> serde_json::Value {
    let resp = search_handler(
        axum::extract::State(Arc::clone(state)),
        axum::extract::Path("exact-7675".to_string()),
        axum::extract::Json(super::tests_dropped_results::drop_test_query(
            text,
            crate::core::indexer::SearchMode::All,
            None,
        )),
    )
    .await;
    let Json(body) = resp.expect("handler must succeed");
    body.get("meta").cloned().expect("meta block present")
}

#[tokio::test]
async fn search_meta_reports_the_exact_match_floor() {
    // Why: #7675 requirement 3 — the response must say which floor applied and
    // which literal it matched, so a caller can explain the top hit.
    // What: an identifier query reports `true` plus the literal; a conceptual
    // query reports `false` and a null literal.
    // Test: this test.
    let (state, _tmp, _emb) = state_with(&[
        (
            "src/render.rs:1:3",
            "src/render.rs",
            "fn render_savings_segment(total: u64) -> String { format!(\"{total}\") }",
        ),
        (
            "src/other.rs:1:3",
            "src/other.rs",
            "fn percent_saved(&self) -> f64 { 0.0 }",
        ),
    ])
    .await;

    let meta = meta_for(&state, "render_savings_segment").await;
    assert_eq!(
        meta["exact_match_floor"],
        serde_json::json!(true),
        "an identifier with a verbatim occurrence must report the floor; meta={meta:?}"
    );
    assert_eq!(
        meta["exact_match_literal"],
        serde_json::json!("render_savings_segment"),
        "the matched literal must travel with the flag; meta={meta:?}"
    );

    let meta = meta_for(&state, "how is percentage saving displayed to a user").await;
    assert_eq!(
        meta["exact_match_floor"],
        serde_json::json!(false),
        "a conceptual query must report no floor; meta={meta:?}"
    );
}
