//! `GET /sessions*` REST routes over the `session.*` JSON-RPC methods
//! (#2983 Slice 2 — read-only routes only, no writes).
//!
//! Why: Slice 1 (`super`) built the `rest::call`/`rpc_error_to_status`
//! bridge but wired no axum routes. This is the first concrete resource
//! group: session state is the thing every other `tcode` client (CLI, TUI,
//! future browser UI) polls most, so it lands first. Every handler below is
//! deliberately thin — parse the path param, build the JSON-RPC `params`,
//! call `super::respond` — because the actual behaviour (session lookup,
//! `-32007 session_not_found` mapping, …) already lives in
//! `crate::session::protocol`'s handlers; duplicating it here as bespoke
//! axum logic would fork the two surfaces (see `super` module docs).
//! What: [`routes`] builds a standalone `axum::Router<()>` (its own
//! `SessionsState` carrying just the `Arc<Router>`, `with_state`-erased
//! before return) mapping:
//!   - `GET /sessions` -> `session.list`
//!   - `GET /sessions/{id}` -> `session.status`
//!   - `GET /sessions/{id}/transcript` -> `session.get_transcript`
//!   - `GET /sessions/{id}/readiness` -> `session.get_readiness`
//!   - `GET /sessions/{id}/goals` -> `session.get_goals`
//!   - `GET /sessions/{id}/budget` -> `session.get_context_budget` (issue
//!     #3015 — the API PR #3014's GUI status bar polls instead of leaving
//!     "budget: unavailable" permanently)
//!
//! `crate::serve::http::build_axum_router` merges this into the daemon's
//! main router alongside `POST /rpc`, `GET /health`, and
//! `GET /sessions/{id}/events` (SSE) — none of those paths collide with the
//! ones here.
//! Test: `tests::*`.

use std::sync::Arc;

use axum::Router as AxumRouter;
use axum::extract::{Path, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use chrono::{DateTime, Utc};
use serde_json::json;

use crate::jsonrpc::Router;
use crate::mode::HarnessMode;
use crate::session::model::Session;
use crate::session::transcript::TranscriptRecord;

use super::{RestResult, call, respond, rpc_error_to_status, throwaway_ctx};

/// Shared axum state for every route in this module: just the JSON-RPC
/// router — every handler here is read-only and goes through
/// [`super::respond`] rather than touching `SessionRegistry` directly.
#[derive(Clone)]
struct SessionsState {
    router: Arc<Router>,
}

/// Build the `GET /sessions*` route group.
///
/// Why: kept separate from `crate::serve::http::build_axum_router` so this
/// resource group is unit-testable via `tower::util::ServiceExt::oneshot`
/// on its own, exactly like `crate::serve::http`'s own routes.
/// What: five `GET` routes (see module docs), all sharing one
/// `SessionsState { router }`, `with_state`-erased to `axum::Router<()>` so
/// the caller can `.merge()` it into a router with a different state type.
/// Test: `tests::list_sessions_returns_sessions_array`.
pub fn routes(router: Arc<Router>) -> AxumRouter {
    AxumRouter::new()
        .route("/sessions", get(list_sessions))
        .route("/sessions/{id}", get(get_session))
        .route("/sessions/{id}/transcript", get(get_transcript))
        .route("/sessions/{id}/transcript.md", get(get_transcript_markdown))
        .route("/sessions/{id}/readiness", get(get_readiness))
        .route("/sessions/{id}/goals", get(get_goals))
        .route("/sessions/{id}/budget", get(get_context_budget))
        .with_state(SessionsState { router })
}

/// `GET /sessions` -> `session.list`.
///
/// Why: enumerates every session the daemon currently owns.
/// What: no params; forwards straight to `session.list`, returning its
/// `{"sessions": [...]}` result verbatim.
/// Test: `tests::list_sessions_returns_sessions_array`.
async fn list_sessions(State(state): State<SessionsState>) -> RestResult {
    respond(&state.router, "session.list", json!({})).await
}

/// `GET /sessions/{id}` -> `session.status`.
///
/// Why: point lookup for one session's current state.
/// What: `404` with a JSON-RPC `session_not_found` envelope for an unknown
/// `id`; otherwise the `Session` JSON.
/// Test: `tests::get_session_found_returns_200_with_session_json`,
/// `tests::get_session_missing_returns_404_session_not_found`.
async fn get_session(State(state): State<SessionsState>, Path(id): Path<String>) -> RestResult {
    respond(&state.router, "session.status", json!({"session_id": id})).await
}

/// `GET /sessions/{id}/transcript` -> `session.get_transcript`.
///
/// Why: read-only access to a session's persisted run record (turns,
/// aggregate usage, cost).
/// What: `404` for an unknown `id`; otherwise the `TranscriptRecord` JSON —
/// a never-run session returns an empty `turns` array, not an error (see
/// `session::protocol::get_transcript`'s docs).
/// Test: `tests::get_transcript_found_returns_200_with_empty_turns`,
/// `tests::get_transcript_missing_returns_404_session_not_found`.
async fn get_transcript(State(state): State<SessionsState>, Path(id): Path<String>) -> RestResult {
    respond(
        &state.router,
        "session.get_transcript",
        json!({"session_id": id}),
    )
    .await
}

/// `GET /sessions/{id}/transcript.md` -> the full transcript rendered as a
/// human-readable Markdown document (`text/markdown`).
///
/// Why: issue #3526 — a workstream that ran 48 min to `deadline_exceeded`
/// with a runaway loop left no way to pull its transcript out for inspection.
/// The GUI's "Download transcript" button needs Markdown; making the DAEMON
/// render it (rather than only the GUI) also makes the transcript observable
/// in local dev independent of the packaged app — a developer running the
/// daemon can `curl http://127.0.0.1:7882/sessions/<id>/transcript.md` and
/// read/watch a run directly. This is the single source of truth for the
/// Markdown format (the GUI download just fetches these bytes), so the format
/// can never drift between a Rust and a TypeScript serializer.
/// What: fetches the same `session.get_transcript` record the JSON route
/// serves PLUS `session.status` (for the header's task/project/mode/workstream
/// context, none of which live on `TranscriptRecord`), renders
/// [`render_transcript_markdown`], and returns it with a
/// `text/markdown; charset=utf-8` content type. A `session_not_found` from
/// either inner call maps to a real `404` via [`rpc_error_to_status`], exactly
/// like the JSON route; the whole surface stays loopback-only because it adds
/// no new bind — it rides `crate::serve::http::build_axum_router`'s existing
/// listener (loopback-only doctrine, ADR-0011).
/// Test: `tests::get_transcript_markdown_renders_turns_and_tool_calls`,
/// `tests::get_transcript_markdown_missing_returns_404`.
async fn get_transcript_markdown(
    State(state): State<SessionsState>,
    Path(id): Path<String>,
) -> Response {
    let ctx = throwaway_ctx();
    let transcript_val = match call(
        &state.router,
        "session.get_transcript",
        json!({"session_id": id}),
        &ctx,
    )
    .await
    {
        Ok(v) => v,
        Err(err) => return (rpc_error_to_status(&err), err.message).into_response(),
    };
    let session_val = match call(
        &state.router,
        "session.status",
        json!({"session_id": id}),
        &ctx,
    )
    .await
    {
        Ok(v) => v,
        Err(err) => return (rpc_error_to_status(&err), err.message).into_response(),
    };

    let record: TranscriptRecord = match serde_json::from_value(transcript_val) {
        Ok(r) => r,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };
    let session: Session = match serde_json::from_value(session_val) {
        Ok(s) => s,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };

    let markdown = render_transcript_markdown(&session, &record, Utc::now());
    (
        [(header::CONTENT_TYPE, "text/markdown; charset=utf-8")],
        markdown,
    )
        .into_response()
}

/// Render a session's transcript as a readable Markdown document.
///
/// Why: the pure core of [`get_transcript_markdown`] (no I/O, no `Utc::now()`
/// inside — `generated_at` is passed in) so the format is unit-testable
/// directly. The tool-run lines are the WHOLE point for the motivating
/// diagnostic case (issue #3526): each `TurnRecord.tool_calls` entry renders
/// as its own ``- `ROLE` ran: <tool>`` bullet so a runaway loop (the same tool
/// fired turn after turn) reads as a visibly repeated column, never a
/// collapsed summary.
/// What: a title, a metadata bullet list (session id, workstream id, project,
/// task, mode, status, session-start ISO, export ISO, turn count, cost), a
/// horizontal rule, then one `##` section per turn — prose verbatim, each tool
/// call as its own ``- `ROLE` ran: <tool>`` bullet, a ``ran the test command``
/// note when flagged, and `_(no output)_` for a turn with neither. Roles are
/// upper-cased to match the live GUI pane's styling and the issue's example.
/// Test: `tests::get_transcript_markdown_*`.
fn render_transcript_markdown(
    session: &Session,
    record: &TranscriptRecord,
    generated_at: DateTime<Utc>,
) -> String {
    let mut out = String::new();
    let title = if session.task.trim().is_empty() {
        record.session_id.clone()
    } else {
        session.task.clone()
    };
    out.push_str(&format!("# Workstream transcript — {title}\n\n"));
    out.push_str(&format!("- **Session:** `{}`\n", record.session_id));
    out.push_str(&format!(
        "- **Workstream:** {}\n",
        session
            .workstream_id
            .as_ref()
            .map(|w| format!("`{w}`"))
            .unwrap_or_else(|| "_(unbound)_".to_string())
    ));
    out.push_str(&format!(
        "- **Project:** {}\n",
        session
            .project
            .clone()
            .unwrap_or_else(|| "_(none)_".to_string())
    ));
    out.push_str(&format!(
        "- **Task:** {}\n",
        if session.task.trim().is_empty() {
            "_(none)_".to_string()
        } else {
            session.task.clone()
        }
    ));
    out.push_str(&format!("- **Mode:** {}\n", mode_label(&session.mode)));
    out.push_str(&format!(
        "- **Status:** {}\n",
        serde_json::to_value(session.status)
            .ok()
            .and_then(|v| v.as_str().map(String::from))
            .unwrap_or_else(|| "unknown".to_string())
    ));
    out.push_str(&format!(
        "- **Session started:** {}\n",
        session.created_at.to_rfc3339()
    ));
    out.push_str(&format!("- **Exported:** {}\n", generated_at.to_rfc3339()));
    out.push_str(&format!("- **Turns:** {}\n", record.turns.len()));
    out.push_str(&format!(
        "- **Cost (USD):** {}\n",
        record
            .cost_usd
            .map(|c| format!("{c:.4}"))
            .unwrap_or_else(|| "_(n/a)_".to_string())
    ));
    out.push_str("\n---\n\n");

    if record.turns.is_empty() {
        out.push_str("_No turns were recorded for this session._\n");
        return out;
    }

    for (i, turn) in record.turns.iter().enumerate() {
        let role_upper = turn.role.to_uppercase();
        if turn.model.is_empty() {
            out.push_str(&format!("## {}. {role_upper}\n\n", i + 1));
        } else {
            out.push_str(&format!("## {}. {role_upper} · {}\n\n", i + 1, turn.model));
        }

        let body = turn.text.trim();
        let mut had_activity = false;
        if !body.is_empty() {
            out.push_str(&turn.text);
            out.push_str("\n\n");
            had_activity = true;
        }
        for tool in &turn.tool_calls {
            out.push_str(&format!("- `{role_upper}` ran: {tool}\n"));
            had_activity = true;
        }
        if turn.ran_test_command {
            out.push_str(&format!("- `{role_upper}` ran the test command\n"));
            had_activity = true;
        }
        if !turn.tool_calls.is_empty() || turn.ran_test_command {
            out.push('\n');
        }
        if !had_activity {
            out.push_str("_(no output)_\n\n");
        }
    }

    out
}

/// Short label for an optional [`HarnessMode`] in the Markdown header —
/// the serde string form, or `_(default)_` when unset.
fn mode_label(mode: &Option<HarnessMode>) -> String {
    match serde_json::to_value(mode) {
        Ok(serde_json::Value::String(s)) => s,
        _ => "_(default)_".to_string(),
    }
}

/// `GET /sessions/{id}/readiness` -> `session.get_readiness`.
///
/// Why: lets a late-attaching client query the one-time index-readiness
/// probe it may have missed on the SSE stream.
/// What: `404` for an unknown `id`; otherwise the `ReadinessQuery` JSON
/// (`{"status":"probed",...}` or `{"status":"never_probed"}`).
/// Test: `tests::get_readiness_found_returns_200_never_probed`,
/// `tests::get_readiness_missing_returns_404_session_not_found`.
async fn get_readiness(State(state): State<SessionsState>, Path(id): Path<String>) -> RestResult {
    respond(
        &state.router,
        "session.get_readiness",
        json!({"session_id": id}),
    )
    .await
}

/// `GET /sessions/{id}/goals` -> `session.get_goals`.
///
/// Why: lets an operator/UI inspect the current 5-slot goal state without
/// pulling the larger transcript payload.
/// What: `404` for an unknown `id`; otherwise `{"goals": [...]}` — `[]` for
/// a session with no transcript yet.
/// Test: `tests::get_goals_found_returns_200_with_empty_goals`,
/// `tests::get_goals_missing_returns_404_session_not_found`.
async fn get_goals(State(state): State<SessionsState>, Path(id): Path<String>) -> RestResult {
    respond(
        &state.router,
        "session.get_goals",
        json!({"session_id": id}),
    )
    .await
}

/// `GET /sessions/{id}/budget` -> `session.get_context_budget` (issue #3015).
///
/// Why: lets a late-attaching/reconnecting client (PR #3014's GUI status
/// bar) query the working-context budget it may have missed on the SSE
/// stream, instead of rendering "budget: unavailable" forever.
/// What: `404` for an unknown `id`; otherwise the `ContextBudgetQuery` JSON
/// (`{"status":"recorded",...}` or `{"status":"never_recorded"}`).
/// Test: `tests::get_context_budget_found_returns_200_never_recorded`,
/// `tests::get_context_budget_missing_returns_404_session_not_found`.
async fn get_context_budget(
    State(state): State<SessionsState>,
    Path(id): Path<String>,
) -> RestResult {
    respond(
        &state.router,
        "session.get_context_budget",
        json!({"session_id": id}),
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::{Body, to_bytes};
    use axum::http::{Request, StatusCode};
    use serde_json::Value;
    use tower::util::ServiceExt;

    /// Build a router wired with every `session.*` method plus a fresh
    /// `SessionRegistry`, then the REST route group over it — mirrors
    /// `crate::serve::http::tests::router_and_sessions` but only needs the
    /// registry to seed sessions directly (these routes never touch it).
    async fn app_and_registry() -> (AxumRouter, Arc<crate::session::SessionRegistry>) {
        let sessions = Arc::new(crate::session::SessionRegistry::new());
        let mut router = Router::new();
        crate::session::protocol::register(
            &mut router,
            sessions.clone(),
            crate::workstreams::test_shared_store().await,
        );
        let app = routes(Arc::new(router));
        (app, sessions)
    }

    async fn body_json(response: axum::response::Response) -> Value {
        let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    async fn body_text(response: axum::response::Response) -> String {
        let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        String::from_utf8(bytes.to_vec()).unwrap()
    }

    async fn get(app: &AxumRouter, uri: &str) -> axum::response::Response {
        app.clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(uri)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap()
    }

    /// `GET /sessions` must return `{"sessions": [...]}` with every session
    /// currently owned by the registry.
    #[tokio::test]
    async fn list_sessions_returns_sessions_array() {
        let (app, sessions) = app_and_registry().await;
        sessions.create("t".to_string(), None, crate::binding::ProjectBinding::None);

        let resp = get(&app, "/sessions").await;
        assert_eq!(resp.status(), StatusCode::OK);
        let v = body_json(resp).await;
        assert_eq!(v["sessions"].as_array().unwrap().len(), 1);
    }

    /// `GET /sessions/{id}` on a real session must return HTTP 200 with the
    /// `Session` JSON, `id` included.
    #[tokio::test]
    async fn get_session_found_returns_200_with_session_json() {
        let (app, sessions) = app_and_registry().await;
        let session = sessions.create("t".to_string(), None, crate::binding::ProjectBinding::None);

        let resp = get(&app, &format!("/sessions/{}", session.id)).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let v = body_json(resp).await;
        assert_eq!(v["id"], session.id);
    }

    /// `GET /sessions/{id}` on an unknown id must be a real HTTP 404 with a
    /// JSON-RPC `session_not_found` envelope, not a 200-wrapped error.
    #[tokio::test]
    async fn get_session_missing_returns_404_session_not_found() {
        let (app, _sessions) = app_and_registry().await;

        let resp = get(&app, "/sessions/does-not-exist").await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        let v = body_json(resp).await;
        assert_eq!(v["error"]["code"], -32007);
    }

    /// A percent-encoded, never-created id must decode through axum's path
    /// extractor and still reach the JSON-RPC layer as a `session_not_found`
    /// 404 — proving the REST id param is never treated as "invalid" at the
    /// routing layer, only at the domain (session lookup) layer, exactly
    /// like the JSON-RPC surface itself has no separate id-format
    /// validation.
    #[tokio::test]
    async fn get_session_percent_encoded_missing_id_returns_404() {
        let (app, _sessions) = app_and_registry().await;

        let resp = get(&app, "/sessions/not%20a%20real%20id").await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        let v = body_json(resp).await;
        assert_eq!(v["error"]["code"], -32007);
    }

    /// `GET /sessions/{id}/transcript` on a never-run session must return
    /// HTTP 200 with an empty `turns` array, not an error.
    #[tokio::test]
    async fn get_transcript_found_returns_200_with_empty_turns() {
        let (app, sessions) = app_and_registry().await;
        let session = sessions.create("t".to_string(), None, crate::binding::ProjectBinding::None);

        let resp = get(&app, &format!("/sessions/{}/transcript", session.id)).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let v = body_json(resp).await;
        assert_eq!(v["turns"].as_array().unwrap().len(), 0);
    }

    /// `GET /sessions/{id}/transcript.md` (issue #3526) must render the stored
    /// transcript as `text/markdown`, preserving every turn's prose AND each
    /// tool-run entry as its own bullet (the runaway-loop diagnostic the
    /// endpoint exists for), under a header carrying the session id + task.
    #[tokio::test]
    async fn get_transcript_markdown_renders_turns_and_tool_calls() {
        let (app, sessions) = app_and_registry().await;
        let session = sessions.create(
            "investigate the loop".to_string(),
            None,
            crate::binding::ProjectBinding::None,
        );
        sessions.set_run_outcome(
            &session.id,
            vec![
                crate::run_task::TurnRecord {
                    role: "pm".to_string(),
                    model: "claude-sonnet".to_string(),
                    text: "delegating to the engineer".to_string(),
                    tool_calls: vec![],
                    ran_test_command: false,
                    usage: crate::perf::TokenUsage::default(),
                },
                crate::run_task::TurnRecord {
                    role: "python-engineer".to_string(),
                    model: "claude-sonnet".to_string(),
                    text: String::new(),
                    tool_calls: vec!["write_files".to_string(), "write_files".to_string()],
                    ran_test_command: false,
                    usage: crate::perf::TokenUsage::default(),
                },
            ],
            crate::perf::TokenUsage::default(),
            Some(0.25),
        );

        let resp = get(&app, &format!("/sessions/{}/transcript.md", session.id)).await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            resp.headers().get(header::CONTENT_TYPE).unwrap(),
            "text/markdown; charset=utf-8"
        );
        let md = body_text(resp).await;
        assert!(
            md.contains("# Workstream transcript — investigate the loop"),
            "{md}"
        );
        assert!(
            md.contains(&format!("- **Session:** `{}`", session.id)),
            "{md}"
        );
        assert!(md.contains("- **Turns:** 2"), "{md}");
        assert!(md.contains("delegating to the engineer"), "{md}");
        // Each tool-run entry is its own bullet — a runaway loop stays visible.
        assert!(
            md.contains(
                "- `PYTHON-ENGINEER` ran: write_files\n- `PYTHON-ENGINEER` ran: write_files"
            ),
            "{md}"
        );
    }

    /// `GET /sessions/{id}/transcript.md` on an unknown id must 404 (same
    /// mapping as the JSON route), not render an empty document.
    #[tokio::test]
    async fn get_transcript_markdown_missing_returns_404() {
        let (app, _sessions) = app_and_registry().await;

        let resp = get(&app, "/sessions/does-not-exist/transcript.md").await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    /// `GET /sessions/{id}/transcript.md` on a never-run session must still be
    /// a 200 Markdown document (empty-turns note), not an error — mirrors the
    /// JSON route's never-run behavior.
    #[tokio::test]
    async fn get_transcript_markdown_never_run_returns_200_empty_note() {
        let (app, sessions) = app_and_registry().await;
        let session = sessions.create("t".to_string(), None, crate::binding::ProjectBinding::None);

        let resp = get(&app, &format!("/sessions/{}/transcript.md", session.id)).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let md = body_text(resp).await;
        assert!(
            md.contains("_No turns were recorded for this session._"),
            "{md}"
        );
    }

    /// `GET /sessions/{id}/transcript` on an unknown id must 404.
    #[tokio::test]
    async fn get_transcript_missing_returns_404_session_not_found() {
        let (app, _sessions) = app_and_registry().await;

        let resp = get(&app, "/sessions/does-not-exist/transcript").await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        let v = body_json(resp).await;
        assert_eq!(v["error"]["code"], -32007);
    }

    /// `GET /sessions/{id}/readiness` on a never-probed session must return
    /// HTTP 200 with `status: "never_probed"`, not an error.
    #[tokio::test]
    async fn get_readiness_found_returns_200_never_probed() {
        let (app, sessions) = app_and_registry().await;
        let session = sessions.create("t".to_string(), None, crate::binding::ProjectBinding::None);

        let resp = get(&app, &format!("/sessions/{}/readiness", session.id)).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let v = body_json(resp).await;
        assert_eq!(v["status"], "never_probed");
    }

    /// `GET /sessions/{id}/readiness` on an unknown id must 404.
    #[tokio::test]
    async fn get_readiness_missing_returns_404_session_not_found() {
        let (app, _sessions) = app_and_registry().await;

        let resp = get(&app, "/sessions/does-not-exist/readiness").await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        let v = body_json(resp).await;
        assert_eq!(v["error"]["code"], -32007);
    }

    /// `GET /sessions/{id}/goals` on a never-run session must return HTTP
    /// 200 with an empty `goals` array, not an error.
    #[tokio::test]
    async fn get_goals_found_returns_200_with_empty_goals() {
        let (app, sessions) = app_and_registry().await;
        let session = sessions.create("t".to_string(), None, crate::binding::ProjectBinding::None);

        let resp = get(&app, &format!("/sessions/{}/goals", session.id)).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let v = body_json(resp).await;
        assert_eq!(v["goals"].as_array().unwrap().len(), 0);
    }

    /// `GET /sessions/{id}/goals` on an unknown id must 404.
    #[tokio::test]
    async fn get_goals_missing_returns_404_session_not_found() {
        let (app, _sessions) = app_and_registry().await;

        let resp = get(&app, "/sessions/does-not-exist/goals").await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        let v = body_json(resp).await;
        assert_eq!(v["error"]["code"], -32007);
    }

    /// `GET /sessions/{id}/budget` on a session with no recorded turn must
    /// return HTTP 200 with `status: "never_recorded"`, not an error.
    #[tokio::test]
    async fn get_context_budget_found_returns_200_never_recorded() {
        let (app, sessions) = app_and_registry().await;
        let session = sessions.create("t".to_string(), None, crate::binding::ProjectBinding::None);

        let resp = get(&app, &format!("/sessions/{}/budget", session.id)).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let v = body_json(resp).await;
        assert_eq!(v["status"], "never_recorded");
    }

    /// `GET /sessions/{id}/budget` on an unknown id must 404.
    #[tokio::test]
    async fn get_context_budget_missing_returns_404_session_not_found() {
        let (app, _sessions) = app_and_registry().await;

        let resp = get(&app, "/sessions/does-not-exist/budget").await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        let v = body_json(resp).await;
        assert_eq!(v["error"]["code"], -32007);
    }
}
