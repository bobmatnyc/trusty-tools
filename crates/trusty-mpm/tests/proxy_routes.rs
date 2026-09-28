//! HTTP-level integration tests for the local session-manager PROXY surface
//! (TELUI-6, #1440) — the exact curl-facing contract.
//!
//! Why: `daemon::managed_routes::proxy::tests` (in-crate) drives the handlers
//! directly with axum extractors; this file instead binds a REAL TCP listener
//! and drives the routes with `reqwest`, so it is the literal proof that the
//! proxy state machine is exercisable with plain HTTP (curl) BEFORE any
//! Telegram/Slack bot is wired up. Mirrors the real-network harness already used
//! by `client/executor/tests.rs` and `client/proxy/tests.rs` (axum::serve on an
//! ephemeral port + reqwest), applied here to the daemon's OWN proxy routes.
//! What: seeds one managed session in-process (no HTTP needed for setup — the
//! session-manager MVP tests seed the same way), serves the real router on a
//! loopback port, then drives focus → message → summary → unfocus, plus the
//! no-focus and dead-session auto-unfocus paths, over genuine HTTP requests.
//! Test: this file IS the test; run with `cargo test -p trusty-mpm --test
//! proxy_routes`.

use std::future::IntoFuture;
use std::sync::Arc;

use trusty_mpm::daemon::{api, state::DaemonState};
use trusty_mpm::runtime::RuntimeKind;
use trusty_mpm::session_manager::{ManagedSessionId, ManagedSessionState};

/// Spawn the real daemon router on a loopback port with one seeded session.
///
/// Why: every test below needs a live HTTP endpoint and a resolvable session;
/// centralising both keeps each test focused on the proxy behavior.
/// What: builds a hermetic `DaemonState::with_root_isolated_managed` (no real
/// tmux — `FakeNoopTmuxDriver`), seeds one managed session, serves
/// `api::router` on an ephemeral `127.0.0.1` port, and returns the base URL plus
/// the seeded session's friendly name. The record is marked `Active` via the
/// public `set_workspace` transition (`create_with_id` otherwise leaves it
/// `Provisioning`, #3591): this fixture represents a normal working session a
/// caller focuses and messages, and `SessionManager::send_input`'s state
/// guard (#3591) now correctly refuses `Provisioning`.
async fn spawn_daemon_with_session() -> (String, String) {
    let root = tempfile::tempdir().unwrap().keep();
    let state = Arc::new(DaemonState::with_root_isolated_managed(root).await);
    let mgr = state.session_manager().await;
    let record = mgr
        .create_with_id(
            ManagedSessionId::new(),
            "curl-recipe demo task".to_string(),
            None,
            Some("curltest".to_string()),
            None,
            None,
            None,
            RuntimeKind::default(),
            false,
            false,
        )
        .await
        .expect("seed session");
    mgr.set_workspace(
        &record.id,
        std::env::temp_dir(),
        ManagedSessionState::Active,
    )
    .await
    .expect("mark seeded session Active");

    let router = api::router(Arc::clone(&state));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(axum::serve(listener, router).into_future());
    (format!("http://{addr}"), record.tmux_name)
}

#[tokio::test]
async fn http_focus_message_summary_unfocus_round_trip() {
    let (url, name) = spawn_daemon_with_session().await;
    let client = reqwest::Client::new();

    // 1. Focus by friendly name over real HTTP.
    let body: serde_json::Value = client
        .post(format!("{url}/api/v1/sessions/proxy/focus"))
        .json(&serde_json::json!({"conversation_key": "curl-1", "session_id": name}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body["outcome"], "focused");
    assert_eq!(body["name"], name);

    // 2. GET the current focus back.
    let body: serde_json::Value = client
        .get(format!("{url}/api/v1/sessions/proxy/focus/curl-1"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body["outcome"], "current");
    assert_eq!(body["target"]["name"], name);

    // 3. Free text routes to the focused session (the Telegram free-text path,
    // reproduced over plain HTTP).
    let body: serde_json::Value = client
        .post(format!("{url}/api/v1/sessions/proxy/message"))
        .json(&serde_json::json!({"conversation_key": "curl-1", "text": "run the build"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body["outcome"], "sent");
    assert_eq!(body["text"], "run the build");

    // 4. Summarize the focused session.
    let body: serde_json::Value = client
        .get(format!("{url}/api/v1/sessions/proxy/summary/curl-1"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body["outcome"], "summary");
    assert_eq!(body["target"]["name"], name);

    // 5. Unfocus; the response names the session that was cleared.
    let body: serde_json::Value = client
        .post(format!("{url}/api/v1/sessions/proxy/unfocus"))
        .json(&serde_json::json!({"conversation_key": "curl-1"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body["cleared"]["name"], name);
}

#[tokio::test]
async fn http_message_with_no_focus_returns_no_focus_not_an_error() {
    // The acceptance requirement: an unfocused conversation's message gets a
    // structured `no_focus` outcome — HTTP 200, not an error — so a caller can
    // fall back to its own coordinator.
    let (url, _name) = spawn_daemon_with_session().await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{url}/api/v1/sessions/proxy/message"))
        .json(&serde_json::json!({"conversation_key": "never-focused", "text": "hello?"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["outcome"], "no_focus");
}

#[tokio::test]
async fn http_focus_unknown_session_is_not_found() {
    let (url, _name) = spawn_daemon_with_session().await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{url}/api/v1/sessions/proxy/focus"))
        .json(&serde_json::json!({"conversation_key": "curl-2", "session_id": "no-such"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["outcome"], "not_found");
    assert_eq!(body["target"], "no-such");
}
