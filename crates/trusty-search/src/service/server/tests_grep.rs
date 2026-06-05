//! Tests for the grep endpoint handlers.
use super::*;
use crate::core::registry::IndexId;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use tokio::sync::RwLock;
/// Test: consumed by the `grep_*` tests below.
async fn stage_grep_index(
    files: &[(&str, &str)],
) -> (Arc<SearchAppState>, IndexId, tempfile::TempDir) {
    use crate::core::chunker::{ChunkType, RawChunk};
    use crate::core::embed::{Embedder, MockEmbedder};
    use crate::core::indexer::CodeIndexer;
    use crate::core::registry::{IndexHandle, IndexRegistry};
    use crate::core::store::{UsearchStore, VectorStore};

    let tmp = tempfile::tempdir().expect("tempdir");
    let dim = 16;
    let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder::new(dim));
    let store: Arc<dyn VectorStore> = Arc::new(UsearchStore::new(dim).expect("usearch"));
    let indexer = CodeIndexer::new("grep-test", tmp.path()).with_components(embedder, store);

    for (i, (rel, content)) in files.iter().enumerate() {
        let abs = tmp.path().join(rel);
        if let Some(parent) = abs.parent() {
            std::fs::create_dir_all(parent).expect("mkdirs");
        }
        std::fs::write(&abs, content).expect("write file");
        let chunk = RawChunk {
            id: format!("c{i}"),
            file: rel.to_string(),
            start_line: 1,
            end_line: 1 + content.lines().count(),
            content: content.to_string(),
            function_name: None,
            language: Some("rust".to_string()),
            chunk_type: ChunkType::Code,
            calls: Vec::new(),
            inherits_from: Vec::new(),
            chunk_depth: 0,
            parent_chunk_id: None,
            child_chunk_ids: Vec::new(),
            nlp_keywords: Vec::new(),
            nlp_code_refs: Vec::new(),
            virtual_terms: Vec::new(),
        };
        indexer.add_chunk(chunk).await.expect("add_chunk");
    }

    let registry = IndexRegistry::new();
    let id = IndexId::new("grep-test");
    registry.register(IndexHandle::bare(
        id.clone(),
        Arc::new(RwLock::new(indexer)),
        tmp.path().to_path_buf(),
    ));
    (Arc::new(SearchAppState::new(registry)), id, tmp)
}

fn grep_req(pattern: &str) -> crate::service::grep::GrepRequest {
    serde_json::from_value(serde_json::json!({ "pattern": pattern })).expect("default grep request")
}

/// `POST /indexes/:id/grep` returns line-accurate matches read fresh from
/// the on-disk files the index knows about.
#[tokio::test]
async fn grep_endpoint_returns_matches() {
    let (state, _id, _tmp) = stage_grep_index(&[
        ("src/auth.rs", "// header\nfn authenticate() {}\n"),
        ("src/util.rs", "fn helper() {}\n"),
    ])
    .await;

    let Json(resp) = grep_handler(
        State(state),
        Path("grep-test".to_string()),
        Json(grep_req("authenticate")),
    )
    .await
    .expect("200");

    assert_eq!(resp.total, 1);
    assert!(!resp.truncated);
    assert_eq!(resp.matches[0].file, "src/auth.rs");
    assert_eq!(resp.matches[0].line, 2);
    assert_eq!(resp.matches[0].text, "fn authenticate() {}");
}

/// The glob filter restricts which indexed files are read.
#[tokio::test]
async fn grep_endpoint_honours_glob() {
    let (state, _id, _tmp) = stage_grep_index(&[
        ("src/auth.rs", "fn target() {}\n"),
        ("docs/readme.md", "target appears here too\n"),
    ])
    .await;

    let mut req = grep_req("target");
    req.glob = Some("**/*.rs".to_string());
    let Json(resp) = grep_handler(State(state), Path("grep-test".to_string()), Json(req))
        .await
        .expect("200");
    assert_eq!(resp.total, 1);
    assert_eq!(resp.matches[0].file, "src/auth.rs");
}

/// A malformed regex yields `400 Bad Request` with a JSON error body.
#[tokio::test]
async fn grep_endpoint_bad_regex_is_400() {
    let (state, _id, _tmp) = stage_grep_index(&[("a.rs", "fn x() {}\n")]).await;
    let err = grep_handler(
        State(state),
        Path("grep-test".to_string()),
        Json(grep_req("(unclosed")),
    )
    .await
    .expect_err("400");
    assert_eq!(err.0, StatusCode::BAD_REQUEST);
    assert!(err.1 .0.get("error").is_some());
}

/// An unknown index id yields `404 Not Found`.
#[tokio::test]
async fn grep_endpoint_unknown_index_is_404() {
    let (state, _id, _tmp) = stage_grep_index(&[("a.rs", "fn x() {}\n")]).await;
    let err = grep_handler(
        State(state),
        Path("does-not-exist".to_string()),
        Json(grep_req("x")),
    )
    .await
    .expect_err("404");
    assert_eq!(err.0, StatusCode::NOT_FOUND);
}

/// `POST /grep` (global) fans out across every registered index.
#[tokio::test]
async fn grep_global_fans_out() {
    let (state, _id, _tmp) = stage_grep_index(&[("src/auth.rs", "fn authenticate() {}\n")]).await;
    let Json(resp) = global_grep_handler(State(state), Json(grep_req("authenticate")))
        .await
        .expect("200");
    assert_eq!(resp.total, 1);
    assert_eq!(resp.matches[0].file, "src/auth.rs");
}

/// Global grep with an `index_id` that doesn't exist returns an empty set
/// (tolerant fan-out), not a 404.
#[tokio::test]
async fn grep_global_respects_index_filter() {
    let (state, _id, _tmp) = stage_grep_index(&[("a.rs", "fn x() {}\n")]).await;
    let mut req = grep_req("x");
    req.index_id = Some("nope".to_string());
    let Json(resp) = global_grep_handler(State(state), Json(req))
        .await
        .expect("200");
    assert_eq!(resp.total, 0);
    assert!(!resp.truncated);
}
