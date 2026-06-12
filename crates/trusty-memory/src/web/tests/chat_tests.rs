//! Tests for chat providers, chat sessions, and messages endpoints.

use super::super::router;
use super::test_state;
use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use serde_json::{json, Value};
use tower::util::ServiceExt;
use trusty_common::memory_core::palace::PalaceId;

/// Why: `/api/v1/chat/providers` must return a well-shaped payload even
/// when no provider is available, so the SPA can render disabled states
/// without special-casing missing fields.
/// What: Hit the endpoint on a fresh state; assert it returns `providers`
/// (an array of length 2) and an `active` field (possibly null).
/// Test: This test itself.
#[tokio::test]
async fn providers_endpoint_returns_payload() {
    let state = test_state();
    let app = router().with_state(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/chat/providers")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = to_bytes(resp.into_body(), 4096).await.unwrap();
    let v: Value = serde_json::from_slice(&bytes).unwrap();
    let arr = v["providers"].as_array().expect("providers array");
    assert_eq!(arr.len(), 2);
    let names: Vec<&str> = arr.iter().filter_map(|p| p["name"].as_str()).collect();
    assert!(names.contains(&"ollama"));
    assert!(names.contains(&"openrouter"));
    // `active` may be null when no provider is configured/reachable.
    assert!(v.get("active").is_some());
}

/// Why: Chat-session CRUD must round-trip end-to-end through the HTTP
/// surface — create returns an id, list shows it, get returns the
/// (empty) history, delete removes it.
/// What: Create a palace, then exercise the four session endpoints
/// sequentially, asserting JSON shapes at each step.
/// Test: This test itself.
#[tokio::test]
async fn chat_session_crud_round_trip() {
    let state = test_state();
    // Pre-create a palace dir so session store has a place to live.
    let palace = trusty_common::memory_core::Palace {
        id: PalaceId::new("sess-test"),
        name: "sess-test".to_string(),
        description: None,
        created_at: chrono::Utc::now(),
        data_dir: state.data_root.join("sess-test"),
    };
    state
        .registry
        .create_palace(&state.data_root, palace)
        .expect("create_palace");
    let app = router().with_state(state);

    // Create
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/palaces/sess-test/chat/sessions")
                .header("content-type", "application/json")
                .body(Body::from(json!({"title":"first chat"}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = to_bytes(resp.into_body(), 4096).await.unwrap();
    let v: Value = serde_json::from_slice(&bytes).unwrap();
    let sid = v["id"].as_str().expect("session id").to_string();

    // List
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v1/palaces/sess-test/chat/sessions")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = to_bytes(resp.into_body(), 4096).await.unwrap();
    let v: Value = serde_json::from_slice(&bytes).unwrap();
    let arr = v.as_array().expect("array");
    assert!(arr.iter().any(|s| s["id"] == sid));

    // Get
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/api/v1/palaces/sess-test/chat/sessions/{sid}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // Delete
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/api/v1/palaces/sess-test/chat/sessions/{sid}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    // Get after delete -> 404
    let resp = app
        .oneshot(
            Request::builder()
                .uri(format!("/api/v1/palaces/sess-test/chat/sessions/{sid}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

/// Why: issue #99 — the HTTP surface for inter-project messaging is what
/// `trusty-memory send-message` and `trusty-memory inbox-check` both
/// drive. We pin the round-trip (send → list-unread → mark-read →
/// list-empty) so a future refactor cannot accidentally break either
/// CLI without a failing test.
/// What: pre-creates the recipient palace, POSTs a message, asserts
/// `unread_only=true` returns exactly one entry with the right
/// envelope fields, POSTs to mark_read, asserts the unread inbox is
/// now empty, and confirms the audit view (`unread_only=false`) still
/// surfaces the read message.
/// Test: this test itself.
#[tokio::test]
async fn messages_endpoint_round_trip() {
    let state = test_state();
    let palace = trusty_common::memory_core::Palace {
        id: PalaceId::new("msg-test"),
        name: "msg-test".to_string(),
        description: None,
        created_at: chrono::Utc::now(),
        data_dir: state.data_root.join("msg-test"),
    };
    state
        .registry
        .create_palace(&state.data_root, palace)
        .expect("create_palace");
    let app = router().with_state(state);

    // POST /api/v1/messages — send.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/messages")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "to_palace":   "msg-test",
                        "from_palace": "sender-palace",
                        "purpose":     "task",
                        "content":     "please refresh schema"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = to_bytes(resp.into_body(), 4096).await.unwrap();
    let send_resp: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(send_resp["status"], "sent");
    let drawer_id = send_resp["drawer_id"]
        .as_str()
        .expect("drawer_id")
        .to_string();

    // GET unread inbox.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v1/messages?palace=msg-test&unread_only=true")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = to_bytes(resp.into_body(), 8192).await.unwrap();
    let list: Vec<Value> = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0]["id"], drawer_id);
    assert_eq!(list[0]["from_palace"], "sender-palace");
    assert_eq!(list[0]["to_palace"], "msg-test");
    assert_eq!(list[0]["purpose"], "task");
    assert_eq!(list[0]["content"], "please refresh schema");
    assert_eq!(list[0]["read"], false);
    assert!(list[0]["formatted"]
        .as_str()
        .unwrap()
        .contains("sender-palace"));

    // POST mark_read.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/messages/mark_read")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"palace": "msg-test", "drawer_id": drawer_id}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = to_bytes(resp.into_body(), 1024).await.unwrap();
    let mark: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(mark["flipped"], true);

    // GET unread again — empty.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v1/messages?palace=msg-test&unread_only=true")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = to_bytes(resp.into_body(), 4096).await.unwrap();
    let list: Vec<Value> = serde_json::from_slice(&bytes).unwrap();
    assert!(list.is_empty(), "inbox cleared after mark_read");

    // GET history (unread_only=false) — still has the message, now read.
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/messages?palace=msg-test&unread_only=false")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = to_bytes(resp.into_body(), 8192).await.unwrap();
    let history: Vec<Value> = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(history.len(), 1);
    assert_eq!(history[0]["read"], true);
}

/// Why: The chat assistant's tool surface is part of the public API — any
/// drift in tool names or required-argument lists is a breaking change for
/// the UI and any external automation. Pin the shape here so a refactor
/// has to acknowledge it.
/// What: Snapshots the names + every tool's `required` array.
/// Test: This test itself.
#[test]
fn all_tools_returns_expected_set() {
    let tools = crate::chat::all_tools();
    let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(
        names,
        vec![
            "list_palaces",
            "get_palace",
            "recall_memories",
            "list_drawers",
            "kg_query",
            "get_config",
            "get_status",
            "get_dream_status",
            "get_palace_dream_status",
            "create_memory",
            "kg_assert",
            "memory_recall_all",
        ]
    );
    // Every tool's `parameters` must be a JSON Schema object with a
    // `required` array (possibly empty).
    for t in &tools {
        assert_eq!(
            t.parameters["type"], "object",
            "tool {} schema type",
            t.name
        );
        assert!(
            t.parameters["required"].is_array(),
            "tool {} required not array",
            t.name
        );
    }
}

/// Why: `execute_tool` is the bridge between the model's tool_call
/// arguments and the live Rust core. We exercise the happy path
/// (`list_palaces` on an empty registry returns `[]`) and the unknown-
/// tool path (returns `{"error": "..."}`) to lock down both branches.
/// What: Calls execute_tool against a fresh `AppState`.
/// Test: This test itself.
#[tokio::test]
async fn execute_tool_dispatches_known_tools() {
    let state = test_state();
    let result = crate::chat::execute_tool("list_palaces", "{}", &state).await;
    assert!(
        result.is_array(),
        "list_palaces should be array, got {result}"
    );
    assert_eq!(result.as_array().unwrap().len(), 0);

    let unknown = crate::chat::execute_tool("not_a_tool", "{}", &state).await;
    assert!(
        unknown["error"]
            .as_str()
            .unwrap_or("")
            .contains("unknown tool"),
        "expected unknown-tool error, got {unknown}"
    );

    let missing = crate::chat::execute_tool("get_palace", "{}", &state).await;
    assert!(
        missing["error"]
            .as_str()
            .unwrap_or("")
            .contains("palace_id"),
        "expected missing-arg error, got {missing}"
    );
}
