//! `POST /tasks` REST route over the `task.run` JSON-RPC method (#2983
//! Slice 4).
//!
//! Why: Slices 2-3 (`super::sessions`/`super::sessions_write`) cover the
//! `session.*` resource — but a REST-only client also needs the explicit
//! "start executing" entry point `task.run` names (see
//! `crate::task::protocol` module docs): mint-or-reuse a session and kick off
//! a background LLM run against it. As with every route in this gateway, the
//! handler stays thin — extract the body, build the JSON-RPC `params`, call
//! `super::respond`/[`respond_accepted`] — because the actual behaviour
//! (session resolution, `HarnessMode` precedence, slot reservation, …)
//! already lives in `crate::task::protocol::task_run`; duplicating it here
//! would fork the two surfaces (see `super` module docs).
//! What: [`routes`] builds a standalone `axum::Router<()>` (its own
//! `TasksState` carrying just the `Arc<Router>`, `with_state`-erased before
//! return, mirroring `super::sessions::SessionsState`) mapping
//! `POST /tasks` -> `task.run`.
//! `crate::serve::http::build_axum_router` merges this into the daemon's main
//! router alongside `POST /rpc`, `GET /health`, `GET /sessions/{id}/events`,
//! and the `session.*` REST routes — `/tasks` collides with none of them.
//! Test: `tests::*`.

use std::sync::Arc;

use axum::Json;
use axum::Router as AxumRouter;
use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::post;
use serde::Deserialize;
use serde_json::{Value, json};
use trusty_common::mcp::Response;

use crate::jsonrpc::Router;

use super::respond;

/// Shared axum state for every route in this module: just the JSON-RPC
/// router, mirroring `super::sessions::SessionsState` — the handler goes
/// through [`respond_accepted`] rather than touching `SessionRegistry`
/// directly.
#[derive(Clone)]
struct TasksState {
    router: Arc<Router>,
}

/// Build the `POST /tasks` route group.
///
/// Why: kept separate from `crate::serve::http::build_axum_router` so this
/// resource group is unit-testable via `tower::util::ServiceExt::oneshot` on
/// its own, exactly like every other `rest::*` group.
/// What: one `POST` route, `TasksState { router }`, `with_state`-erased to
/// `axum::Router<()>` so the caller can `.merge()` it alongside the other
/// resource groups into the daemon's main router.
/// Test: `tests::run_task_returns_202_with_running_session`.
pub fn routes(router: Arc<Router>) -> AxumRouter {
    AxumRouter::new()
        .route("/tasks", post(run_task))
        .with_state(TasksState { router })
}

/// Request body for `POST /tasks`, shaped like `task::protocol`'s private
/// `TaskRunRequestParams` — forwarded verbatim into the RPC `params` `Value`,
/// so `task.run`'s own validation (empty `task_description`, unknown
/// `session_id`, invalid `project`, …) applies identically either way.
/// `project` stays a `String` here (mirroring `sessions_write::CreateBody`'s
/// identical choice) — it is forwarded verbatim into the RPC `params`
/// `Value`, and `task.run`'s own `PathBuf` deserialization +
/// `ProjectBinding::resolve` apply identically either way (#3178).
#[derive(Deserialize)]
struct RunTaskBody {
    task_description: String,
    #[serde(default)]
    agent_name: Option<String>,
    #[serde(default)]
    context: Option<String>,
    #[serde(default)]
    model_override: Option<String>,
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default)]
    mode: Option<String>,
    #[serde(default)]
    deadline_secs: Option<u64>,
    #[serde(default)]
    project: Option<String>,
    /// (Issue #3298) Forwarded verbatim to `task.run`'s own `workstream_id`
    /// param — see `task::protocol`'s docs for the mint-vs-reuse resolution
    /// rules.
    #[serde(default)]
    workstream_id: Option<String>,
}

/// Like [`super::respond`] but reports `202 Accepted` on success instead of
/// the implicit `200` `Json<Value>` carries.
///
/// Why: `task.run` is deliberately asynchronous — it reserves the execution
/// slot synchronously and then returns immediately, before the LLM run
/// itself has produced any result; the caller observes progress via
/// `session.attach`/`GET /sessions/{id}/events`, not this response body (see
/// `crate::task::protocol` module docs). That is the textbook `202 Accepted`
/// case ("the request has been accepted for processing, but the processing
/// has not been completed") rather than `201 Created`: unlike `POST
/// /sessions` (Slice 3), this route does not always mint a brand-new
/// resource — a `session_id` in the body reuses an EXISTING session — so
/// `201`'s "a new resource now exists at a location" framing does not fit
/// uniformly. `200` would also be wrong: it implies the body already
/// reflects the run's outcome, which it never does.
/// What: delegates to [`respond`], then rewraps a successful `Json<Value>` as
/// `(StatusCode::ACCEPTED, Json<Value>)`; the error arm is untouched —
/// `rpc_error_to_status` already returns the correct 4xx/5xx.
/// Test: `tests::run_task_returns_202_with_running_session`.
async fn respond_accepted(
    router: &Router,
    method: &str,
    params: Value,
) -> Result<(StatusCode, Json<Value>), (StatusCode, Json<Response>)> {
    respond(router, method, params)
        .await
        .map(|Json(v)| (StatusCode::ACCEPTED, Json(v)))
}

/// `POST /tasks` -> `task.run`.
///
/// Why: the REST entry point for "start executing a task", mint-or-reuse a
/// session per `crate::task::protocol::task_run`'s docs.
/// What: `202 Accepted` with `{session_id, status, mode, binding}` on
/// success (see [`respond_accepted`] for the status-code rationale); `400`
/// for an empty `task_description` or an invalid `project` (both `-32003
/// invalid_argument`, #3178) or a syntactically malformed body (rejected by
/// axum's `Json` extractor before this handler runs); `404` for an unknown
/// `session_id` (`-32007 session_not_found`). `project` is forwarded
/// verbatim to `task.run` — no independent resolution happens in this REST
/// layer (see the module docs' "duplicating it here would fork the two
/// surfaces" rationale).
/// Test: `tests::run_task_returns_202_with_running_session`,
/// `tests::run_task_empty_task_description_returns_400`,
/// `tests::run_task_unknown_session_id_returns_404`,
/// `tests::run_task_malformed_body_returns_400`,
/// `tests::run_task_with_project_binds_a_real_directory`,
/// `tests::run_task_invalid_project_returns_400`.
async fn run_task(
    State(state): State<TasksState>,
    Json(body): Json<RunTaskBody>,
) -> Result<(StatusCode, Json<Value>), (StatusCode, Json<Response>)> {
    respond_accepted(
        &state.router,
        "task.run",
        json!({
            "task_description": body.task_description,
            "agent_name": body.agent_name,
            "context": body.context,
            "model_override": body.model_override,
            "session_id": body.session_id,
            "mode": body.mode,
            "deadline_secs": body.deadline_secs,
            "project": body.project,
            "workstream_id": body.workstream_id,
        }),
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::{Body, to_bytes};
    use axum::http::Request;
    use std::sync::Arc as StdArc;
    use tower::util::ServiceExt;

    use crate::binding::ProjectBinding;
    use crate::session::SessionRegistry;

    /// Build a router wired with a real `task.run` (mock LLM, so no real
    /// subprocess/network call happens), then this module's route group over
    /// it — mirrors `sessions::tests::app_and_registry`.
    async fn app_and_registry() -> (AxumRouter, StdArc<SessionRegistry>, tempfile::TempDir) {
        let sessions = StdArc::new(SessionRegistry::new());
        let agents = tempfile::tempdir().expect("agents tempdir");
        std::fs::write(
            agents.path().join("pm.md"),
            "---\nname: pm\nmodel: openai/gpt-4o-mini\n---\n\nYou are the PM.\n",
        )
        .expect("write pm.md");
        let project = tempfile::tempdir().expect("project tempdir");
        let workstreams = crate::workstreams::test_shared_store().await;
        let mut router = Router::new();
        crate::session::protocol::register(&mut router, sessions.clone(), workstreams.clone());
        crate::task::protocol::register(
            &mut router,
            sessions.clone(),
            ProjectBinding::resolve(Some(project.path().to_path_buf())).expect("tempdir must bind"),
            agents.path().to_path_buf(),
            workstreams,
        );
        let app = routes(StdArc::new(router));
        (app, sessions, project)
    }

    async fn body_json(response: axum::response::Response) -> Value {
        let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    async fn post(app: &AxumRouter, uri: &str, body: &str) -> axum::response::Response {
        app.clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(uri)
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap()
    }

    /// A well-formed `POST /tasks` must return `202 Accepted` with a running
    /// session.
    #[tokio::test]
    async fn run_task_returns_202_with_running_session() {
        let _guard = crate::task::mock_llm::MOCK_LLM_ENV_LOCK.lock().await;
        // SAFETY: test-only env mutation; serialized by `ENV_LOCK` above.
        unsafe {
            std::env::set_var(
                crate::task::mock_llm::MOCK_LLM_ENV,
                crate::task::mock_llm::MOCK_LLM_ECHO,
            );
        }
        let (app, _sessions, _project) = app_and_registry().await;

        let resp = post(&app, "/tasks", r#"{"task_description": "say hi"}"#).await;
        unsafe {
            std::env::remove_var(crate::task::mock_llm::MOCK_LLM_ENV);
        }
        assert_eq!(resp.status(), StatusCode::ACCEPTED);
        let v = body_json(resp).await;
        assert_eq!(v["status"], "running");
    }

    /// An empty `task_description` must map to `400` (`-32003
    /// invalid_argument`).
    #[tokio::test]
    async fn run_task_empty_task_description_returns_400() {
        let (app, _sessions, _project) = app_and_registry().await;

        let resp = post(&app, "/tasks", r#"{"task_description": "   "}"#).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let v = body_json(resp).await;
        assert_eq!(v["error"]["code"], -32003);
    }

    /// A `session_id` naming a session that does not exist must be a real
    /// `404` with a `session_not_found` envelope — distinct from the `400`
    /// above.
    #[tokio::test]
    async fn run_task_unknown_session_id_returns_404() {
        let (app, _sessions, _project) = app_and_registry().await;

        let resp = post(
            &app,
            "/tasks",
            r#"{"task_description": "say hi", "session_id": "does-not-exist"}"#,
        )
        .await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        let v = body_json(resp).await;
        assert_eq!(v["error"]["code"], -32007);
    }

    /// Syntactically invalid JSON must never reach the handler — axum's
    /// `Json` extractor rejects it with a client error before `task.run`
    /// runs, matching `sessions_write_tests::create_session_malformed_body_returns_400`'s
    /// identical case for `POST /sessions`.
    #[tokio::test]
    async fn run_task_malformed_body_returns_400() {
        let (app, _sessions, _project) = app_and_registry().await;

        let resp = post(&app, "/tasks", "{not valid json").await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    /// A `project` in the body must bind a real directory, overriding the
    /// daemon's boot-time binding for this call, and the response must
    /// reflect it (#3178) — the REST-level half of the `task.run` RPC
    /// coverage in `task::protocol::tests::task_run_with_project_overrides_boot_binding`.
    #[tokio::test]
    async fn run_task_with_project_binds_a_real_directory() {
        let _guard = crate::task::mock_llm::MOCK_LLM_ENV_LOCK.lock().await;
        unsafe {
            std::env::set_var(
                crate::task::mock_llm::MOCK_LLM_ENV,
                crate::task::mock_llm::MOCK_LLM_ECHO,
            );
        }
        let (app, _sessions, _boot_project) = app_and_registry().await;
        let call_project = tempfile::tempdir().expect("call project tempdir");

        let body = json!({
            "task_description": "say hi",
            "project": call_project.path().to_string_lossy(),
        })
        .to_string();
        let resp = post(&app, "/tasks", &body).await;
        unsafe {
            std::env::remove_var(crate::task::mock_llm::MOCK_LLM_ENV);
        }
        assert_eq!(resp.status(), StatusCode::ACCEPTED);
        let v = body_json(resp).await;
        assert_eq!(
            v["binding"]["state"], "directory",
            "the per-call project must bind: {v}"
        );
    }

    /// A `project` naming a nonexistent path must map to `400` (`-32003
    /// invalid_argument`), matching `task.run`'s RPC-level error mapping.
    #[tokio::test]
    async fn run_task_invalid_project_returns_400() {
        let (app, _sessions, _project) = app_and_registry().await;

        let resp = post(
            &app,
            "/tasks",
            r#"{"task_description": "say hi", "project": "/no/such/path/anywhere"}"#,
        )
        .await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let v = body_json(resp).await;
        assert_eq!(v["error"]["code"], -32003);
    }

    /// A `session_id` reusing an existing session, paired with a `project`
    /// naming a DIFFERENT root than that session's own persisted binding,
    /// must be rejected with `400`/`-32003 invalid_argument` (#3178,
    /// code-critic HIGH finding, PR #3189) — the REST-level half of
    /// `task::protocol::tests::task_run_session_id_with_mismatched_project_is_rejected`.
    #[tokio::test]
    async fn run_task_session_id_with_mismatched_project_returns_400() {
        let (app, sessions, _boot_project) = app_and_registry().await;
        let session_project = tempfile::tempdir().expect("session project tempdir");
        let other_project = tempfile::tempdir().expect("other project tempdir");
        let session_binding = ProjectBinding::resolve(Some(session_project.path().to_path_buf()))
            .expect("tempdir must bind");
        let existing = sessions.create("say hi".to_string(), None, session_binding);

        let body = json!({
            "task_description": "say hi",
            "session_id": existing.id,
            "project": other_project.path().to_string_lossy(),
        })
        .to_string();
        let resp = post(&app, "/tasks", &body).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let v = body_json(resp).await;
        assert_eq!(v["error"]["code"], -32003);
        assert!(
            v["error"]["message"]
                .as_str()
                .unwrap_or_default()
                .contains("does not match session"),
            "error message must name the mismatch: {v}"
        );
    }
}
