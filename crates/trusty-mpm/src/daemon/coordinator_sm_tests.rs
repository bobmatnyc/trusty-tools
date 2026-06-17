//! Session-Manager endpoint tests for `/api/v1/sessions/chat` (DOC-14 SM-7).
//!
//! Why: SM-7 rewires `POST /api/v1/sessions/chat` to route through the SM
//! agent when enabled + provisioned, adds `/api/v1/session-manager/*` aliases,
//! and preserves the DOC-13 TUI contract. These tests pin every one of those:
//! the SM turn returns reply + cost; degraded maps to 503; `enabled = false`
//! falls back to the legacy path with no regression; the alias route resolves to
//! the same handler; and the old request/response shapes still (de)serialize.
//! All deterministic — a mock resolver, a tempdir, no network.
//! What: drives the `coordinator_chat` handler directly and the router via
//! `oneshot` for the alias/contract assertions.
//! Test: this *is* the test module.

use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::{Json, body::Body, http::Request};
use serde_json::Value;
use tempfile::TempDir;
use tower::ServiceExt;

use super::{CoordinatorChatRequest, coordinator_chat, router};
use crate::core::sm::SessionManagerAgent;
use crate::core::sm::agent::mock::{MockChatProvider, MockResolver};
use crate::core::sm::config::SessionManagerConfig;
use crate::daemon::state::DaemonState;

/// An enabled SM config.
fn enabled_sm() -> SessionManagerConfig {
    SessionManagerConfig {
        enabled: true,
        ..SessionManagerConfig::default()
    }
}

/// Build a `DaemonState` whose SM agent is enabled and wired to `resolver`,
/// rooted at `tmp`. Returns both so the caller keeps the tempdir alive.
fn state_with_sm(resolver: Arc<MockResolver>, tmp: &TempDir) -> Arc<DaemonState> {
    let agent = Arc::new(SessionManagerAgent::for_test(
        enabled_sm(),
        resolver,
        tmp.path().to_path_buf(),
    ));
    Arc::new(DaemonState::with_session_manager_agent(agent))
}

/// Why: the headline SM-7 acceptance at the endpoint — with the SM enabled and a
/// (mock) provider, `coordinator_chat` drives the SM turn and returns the reply
/// PLUS the additive per-call `cost` and `conv_id`.
/// What: posts a plain message; asserts reply, cost, conv_id, and that the
/// routed/output fields stay `None`.
/// Test: this is the test.
#[tokio::test]
async fn sm_chat_returns_reply_and_cost() {
    let tmp = TempDir::new().unwrap();
    let provider = MockChatProvider::new("SM plan ready", 0.0123);
    let state = state_with_sm(Arc::new(MockResolver::with_provider(provider)), &tmp);

    let resp = coordinator_chat(
        State(state),
        Json(CoordinatorChatRequest {
            message: "plan the migration".into(),
            history: Vec::new(),
            conv_id: Some("endpoint-conv".into()),
        }),
    )
    .await
    .expect("SM chat succeeds with a provider");

    assert_eq!(resp.reply, "SM plan ready");
    assert_eq!(resp.cost, Some(0.0123));
    assert_eq!(resp.conv_id.as_deref(), Some("endpoint-conv"));
    assert!(resp.routed_to_session.is_none());
    assert!(resp.command_output.is_none());
}

/// Why: degraded mode (§5.3) — with the SM enabled but NO provider (the resolver
/// reports degraded), the endpoint returns the documented 503 fallback rather
/// than a hard error.
/// What: builds an enabled SM over a degraded resolver; asserts 503.
/// Test: this is the test.
#[tokio::test]
async fn sm_chat_degraded_is_503() {
    let tmp = TempDir::new().unwrap();
    let state = state_with_sm(Arc::new(MockResolver::degraded()), &tmp);

    let err = coordinator_chat(
        State(state),
        Json(CoordinatorChatRequest {
            message: "anything".into(),
            history: Vec::new(),
            conv_id: None,
        }),
    )
    .await
    .unwrap_err();
    assert_eq!(err.status(), StatusCode::SERVICE_UNAVAILABLE);
}

/// Why: NO REGRESSION — with `enabled = false` (the default), the endpoint must
/// behave exactly like today's `LlmOverseer` path. A default daemon has no key,
/// so a non-prefixed message returns 503 (the legacy semantics), NOT the SM path.
/// What: a default `DaemonState` (SM disabled) → non-prefixed chat → 503.
/// Test: this is the test.
#[tokio::test]
async fn disabled_sm_falls_back_to_legacy_503() {
    // Build state with an EXPLICITLY-disabled SM agent (hermetic: never depends
    // on the operator's real config). Even though its resolver could serve, the
    // endpoint must NOT route through it because `enabled = false`.
    let tmp = TempDir::new().unwrap();
    let disabled_agent = Arc::new(SessionManagerAgent::for_test(
        SessionManagerConfig::default(), // enabled = false
        Arc::new(MockResolver::with_provider(MockChatProvider::new(
            "unused", 0.0,
        ))),
        tmp.path().to_path_buf(),
    ));
    let state = Arc::new(DaemonState::with_session_manager_agent(disabled_agent));
    // Sanity: the agent is disabled, so the SM path is never taken.
    assert!(!state.session_manager_agent().is_enabled());

    let err = coordinator_chat(
        State(state),
        Json(CoordinatorChatRequest {
            message: "what is happening?".into(),
            history: Vec::new(),
            conv_id: None,
        }),
    )
    .await
    .unwrap_err();
    assert_eq!(err.status(), StatusCode::SERVICE_UNAVAILABLE);
}

/// Why: D0.1 — the `/api/v1/session-manager/chat` alias must resolve to the SAME
/// handler as `/api/v1/sessions/chat`, so identical input yields identical
/// output. Driving both through the router via `oneshot` proves the alias wiring.
/// What: posts the same SM request to both paths; asserts both 200 with equal
/// `reply`/`cost`/`conv_id`.
/// Test: this is the test.
#[tokio::test]
async fn session_manager_chat_alias_matches_coordinator() {
    let tmp = TempDir::new().unwrap();
    let provider = MockChatProvider::new("aliased reply", 0.005);
    // Keep a handle so we can assert the shared provider was invoked once per
    // router call (a refactor that dedups the two calls would silently regress).
    let provider_handle = provider.clone();
    let state = state_with_sm(Arc::new(MockResolver::with_provider(provider)), &tmp);

    let body = serde_json::json!({ "message": "hi", "conv_id": "alias-conv" });

    let post = |path: &str| {
        let app = router(Arc::clone(&state));
        let body = body.clone();
        let path = path.to_string();
        async move {
            let resp = app
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri(path)
                        .header("content-type", "application/json")
                        .body(Body::from(body.to_string()))
                        .unwrap(),
                )
                .await
                .unwrap();
            let status = resp.status();
            let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
                .await
                .unwrap();
            let json: Value = serde_json::from_slice(&bytes).unwrap();
            (status, json)
        }
    };

    let (coord_status, coord_json) = post("/api/v1/sessions/chat").await;
    let (sm_status, sm_json) = post("/api/v1/session-manager/chat").await;

    assert_eq!(coord_status, StatusCode::OK);
    assert_eq!(sm_status, StatusCode::OK);
    assert_eq!(coord_json["reply"], sm_json["reply"]);
    assert_eq!(coord_json["reply"], "aliased reply");
    assert_eq!(coord_json["cost"], sm_json["cost"]);
    assert_eq!(coord_json["conv_id"], sm_json["conv_id"]);

    // Both router calls must have actually driven the shared provider exactly once
    // each (one orchestration reply per turn; no compaction triggers on a single
    // round at the default window). A refactor that dedups the calls fails here.
    assert_eq!(
        provider_handle.request_count(),
        2,
        "the shared provider must be invoked once per router call"
    );
}

/// Why: DOC-13 TUI contract — a request in the OLD shape (just `message` +
/// `history`, NO `conv_id`) must still deserialize, and the response must still
/// carry the original `reply` field. The additive `cost`/`conv_id` are present
/// only on the SM path and are skipped otherwise.
/// What: deserializes the legacy request JSON; serializes a legacy-style response
/// and asserts the original field is present and the additive ones are absent.
/// Test: this is the test.
#[test]
fn doc13_tui_contract_is_preserved() {
    // Old request shape (pre-SM-7) still deserializes — `conv_id` defaults absent.
    let legacy: CoordinatorChatRequest =
        serde_json::from_str(r#"{ "message": "hello", "history": [] }"#)
            .expect("legacy request shape must still deserialize");
    assert_eq!(legacy.message, "hello");
    assert!(legacy.conv_id.is_none());

    // A legacy-style response (no SM additive fields) serializes with the
    // original `reply` field and WITHOUT `cost`/`conv_id` (they skip when None).
    let resp = super::CoordinatorChatResponse {
        reply: "ok".into(),
        routed_to_session: None,
        command_output: None,
        cost: None,
        conv_id: None,
    };
    let json: Value = serde_json::to_value(&resp).unwrap();
    assert_eq!(json["reply"], "ok");
    assert!(json.get("cost").is_none(), "cost must be absent when None");
    assert!(
        json.get("conv_id").is_none(),
        "conv_id must be absent when None"
    );
    assert!(json.get("routed_to_session").is_none());
}

/// Why: #1392 — the cross-session coordinator surface moved under the plural
/// `/api/v1/sessions/*` namespace and the former `/api/v1/coordinator/*` paths
/// were removed. This pins both halves of that contract at the router level so a
/// future re-add of the old mount (or a typo in the new one) fails loudly.
/// What: drives the `router` via `oneshot` and asserts `GET /api/v1/sessions/context`
/// answers `200` while the retired `GET /api/v1/coordinator/context` answers `404`.
/// Test: this is the test.
#[tokio::test]
async fn sessions_context_replaces_coordinator_namespace() {
    let get = |path: &str| {
        let app = router(DaemonState::shared());
        let path = path.to_string();
        async move {
            app.oneshot(
                Request::builder()
                    .method("GET")
                    .uri(path)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap()
            .status()
        }
    };

    // New plural namespace responds.
    assert_eq!(get("/api/v1/sessions/context").await, StatusCode::OK);
    // Retired coordinator namespace is gone.
    assert_eq!(
        get("/api/v1/coordinator/context").await,
        StatusCode::NOT_FOUND,
    );
}
