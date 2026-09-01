//! The embedding pause/resume socket surface and its status projection (#6524).
//!
//! Why: three claims the console proxy and UI are about to build on — the two
//! methods answer one shape, an unknown index is refused rather than silently
//! accepted, and `GET /indexes/{id}/status` reports the pause where a consumer
//! will look for it. None of them holds on `origin/main`, where the methods and
//! the field do not exist.
//!
//! Test: this file IS the test module for `super::embedding_pause`.

use std::sync::Arc;

use trusty_common::uds::server::RpcRouter;

use crate::core::indexer::CodeIndexer;
use crate::core::registry::{IndexHandle, IndexId, IndexRegistry};
use crate::service::rpc::error::CODE_NOT_FOUND;
use crate::service::rpc::writes;
use crate::service::server::{index_status_report, SearchAppState};

/// One state carrying one registered index, plus the write router built on it.
async fn state_with_index(id: &str) -> (Arc<SearchAppState>, RpcRouter) {
    let registry = IndexRegistry::new();
    let root = format!("/nonexistent/pause-{id}");
    registry.register(IndexHandle::bare(
        IndexId::new(id.to_string()),
        Arc::new(tokio::sync::RwLock::new(CodeIndexer::new(id, &root))),
        root.into(),
    ));
    let state = Arc::new(SearchAppState::new(registry));
    let rpc = writes::register(RpcRouter::new(), &state);
    (state, rpc)
}

async fn dispatch(
    rpc: &RpcRouter,
    method: &str,
    params: serde_json::Value,
) -> trusty_common::uds::server::RpcResponse {
    let frame = serde_json::to_vec(&serde_json::json!({
        "jsonrpc": "2.0", "id": 1, "method": method, "params": params,
    }))
    .expect("encode the frame");
    rpc.dispatch(&frame).await
}

async fn rpc_ok(rpc: &RpcRouter, method: &str, id: &str) -> serde_json::Value {
    let response = dispatch(rpc, method, serde_json::json!({ "index_id": id })).await;
    assert!(
        response.error.is_none(),
        "{method} must answer a result: {:?}",
        response.error
    );
    response.result.expect("a non-error frame carries a result")
}

/// Pause then resume, over the socket, answering the documented shape both ways.
///
/// Why: `{"index_id", "embedding_paused"}` is the contract the console proxy
/// (#6524 PR 2) and the UI toggle (PR 3) are written against, so the field
/// names and the boolean's direction are the thing to pin.
/// What: dispatches both methods and asserts the body and the resulting handle
/// state after each.
/// Test: this test.
#[tokio::test]
async fn pause_then_resume_round_trips_over_the_socket() {
    let (state, rpc) = state_with_index("pause-round-trip").await;
    let handle = state
        .registry
        .get(&IndexId::new("pause-round-trip"))
        .expect("the fixture index is registered");
    assert!(!handle.embedding_pause.is_paused(), "starts un-paused");

    let paused = rpc_ok(
        &rpc,
        writes::METHOD_INDEX_PAUSE_EMBEDDING,
        "pause-round-trip",
    )
    .await;
    assert_eq!(
        paused,
        serde_json::json!({ "index_id": "pause-round-trip", "embedding_paused": true })
    );
    assert!(handle.embedding_pause.is_paused());

    let resumed = rpc_ok(
        &rpc,
        writes::METHOD_INDEX_RESUME_EMBEDDING,
        "pause-round-trip",
    )
    .await;
    assert_eq!(
        resumed,
        serde_json::json!({ "index_id": "pause-round-trip", "embedding_paused": false })
    );
    assert!(!handle.embedding_pause.is_paused());
}

/// Both methods are idempotent — a repeat is a success, not an error.
///
/// Why: two consoles, or one operator clicking twice, must not produce a
/// refusal. The caller asked "is it paused now", and the answer does not depend
/// on how many times it was asked.
/// What: dispatches each method twice and asserts both bodies are identical.
/// Test: this test.
#[tokio::test]
async fn pause_and_resume_are_idempotent_over_the_socket() {
    let (_state, rpc) = state_with_index("pause-idempotent").await;
    let first = rpc_ok(
        &rpc,
        writes::METHOD_INDEX_PAUSE_EMBEDDING,
        "pause-idempotent",
    )
    .await;
    let second = rpc_ok(
        &rpc,
        writes::METHOD_INDEX_PAUSE_EMBEDDING,
        "pause-idempotent",
    )
    .await;
    assert_eq!(first, second, "a repeated pause answers the same body");

    let first = rpc_ok(
        &rpc,
        writes::METHOD_INDEX_RESUME_EMBEDDING,
        "pause-idempotent",
    )
    .await;
    let second = rpc_ok(
        &rpc,
        writes::METHOD_INDEX_RESUME_EMBEDDING,
        "pause-idempotent",
    )
    .await;
    assert_eq!(first, second, "a repeated resume answers the same body");
}

/// An unknown index is refused on both methods.
///
/// Why: a control that silently accepts a typo'd id reports success for a pause
/// that never happened — the worst outcome for a surface whose whole job is to
/// tell an operator whether the heavy work stopped.
/// What: dispatches both methods against an unregistered id and asserts the
/// not-found JSON-RPC code.
/// Test: this test.
#[tokio::test]
async fn pausing_or_resuming_an_unknown_index_is_refused() {
    let (_state, rpc) = state_with_index("pause-known").await;
    for method in [
        writes::METHOD_INDEX_PAUSE_EMBEDDING,
        writes::METHOD_INDEX_RESUME_EMBEDDING,
    ] {
        let response = dispatch(
            &rpc,
            method,
            serde_json::json!({ "index_id": "no-such-index-6524" }),
        )
        .await;
        let error = response
            .error
            .unwrap_or_else(|| panic!("{method} must refuse an unknown index"));
        assert_eq!(
            error.code, CODE_NOT_FOUND,
            "{method} must refuse with not-found, got {error:?}"
        );
    }
}

/// `search.index.status` reports the pause on `stages.semantic`, and only there.
///
/// Why: the status body is where a UI reads the pause back, and it must not
/// change the `status` string a consumer already branches on — `in_progress`
/// with `paused: true` beside it is the whole contract.
/// What: reads the status body before and after a pause, asserting
/// `stages.semantic.paused` flips and the other two stages never do.
/// Test: this test.
#[tokio::test]
async fn status_reports_the_semantic_stage_as_paused() {
    let (state, rpc) = state_with_index("pause-status").await;

    let before = index_status_report(&state, "pause-status")
        .await
        .expect("a registered index answers a status body");
    assert_eq!(before["stages"]["semantic"]["paused"], false);

    rpc_ok(&rpc, writes::METHOD_INDEX_PAUSE_EMBEDDING, "pause-status").await;

    let after = index_status_report(&state, "pause-status")
        .await
        .expect("a registered index answers a status body");
    assert_eq!(after["stages"]["semantic"]["paused"], true);
    assert_eq!(
        after["stages"]["lexical"]["paused"], false,
        "only embedding is pausable"
    );
    assert_eq!(after["stages"]["graph"]["paused"], false);
    assert_eq!(
        after["stages"]["semantic"]["status"], before["stages"]["semantic"]["status"],
        "a pause must not rewrite the stage status consumers branch on"
    );
}
