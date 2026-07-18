//! `session.*` JSON-RPC method handlers (#2054, vision spec Axiom 4 / §4.3).
//!
//! Why: this is the API surface Axiom 4 requires — every session operation
//! goes through JSON-RPC, reachable identically over STDIO and HTTP, so the
//! CLI (and later TUI/TELGUI/REST) never touch `SessionRegistry` directly.
//! What: [`register`] wires `session.create`, `session.list`,
//! `session.status`, `session.send`, `session.attach`, `session.detach`,
//! `session.cancel`, (#2058) `session.get_transcript`, (#2350)
//! `session.set_goal`/`session.clear_goal`/`session.get_goals`,
//! (DOC-39 §5.6 Slice D) `session.get_readiness`, (DOC-39 §5.4)
//! `session.get_agents`, (issue #3015) `session.get_context_budget`, and
//! (issue #3072) `session.get_search_audit` onto a [`Router`], all
//! closed over the SAME `Arc<SessionRegistry>` so every method sees a
//! consistent view. Each handler parses its typed `params`, forwards to the
//! matching `SessionRegistry` method, and maps the result onto the JSON-RPC
//! result shape the vision spec's §4.3 examples describe.
//! The three goal methods' handler bodies live in the sibling
//! `protocol_goals` module, `session.get_readiness`'s in the sibling
//! `protocol_readiness` module, `session.get_agents`'s in the sibling
//! `protocol_agents` module, and `session.get_context_budget`'s in the
//! sibling `protocol_budget` module (all kept out of this file purely for
//! the 500-SLOC cap — see each module's docs), but are registered here
//! alongside every other `session.*` method so this remains the one place
//! listing the full surface.
//! Test: `protocol::tests::*` (parameter validation, error mapping);
//! `protocol_goals::tests::*` (the three goal methods);
//! `protocol_readiness::tests::*` (`session.get_readiness`);
//! `protocol_agents::tests::*` (`session.get_agents`);
//! `protocol_budget::tests::*` (`session.get_context_budget`); the full
//! attach/detach streaming behaviour is covered by `session::registry_tests`
//! (registry-level) and `tests/session_e2e.rs`/`tests/task_e2e.rs`
//! (API-driven, real daemon).

use std::path::PathBuf;
use std::sync::Arc;

use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::binding::ProjectBinding;
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

    let r = registry.clone();
    router.register(
        "session.get_transcript",
        move |params: Value, ctx: ConnectionContext| {
            let r = r.clone();
            async move { get_transcript(&r, params, ctx).await }
        },
    );

    let r = registry.clone();
    router.register(
        "session.set_goal",
        move |params: Value, ctx: ConnectionContext| {
            let r = r.clone();
            async move { protocol_goals::set_goal(&r, params, ctx).await }
        },
    );

    let r = registry.clone();
    router.register(
        "session.clear_goal",
        move |params: Value, ctx: ConnectionContext| {
            let r = r.clone();
            async move { protocol_goals::clear_goal(&r, params, ctx).await }
        },
    );

    let r = registry.clone();
    router.register(
        "session.get_goals",
        move |params: Value, ctx: ConnectionContext| {
            let r = r.clone();
            async move { protocol_goals::get_goals(&r, params, ctx).await }
        },
    );

    let r = registry.clone();
    router.register(
        "session.get_readiness",
        move |params: Value, ctx: ConnectionContext| {
            let r = r.clone();
            async move { protocol_readiness::get_readiness(&r, params, ctx).await }
        },
    );

    let r = registry.clone();
    router.register(
        "session.get_agents",
        move |params: Value, ctx: ConnectionContext| {
            let r = r.clone();
            async move { protocol_agents::get_agents(&r, params, ctx).await }
        },
    );

    let r = registry.clone();
    router.register(
        "session.get_context_budget",
        move |params: Value, ctx: ConnectionContext| {
            let r = r.clone();
            async move { protocol_budget::get_context_budget(&r, params, ctx).await }
        },
    );

    let r = registry.clone();
    router.register(
        "session.get_search_audit",
        move |params: Value, ctx: ConnectionContext| {
            let r = r.clone();
            async move { protocol_search_audit::get_search_audit(&r, params, ctx).await }
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
///
/// `project` is now a project PATH, not the free-form label it used to be — see
/// [`create`]'s docs for the reconciliation and its compatibility implications.
#[derive(Deserialize)]
struct CreateParams {
    task: String,
    #[serde(default)]
    agent: Option<String>,
    #[serde(default)]
    project: Option<PathBuf>,
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
/// otherwise), resolves `project` into a typed [`ProjectBinding`], then
/// delegates to `SessionRegistry::create`. The returned `Session` already has
/// `status: "running"` — see `session::model`'s docs on why M1 has no
/// queued/created-but-not-running state.
///
/// **The `project` reconciliation (spec DOC-39 §5.5, AC-16.2).** This param was
/// an untyped, free-form `Option<String>` LABEL: never validated, never bound,
/// not enumerable, and disconnected from the `PathBuf` `task.run` demanded. One
/// concept lived on two surfaces that could not agree. It is now the same
/// `ProjectBinding` `task.run` uses, resolved from a project PATH.
///
/// This is a deliberate BREAKING change to this method's contract: a caller that
/// previously passed a decorative label (`"my-app"`) now receives
/// `-32003 invalid_argument` unless that string names a real directory. Erroring
/// is the point — silently accepting a label that binds nothing and indexes
/// nothing is exactly the failure this reconciliation exists to end, and a
/// caller who learns their "project" was never real is strictly better off than
/// one who never finds out. Omitting `project` entirely remains valid and means
/// PROJECTLESS — a supported state, never an error (AC-2.1).
/// Test: `protocol::tests::create_rejects_empty_task`,
/// `protocol::tests::create_returns_running_session`,
/// `protocol::tests::create_without_project_is_projectless`,
/// `protocol::tests::create_binds_a_real_directory`,
/// `protocol::tests::create_rejects_a_label_that_is_not_a_directory`.
async fn create(
    registry: &SessionRegistry,
    params: Value,
    _ctx: ConnectionContext,
) -> Result<Value, RpcError> {
    let p: CreateParams = parse(params, "session.create")?;
    if p.task.trim().is_empty() {
        return Err(RpcError::invalid_argument("task must not be empty"));
    }
    let binding = ProjectBinding::resolve(p.project)
        .map_err(|e| RpcError::invalid_argument(format!("session.create: {e}")))?;
    let session = registry.create(p.task, p.agent, binding);
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
/// Why: explicit termination signal. #2056 splits this into two paths: a
/// session with a background `task.run` execution in flight must be
/// signalled COOPERATIVELY (the executing `AgentLoop`(s) observe the flag at
/// their next turn boundary and unwind themselves — see
/// `agent_loop::AgentLoopError::Cancelled` — before `crate::task::executor`
/// lands the terminal transition via `SessionRegistry::finish`); a session
/// with nothing executing keeps the original #2054 immediate-transition
/// behaviour.
/// What: idempotent either way. `-32007 session_not_found` if `session_id`
/// is unknown. When `SessionRegistry::is_executing` is true, calls
/// `request_cancel` (sets the flag) and returns the CURRENT snapshot — still
/// `status: "running"` until the executor actually observes cancellation and
/// transitions it. Otherwise falls back to `SessionRegistry::cancel` (the
/// #2054 immediate `status: "cancelled"` path).
/// Test: `protocol::tests::cancel_unknown_session_maps_to_session_not_found`,
/// `protocol::tests::cancel_executing_session_requests_cooperative_cancel`.
async fn cancel(
    registry: &SessionRegistry,
    params: Value,
    _ctx: ConnectionContext,
) -> Result<Value, RpcError> {
    let p: SessionIdParams = parse(params, "session.cancel")?;
    if registry.is_executing(&p.session_id) {
        registry.request_cancel(&p.session_id)?;
        Ok(json!(registry.status(&p.session_id)?))
    } else {
        Ok(json!(registry.cancel(&p.session_id)?))
    }
}

/// `session.get_transcript(session_id) -> TranscriptRecord` (#2058, vision
/// spec §11.1 Transcript Persistence / §4.3 API Surface).
///
/// Why: completes the M1 cut-line's "inspect transcript" verb — read-only
/// access to the run record `task.run` (#2056) persists via
/// `SessionRegistry::set_run_outcome`.
/// What: `-32007 session_not_found` if `session_id` is unknown; otherwise
/// `SessionRegistry::get_transcript`'s `TranscriptRecord` verbatim — turns,
/// aggregate usage, and stored cost, never recomputed here. A session that
/// has never run a task returns a `TranscriptRecord` with an empty `turns`
/// array rather than an error (see `SessionRegistry::get_transcript`'s docs).
/// Test: `protocol::tests::get_transcript_unknown_session_maps_to_session_not_found`,
/// `protocol::tests::get_transcript_on_never_run_session_is_empty`.
async fn get_transcript(
    registry: &SessionRegistry,
    params: Value,
    _ctx: ConnectionContext,
) -> Result<Value, RpcError> {
    let p: SessionIdParams = parse(params, "session.get_transcript")?;
    Ok(json!(registry.get_transcript(&p.session_id)?))
}

/// `session.set_goal`/`session.clear_goal`/`session.get_goals` (#2350),
/// split into their own file purely to keep this production file under the
/// crate's 500-SLOC cap — a child module of `protocol` (declared via
/// `#[path = ...] mod protocol_goals;`), so it shares full access to this
/// module's private `SessionIdParams`/`parse` helpers exactly as if these
/// handlers were still defined here.
#[path = "protocol_goals.rs"]
mod protocol_goals;

/// `session.get_readiness` (DOC-39 §5.6 Slice D), split into its own file for
/// the same 500-SLOC-cap reason as `protocol_goals` above.
#[path = "protocol_readiness.rs"]
mod protocol_readiness;

/// `session.get_agents` (DOC-39 §5.4), split into its own file for the same
/// 500-SLOC-cap reason as `protocol_goals` above.
#[path = "protocol_agents.rs"]
mod protocol_agents;

/// `session.get_context_budget` (issue #3015), split into its own file for
/// the same 500-SLOC-cap reason as `protocol_goals` above.
#[path = "protocol_budget.rs"]
mod protocol_budget;

/// `session.get_search_audit` (issue #3072), split into its own file for the
/// same 500-SLOC-cap reason as `protocol_goals` above.
#[path = "protocol_search_audit.rs"]
mod protocol_search_audit;

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

        let session = registry.create("t".to_string(), None, crate::binding::ProjectBinding::None);

        let cases: &[(&str, Value)] = &[
            ("session.list", json!({})),
            ("session.status", json!({"session_id": session.id})),
            (
                "session.send",
                json!({"session_id": session.id, "input": "hi"}),
            ),
            ("session.attach", json!({"session_id": session.id})),
            ("session.detach", json!({"session_id": session.id})),
            ("session.get_transcript", json!({"session_id": session.id})),
            ("session.get_goals", json!({"session_id": session.id})),
            ("session.get_readiness", json!({"session_id": session.id})),
            ("session.get_agents", json!({"session_id": session.id})),
            (
                "session.get_context_budget",
                json!({"session_id": session.id}),
            ),
            (
                "session.get_search_audit",
                json!({"session_id": session.id}),
            ),
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

    /// `session.set_goal`/`session.clear_goal` must be reachable through the
    /// `Router` (proving the wiring) even though a freshly-created session
    /// with no `task.run` yet has no transcript to write into — the
    /// documented #2350 "no transcript yet" error IS the proof the request
    /// was routed to the real handler rather than `-32601 method not found`.
    #[tokio::test]
    async fn register_wires_set_goal_and_clear_goal() {
        let registry = Arc::new(SessionRegistry::new());
        let mut router = Router::new();
        register(&mut router, registry.clone());
        let session = registry.create("t".to_string(), None, crate::binding::ProjectBinding::None);

        for method in ["session.set_goal", "session.clear_goal"] {
            let req = Request {
                jsonrpc: Some("2.0".to_string()),
                id: Some(json!(1)),
                method: method.to_string(),
                params: Some(json!({"session_id": session.id, "slot": 1, "text": "x"})),
            };
            let resp = router.dispatch(req, &test_ctx()).await;
            let error = resp
                .error
                .unwrap_or_else(|| panic!("{method} must error (no transcript yet)"));
            assert_eq!(error.code, -32003, "{method} wrong error code");
        }
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
        registry.create("a".to_string(), None, crate::binding::ProjectBinding::None);
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

    /// `session.get_transcript` on an unknown id must map to
    /// `session_not_found` (#2058).
    #[tokio::test]
    async fn get_transcript_unknown_session_maps_to_session_not_found() {
        let registry = SessionRegistry::new();
        let err = get_transcript(&registry, json!({"session_id": "nope"}), test_ctx())
            .await
            .unwrap_err();
        assert_eq!(err.code, -32007);
    }

    /// `session.get_transcript` on a session that has never run a task
    /// returns an empty transcript, not an error (#2058).
    #[tokio::test]
    async fn get_transcript_on_never_run_session_is_empty() {
        let registry = SessionRegistry::new();
        let session = registry.create("t".to_string(), None, crate::binding::ProjectBinding::None);
        let result = get_transcript(&registry, json!({"session_id": session.id}), test_ctx())
            .await
            .unwrap();
        assert_eq!(result["session_id"], session.id);
        assert_eq!(result["turns"].as_array().unwrap().len(), 0);
        assert_eq!(result["cost_usd"], Value::Null);
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

    /// `session.cancel` on a session with an in-flight execution must request
    /// cooperative cancellation (set the flag) rather than immediately
    /// transitioning to `cancelled` — the executor lands that transition once
    /// it actually observes the flag.
    #[tokio::test]
    async fn cancel_executing_session_requests_cooperative_cancel() {
        let registry = SessionRegistry::new();
        let session = registry.create("t".to_string(), None, crate::binding::ProjectBinding::None);
        let flag = registry.begin_execution(&session.id).unwrap();

        let result = cancel(&registry, json!({"session_id": &session.id}), test_ctx())
            .await
            .unwrap();

        assert_eq!(
            result["status"], "running",
            "status must NOT be transitioned immediately for an executing session"
        );
        assert!(
            flag.load(std::sync::atomic::Ordering::Relaxed),
            "the shared cancel flag must have been set"
        );
    }
}

#[cfg(test)]
mod binding_tests {
    use super::*;
    use tokio::sync::mpsc;

    fn ctx() -> ConnectionContext {
        let (tx, _rx) = mpsc::unbounded_channel();
        ConnectionContext::new(tx)
    }

    /// Omitting `project` is VALID and means projectless — a supported state,
    /// never an error (AC-2.1). This is the entry state screen 7a renders.
    #[tokio::test]
    async fn create_without_project_is_projectless() {
        let registry = SessionRegistry::new();
        let value = create(&registry, json!({"task": "just chat"}), ctx())
            .await
            .expect("omitting project must be valid");

        assert_eq!(value["binding"]["state"], "projectless");
        assert!(value["binding"]["root"].is_null());
        assert!(
            value["project"].is_null(),
            "projectless must derive no label"
        );
    }

    /// A real directory binds, and the derived label matches the binding — the
    /// reconciliation in one assertion (AC-16.2).
    #[tokio::test]
    async fn create_binds_a_real_directory() {
        let registry = SessionRegistry::new();
        let dir = tempfile::tempdir().expect("tempdir");
        let value = create(
            &registry,
            json!({"task": "t", "project": dir.path().to_string_lossy()}),
            ctx(),
        )
        .await
        .expect("a real directory must bind");

        // A non-git tempdir binds as `directory` — NOT projectless (#2728).
        assert_eq!(value["binding"]["state"], "directory");
        assert_eq!(
            value["project"], value["binding"]["root"],
            "the label must be derived from the binding root, not independent of it"
        );
    }

    /// The old free-form label is now rejected: `"my-app"` names no directory,
    /// so it can bind nothing and index nothing. Erroring is the point — see
    /// `create`'s docs. This is the deliberate breaking change.
    #[tokio::test]
    async fn create_rejects_a_label_that_is_not_a_directory() {
        let registry = SessionRegistry::new();
        let err = create(&registry, json!({"task": "t", "project": "my-app"}), ctx())
            .await
            .expect_err("a decorative label must no longer be silently accepted");

        assert_eq!(err.code, -32003, "expected invalid_argument, got {err:?}");
    }
}
