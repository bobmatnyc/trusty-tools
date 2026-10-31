//! The `search.index.embed_missing` socket surface (#10551).
//!
//! Why: the MCP tool and the CLI both call this method, so its refusal codes
//! and its `retryable` field are the contract they relay.
//! Test: this file IS the test module for `super::embed_missing`.

use std::sync::Arc;

use trusty_common::uds::server::RpcRouter;

use crate::core::embed::{Embedder, MockEmbedder};
use crate::core::indexer::CodeIndexer;
use crate::core::registry::{IndexHandle, IndexId, IndexRegistry};
use crate::core::store::{UsearchStore, VectorStore};
use crate::service::rpc::writes;
use crate::service::server::SearchAppState;

fn router_over(handle: IndexHandle) -> RpcRouter {
    let registry = IndexRegistry::new();
    registry.register(handle);
    let state = Arc::new(SearchAppState::new(registry));
    writes::register(RpcRouter::new(), &state)
}

fn bare(id: &str, indexer: CodeIndexer) -> IndexHandle {
    IndexHandle::bare(
        IndexId::new(id.to_string()),
        Arc::new(tokio::sync::RwLock::new(indexer)),
        "/nonexistent/embed-missing".into(),
    )
}

async fn call(rpc: &RpcRouter, id: &str) -> trusty_common::uds::server::RpcResponse {
    let frame = serde_json::to_vec(&serde_json::json!({
        "jsonrpc": "2.0", "id": 1, "method": writes::METHOD_INDEX_EMBED_MISSING,
        "params": { "index_id": id, "body": { "dry_run": false } },
    }))
    .expect("encode the frame");
    rpc.dispatch(&frame).await
}

/// A covered index answers `noop` with the documented fields.
/// Test: this test.
#[tokio::test]
#[serial_test::parallel]
async fn embed_missing_over_the_socket_matches_the_core_body() {
    let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder::new(8));
    let store: Arc<dyn VectorStore> = Arc::new(UsearchStore::new(8).expect("usearch"));
    let indexer = CodeIndexer::new("em-socket", "/nonexistent/embed-missing")
        .with_components(embedder, store);
    let rpc = router_over(bare("em-socket", indexer));

    let response = call(&rpc, "em-socket").await;
    let body = response.result.expect("a result");
    assert_eq!(body["action"], "noop");
    assert_eq!(body["missing"], 0);
    assert_eq!(body["index_id"], "em-socket");
}

/// An index with no embedder is a non-retryable 409 refusal.
/// Test: this test.
#[tokio::test]
#[serial_test::parallel]
async fn embed_missing_without_an_embedder_is_a_409_over_the_socket() {
    let indexer = CodeIndexer::new("em-none", "/nonexistent/embed-missing");
    let rpc = router_over(bare("em-none", indexer));

    let response = call(&rpc, "em-none").await;
    let error = response.error.expect("a refusal");
    let data = error.data.expect("the refusal body");
    assert_eq!(data["error"], "embed_missing_not_applicable");
    assert_eq!(data["retryable"], false);
}

/// An unknown index is a 404.
/// Test: this test.
#[tokio::test]
#[serial_test::parallel]
async fn embed_missing_on_an_unknown_index_is_a_404() {
    let indexer = CodeIndexer::new("em-known", "/nonexistent/embed-missing");
    let rpc = router_over(bare("em-known", indexer));
    let response = call(&rpc, "em-absent").await;
    assert!(response.error.is_some(), "an unknown index must be refused");
}
