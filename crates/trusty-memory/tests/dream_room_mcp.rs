//! Integration tests for the `dream_consolidate_room` and `palace_dream` MCP
//! tools (spec-001 Phase 3, issue #1721).
//!
//! Why: confirms both on-demand consolidation tools are wired into the
//! dispatcher, accept the spec's arguments (palace / room / max_age_days), and
//! return the `{ summary_facts_created, facts_evicted }` shape synchronously.
//! `palace_dream` is the canonical name; `dream_consolidate_room` is retained
//! for backward compatibility.
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

/// `palace_dream` (issue #1721) is wired as an alias for `dream_consolidate_room`.
///
/// Why: issue #1721 specifies the on-demand consolidation tool be callable as
/// `palace_dream` to follow the `palace_*` naming convention. Both names must
/// route to the same underlying logic; this test verifies the alias.
/// What: creates an empty palace and calls `palace_dream` with room + age
/// filters; verifies the response shape matches the spec.
#[tokio::test]
async fn palace_dream_no_inference_returns_gracefully() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = ready_state(&tmp);
    let cwd = tmp.path().to_string_lossy().to_string();

    dispatch_tool(
        &state,
        "palace_create",
        json!({ "name": "dreamapp2", "force": true, "cwd": cwd }),
    )
    .await
    .expect("create palace");

    // With no OPENROUTER_API_KEY and an empty palace, consolidation is a no-op.
    let result = dispatch_tool(
        &state,
        "palace_dream",
        json!({ "palace": "dreamapp2", "room": "General", "max_age_days": 7 }),
    )
    .await
    .expect("palace_dream returns gracefully even without inference");

    assert_eq!(result["palace"], "dreamapp2");
    assert_eq!(result["summary_facts_created"], 0);
    assert_eq!(result["facts_evicted"], 0);
}
