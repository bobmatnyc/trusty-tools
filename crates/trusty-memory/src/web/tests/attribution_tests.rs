//! Tests for MCP/HTTP drawer creator attribution and hook emit failure isolation.

use super::super::router;
use super::test_state;
use crate::AppState;
use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use serde_json::{json, Value};
use tower::util::ServiceExt;
use trusty_common::memory_core::palace::PalaceId;

/// Why (submission-logging Part A): every hook firing must produce an
/// activity-feed entry tagged `source=hook` so a normal Claude Code
/// session that only triggers hooks no longer leaves the TUI feed
/// empty. The simplest direct check is to POST to the hook ingestion
/// endpoint and confirm the new entry shows up in `GET /api/v1/activity`.
/// What: posts a `HookEventPayload` to `/api/v1/activity/hook`, then
/// queries `/api/v1/activity?source=hook&limit=1` and asserts a row
/// exists with the matching event_type and source.
/// Test: itself.
#[tokio::test]
async fn hook_fired_activity_emit_smoke() {
    let state = test_state();
    let app = router().with_state(state.clone());

    let payload = serde_json::json!({
        "palace_id": "alpha",
        "palace_name": "alpha",
        "hook_type": "UserPromptSubmit",
        "injection_kind": "prompt-context",
        "injection_length": 256,
        "trigger_prompt_excerpt": "test prompt",
        "duration_ms": 12,
    });
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/activity/hook")
                .header("content-type", "application/json")
                .body(Body::from(payload.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    // Issue #232: the hook handler emits via the fire-and-forget
    // `spawn_blocking` path; wait for the write to settle before
    // reading the activity history endpoint.
    state.flush_activity_writes().await;

    // Read it back through the activity history endpoint.
    let app = router().with_state(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/activity?source=hook&limit=10")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = to_bytes(resp.into_body(), 4096).await.unwrap();
    let v: Value = serde_json::from_slice(&bytes).unwrap();
    let entries = v["entries"].as_array().expect("entries array");
    assert!(
        !entries.is_empty(),
        "expected at least one hook activity row, got {entries:?}"
    );
    let first = &entries[0];
    assert_eq!(first["source"], "hook");
    assert_eq!(first["event_type"], "hook_fired");
    assert_eq!(first["palace_id"], "alpha");
    let body = &first["payload"];
    assert_eq!(body["hook_type"], "UserPromptSubmit");
    assert_eq!(body["injection_kind"], "prompt-context");
}

/// Why (submission-logging Part B): an HTTP drawer write with no
/// client-identifying header must still produce a drawer carrying a
/// `creator:client=unknown-http-client` tag so operators can recognise
/// "writer didn't self-identify" as distinct from "writer is known".
/// What: creates a palace via the registry, POSTs a drawer with no
/// `X-Trusty-Client-Name` header, lists the palace drawers, asserts
/// the new drawer carries the four creator tags with the default
/// client name and `source=http`.
/// Test: itself.
#[tokio::test]
async fn drawer_creator_attribution_http_default() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().to_path_buf();
    let state = AppState::new(root);
    let palace = trusty_common::memory_core::Palace {
        id: PalaceId::new("cred-default"),
        name: "cred-default".to_string(),
        description: None,
        created_at: chrono::Utc::now(),
        data_dir: state.data_root.join("cred-default"),
    };
    state
        .registry
        .create_palace(&state.data_root, palace)
        .expect("create palace");

    let app = router().with_state(state.clone());
    let body = serde_json::json!({
        "content": "hello world from anonymous client",
        "tags": ["user-tag"],
    });
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/palaces/cred-default/drawers")
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // Inspect the persisted drawer's tags.
    let app = router().with_state(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/palaces/cred-default/drawers?limit=10")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = to_bytes(resp.into_body(), 8192).await.unwrap();
    let v: Value = serde_json::from_slice(&bytes).unwrap();
    let drawers = v.as_array().expect("drawers array");
    assert_eq!(drawers.len(), 1, "expected one drawer, got {drawers:?}");
    let tags: Vec<&str> = drawers[0]["tags"]
        .as_array()
        .expect("tags array")
        .iter()
        .filter_map(|t| t.as_str())
        .collect();
    assert!(
        tags.contains(&"user-tag"),
        "user-supplied tag must survive; got {tags:?}"
    );
    assert!(
        tags.contains(&"creator:client=unknown-http-client"),
        "expected default client tag; got {tags:?}"
    );
    assert!(
        tags.contains(&"creator:source=http"),
        "expected http source tag; got {tags:?}"
    );
    assert!(
        tags.iter().any(|t| t.starts_with("creator:version=")),
        "expected creator:version tag; got {tags:?}"
    );
}

/// Why (submission-logging Part B): when an HTTP client *does* set
/// `X-Trusty-Client-Name`, the drawer must carry that exact name in
/// its `creator:client=` tag so operators can trace which client wrote
/// which drawer.
/// What: POST with `X-Trusty-Client-Name: qa-curl` and assert the
/// rendered tag matches.
/// Test: itself.
#[tokio::test]
async fn drawer_creator_attribution_http_header() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().to_path_buf();
    let state = AppState::new(root);
    let palace = trusty_common::memory_core::Palace {
        id: PalaceId::new("cred-header"),
        name: "cred-header".to_string(),
        description: None,
        created_at: chrono::Utc::now(),
        data_dir: state.data_root.join("cred-header"),
    };
    state
        .registry
        .create_palace(&state.data_root, palace)
        .expect("create palace");

    let app = router().with_state(state.clone());
    let body = serde_json::json!({
        "content": "this is enough content to pass the signal/noise filter applied by remember",
        "tags": [],
    });
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/palaces/cred-header/drawers")
                .header("content-type", "application/json")
                .header("x-trusty-client-name", "qa-curl")
                .header("x-trusty-client-cwd", "/tmp/qa")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let app = router().with_state(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/palaces/cred-header/drawers?limit=10")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let bytes = to_bytes(resp.into_body(), 8192).await.unwrap();
    let v: Value = serde_json::from_slice(&bytes).unwrap();
    let tags: Vec<&str> = v[0]["tags"]
        .as_array()
        .expect("tags")
        .iter()
        .filter_map(|t| t.as_str())
        .collect();
    assert!(
        tags.contains(&"creator:client=qa-curl"),
        "expected custom client tag; got {tags:?}"
    );
    assert!(
        tags.contains(&"creator:cwd=/tmp/qa"),
        "expected cwd tag from header; got {tags:?}"
    );
}

/// Why (submission-logging Part B): drawers written through the MCP
/// tool surface (`memory_remember`) must carry
/// `creator:client=trusty-memory-mcp` and `creator:source=mcp` so
/// operators can tell MCP-origin drawers apart from HTTP / CLI writes.
/// What: dispatches `memory_remember` directly against an in-process
/// `AppState` (no HTTP), then lists the palace drawers and asserts
/// the MCP attribution tags landed.
/// Test: itself.
#[tokio::test]
async fn drawer_creator_attribution_mcp_default() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().to_path_buf();
    std::mem::forget(tmp);
    let state = AppState::new(root);
    // Flip to Ready so the issue #911 warming preflight allows dispatch_tool
    // to proceed (the call below goes through the MCP handler path).
    state.set_ready();
    let palace = trusty_common::memory_core::Palace {
        id: PalaceId::new("cred-mcp"),
        name: "cred-mcp".to_string(),
        description: None,
        created_at: chrono::Utc::now(),
        data_dir: state.data_root.join("cred-mcp"),
    };
    state
        .registry
        .create_palace(&state.data_root, palace)
        .expect("create palace");

    let _ = crate::tools::dispatch_tool(
        &state,
        "memory_remember",
        json!({
            "palace": "cred-mcp",
            "text": "remember a sentence with enough tokens to pass filters please",
            "room": "General",
            "tags": ["from-test"],
        }),
    )
    .await
    .expect("memory_remember dispatch");

    let app = router().with_state(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/palaces/cred-mcp/drawers?limit=10")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let bytes = to_bytes(resp.into_body(), 8192).await.unwrap();
    let v: Value = serde_json::from_slice(&bytes).unwrap();
    let drawers = v.as_array().expect("drawers array");
    assert!(!drawers.is_empty(), "expected at least one drawer");
    let tags: Vec<&str> = drawers[0]["tags"]
        .as_array()
        .expect("tags array")
        .iter()
        .filter_map(|t| t.as_str())
        .collect();
    assert!(
        tags.contains(&"creator:client=trusty-memory-mcp"),
        "expected MCP client tag; got {tags:?}"
    );
    assert!(
        tags.contains(&"creator:source=mcp"),
        "expected MCP source tag; got {tags:?}"
    );
}

/// Why (submission-logging Part A, failure isolation): if the daemon
/// is unreachable when the hook fires, the hook command MUST still
/// return `Ok(())` so the user's prompt is not blocked. The activity
/// emit failure is surfaced via a stderr warn-log only.
/// What: pins a tempdir as the data dir (so `read_daemon_addr`
/// returns `Ok(None)` — no http_addr file), runs `handle_prompt_context`,
/// and asserts it returns `Ok(())`. Separately verifies the emit
/// helper does not panic — covered by `post_hook_event_no_daemon_is_noop`
/// in `hook_emit::tests`.
/// Test: itself.
#[tokio::test]
async fn hook_emit_failure_isolated() {
    let _guard = crate::commands::env_test_lock().lock().await;
    let tmp = tempfile::tempdir().expect("tempdir");
    // SAFETY: test serialised via env_test_lock.
    unsafe {
        std::env::set_var(trusty_common::DATA_DIR_OVERRIDE_ENV, tmp.path());
    }
    let res = crate::commands::prompt_context::handle_prompt_context().await;
    unsafe {
        std::env::remove_var(trusty_common::DATA_DIR_OVERRIDE_ENV);
    }
    assert!(
        res.is_ok(),
        "hook must complete even when daemon emit fails; got {res:?}"
    );
}
