//! Integration tests for the chat-session MCP tools (spec-001 Phase 2).
//!
//! Why: trusty-memory exposes a redb-backed per-palace chat store over MCP so
//! applications can persist prompt/response turns. These tests assert the four
//! tools round-trip correctly, that turns survive a store/daemon restart (a new
//! `AppState` over the same data root), and that turns are queryable by session
//! id — the core acceptance criteria for Phase 2.
//! What: drives `dispatch_tool` against an `AppState` rooted at a tempdir. The
//! chat store auto-creates the palace data dir on first use, so no
//! `palace_create` (and no slug-enforcement bypass) is needed.
//! Test: this IS the test module.

use serde_json::json;
use tempfile::TempDir;
use trusty_memory::tools::dispatch_tool;
use trusty_memory::AppState;

const PALACE: &str = "chat-app";

/// Build a ready `AppState` rooted at `root`.
///
/// Why: persistence tests need to construct a second `AppState` over the same
/// root after the first is dropped, so the root is passed in explicitly.
/// What: returns a ready state.
/// Test: used by every test below.
fn state_at(root: &TempDir) -> AppState {
    let state = AppState::new(root.path().to_path_buf());
    state.set_ready();
    state
}

/// `chat_session_create` mints a session and reports a zero message count.
#[tokio::test]
async fn chat_session_create_returns_id() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = state_at(&tmp);
    let created = dispatch_tool(
        &state,
        "chat_session_create",
        json!({ "palace": PALACE, "title": "First chat" }),
    )
    .await
    .expect("create");
    let sid = created["session_id"].as_str().expect("session_id");
    assert!(!sid.is_empty(), "session id is non-empty");
    assert_eq!(created["message_count"], 0);
    assert!(created["created_at"].is_string(), "created_at present");
}

/// A caller-supplied `session_id` is honoured and is idempotent.
#[tokio::test]
async fn chat_session_create_with_explicit_id() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = state_at(&tmp);
    let first = dispatch_tool(
        &state,
        "chat_session_create",
        json!({ "palace": PALACE, "session_id": "fixed-123" }),
    )
    .await
    .expect("create");
    assert_eq!(first["session_id"], "fixed-123");

    // Add a turn, then re-create with the same id: history must be preserved.
    dispatch_tool(
        &state,
        "chat_session_add_turn",
        json!({ "palace": PALACE, "session_id": "fixed-123", "role": "user", "content": "hi" }),
    )
    .await
    .expect("add turn");

    let again = dispatch_tool(
        &state,
        "chat_session_create",
        json!({ "palace": PALACE, "session_id": "fixed-123" }),
    )
    .await
    .expect("re-create idempotent");
    assert_eq!(again["session_id"], "fixed-123");
    assert_eq!(again["message_count"], 1, "existing history preserved");
}

/// `chat_session_add_turn` appends turns and reports the running count.
#[tokio::test]
async fn chat_session_add_turn_appends() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = state_at(&tmp);
    let sid = dispatch_tool(&state, "chat_session_create", json!({ "palace": PALACE }))
        .await
        .expect("create")["session_id"]
        .as_str()
        .expect("sid")
        .to_string();

    let r1 = dispatch_tool(
        &state,
        "chat_session_add_turn",
        json!({ "palace": PALACE, "session_id": sid, "role": "user", "content": "What is redb?" }),
    )
    .await
    .expect("turn 1");
    assert_eq!(r1["message_count"], 1);

    let r2 = dispatch_tool(
        &state,
        "chat_session_add_turn",
        json!({ "palace": PALACE, "session_id": sid, "role": "assistant", "content": "An embedded KV store." }),
    )
    .await
    .expect("turn 2");
    assert_eq!(r2["message_count"], 2);
    assert!(r2["updated_at"].is_string());
}

/// An unknown role is rejected before any write.
#[tokio::test]
async fn chat_session_add_turn_rejects_bad_role() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = state_at(&tmp);
    let err = dispatch_tool(
        &state,
        "chat_session_add_turn",
        json!({ "palace": PALACE, "session_id": "s1", "role": "robot", "content": "x" }),
    )
    .await
    .expect_err("bad role rejected");
    assert!(format!("{err:#}").contains("invalid role"));
}

/// `chat_session_get` returns the full ordered history; missing id errors.
#[tokio::test]
async fn chat_session_get_round_trips() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = state_at(&tmp);
    let sid = dispatch_tool(&state, "chat_session_create", json!({ "palace": PALACE }))
        .await
        .expect("create")["session_id"]
        .as_str()
        .expect("sid")
        .to_string();
    for (role, content) in [("user", "a"), ("assistant", "b"), ("system", "c")] {
        dispatch_tool(
            &state,
            "chat_session_add_turn",
            json!({ "palace": PALACE, "session_id": sid, "role": role, "content": content }),
        )
        .await
        .expect("turn");
    }

    let got = dispatch_tool(
        &state,
        "chat_session_get",
        json!({ "palace": PALACE, "session_id": sid }),
    )
    .await
    .expect("get");
    let history = got["history"].as_array().expect("history");
    assert_eq!(history.len(), 3);
    assert_eq!(history[0]["role"], "user");
    assert_eq!(history[0]["content"], "a");
    assert_eq!(history[2]["role"], "system");

    let missing = dispatch_tool(
        &state,
        "chat_session_get",
        json!({ "palace": PALACE, "session_id": "does-not-exist" }),
    )
    .await;
    assert!(missing.is_err(), "missing session must error");
}

/// `chat_session_list` reports total_count and applies offset + limit.
#[tokio::test]
async fn chat_session_list_paginates() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = state_at(&tmp);
    for i in 0..3 {
        dispatch_tool(
            &state,
            "chat_session_create",
            json!({ "palace": PALACE, "session_id": format!("s{i}") }),
        )
        .await
        .expect("create");
    }

    let all = dispatch_tool(&state, "chat_session_list", json!({ "palace": PALACE }))
        .await
        .expect("list");
    assert_eq!(all["total_count"], 3);
    assert_eq!(all["sessions"].as_array().expect("sessions").len(), 3);

    let page = dispatch_tool(
        &state,
        "chat_session_list",
        json!({ "palace": PALACE, "limit": 1, "offset": 1 }),
    )
    .await
    .expect("list paged");
    assert_eq!(page["total_count"], 3, "total ignores pagination");
    assert_eq!(page["sessions"].as_array().expect("sessions").len(), 1);
}

/// Turns persist across a daemon/store restart and stay queryable by id.
///
/// Why: spec-001 acceptance criterion 2 — durability is the whole point of the
/// redb backing. Dropping the first `AppState` (and its cached store handle)
/// then opening a fresh one over the same data root simulates a restart.
#[tokio::test]
async fn turns_persist_across_restart() {
    let tmp = tempfile::tempdir().expect("tempdir");

    let sid = {
        let state = state_at(&tmp);
        let sid = dispatch_tool(
            &state,
            "chat_session_create",
            json!({ "palace": PALACE, "session_id": "durable" }),
        )
        .await
        .expect("create")["session_id"]
            .as_str()
            .expect("sid")
            .to_string();
        dispatch_tool(
            &state,
            "chat_session_add_turn",
            json!({ "palace": PALACE, "session_id": sid, "role": "user", "content": "remember me" }),
        )
        .await
        .expect("turn");
        sid
        // `state` (and its session-store cache) dropped here.
    };

    // Fresh AppState over the same root — must re-open the on-disk redb store.
    let state2 = state_at(&tmp);
    let got = dispatch_tool(
        &state2,
        "chat_session_get",
        json!({ "palace": PALACE, "session_id": sid }),
    )
    .await
    .expect("get after restart");
    let history = got["history"].as_array().expect("history");
    assert_eq!(history.len(), 1, "turn survived restart");
    assert_eq!(history[0]["content"], "remember me");
}
