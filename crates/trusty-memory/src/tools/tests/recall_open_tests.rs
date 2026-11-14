//! #11255: a single-palace recall never parks a runtime worker on a palace
//! open, and a slow open answers a typed, retryable `PalaceOpening`.
//!
//! Why: `memory_recall` opened its palace inline on the worker, so a cold or
//! queued open held the call (and the worker) for up to the 60 s open-queue
//! bound and then failed with a generic `-32603`.
//! What: stalls the open by holding the palace's open-queue lock, drives
//! `tools/call` through `handle_message` on the test's single-threaded
//! runtime, and checks the wire answer, the bounded wait, and that the open
//! goes on to register the palace so a retry succeeds. A stalled open is
//! injected; no test waits on a real 15 s or 60 s bound.
//! Test: this IS the test module.

use super::*;
use crate::engine::PalaceRegistry;
use crate::service::op_limits::RecallLimits;
use crate::tools::recall_open::{CODE_PALACE_OPENING, PalaceOpening};
use serde_json::Value;
use std::sync::Arc;
use std::time::Duration;

/// The open wait the test injects; far below the registry's queue bound.
const OPEN_WAIT: Duration = Duration::from_millis(200);
/// The registry's open-queue bound in this test; an inline open waits this.
const QUEUE_BOUND: Duration = Duration::from_secs(5);

/// A `tools/call` of `memory_recall` for `palace`.
fn recall_call(palace: &str, query: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {"name": "memory_recall", "arguments": {"palace": palace, "query": query}},
    })
}

/// Why (#11255, F1): the defect and its error arm. With the open stalled, the
/// call must come back after the open wait — not the queue bound — with code
/// `-32007` and `data.kind == "palace_opening"`, `retryable: true`. On the
/// single-threaded test runtime an inline open would block the only worker,
/// so the timer could not fire before the queue bound.
/// What: once the lock is released the detached open registers the palace,
/// and the same call then succeeds.
/// Test: this function.
#[tokio::test]
#[allow(clippy::await_holding_lock)] // #11255: holding the open lock is the point
async fn a_held_palace_open_answers_palace_opening_and_the_open_continues() {
    let (state, tmp) = test_state();
    let mut state = state.with_recall_limits(RecallLimits {
        open_wait: OPEN_WAIT,
        all_deadline: Duration::from_secs(20),
    });
    state.registry = Arc::new(
        PalaceRegistry::from_env()
            .with_data_root(tmp.path())
            .with_open_queue_timeout(QUEUE_BOUND),
    );
    dispatch_tool(&state, "palace_create", json!({"name": "open-held"}))
        .await
        .expect("palace_create");
    let text = "A recall of a palace still opening answers palace_opening";
    dispatch_tool(
        &state,
        "memory_remember",
        json!({"palace": "open-held", "text": text, "room": "General"}),
    )
    .await
    .expect("memory_remember");
    let id = PalaceId::new("open-held");
    state.registry.remove(&id);

    let gate = state.registry.open_lock_for_test(&id);
    let held = gate.lock();
    let started = std::time::Instant::now();
    let reply = crate::handle_message(&state, recall_call("open-held", text)).await;
    let waited = started.elapsed();
    assert_eq!(reply["error"]["code"], CODE_PALACE_OPENING, "{reply}");
    assert_eq!(reply["error"]["data"]["kind"], "palace_opening", "{reply}");
    assert_eq!(reply["error"]["data"]["retryable"], true, "{reply}");
    assert_eq!(reply["error"]["data"]["palace"], "open-held", "{reply}");
    assert!(
        waited < QUEUE_BOUND,
        "the recall waited {waited:?}, the whole queue bound"
    );
    drop(held);

    // The open was left running; it registers the palace once the lock frees.
    let bound = tokio::time::Instant::now() + Duration::from_secs(15);
    while state.registry.peek(&id).is_none() {
        assert!(
            tokio::time::Instant::now() < bound,
            "the detached open never registered the palace"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let retry = crate::handle_message(&state, recall_call("open-held", text)).await;
    assert!(retry.get("error").is_none(), "retry failed: {retry}");
    assert!(retry["result"]["content"][0]["text"].is_string(), "{retry}");
}

/// Why (#11255, F1): the daemon's raw JSON-RPC wire must branch the same way
/// as the MCP wire, and the type must survive an added context.
/// What: a context-wrapped `PalaceOpening` through `from_anyhow`.
/// Test: this function.
#[test]
fn palace_opening_maps_to_its_own_rpc_code() {
    let opening = PalaceOpening {
        palace: "p".into(),
        waited: Duration::from_millis(1500),
    };
    let e = anyhow::Error::from(opening).context("memory_recall");
    let reply = crate::transport::rpc::JsonRpcResponse::from_anyhow(json!(1), e);
    let err = reply.error.expect("an error response");
    assert_eq!(err.code, CODE_PALACE_OPENING);
    assert_eq!(
        err.data,
        Some(
            json!({"kind": "palace_opening", "retryable": true, "palace": "p", "waited_ms": 1500})
        )
    );
}
