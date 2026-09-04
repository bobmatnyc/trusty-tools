//! Route-level coverage for `POST /indexes/:id/quantize` and the live
//! precision `GET /indexes/:id/status` reports (issue #6822).
//!
//! Why: the store-level conversion is proved in
//! `tests/vector_quant_default_6822.rs`; what these add is the DAEMON contract
//! an operator actually drives — that the dry run names the index and its chunk
//! count, that an absent `quant` field means the current default rather than
//! nothing, and that an unrecognised precision is refused instead of silently
//! defaulted on a one-way whole-arena rewrite.
//!
//! Why no env mutation: these build the store at the #6822 default (f16, no env
//! var needed) and convert DOWN to f32, so nothing here writes
//! `TRUSTY_VECTOR_QUANT` — the race the #3769 note on `store_config.rs`'s test
//! module records is avoided by construction rather than by a lock.
//! Test: this module. Run with `cargo test -p trusty-search tests_quantize_6822`.

use super::quantize_handlers::{quantize_handler, QuantizeRequest};
use super::state::SearchAppState;
use super::status::index_status_handler;
use crate::core::embed::{Embedder, MockEmbedder};
use crate::core::indexer::CodeIndexer;
use crate::core::registry::{IndexHandle, IndexId, IndexRegistry};
use crate::core::store::{UsearchStore, VectorStore};
use axum::extract::{Json, Path, State};
use std::sync::Arc;
use tokio::sync::RwLock;

/// Seed one index whose vector store is built at the #6822 default precision
/// and already has an on-disk snapshot (so a conversion has somewhere to
/// publish to), and return the app state beside the temp dirs that must outlive
/// it.
async fn seeded(
    id: &str,
) -> (
    Arc<SearchAppState>,
    tempfile::TempDir,
    tempfile::TempDir,
    Arc<UsearchStore>,
) {
    let (root_dir, root) = super::test_support::allowlisted_index_root("ts-6822-");
    let snap_dir = tempfile::tempdir().expect("snapshot dir");
    std::fs::create_dir_all(root.join("src")).expect("create src");
    let contents = "fn quantize_probe() { /* probe */ }";
    std::fs::write(root.join("src/probe.rs"), contents).expect("write source");

    let dim = 16;
    let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder::new(dim));
    let store = Arc::new(UsearchStore::new(dim).expect("usearch"));
    let indexer = CodeIndexer::new(id, &root).with_components(
        Arc::clone(&embedder),
        Arc::clone(&store) as Arc<dyn VectorStore>,
    );
    indexer
        .index_files_batch(&[("src/probe.rs".to_string(), contents.to_string())])
        .await
        .expect("seed one chunk");
    // Record a snapshot path on the store — `requantize` publishes to the path
    // the store itself recorded, and a never-saved store has none.
    store
        .save_to(&snap_dir.path().join("hnsw.usearch"))
        .await
        .expect("seed snapshot");

    let registry = IndexRegistry::new();
    registry.register(IndexHandle::bare(
        IndexId::new(id),
        Arc::new(RwLock::new(indexer)),
        root.clone(),
    ));
    let state = Arc::new(SearchAppState::new(registry));
    state.install_embedder(Arc::clone(&embedder)).await;
    (state, root_dir, snap_dir, store)
}

/// #6822: `/status` must report the precision the LIVE index holds. Reporting
/// `TRUSTY_VECTOR_QUANT` instead would name what the NEXT index gets, which is
/// the opposite of the truth on every index the backfill exists for.
#[tokio::test]
async fn status_reports_the_live_vector_quantization() {
    let (state, _root, _snap, _store) = seeded("q-status").await;
    let Json(body) = index_status_handler(State(state), Path("q-status".to_string()))
        .await
        .expect("status must succeed");
    assert_eq!(
        body["semantic_coverage"]["vector_quant"],
        serde_json::json!("f16"),
        "a store built under the #6822 default must report f16: {body}"
    );
}

/// #6822: the dry run reports without writing, and the applied run converts the
/// live index. Both name the index and its chunk count — the confirmation an
/// operator acts on.
#[tokio::test]
async fn quantize_converts_the_live_index_and_reports_the_chunk_count() {
    let (state, _root, _snap, store) = seeded("q-convert").await;

    let Json(preview) = quantize_handler(
        State(Arc::clone(&state)),
        Path("q-convert".to_string()),
        Some(Json(QuantizeRequest {
            quant: Some("f32".to_string()),
            dry_run: Some(true),
        })),
    )
    .await
    .expect("dry run must succeed");
    assert_eq!(preview["index_id"], serde_json::json!("q-convert"));
    assert!(
        preview["chunk_count"].as_u64().is_some_and(|n| n > 0),
        "the dry run must name a chunk count: {preview}"
    );
    assert_eq!(preview["report"]["current"], serde_json::json!("f16"));
    assert_eq!(preview["report"]["target"], serde_json::json!("f32 (none)"));
    assert_eq!(preview["report"]["applied"], serde_json::json!(false));
    assert_eq!(preview["report"]["dry_run"], serde_json::json!(true));
    assert_eq!(
        store.live_quant().await.map(|q| q.label()),
        Some("f16"),
        "a dry run must not touch the live index"
    );

    let Json(applied) = quantize_handler(
        State(Arc::clone(&state)),
        Path("q-convert".to_string()),
        Some(Json(QuantizeRequest {
            quant: Some("f32".to_string()),
            dry_run: Some(false),
        })),
    )
    .await
    .expect("applied run must succeed");
    assert_eq!(applied["report"]["applied"], serde_json::json!(true));
    assert_eq!(
        store.live_quant().await.map(|q| q.label()),
        Some("f32 (none)"),
        "the applied run must convert the live index: {applied}"
    );
}

/// #6822: an absent `quant` field means the current default. A body-less POST
/// against an index already at that default is therefore a reported no-op, not
/// a conversion — which is what makes the backfill safe to re-run over a fleet.
#[tokio::test]
async fn quantize_defaults_to_the_env_default() {
    let (state, _root, _snap, _store) = seeded("q-default").await;
    let Json(body) = quantize_handler(State(state), Path("q-default".to_string()), None)
        .await
        .expect("body-less POST must succeed");
    assert_eq!(body["report"]["target"], serde_json::json!("f16"));
    assert_eq!(
        body["report"]["applied"],
        serde_json::json!(false),
        "already at the target precision is a no-op: {body}"
    );
}

/// #6822: a mistyped precision is refused. The env parser degrades to the
/// default by design; a request for a one-way whole-arena rewrite must not.
#[tokio::test]
async fn quantize_rejects_an_unknown_precision() {
    let (state, _root, _snap, _store) = seeded("q-bad").await;
    let err = quantize_handler(
        State(state),
        Path("q-bad".to_string()),
        Some(Json(QuantizeRequest {
            quant: Some("fp8".to_string()),
            dry_run: Some(true),
        })),
    )
    .await
    .expect_err("an unknown precision must be refused");
    assert_eq!(err.0, axum::http::StatusCode::BAD_REQUEST);
    assert!(
        err.1["error"].as_str().is_some_and(|e| e.contains("fp8")),
        "the refusal must name the value: {:?}",
        err.1
    );
}
