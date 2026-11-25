//! #11613: a per-palace tool whose palace cannot open within the client's
//! exchange answers `-32007` before its handler runs.
//!
//! Why: the first call after an eviction re-opened the palace inside the
//! request and outlasted the client's 5 s exchange.
//! What: evicts or removes the palace, stalls its re-open by holding the
//! registry's open lock, and drives calls through `handle_message`.
//! Test: this IS the test module.

use super::*;
use crate::engine::PalaceRegistry;
use crate::service::op_limits::RecallLimits;
use crate::tools::recall_open::CODE_PALACE_OPENING;
use serde_json::Value;
use std::sync::Arc;
use std::time::Duration;

/// The client's per-exchange budget (`memory_rpc::DEFAULT_TIMEOUT`).
const CLIENT_EXCHANGE: Duration = Duration::from_secs(5);

/// A `tools/call` of `tool` with `args`.
fn call(tool: &str, args: Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {"name": tool, "arguments": args},
    })
}

/// A state with one created palace `name`, open wait `open_wait`, and an
/// open-queue bound `queue_bound` (an inline open waits this long).
async fn state_with(
    name: &str,
    open_wait: Duration,
    queue_bound: Duration,
) -> (AppState, tempfile::TempDir) {
    let (state, tmp) = test_state();
    let mut state = state.with_recall_limits(RecallLimits {
        open_wait,
        all_deadline: Duration::from_secs(20),
    });
    state.registry = Arc::new(
        PalaceRegistry::from_env()
            .with_data_root(tmp.path())
            .with_open_queue_timeout(queue_bound),
    );
    dispatch_tool(&state, "palace_create", json!({"name": name}))
        .await
        .expect("palace_create");
    (state, tmp)
}

/// Wait, bounded, until the registry holds `id`.
async fn until_resident(state: &AppState, id: &PalaceId) {
    let bound = tokio::time::Instant::now() + Duration::from_secs(15);
    while state.registry.peek(id).is_none() {
        assert!(tokio::time::Instant::now() < bound, "never registered");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// The drawers `palace` holds, through `memory_list`.
async fn drawer_count(state: &AppState, palace: &str) -> usize {
    let listed = dispatch_tool(state, "memory_list", json!({"palace": palace, "limit": 50}))
        .await
        .expect("memory_list");
    listed["drawers"].as_array().expect("drawers").len()
}

/// Why (#11613, criterion 3): the first call after an idle sweep re-opened
/// the palace inline; an open slower than the client's exchange broke it.
/// What: the palace IS evicted by `evict_idle`, and its re-open is stalled
/// past the exchange (held open lock, 8 s queue bound). `memory_list` must
/// answer inside the exchange, with `-32007`. Before #11613 it waited on the
/// inline open for the 8 s bound.
/// Test: this function.
#[tokio::test]
#[allow(clippy::await_holding_lock)] // #11613: holding the open lock is the point
async fn first_call_after_eviction_answers_within_the_exchange_budget() {
    let (state, _tmp) =
        state_with("evicted", Duration::from_secs(15), Duration::from_secs(8)).await;
    let id = PalaceId::new("evicted");
    state
        .registry
        .peek(&id)
        .expect("resident after create")
        .last_accessed
        .store(0, std::sync::atomic::Ordering::Relaxed);
    assert_eq!(state.registry.evict_idle(Duration::from_secs(1)), 1);
    assert!(state.registry.peek(&id).is_none(), "the sweep evicted it");

    let gate = state.registry.open_lock_for_test(&id);
    let held = gate.lock();
    let started = std::time::Instant::now();
    let reply =
        crate::handle_message(&state, call("memory_list", json!({"palace": "evicted"}))).await;
    let waited = started.elapsed();
    drop(held);
    assert!(waited < CLIENT_EXCHANGE, "waited {waited:?}: {reply}");
    assert_eq!(reply["error"]["code"], CODE_PALACE_OPENING, "{reply}");
    until_resident(&state, &id).await;
}

/// Why (#11613, Fail-Open Check): a client re-sends a write that got
/// `-32007`; that is safe only if the refused write did no work.
/// What: a `memory_remember` against a stalled open answers `-32007`; once the
/// open finishes the palace holds no drawer; the re-sent write stores one.
/// Test: this function.
#[tokio::test]
#[allow(clippy::await_holding_lock)] // #11613: holding the open lock is the point
async fn a_write_to_a_palace_opening_past_the_budget_is_refused_then_applied_once() {
    let (state, _tmp) = state_with("w", Duration::from_millis(200), Duration::from_secs(5)).await;
    let id = PalaceId::new("w");
    state.registry.remove(&id);
    let write = call(
        "memory_remember",
        json!({"palace": "w", "text": "The gate refuses a write before it runs", "room": "General"}),
    );

    let gate = state.registry.open_lock_for_test(&id);
    let held = gate.lock();
    let reply = crate::handle_message(&state, write.clone()).await;
    drop(held);
    assert_eq!(reply["error"]["code"], CODE_PALACE_OPENING, "{reply}");

    until_resident(&state, &id).await;
    assert_eq!(drawer_count(&state, "w").await, 0, "the refused write ran");
    let retry = crate::handle_message(&state, write).await;
    assert!(retry.get("error").is_none(), "retry failed: {retry}");
    assert_eq!(drawer_count(&state, "w").await, 1, "applied exactly once");
}

/// Why (#11613, Fail-Open Check): a background open that fails must reach the
/// caller as an error on its next ask, never as an empty or successful answer.
/// What: the open is stalled, the call answers `-32007`, the palace's
/// `palace.json` is then corrupted and the lock released, so the background
/// open fails. The next `memory_list` is an error that is not `-32007`.
/// Test: this function.
#[tokio::test]
#[allow(clippy::await_holding_lock)] // #11613: holding the open lock is the point
async fn a_failed_background_open_is_an_error_on_the_next_ask() {
    let (state, tmp) =
        state_with("broken", Duration::from_millis(200), Duration::from_secs(5)).await;
    let id = PalaceId::new("broken");
    state.registry.remove(&id);
    let list = call("memory_list", json!({"palace": "broken"}));

    let gate = state.registry.open_lock_for_test(&id);
    let held = gate.lock();
    let reply = crate::handle_message(&state, list.clone()).await;
    assert_eq!(reply["error"]["code"], CODE_PALACE_OPENING, "{reply}");
    std::fs::write(tmp.path().join("broken").join("palace.json"), b"{not json")
        .expect("corrupt palace.json");
    drop(held);
    // Let the background open finish (and fail) before the next ask.
    tokio::time::sleep(Duration::from_millis(500)).await;

    let next = crate::handle_message(&state, list).await;
    let code = &next["error"]["code"];
    assert!(code.is_i64(), "a failed open answered success: {next}");
    assert_ne!(code, CODE_PALACE_OPENING, "{next}");
    assert!(state.registry.peek(&id).is_none(), "{next}");
}
