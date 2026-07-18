//! `POST`/`PUT`/`DELETE /sessions*` REST routes over the `session.*`
//! JSON-RPC methods (#2983 Slice 3 — write routes).
//!
//! Why: Slice 2 (`super::sessions`) covered the five read-only `GET`
//! routes; a REST client also needs to mint sessions, drive them, and
//! manage their goal slots without dropping to raw JSON-RPC. Every handler
//! below stays as thin as Slice 2's — extract path/body, build the
//! JSON-RPC `params`, call `super::respond` (or [`respond_created`] for the
//! one route that needs a non-200 success status) — because the actual
//! behaviour (session lookup, validation, `-32007 session_not_found`
//! mapping, …) already lives in `crate::session::protocol`'s handlers;
//! duplicating it here as bespoke axum logic would fork the two surfaces
//! (see `super` module docs).
//! What: [`routes`] builds a standalone `axum::Router<()>` (its own
//! `SessionsWriteState` carrying just the `Arc<Router>`, `with_state`-erased
//! before return, mirroring `super::sessions::SessionsState`) mapping:
//!   - `POST /sessions` -> `session.create` (`201 Created` — this route
//!     mints a brand-new resource, so it follows the standard REST
//!     creation convention rather than Slice 1-2's uniform `200`/mapped-4xx;
//!     every other route below acts on an EXISTING resource, so it keeps
//!     `200` on success like every Slice 2 route)
//!   - `POST /sessions/{id}/messages` -> `session.send`
//!   - `POST /sessions/{id}/cancel` -> `session.cancel`
//!   - `PUT /sessions/{id}/goal` -> `session.set_goal`
//!   - `DELETE /sessions/{id}/goal` -> `session.clear_goal`
//!
//! Request bodies deserialize into private structs shaped identically to
//! the RPC layer's own `params` structs (`crate::session::protocol`'s
//! `CreateParams`/`SendParams`, `protocol_goals`'s
//! `SetGoalParams`/`ClearGoalParams`) minus whatever the URL path already
//! supplies (`session_id`) — a malformed body never reaches a handler at
//! all; axum's `Json<T>` extractor rejects it before the handler runs
//! (`400`/`422`, see module docs on [`super`]), so no handler here
//! hand-rolls body validation.
//! `crate::serve::http::build_axum_router` merges this into the daemon's
//! main router alongside `POST /rpc`, `GET /health`,
//! `GET /sessions/{id}/events`, and `super::sessions`'s read routes — none
//! of those paths collide with the ones here (`PUT`/`DELETE
//! /sessions/{id}/goal` share a path with different methods, which axum
//! routes independently).
//! Test: `tests::*` (`sessions_write_tests.rs`, split out purely for the
//! 500-SLOC cap — see `crate::session::registry`'s identical
//! `#[path = "registry_tests.rs"]` split for the established convention).

use std::sync::Arc;

use axum::Json;
use axum::Router as AxumRouter;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{post, put};
use serde::Deserialize;
use serde_json::{Value, json};
use trusty_common::mcp::Response;

use crate::jsonrpc::Router;

use super::{RestResult, respond};

/// Shared axum state for every route in this module: just the JSON-RPC
/// router, mirroring `super::sessions::SessionsState` — every handler here
/// goes through [`super::respond`]/[`respond_created`] rather than touching
/// `SessionRegistry` directly.
#[derive(Clone)]
struct SessionsWriteState {
    router: Arc<Router>,
}

/// Build the `POST`/`PUT`/`DELETE /sessions*` write route group.
///
/// Why: kept separate from `crate::serve::http::build_axum_router` so this
/// resource group is unit-testable via `tower::util::ServiceExt::oneshot`
/// on its own, exactly like `super::sessions::routes`.
/// What: five write routes (see module docs), all sharing one
/// `SessionsWriteState { router }`, `with_state`-erased to
/// `axum::Router<()>` so the caller can `.merge()` it alongside
/// `super::sessions::routes` into the daemon's main router.
/// Test: `tests::create_session_returns_201_with_running_session`.
pub fn routes(router: Arc<Router>) -> AxumRouter {
    AxumRouter::new()
        .route("/sessions", post(create_session))
        .route("/sessions/{id}/messages", post(send_message))
        .route("/sessions/{id}/cancel", post(cancel_session))
        .route("/sessions/{id}/goal", put(set_goal).delete(clear_goal))
        .with_state(SessionsWriteState { router })
}

/// Request body for `POST /sessions`, shaped like `session::protocol`'s
/// private `CreateParams` (`project` stays a `String` here — it is
/// forwarded verbatim into the RPC `params` `Value`, and `session.create`'s
/// own `PathBuf` deserialization applies identically either way).
#[derive(Deserialize)]
struct CreateBody {
    task: String,
    #[serde(default)]
    agent: Option<String>,
    #[serde(default)]
    project: Option<String>,
}

/// Request body for `POST /sessions/{id}/messages` — `session_id` comes
/// from the path, so only `input` remains.
#[derive(Deserialize)]
struct SendMessageBody {
    input: String,
}

/// Request body for `PUT /sessions/{id}/goal` — `session_id` comes from the
/// path, so only `slot`/`text` remain (mirrors `protocol_goals`'s private
/// `SetGoalParams`).
#[derive(Deserialize)]
struct SetGoalBody {
    slot: usize,
    text: String,
}

/// Request body for `DELETE /sessions/{id}/goal` — `session_id` comes from
/// the path, so only `slot` remains (mirrors `protocol_goals`'s private
/// `ClearGoalParams`). `DELETE` carrying a JSON body is unusual but valid
/// HTTP; it keeps this route's shape symmetric with `PUT`'s rather than
/// introducing a query-string convention nowhere else in this crate uses.
#[derive(Deserialize)]
struct ClearGoalBody {
    slot: usize,
}

/// Like [`super::respond`] but reports `201 Created` on success instead of
/// the implicit `200` `Json<Value>` carries — the one route in this module
/// that creates a brand-new resource rather than acting on an existing one.
///
/// Why: every other handler in this crate returns [`RestResult`], whose
/// `Ok` arm is a bare `Json<Value>` (200 via axum's blanket `IntoResponse`);
/// `POST /sessions` needs a different status on the SAME success path
/// without duplicating [`respond`]'s error-mapping logic.
/// What: delegates to [`respond`], then rewraps a successful `Json<Value>`
/// as `(StatusCode::CREATED, Json<Value>)`; the error arm is untouched —
/// `rpc_error_to_status` already returns the correct 4xx/5xx.
/// Test: `tests::create_session_returns_201_with_running_session`.
async fn respond_created(
    router: &Router,
    method: &str,
    params: Value,
) -> Result<(StatusCode, Json<Value>), (StatusCode, Json<Response>)> {
    respond(router, method, params)
        .await
        .map(|Json(v)| (StatusCode::CREATED, Json(v)))
}

/// `POST /sessions` -> `session.create`.
///
/// Why: the write half of the resource `GET /sessions` (Slice 2) already
/// enumerates — the only way a REST-only client can bring a session into
/// existence at all.
/// What: `201 Created` with the new `Session` JSON on success; `400` if
/// `task` is empty or `project` names no real directory (`-32003
/// invalid_argument`, see `session::protocol::create`'s docs on the AC-16.2
/// project-path reconciliation).
/// Test: `tests::create_session_returns_201_with_running_session`,
/// `tests::create_session_empty_task_returns_400`,
/// `tests::create_session_malformed_body_returns_400`.
async fn create_session(
    State(state): State<SessionsWriteState>,
    Json(body): Json<CreateBody>,
) -> Result<(StatusCode, Json<Value>), (StatusCode, Json<Response>)> {
    respond_created(
        &state.router,
        "session.create",
        json!({"task": body.task, "agent": body.agent, "project": body.project}),
    )
    .await
}

/// `POST /sessions/{id}/messages` -> `session.send`.
///
/// Why: the client -> daemon input path as a REST resource — a "message"
/// added to the session's input stream.
/// What: `200` with `{"acknowledged": true}` on success; `404` for an
/// unknown `id` (`-32007 session_not_found`).
/// Test: `tests::send_message_returns_200_acknowledged`,
/// `tests::send_message_missing_session_returns_404`,
/// `tests::send_message_malformed_body_returns_400`.
async fn send_message(
    State(state): State<SessionsWriteState>,
    Path(id): Path<String>,
    Json(body): Json<SendMessageBody>,
) -> RestResult {
    respond(
        &state.router,
        "session.send",
        json!({"session_id": id, "input": body.input}),
    )
    .await
}

/// `POST /sessions/{id}/cancel` -> `session.cancel`.
///
/// Why: explicit REST termination signal, mirroring the JSON-RPC method's
/// cooperative-vs-immediate cancel semantics (see
/// `session::protocol::cancel`'s docs) — this route never needs a body,
/// `session_id` from the path is the whole request.
/// What: `200` with the (possibly still-`running`, cooperative-cancel-
/// pending) `Session` snapshot on success; `404` for an unknown `id`.
/// Test: `tests::cancel_session_returns_200_with_session_json`,
/// `tests::cancel_session_missing_session_returns_404`,
/// `tests::cancel_session_is_idempotent`.
async fn cancel_session(
    State(state): State<SessionsWriteState>,
    Path(id): Path<String>,
) -> RestResult {
    respond(&state.router, "session.cancel", json!({"session_id": id})).await
}

/// `PUT /sessions/{id}/goal` -> `session.set_goal`.
///
/// Why: the operator-facing REST entry point onto a session's 5 fixed goal
/// slots (see `protocol_goals` module docs) — `PUT` because it is a full
/// replace-by-slot, idempotent write.
/// What: `200` with `{}` on success; `404` for an unknown `id`; `400` if
/// the session has no transcript yet or `slot` is out of range (`-32003
/// invalid_argument`, see `protocol_goals::set_goal`'s docs).
/// Test: `tests::set_goal_returns_200_empty_object`,
/// `tests::set_goal_missing_session_returns_404`,
/// `tests::set_goal_malformed_body_returns_400`.
async fn set_goal(
    State(state): State<SessionsWriteState>,
    Path(id): Path<String>,
    Json(body): Json<SetGoalBody>,
) -> RestResult {
    respond(
        &state.router,
        "session.set_goal",
        json!({"session_id": id, "slot": body.slot, "text": body.text}),
    )
    .await
}

/// `DELETE /sessions/{id}/goal` -> `session.clear_goal`.
///
/// Why: the operator-facing REST counterpart to `PUT` above.
/// What: `200` with `{}` on success — clearing an already-empty slot is
/// idempotent success, matching `protocol_goals::clear_goal`'s convention,
/// so this route never needs its own idempotency special-case; `404` for an
/// unknown `id`; `400` for an out-of-range `slot` (`-32003
/// invalid_argument`).
/// Test: `tests::clear_goal_returns_200_empty_object`,
/// `tests::clear_goal_missing_session_returns_404`,
/// `tests::clear_goal_malformed_body_returns_400`.
async fn clear_goal(
    State(state): State<SessionsWriteState>,
    Path(id): Path<String>,
    Json(body): Json<ClearGoalBody>,
) -> RestResult {
    respond(
        &state.router,
        "session.clear_goal",
        json!({"session_id": id, "slot": body.slot}),
    )
    .await
}

#[cfg(test)]
#[path = "sessions_write_tests.rs"]
mod tests;
