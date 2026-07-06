//! `session.*` JSON-RPC method handlers (#2054, vision spec Axiom 4 / §4.3).
//!
//! Why: this is the API surface Axiom 4 requires — every session operation
//! goes through JSON-RPC, reachable identically over STDIO and HTTP, so the
//! CLI (and later TUI/TELGUI/REST) never touch `SessionRegistry` directly.
//! What: [`register`] wires `session.create`, `session.list`,
//! `session.status`, `session.send`, `session.attach`, `session.detach`, and
//! `session.cancel` onto a [`Router`], all closed over the SAME
//! `Arc<SessionRegistry>` so every method sees a consistent view. Each
//! handler parses its typed `params`, forwards to the matching
//! `SessionRegistry` method, and maps the result onto the JSON-RPC result
//! shape the vision spec's §4.3 examples describe.
//! Test: `protocol::tests::*` (parameter validation, error mapping); the
//! full attach/detach streaming behaviour is covered by
//! `session::registry_tests` (registry-level) and
//! `tests/session_e2e.rs` (API-driven, real daemon).

use std::sync::Arc;

use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::jsonrpc::{ConnectionContext, Router, RpcError};

use super::registry::SessionRegistry;

/// Register every `session.*` method onto `router`, all sharing `registry`.
///
/// Why: the one place that lists the full `session.*` surface — mirrors
/// `crate::serve::methods::register`'s role for the proof-of-life methods.
/// What: clones `registry` once per method (cheap — `Arc`) into a small
/// adapter closure that forwards to the corresponding free function below.
/// Test: `protocol::tests::register_wires_every_session_method`.
pub fn register(router: &mut Router, registry: Arc<SessionRegistry>) {
    let r = registry.clone();
    router.register(
        "session.create",
        move |params: Value, ctx: ConnectionContext| {
            let r = r.clone();
            async move { create(&r, params, ctx).await }
        },
    );

    let r = registry.clone();
    router.register(
        "session.list",
        move |params: Value, ctx: ConnectionContext| {
            let r = r.clone();
            async move { list(&r, params, ctx).await }
        },
    );

    let r = registry.clone();
    router.register(
        "session.status",
        move |params: Value, ctx: ConnectionContext| {
            let r = r.clone();
            async move { status(&r, params, ctx).await }
        },
    );

    let r = registry.clone();
    router.register(
        "session.send",
        move |params: Value, ctx: ConnectionContext| {
            let r = r.clone();
            async move { send(&r, params, ctx).await }
        },
    );

    let r = registry.clone();
    router.register(
        "session.attach",
        move |params: Value, ctx: ConnectionContext| {
            let r = r.clone();
            async move { attach(&r, params, ctx).await }
        },
    );

    let r = registry.clone();
    router.register(
        "session.detach",
        move |params: Value, ctx: ConnectionContext| {
            let r = r.clone();
            async move { detach(&r, params, ctx).await }
        },
    );

    let r = registry.clone();
    router.register(
        "session.cancel",
        move |params: Value, ctx: ConnectionContext| {
            let r = r.clone();
            async move { cancel(&r, params, ctx).await }
        },
    );
}

/// `params` shape shared by every method that only needs a session id
/// (`status`, `attach`, `detach`, `cancel`).
#[derive(Deserialize)]
struct SessionIdParams {
    session_id: String,
}

/// `params` shape for `session.create`.
#[derive(Deserialize)]
struct CreateParams {
    task: String,
    #[serde(default)]
    agent: Option<String>,
    #[serde(default)]
    project: Option<String>,
}

/// `params` shape for `session.send`.
#[derive(Deserialize)]
struct SendParams {
    session_id: String,
    input: String,
}

/// Deserialise `params` into `T`, mapping a failure onto
/// `-32602 Invalid params` with the method name for context.
fn parse<T: DeserializeOwned>(params: Value, method: &str) -> Result<T, RpcError> {
    serde_json::from_value(params).map_err(|e| RpcError::invalid_params(format!("{method}: {e}")))
}

/// `session.create(task, agent?, project?) -> Session` (vision spec Axiom 4).
///
/// Why: mints a brand-new daemon-owned session.
/// What: validates `task` is non-empty (`-32003 invalid_argument`
/// otherwise), then delegates to `SessionRegistry::create`. The returned
/// `Session` already has `status: "running"` — see `session::model`'s docs
/// on why M1 has no queued/created-but-not-running state.
/// Test: `protocol::tests::create_rejects_empty_task`,
/// `protocol::tests::create_returns_running_session`.
async fn create(
    registry: &SessionRegistry,
    params: Value,
    _ctx: ConnectionContext,
) -> Result<Value, RpcError> {
    let p: CreateParams = parse(params, "session.create")?;
    if p.task.trim().is_empty() {
        return Err(RpcError::invalid_argument("task must not be empty"));
    }
    let session = registry.create(p.task, p.agent, p.project);
    Ok(json!(session))
}

/// `session.list() -> [Session]` (vision spec Axiom 4).
///
/// Why: enumerates every session currently owned by the daemon.
/// What: wraps `SessionRegistry::list` as `{"sessions": [...]}`.
/// Test: `protocol::tests::list_returns_sessions_key`.
async fn list(
    registry: &SessionRegistry,
    _params: Value,
    _ctx: ConnectionContext,
) -> Result<Value, RpcError> {
    Ok(json!({ "sessions": registry.list() }))
}

/// `session.status(session_id) -> Session` (vision spec Axiom 4).
///
/// Why: point lookup for one session's current state.
/// What: `-32007 session_not_found` if unknown; otherwise the `Session`.
/// Test: `protocol::tests::status_unknown_session_maps_to_session_not_found`.
async fn status(
    registry: &SessionRegistry,
    params: Value,
    _ctx: ConnectionContext,
) -> Result<Value, RpcError> {
    let p: SessionIdParams = parse(params, "session.status")?;
    Ok(json!(registry.status(&p.session_id)?))
}

/// `session.send(session_id, input) -> { acknowledged }` (vision spec
/// Axiom 4).
///
/// Why: the client -> daemon input path.
/// What: `-32007 session_not_found` if unknown; otherwise
/// `{"acknowledged": true}` after `SessionRegistry::send` publishes the
/// observable `Event::SessionInput`.
/// Test: `protocol::tests::send_unknown_session_maps_to_session_not_found`.
async fn send(
    registry: &SessionRegistry,
    params: Value,
    _ctx: ConnectionContext,
) -> Result<Value, RpcError> {
    let p: SendParams = parse(params, "session.send")?;
    registry.send(&p.session_id, &p.input)?;
    Ok(json!({ "acknowledged": true }))
}

/// `session.attach(session_id) -> { session_id, events, stream_url }`
/// (vision spec Axiom 4 / §4.4 session-attach protocol).
///
/// Why: the streaming half of the protocol. `events` is the ring-buffer
/// replay (§12, 11.2) so a freshly-attached client sees recent history
/// immediately; live events follow as server-initiated notifications.
/// What: over STDIO, `ctx.notify` is the long-lived per-process channel —
/// live events are pushed on the SAME connection as
/// `{"jsonrpc":"2.0","method":"session.event","params":{...}}` lines,
/// interleaved with ordinary responses. Over HTTP, `ctx.notify` is a
/// throwaway per-request channel (the forwarder self-terminates once the
/// response is written); the real HTTP live-streaming path is the
/// dedicated `GET /sessions/{id}/events` SSE route
/// (`crate::serve::http::session_events_sse`), which is why `stream_url` is
/// always included — HTTP clients need it, STDIO clients simply ignore it.
/// `-32007 session_not_found` if `session_id` is unknown.
/// Test: `protocol::tests::attach_unknown_session_maps_to_session_not_found`;
/// the streaming behaviour itself is covered by
/// `session::registry_tests::attach_forwards_live_events_until_detach` and
/// the API-driven `tests/session_e2e.rs`.
async fn attach(
    registry: &SessionRegistry,
    params: Value,
    ctx: ConnectionContext,
) -> Result<Value, RpcError> {
    let p: SessionIdParams = parse(params, "session.attach")?;
    let events = registry.attach(&p.session_id, ctx.connection_id, ctx.notify.clone())?;
    Ok(json!({
        "session_id": p.session_id,
        "events": events,
        "stream_url": format!("/sessions/{}/events", p.session_id),
    }))
}

/// `session.detach(session_id) -> {}` (vision spec Axiom 4).
///
/// Why: stops this connection's live-event forwarding for the session.
/// What: idempotent — detaching without a prior attach is a success no-op
/// (see `SessionRegistry::detach`). `-32007 session_not_found` if
/// `session_id` itself is unknown.
/// Test: `protocol::tests::detach_unknown_session_maps_to_session_not_found`.
async fn detach(
    registry: &SessionRegistry,
    params: Value,
    ctx: ConnectionContext,
) -> Result<Value, RpcError> {
    let p: SessionIdParams = parse(params, "session.detach")?;
    registry.detach(&p.session_id, ctx.connection_id)?;
    Ok(json!({}))
}

/// `session.cancel(session_id) -> Session` (vision spec §12, 11.6
/// Cancellation Semantics).
///
/// Why: explicit termination signal.
/// What: idempotent on an already-terminal session (see
/// `SessionRegistry::cancel`). `-32007 session_not_found` if `session_id`
/// is unknown; otherwise the post-cancellation `Session` snapshot
/// (`status: "cancelled"`).
/// Test: `protocol::tests::cancel_unknown_session_maps_to_session_not_found`.
async fn cancel(
    registry: &SessionRegistry,
    params: Value,
    _ctx: ConnectionContext,
) -> Result<Value, RpcError> {
    let p: SessionIdParams = parse(params, "session.cancel")?;
    Ok(json!(registry.cancel(&p.session_id)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc;
    use trusty_common::mcp::Request;

    fn test_ctx() -> ConnectionContext {
        let (tx, _rx) = mpsc::unbounded_channel();
        ConnectionContext::new(tx)
    }

    /// Every `session.*` method must be reachable through a `Router` built
    /// by `register` (proves the wiring, not just the free functions).
    #[tokio::test]
    async fn register_wires_every_session_method() {
        let registry = Arc::new(SessionRegistry::new());
        let mut router = Router::new();
        register(&mut router, registry.clone());

        let session = registry.create("t".to_string(), None, None);

        let cases: &[(&str, Value)] = &[
            ("session.list", json!({})),
            ("session.status", json!({"session_id": session.id})),
            (
                "session.send",
                json!({"session_id": session.id, "input": "hi"}),
            ),
            ("session.attach", json!({"session_id": session.id})),
            ("session.detach", json!({"session_id": session.id})),
        ];
        for (method, params) in cases {
            let req = Request {
                jsonrpc: Some("2.0".to_string()),
                id: Some(json!(1)),
                method: method.to_string(),
                params: Some(params.clone()),
            };
            let resp = router.dispatch(req, &test_ctx()).await;
            assert!(
                resp.error.is_none(),
                "{method} should succeed, got {:?}",
                resp.error
            );
        }

        // `session.cancel` last since it terminates the session.
        let req = Request {
            jsonrpc: Some("2.0".to_string()),
            id: Some(json!(1)),
            method: "session.cancel".to_string(),
            params: Some(json!({"session_id": session.id})),
        };
        let resp = router.dispatch(req, &test_ctx()).await;
        assert!(
            resp.error.is_none(),
            "session.cancel should succeed, got {:?}",
            resp.error
        );
    }

    /// An empty `task` must map to `-32003 invalid_argument`, not silently
    /// create a blank session.
    #[tokio::test]
    async fn create_rejects_empty_task() {
        let registry = SessionRegistry::new();
        let err = create(&registry, json!({"task": "   "}), test_ctx())
            .await
            .unwrap_err();
        assert_eq!(err.code, -32003);
    }

    /// A well-formed `session.create` call must return a running session.
    #[tokio::test]
    async fn create_returns_running_session() {
        let registry = SessionRegistry::new();
        let result = create(&registry, json!({"task": "do it"}), test_ctx())
            .await
            .unwrap();
        assert_eq!(result["status"], "running");
        assert_eq!(result["task"], "do it");
    }

    /// `session.list` must wrap its result under a `"sessions"` key.
    #[tokio::test]
    async fn list_returns_sessions_key() {
        let registry = SessionRegistry::new();
        registry.create("a".to_string(), None, None);
        let result = list(&registry, Value::Null, test_ctx()).await.unwrap();
        assert_eq!(result["sessions"].as_array().unwrap().len(), 1);
    }

    /// `session.status` on an unknown id must map to `session_not_found`.
    #[tokio::test]
    async fn status_unknown_session_maps_to_session_not_found() {
        let registry = SessionRegistry::new();
        let err = status(&registry, json!({"session_id": "nope"}), test_ctx())
            .await
            .unwrap_err();
        assert_eq!(err.code, -32007);
    }

    /// `session.send` on an unknown id must map to `session_not_found`.
    #[tokio::test]
    async fn send_unknown_session_maps_to_session_not_found() {
        let registry = SessionRegistry::new();
        let err = send(
            &registry,
            json!({"session_id": "nope", "input": "hi"}),
            test_ctx(),
        )
        .await
        .unwrap_err();
        assert_eq!(err.code, -32007);
    }

    /// `session.attach` on an unknown id must map to `session_not_found`.
    #[tokio::test]
    async fn attach_unknown_session_maps_to_session_not_found() {
        let registry = SessionRegistry::new();
        let err = attach(&registry, json!({"session_id": "nope"}), test_ctx())
            .await
            .unwrap_err();
        assert_eq!(err.code, -32007);
    }

    /// `session.detach` on an unknown id must map to `session_not_found`.
    #[tokio::test]
    async fn detach_unknown_session_maps_to_session_not_found() {
        let registry = SessionRegistry::new();
        let err = detach(&registry, json!({"session_id": "nope"}), test_ctx())
            .await
            .unwrap_err();
        assert_eq!(err.code, -32007);
    }

    /// `session.cancel` on an unknown id must map to `session_not_found`.
    #[tokio::test]
    async fn cancel_unknown_session_maps_to_session_not_found() {
        let registry = SessionRegistry::new();
        let err = cancel(&registry, json!({"session_id": "nope"}), test_ctx())
            .await
            .unwrap_err();
        assert_eq!(err.code, -32007);
    }
}
