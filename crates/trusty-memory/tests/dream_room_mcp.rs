//! Integration tests for the `dream_consolidate_room` MCP tool (spec-001 Phase 3).
//!
//! Why: confirms the on-demand consolidation tool is wired into the dispatcher,
//! accepts the spec's arguments (palace / room / max_age_days), and returns the
//! `{ summary_facts_created, facts_evicted }` shape synchronously.
//! What: drives `dispatch_tool` against an `AppState` rooted at a tempdir. The
//! palace is created empty, so the scoped consolidation short-circuits on an
//! empty snapshot and never invokes any inference backend — keeping the test
//! deterministic and offline regardless of ambient OpenRouter configuration.
//! Behavioural coverage (room filtering, Task skip, eviction) lives in
//! `trusty_common::memory_core::dream::tests::consolidate_scoped_*`.
//! Test: this IS the test module.

use serde_json::json;
use tempfile::TempDir;
use trusty_memory::tools::dispatch_tool;
use trusty_memory::AppState;

fn ready_state(tmp: &TempDir) -> AppState {
    let state = AppState::new(tmp.path().to_path_buf());
    state.set_ready();
    state
}

/// The tool dispatches, accepts room + max_age_days, and returns zero counts
/// for an empty palace (no inference call made).
#[tokio::test]
async fn dream_consolidate_room_returns_shape() {
    // No env mutation: the palace is created empty, so `consolidate_scoped`
    // short-circuits on an empty drawer snapshot and never calls an inference
    // backend — the result is zero counts regardless of any ambient
    // OPENROUTER_API_KEY. Mutating `std::env` here would be a data race under
    // the default multi-threaded `#[tokio::test]` runtime.
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = ready_state(&tmp);
    let cwd = tmp.path().to_string_lossy().to_string();

    // Create a custom-slug palace via the Phase 1 force flag.
    dispatch_tool(
        &state,
        "palace_create",
        json!({ "name": "dreamapp", "force": true, "cwd": cwd }),
    )
    .await
    .expect("create palace");

    // All rooms (room omitted).
    let all = dispatch_tool(
        &state,
        "dream_consolidate_room",
        json!({ "palace": "dreamapp" }),
    )
    .await
    .expect("consolidate all rooms");
    assert_eq!(all["palace"], "dreamapp");
    assert_eq!(all["summary_facts_created"], 0);
    assert_eq!(all["facts_evicted"], 0);

    // Scoped to a specific room with an explicit age window.
    let scoped = dispatch_tool(
        &state,
        "dream_consolidate_room",
        json!({ "palace": "dreamapp", "room": "Backend", "max_age_days": 3 }),
    )
    .await
    .expect("consolidate Backend");
    assert_eq!(scoped["room"], "Backend");
    assert_eq!(scoped["summary_facts_created"], 0);
    assert_eq!(scoped["facts_evicted"], 0);
}
