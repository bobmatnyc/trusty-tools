//! Daemon HTTP API.
//!
//! Why: the CLI, TUI, and Telegram bot are separate processes; they need a
//! transport to the daemon. HTTP/JSON over a loopback port is simple, debuggable
//! with `curl`, and lets the universal hook relay receive events from a tiny
//! forwarder shim with no client library.
//! What: builds the axum [`Router`] — health, session listing, the hook-event
//! relay endpoint, the live event feed, and the per-agent breaker view. State
//! is injected as `Arc<DaemonState>` via axum's `State` extractor.
//! Test: `cargo test -p trusty-mpm-daemon` drives the handlers directly with an
//! in-memory state (no socket bind needed).

use std::convert::Infallible;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    response::sse::{Event, KeepAlive, Sse},
    routing::{get, post},
};
use futures::Stream;
use tokio_stream::StreamExt;
use tokio_stream::wrappers::BroadcastStream;
use utoipa::OpenApi;
use utoipa_swagger_ui::SwaggerUi;

use crate::core::compress::CompressionLevel;
use crate::core::hook::HookEvent;
use crate::core::project::ProjectInfo;
use crate::core::session::{ControlModel, Session, SessionId, SessionStatus};

use super::error::DaemonError;
use super::services::{HookDecision, HookService, PairingService, SessionService, TmuxService};
use super::state::DaemonState;

/// The Claude Code configuration analyzer routes (`/claude-config/*`).
///
/// Why: that endpoint cluster is cohesive and large; keeping it in its own
/// module keeps `api.rs` focused on the core session / hook / tmux surface.
/// The handlers are re-exported below so `router` and `openapi.rs` can keep
/// referring to them as `super::api::<handler>`.
pub mod claude_config_routes;
pub use claude_config_routes::*;

/// The cross-session coordinator routes (`/api/v1/coordinator/*`).
///
/// Why: the coordinator's context and chat endpoints form their own cohesive
/// cluster; keeping them in a sibling module mirrors `claude_config_routes` and
/// keeps `api.rs` focused on the core session / hook / tmux surface.
/// The handlers are re-exported so `router` can refer to them as
/// `super::api::<handler>`.
pub mod coordinator_routes;
pub use coordinator_routes::*;

/// Typed HTTP response bodies for every endpoint.
///
/// Why: keeping the response structs in their own module keeps `api.rs`
/// focused on routing and handler logic. Re-exported so handlers and tests
/// refer to them as `super::api::<Type>`.
pub mod types;
pub use types::*;

/// Build the daemon's HTTP router with shared state injected.
///
/// Why: one place wires every route so `main` stays a thin bootstrap.
/// What: returns an axum `Router` already carrying `Arc<DaemonState>`.
/// Test: `health_endpoint_responds` and the hook-relay tests call handlers via
/// this router's logic.
pub fn router(state: Arc<DaemonState>) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/sessions", get(list_sessions).post(register_session))
        .route("/api/v1/sessions/connect", post(connect_session))
        .route("/sessions/dead", axum::routing::delete(reap_sessions))
        .route("/sessions/discover", post(discover_sessions))
        .route("/sessions/{id}", get(get_session).delete(remove_session))
        .route("/sessions/{id}/events", get(stream_session_events))
        .route("/sessions/{id}/events/poll", get(session_events))
        .route("/sessions/{id}/pause", post(pause_session))
        .route("/sessions/{id}/resume", post(resume_session))
        .route("/sessions/{id}/command", post(send_command))
        .route("/sessions/{id}/output", get(get_output))
        .route("/sessions/{id}/pane", get(get_output))
        .route("/sessions/{id}/pid", axum::routing::patch(set_session_pid))
        .route("/projects", get(list_projects).post(register_project))
        .route("/projects/current", get(current_project))
        .route("/projects/discover", get(discover_projects))
        .route("/events", get(stream_events))
        .route("/events/poll", get(recent_events))
        .route("/hooks", post(ingest_hook))
        .route("/breakers", get(breakers))
        .route("/optimizer", get(get_optimizer))
        .route("/overseer", get(get_overseer))
        .route("/llm/chat", post(llm_chat))
        .route("/api/v1/coordinator/context", get(coordinator_context))
        .route("/api/v1/coordinator/chat", post(coordinator_chat))
        .route("/tmux/sessions", get(list_tmux_sessions))
        .route("/tmux/sessions/{name}/snapshot", get(tmux_snapshot))
        .route("/tmux/adopt", post(adopt_tmux_session))
        .route("/claude-config", get(get_claude_config))
        .route("/claude-config/apply", post(apply_claude_config))
        .route("/claude-config/restart", post(restart_claude_code))
        .route(
            "/claude-config/checkpoints",
            get(list_checkpoints).post(create_checkpoint),
        )
        .route(
            "/claude-config/checkpoints/{id}",
            axum::routing::delete(delete_checkpoint),
        )
        .route("/claude-config/restore", post(restore_checkpoint))
        .route("/claude-config/profiles", get(list_profiles))
        .route("/claude-config/deploy", post(deploy_profile))
        .route("/pair/request", post(pair_request))
        .route("/pair/confirm", post(pair_confirm))
        .route("/pair/status", get(pair_status))
        .route("/pair/reset", post(pair_reset))
        .route("/api/v1/doctor", get(doctor))
        .route("/api/v1/errors", get(list_errors))
        .route("/api/v1/report-bug", post(report_bug_http))
        .merge(
            SwaggerUi::new("/api-docs")
                .url("/api-docs/openapi.json", super::openapi::ApiDoc::openapi()),
        )
        .with_state(state)
}

/// Liveness probe — always returns `ok` while the daemon is up.
#[utoipa::path(
    get,
    path = "/health",
    tag = "config",
    responses((status = 200, description = "Daemon is alive", body = String))
)]
pub async fn health() -> &'static str {
    "ok"
}

/// Query parameters for `GET /sessions`.
///
/// Why: `trusty-mpm session list` scopes the listing to one project; an
/// optional `?project=<path>` filter keeps the endpoint usable both ways.
/// What: an optional project path; when absent, all sessions are returned.
/// Test: `list_sessions_filters_by_project`.
#[derive(serde::Deserialize, Default)]
pub struct SessionQuery {
    /// Optional project path to filter sessions by.
    pub project: Option<PathBuf>,
}

/// `GET /sessions` — snapshot of managed sessions, optionally project-scoped.
#[utoipa::path(
    get,
    path = "/sessions",
    tag = "sessions",
    params(("project" = Option<String>, Query, description = "Filter by project path")),
    responses((status = 200, description = "Array of managed sessions", body = [Session]))
)]
pub async fn list_sessions(
    State(state): State<Arc<DaemonState>>,
    Query(query): Query<SessionQuery>,
) -> Json<SessionsResponse> {
    let sessions = match query.project {
        Some(path) => state.list_sessions_for_project(&path),
        None => state.list_sessions(),
    };
    Json(SessionsResponse { sessions })
}

/// `GET /events/poll` — JSON snapshot of recent hook events (legacy / fallback).
///
/// Why: SSE-incapable clients (curl in a one-shot script, legacy CLI tooling)
/// still need a way to read the ring buffer. The push-based feed lives at
/// `GET /events`; this endpoint preserves the original synchronous-snapshot
/// contract under a `/poll` suffix for backward compatibility.
/// What: returns the bounded ring buffer of recent [`HookEventRecord`]s as
/// `{ "events": [...] }`.
/// Test: covered transitively by `hook_relay_ingests_known_event`.
#[utoipa::path(
    get,
    path = "/events/poll",
    tag = "events",
    responses((status = 200, description = "Recent hook events across all sessions"))
)]
pub async fn recent_events(State(state): State<Arc<DaemonState>>) -> Json<EventsResponse> {
    Json(EventsResponse {
        events: state.recent_hook_events(),
    })
}

/// `GET /events` — live SSE stream of every hook event.
///
/// Why: the GUI and other real-time consumers need push notifications when
/// hook events arrive rather than polling `GET /events/poll`. SSE is the
/// lowest-friction streaming protocol for browser and `reqwest` clients —
/// plain HTTP, no upgrade dance, automatic reconnection in the browser.
/// What: subscribes to the daemon's broadcast channel and streams each event
/// as one SSE `data:` line (JSON-encoded). A `KeepAlive` comment ping every
/// 15 seconds prevents idle proxies from closing the connection.
/// Test: `events_sse_streams_one_frame` posts a hook and reads one SSE frame
/// from this handler.
pub async fn stream_events(
    State(state): State<Arc<DaemonState>>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let rx = state.event_subscribe();
    let stream = BroadcastStream::new(rx).filter_map(|result| match result {
        Ok(val) => Some(Ok(Event::default().data(val.to_string()))),
        // Lagged / dropped frames are skipped — the channel intentionally
        // sheds load when a subscriber falls behind.
        Err(_) => None,
    });
    Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("ping"),
    )
}

/// JSON body for registering a session via `POST /sessions`.
///
/// Why: a session created by an external launcher (or the CLI) must announce
/// itself so the dashboard and MCP tools can see it. The GUI's "New Session"
/// button additionally needs the daemon to *spawn* a fresh session — create
/// the tmux host and start `claude` — without going through the CLI; that
/// path is opted into by supplying `workdir`.
/// What: the project directory the session runs in, plus an optional project
/// association, an optional caller-supplied tmux session name, and an
/// optional `workdir` that switches the request from registration-only to
/// spawn mode.
/// Test: `register_and_remove_session` (registration-only),
/// `spawn_session_without_claude_returns_422` (spawn mode failure path),
/// `spawn_session_without_tmux_returns_422` (spawn mode no-tmux path).
#[derive(serde::Deserialize, utoipa::ToSchema)]
pub struct RegisterSession {
    /// Project directory the session was launched in (the session's working
    /// directory). Named `project` to match the CLI `project` argument.
    pub project: String,
    /// Optional project this session belongs to. When present, the session is
    /// associated with that registered project so `session list` can scope to
    /// it.
    #[serde(default)]
    #[schema(value_type = Option<String>)]
    pub project_path: Option<PathBuf>,
    /// Optional caller-supplied tmux session name.
    ///
    /// Why: the CLI computes a `tmpm-<folder>` name from the project directory
    /// and creates the tmux session under that name; passing it here keeps the
    /// daemon registry's `tmux_name` consistent with the live tmux session.
    /// What: when present and non-empty it is used as the session's
    /// `tmux_name`; when absent the daemon derives one itself.
    #[serde(default)]
    pub name: Option<String>,
    /// Optional working directory in which to spawn a brand-new session.
    ///
    /// Why: the GUI's "New Session" button needs the daemon to create the
    /// tmux host and start `claude` itself — going through the CLI is not an
    /// option from a browser. Presence of `workdir` is the explicit opt-in
    /// for spawn semantics; absence preserves the registration-only behaviour
    /// every other caller (the CLI, `session start`, the hook auto-register
    /// path) depends on.
    /// What: when present, the daemon creates a tmux session in this
    /// directory and launches `claude` inside it; when absent, only
    /// bookkeeping happens.
    /// Test: `spawn_session_without_claude_returns_422`,
    /// `spawn_session_without_tmux_returns_422`.
    #[serde(default)]
    #[schema(value_type = Option<String>)]
    pub workdir: Option<PathBuf>,
}

/// `POST /sessions` — register a new managed session, returning its id.
///
/// Why: registering a session is pure bookkeeping — it records that a session
/// exists so the dashboard and MCP tools can see it. In addition, the GUI's
/// "New Session" button needs the daemon to actually *spawn* a fresh session
/// (create the tmux host and start `claude`) without going through the CLI;
/// supplying `workdir` switches the request into that spawn mode. Sessions
/// without `workdir` are still recorded as pure bookkeeping (no tmux window
/// is created — that behaviour is unchanged so the existing CLI and hook
/// auto-registration paths keep working).
/// What: builds the `Session` record and registers it in state. When `workdir`
/// is present, additionally creates the tmux session via [`TmuxService::spawn_claude`]
/// and starts `claude` in it; failures map to HTTP 422 (`claude` missing or
/// tmux missing) or 500 (tmux command failed). On success the new session
/// appears immediately in `GET /sessions`.
/// Test: `register_and_remove_session` covers the bookkeeping path;
/// `spawn_session_without_claude_returns_422` and
/// `spawn_session_without_tmux_returns_422` cover the spawn-mode error paths;
/// `spawn_session_registers_session_when_possible` covers the happy/observable
/// state path.
#[utoipa::path(
    post,
    path = "/sessions",
    tag = "sessions",
    request_body = RegisterSession,
    responses(
        (status = 201, description = "Session registered; returns its id and name"),
        (status = 422, description = "Spawn requested but `claude` binary or tmux is unavailable"),
        (status = 500, description = "tmux command failed while creating the session"),
    )
)]
pub async fn register_session(
    State(state): State<Arc<DaemonState>>,
    Json(body): Json<RegisterSession>,
) -> Result<Json<RegisterSessionResponse>, DaemonError> {
    // Derive the tmux name from the project directory (`tmpm-<folder>`) so the
    // registry name matches the folder-based session the CLI creates. A
    // caller-supplied `name` always wins; otherwise fall back to the UUID name.
    let project_dir = body.project_path.as_deref();
    let mut session = Session::new(
        SessionId::new(),
        body.project.clone(),
        ControlModel::Tmux,
        project_dir,
    );
    session.project_path = body.project_path.clone();
    if let Some(name) = body.name.as_deref().filter(|n| !n.is_empty()) {
        session.tmux_name = name.to_string();
    }
    if let Some(workdir) = body.workdir.as_deref() {
        // Mirror the workdir onto the session record so the dashboard and the
        // reaper see the spawn directory, not the project label. The
        // `Session::workdir` field is the per-session working directory; the
        // `project` field is a label / association.
        session.workdir = workdir.to_string_lossy().into_owned();
    }

    // Spawn mode: create the tmux host and start `claude` *before* the session
    // is registered, so a 422 or 500 leaves the registry untouched. This
    // matches the standard HTTP contract — a failed POST should not leave a
    // half-created resource visible to subsequent GETs.
    if let Some(workdir) = body.workdir.as_deref() {
        TmuxService::spawn_claude(&session.tmux_name, workdir)?;
        // The session is now actively running `claude`; mark it Active so the
        // dashboard reflects reality rather than the default `Starting` state.
        session.status = SessionStatus::Active;
    }

    let id = session.id;
    let tmux_name = session.tmux_name.clone();
    state.register_session(session);

    // Discover the `claude` PID inside the registered tmux pane in the
    // background so the reaper can monitor process liveness. This is the
    // daemon-side counterpart of the CLI's post-launch PID capture; it does not
    // block the response, and a failure is logged, never fatal.
    super::services::session_service::spawn_pid_capture(Arc::clone(&state), id, tmux_name.clone());

    Ok(Json(RegisterSessionResponse {
        id,
        name: tmux_name,
    }))
}

/// `POST /api/v1/sessions/connect` — register a session for a *connect* (no
/// deployment) launch.
///
/// Why: `tm connect` deliberately skips the framework-deployment sequence that
/// `tm launch` runs (instructions, agents, skills) — it only wants the daemon
/// to start or attach to the tmux-hosted session. The deployment work lives in
/// the client/CLI, not the daemon, so the daemon-side bookkeeping for a
/// `connect` is identical to `register_session`: it records that the session
/// exists. A distinct endpoint keeps the two intents observable on the wire and
/// gives `connect` its own seam should the daemon need to diverge later.
/// What: delegates to [`register_session`] — the daemon does no deployment in
/// either path, so the registration body and response are the same.
/// Test: `connect_session_registers_without_deploy` in `api_tests.rs`.
#[utoipa::path(
    post,
    path = "/api/v1/sessions/connect",
    tag = "sessions",
    request_body = RegisterSession,
    responses((status = 201, description = "Session registered for connect; returns its id and name"))
)]
pub async fn connect_session(
    state: State<Arc<DaemonState>>,
    body: Json<RegisterSession>,
) -> Result<Json<RegisterSessionResponse>, DaemonError> {
    register_session(state, body).await
}

/// `GET /sessions/:id` — fetch a single session's detail.
///
/// Why: clients need to fetch one session without paging through the full
/// `GET /sessions` list — avoids over-fetching and simplifies client state
/// management when the caller already knows the session id.
/// What: parses `id` as a UUID, looks the session up in [`DaemonState`], and
/// returns it as JSON. A malformed id is a `400`; an unknown id is a `404`.
/// Test: `get_session_returns_session`, `get_session_unknown_is_404`.
#[utoipa::path(
    get,
    path = "/sessions/{id}",
    tag = "sessions",
    params(("id" = String, Path, description = "Session UUID")),
    responses(
        (status = 200, description = "Session detail", body = Session),
        (status = 400, description = "Malformed session id"),
        (status = 404, description = "No session with that id"),
    )
)]
pub async fn get_session(
    State(state): State<Arc<DaemonState>>,
    Path(id): Path<String>,
) -> Result<Json<Session>, DaemonError> {
    let session_id = parse_id(&id)?;
    state
        .session(session_id)
        .map(Json)
        .ok_or(DaemonError::SessionNotFound { id })
}

/// `DELETE /sessions/:id` — deregister a session.
#[utoipa::path(
    delete,
    path = "/sessions/{id}",
    tag = "sessions",
    params(("id" = String, Path, description = "Session UUID")),
    responses(
        (status = 200, description = "Session removed"),
        (status = 404, description = "No session with that id"),
    )
)]
pub async fn remove_session(
    State(state): State<Arc<DaemonState>>,
    Path(id): Path<String>,
) -> Result<Json<RemoveSessionResponse>, DaemonError> {
    let session = parse_id(&id)?;
    match state.remove_session(session) {
        Some(_) => Ok(Json(RemoveSessionResponse { removed: id })),
        None => Err(DaemonError::SessionNotFound { id }),
    }
}

/// `DELETE /sessions/dead` — reap registry entries with no live tmux session.
///
/// Why: dead sessions accumulate forever otherwise; an operator (or a periodic
/// task) needs a way to prune the registry down to what tmux actually hosts.
/// What: discovers tmux, calls [`DaemonState::reap_dead_sessions`], and returns
/// `{ "removed": <count> }`. If tmux is unavailable nothing is reaped (returns
/// `0`) — reaping against an empty list would wrongly delete every session.
/// Test: `reap_dead_sessions` in `state.rs` covers the core logic.
#[utoipa::path(
    delete,
    path = "/sessions/dead",
    tag = "sessions",
    responses((status = 200, description = "Dead sessions reaped; returns the removed count"))
)]
pub async fn reap_sessions(State(state): State<Arc<DaemonState>>) -> Json<ReapResponse> {
    let result = SessionService::new(&state).reap();
    Json(ReapResponse {
        removed: result.reaped,
        stopped: result.stopped,
    })
}

/// JSON body for `PATCH /sessions/{id}/pid`.
///
/// Why: after launching `claude` inside a tmux pane the CLI discovers the real
/// process PID and reports it back so the daemon can monitor process liveness.
/// What: the OS-level `claude` process id.
/// Test: `set_session_pid_records_pid`.
#[derive(serde::Deserialize, utoipa::ToSchema)]
pub struct SetPidRequest {
    /// OS-level `claude` process id discovered inside the session's tmux pane.
    pub pid: u32,
}

/// `PATCH /sessions/{id}/pid` — record the OS-level `claude` process PID.
///
/// Why: a tmux session can outlive the `claude` process inside it; tracking the
/// real PID lets the reaper detect a stopped session. The CLI (and the daemon's
/// own launch path) discover the PID a few seconds after `send-keys` and report
/// it here.
/// What: resolves the session by UUID, sets `session.pid`, and echoes the id
/// and PID. An unknown id is `404`.
/// Test: `set_session_pid_records_pid`, `set_session_pid_unknown_is_404`.
#[utoipa::path(
    patch,
    path = "/sessions/{id}/pid",
    tag = "sessions",
    params(("id" = String, Path, description = "Session UUID")),
    request_body = SetPidRequest,
    responses(
        (status = 200, description = "PID recorded for the session"),
        (status = 404, description = "No session with that id"),
    )
)]
pub async fn set_session_pid(
    State(state): State<Arc<DaemonState>>,
    Path(id): Path<String>,
    Json(body): Json<SetPidRequest>,
) -> Result<Json<SetPidResponse>, DaemonError> {
    let session = parse_id(&id)?;
    if state.set_session_pid(session, body.pid) {
        Ok(Json(SetPidResponse {
            session_id: id,
            pid: body.pid,
        }))
    } else {
        Err(DaemonError::SessionNotFound { id })
    }
}

/// `POST /sessions/discover` — auto-discover Claude Code sessions.
///
/// Why: `GET /sessions` only reports daemon-managed sessions; operators run
/// `claude` / `claude-code` / `claude-mpm` / `tm` in tmux panes — and, more
/// commonly, in native Terminal.app windows — that the daemon never created.
/// This endpoint scans both and registers the ones running Claude Code so they
/// appear in the dashboard and the Telegram bot.
/// What: runs [`super::discovery::discover_all`] (tmux panes plus native `ps`
/// processes) and returns `{ "discovered": <count>, "sessions": [name, ...] }`.
/// A missing tmux or `ps` yields a zero count rather than an error.
/// Test: `discover_sessions_returns_count` in `api_tests.rs`.
#[utoipa::path(
    post,
    path = "/sessions/discover",
    tag = "sessions",
    responses((status = 200, description = "tmux sessions running Claude Code, newly registered"))
)]
pub async fn discover_sessions(State(state): State<Arc<DaemonState>>) -> Json<DiscoverResponse> {
    let result = super::discovery::discover_all(&state);
    Json(DiscoverResponse {
        discovered: result.adopted,
        sessions: result.sessions,
    })
}

/// `GET /sessions/:id/events/poll` — JSON snapshot of one session's hook events.
///
/// Why: the push-based per-session feed lives at `GET /sessions/{id}/events`;
/// this endpoint preserves the original synchronous-snapshot contract under
/// a `/poll` suffix for clients that cannot or do not need to stream.
/// What: parses the session id and returns the bounded ring buffer filtered
/// to that session.
/// Test: covered transitively by the `state::tests::hook_history_is_bounded`
/// test, which exercises the underlying `hook_events_for` query.
#[utoipa::path(
    get,
    path = "/sessions/{id}/events/poll",
    tag = "events",
    params(("id" = String, Path, description = "Session UUID")),
    responses(
        (status = 200, description = "Recent hook events for the session"),
        (status = 404, description = "No session with that id"),
    )
)]
pub async fn session_events(
    State(state): State<Arc<DaemonState>>,
    Path(id): Path<String>,
) -> Result<Json<EventsResponse>, DaemonError> {
    let session = parse_id(&id)?;
    Ok(Json(EventsResponse {
        events: state.hook_events_for(session),
    }))
}

/// `GET /sessions/{id}/events` — live SSE stream of one session's hook events.
///
/// Why: per-session event filtering lets the GUI subscribe to one session's
/// activity without receiving noise from every other concurrent session.
/// What: subscribes to the broadcast channel and forwards only events whose
/// serialized JSON contains the session id (matching the
/// `"session": "<uuid>"` field every [`HookEventRecord`] carries). A 15-second
/// `KeepAlive` ping keeps idle proxies from closing the connection.
/// Test: `session_events_sse_filters_by_session` posts a hook for one
/// session and confirms only that session's stream sees it.
pub async fn stream_session_events(
    Path(id): Path<String>,
    State(state): State<Arc<DaemonState>>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let rx = state.event_subscribe();
    let stream = BroadcastStream::new(rx).filter_map(move |result| match result {
        Ok(val) => {
            // Include the event only if it mentions this session id. The
            // record serializes its `SessionId` as the UUID string, so a
            // simple substring match is sufficient and avoids a typed parse
            // per frame.
            if val.to_string().contains(&id) {
                Some(Ok(Event::default().data(val.to_string())))
            } else {
                None
            }
        }
        Err(_) => None,
    });
    Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("ping"),
    )
}

/// Result of applying an optional compression level to captured output.
///
/// Why: the command and output endpoints share the same compress-then-return
/// shape; bundling the text and stats lets one helper produce both.
/// What: the (possibly compressed) text, the byte stats, and the level as a
/// lowercase wire string (`None` when no compression was applied).
/// Test: `apply_compression_off_is_passthrough`, `apply_compression_summarise`.
struct CompressedOutput {
    /// The output text after compression (or unchanged when off).
    text: String,
    /// Byte counts before and after compression.
    stats: crate::core::compress::CompressionStats,
    /// Lowercase wire name of the level applied, or `None` when uncompressed.
    level_label: Option<String>,
}

/// Apply an optional compression level to captured pane output.
///
/// Why: `POST .../command` and `GET .../output` both accept an optional
/// `?compress=` query param; doing the compress-or-passthrough decision once
/// keeps the two handlers identical.
/// What: when `level` is `Some`, runs [`compress_output`] and records the
/// level's lowercase label; when `None`, returns the raw text with empty stats
/// and no label.
/// Test: `apply_compression_off_is_passthrough`, `apply_compression_summarise`.
fn apply_compression(level: Option<CompressionLevel>, raw: &str) -> CompressedOutput {
    match level {
        Some(level) => {
            let (text, stats) = crate::core::compress::compress_output(raw, level);
            CompressedOutput {
                text,
                stats,
                level_label: Some(compression_level_label(level)),
            }
        }
        None => CompressedOutput {
            text: raw.to_string(),
            stats: crate::core::compress::CompressionStats::default(),
            level_label: None,
        },
    }
}

/// Lowercase wire name for a [`CompressionLevel`].
///
/// Why: API responses report the applied level as a stable lowercase string,
/// matching the `snake_case` serde representation of the enum.
/// What: maps each variant to its `serde` wire name.
/// Test: `compress_level_label_matches_serde`.
fn compression_level_label(level: CompressionLevel) -> String {
    match level {
        CompressionLevel::Off => "off",
        CompressionLevel::Trim => "trim",
        CompressionLevel::Summarise => "summarise",
        CompressionLevel::Caveman => "caveman",
    }
    .to_string()
}

/// JSON body for `POST /sessions/{id}/pause`.
///
/// Why: a pause may carry an optional operator note describing where the
/// session was left off; when absent the daemon derives one from pane output.
/// What: an optional free-form summary string.
/// Test: `pause_then_resume_round_trips`.
#[derive(serde::Deserialize, utoipa::ToSchema, Default)]
pub struct PauseRequest {
    /// Optional note about where the session was left off.
    #[serde(default)]
    pub summary: Option<String>,
}

/// `POST /sessions/{id}/pause` — pause a session, saving its state for resume.
///
/// Why: an operator stepping away needs the session frozen with a "where I left
/// off" note that survives a daemon restart.
/// What: resolves the session by UUID or friendly name, captures the last 50
/// pane lines, sets `status = Paused` / `paused_at = now` / `pause_summary`
/// (the request note, or the first 500 chars of the `Summarise`-compressed
/// captured output), and mirrors the pause record to disk via
/// `session_store::save_pause`.
/// Test: `pause_then_resume_round_trips`, `pause_unknown_session_is_404`.
#[utoipa::path(
    post,
    path = "/sessions/{id}/pause",
    tag = "sessions",
    params(("id" = String, Path, description = "Session UUID or friendly name")),
    request_body = PauseRequest,
    responses(
        (status = 200, description = "Session paused; returns the pause summary"),
        (status = 404, description = "No session with that id or name"),
    )
)]
pub async fn pause_session(
    State(state): State<Arc<DaemonState>>,
    Path(id): Path<String>,
    Json(body): Json<PauseRequest>,
) -> Result<Json<PauseResponse>, DaemonError> {
    let result = SessionService::new(&state).pause(&id, body.summary)?;
    Ok(Json(PauseResponse {
        paused: true,
        session_id: result.session_id,
        summary: result.summary,
    }))
}

/// `POST /sessions/{id}/resume` — resume a previously-paused session.
///
/// Why: the counterpart to pause; clears the frozen state and the on-disk
/// pause record so the session is active again.
/// What: resolves the session, requires `status == Paused` (else `409`), sets
/// `status = Active` / `paused_at = None` / `pause_summary = None`, and removes
/// the pause file via `session_store::clear_pause`.
/// Test: `pause_then_resume_round_trips`, `resume_unpaused_session_is_409`.
#[utoipa::path(
    post,
    path = "/sessions/{id}/resume",
    tag = "sessions",
    params(("id" = String, Path, description = "Session UUID or friendly name")),
    responses(
        (status = 200, description = "Session resumed"),
        (status = 404, description = "No session with that id or name"),
        (status = 409, description = "Session is not paused"),
    )
)]
pub async fn resume_session(
    State(state): State<Arc<DaemonState>>,
    Path(id): Path<String>,
) -> Result<Json<ResumeResponse>, DaemonError> {
    SessionService::new(&state).resume(&id)?;
    Ok(Json(ResumeResponse { resumed: true }))
}

/// JSON body for `POST /sessions/{id}/command`.
///
/// Why: feeding a command into a session's tmux pane is how the operator (and
/// the Telegram bot) drives Claude Code remotely.
/// What: the command line to type into the pane.
/// Test: `send_command_returns_output_shape`.
#[derive(serde::Deserialize, utoipa::ToSchema)]
pub struct CommandRequest {
    /// The command line to send to the session's tmux pane.
    pub command: String,
}

/// Query parameters for `POST /sessions/{id}/command`.
///
/// Why: the caller may want the captured output summarised before it returns,
/// completing the "summarize output" step of the full user cycle.
/// What: an optional compression level (`off`, `trim`, `summarise`,
/// `caveman`); when absent the raw pane capture is returned unchanged.
/// Test: `send_command_compress_query_defaults_off`.
#[derive(serde::Deserialize, Default)]
pub struct CommandQuery {
    /// Compression level to apply to the captured output before returning.
    /// Values: off, trim, summarise, caveman. Defaults to none (raw output).
    #[serde(default)]
    pub compress: Option<CompressionLevel>,
}

/// `POST /sessions/{id}/command` — send a command to a session's tmux pane.
///
/// Why: remote control of a running session — type a line, let it run, read
/// back what happened.
/// What: resolves the session (`404` if missing, `409` if `Stopped`), sends the
/// command via `TmuxDriver::send_line`, waits 500ms for output to settle, then
/// captures the last 100 pane lines. When `?compress=` is supplied the capture
/// is compressed at that level before returning. tmux errors are logged, not
/// fatal — the endpoint still returns `200` with whatever output was captured.
/// Test: `send_command_returns_output_shape`, `command_to_stopped_session_is_409`.
#[utoipa::path(
    post,
    path = "/sessions/{id}/command",
    tag = "sessions",
    params(
        ("id" = String, Path, description = "Session UUID or friendly name"),
        ("compress" = Option<String>, Query, description = "Compression level: off, trim, summarise, caveman"),
    ),
    request_body = CommandRequest,
    responses(
        (status = 200, description = "Command sent; returns captured pane output"),
        (status = 404, description = "No session with that id or name"),
        (status = 409, description = "Session is stopped"),
    )
)]
pub async fn send_command(
    State(state): State<Arc<DaemonState>>,
    Path(id): Path<String>,
    Query(query): Query<CommandQuery>,
    Json(body): Json<CommandRequest>,
) -> Result<Json<CommandResponse>, DaemonError> {
    let session = SessionService::new(&state).command_target(&id)?;
    TmuxService::send_command(&session, &body.command);

    // Give the pane a moment to render the command's output before capturing.
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    let raw = TmuxService::capture(&session, 100);
    let compressed = apply_compression(query.compress, &raw);

    Ok(Json(CommandResponse {
        sent: true,
        output: compressed.text,
        original_bytes: compressed.stats.original_bytes,
        compressed_bytes: compressed.stats.compressed_bytes,
        compress_level: compressed.level_label,
    }))
}

/// Query parameters for `GET /sessions/{id}/output`.
///
/// Why: the caller chooses how much scrollback to capture and whether to
/// summarise it; defaults keep the endpoint usable with no query string.
/// What: an optional line count (defaulting to 50 when absent) and an optional
/// compression level applied to the capture before returning.
/// Test: `get_output_returns_output_shape`, `output_query_defaults`.
#[derive(serde::Deserialize, Default)]
pub struct OutputQuery {
    /// Number of trailing pane lines to capture (default 50 when absent).
    #[serde(default)]
    pub lines: Option<u32>,
    /// Compression level to apply to the captured output before returning.
    /// Values: off, trim, summarise, caveman. Defaults to none (raw output).
    #[serde(default)]
    pub compress: Option<CompressionLevel>,
}

/// Default trailing-line count for `GET /sessions/{id}/output`.
fn default_output_lines() -> u32 {
    50
}

/// `GET /sessions/{id}/output` — capture the current tmux pane output.
///
/// Why: the dashboard and the Telegram bot show a session's recent output
/// without sending it a command.
/// What: resolves the session (`404` if missing), captures the last `?lines=N`
/// pane lines (default 50), optionally compresses it at `?compress=`, and
/// returns `{ output, lines, original_bytes, compressed_bytes, compress_level }`.
/// tmux being unavailable yields an empty `output` rather than an error.
/// Test: `get_output_returns_output_shape`, `output_unknown_session_is_404`.
#[utoipa::path(
    get,
    path = "/sessions/{id}/output",
    tag = "sessions",
    params(
        ("id" = String, Path, description = "Session UUID or friendly name"),
        ("lines" = Option<u32>, Query, description = "Trailing lines to capture (default 50)"),
        ("compress" = Option<String>, Query, description = "Compression level: off, trim, summarise, caveman"),
    ),
    responses(
        (status = 200, description = "Captured pane output"),
        (status = 404, description = "No session with that id or name"),
    )
)]
pub async fn get_output(
    State(state): State<Arc<DaemonState>>,
    Path(id): Path<String>,
    Query(query): Query<OutputQuery>,
) -> Result<Json<OutputResponse>, DaemonError> {
    let session = SessionService::new(&state).resolve(&id)?;
    let lines = query.lines.unwrap_or_else(default_output_lines);
    let raw = TmuxService::capture(&session, lines);
    let compressed = apply_compression(query.compress, &raw);
    Ok(Json(OutputResponse {
        output: compressed.text,
        lines,
        original_bytes: compressed.stats.original_bytes,
        compressed_bytes: compressed.stats.compressed_bytes,
        compress_level: compressed.level_label,
    }))
}

/// `GET /breakers` — every agent's circuit-breaker state.
#[utoipa::path(
    get,
    path = "/breakers",
    tag = "config",
    responses((status = 200, description = "Array of per-agent circuit-breaker states"))
)]
pub async fn breakers(State(state): State<Arc<DaemonState>>) -> Json<BreakersResponse> {
    let breakers = state
        .all_breakers()
        .into_iter()
        .map(|(agent, breaker)| BreakerEntry { agent, breaker })
        .collect();
    Json(BreakersResponse { breakers })
}

/// JSON body for the universal hook relay endpoint.
///
/// Why: the forwarder shim posts raw Claude Code hook events here; a typed
/// body documents the contract.
/// What: session id, the Claude Code event name, and the opaque payload.
/// Test: `hook_relay_ingests_known_event`.
#[derive(serde::Deserialize, utoipa::ToSchema)]
pub struct HookPost {
    /// Session the event came from (UUID string).
    pub session_id: String,
    /// Claude Code event, e.g. `PreToolUse`. Deserialization rejects any name
    /// that is not a known [`HookEvent`] variant, so an unknown event is a
    /// `400` before the handler runs.
    #[schema(value_type = String)]
    pub event: HookEvent,
    /// Raw event payload (shape varies per event).
    #[serde(default)]
    #[schema(value_type = Object)]
    pub payload: serde_json::Value,
}

/// `POST /hooks` — universal hook relay; ingests one Claude Code hook event.
///
/// Why: this is how the daemon achieves full observability — a forwarder shim
/// configured for *all* 32 hook events posts each one here. It is also the
/// enforcement point for the optional session overseer.
/// What: parses the session id and event name. On a `SessionStart` event for
/// an unknown session it auto-registers that session (this is how a claude
/// session announces itself to the daemon — connection-driven registration,
/// not `POST /sessions`). It then runs the overseer on tool-use events
/// (auditing every decision; a `Block` returns `403` early), compresses
/// `PostToolUse` output, then appends a `HookEventRecord` to the ring buffer.
/// Rejects malformed ids with `400`.
/// Test: `hook_relay_ingests_known_event`, `hook_relay_rejects_unknown_event`,
/// `overseer_blocks_pre_tool_use`, `session_start_auto_registers_session`.
#[utoipa::path(
    post,
    path = "/hooks",
    tag = "internal",
    request_body = HookPost,
    responses(
        (status = 200, description = "Hook event accepted"),
        (status = 400, description = "Unknown event name or malformed session id"),
        (status = 403, description = "Overseer blocked the event"),
    )
)]
pub async fn ingest_hook(
    State(state): State<Arc<DaemonState>>,
    Json(post): Json<HookPost>,
) -> Result<Json<HookAcceptedResponse>, DaemonError> {
    let session = parse_id(&post.session_id)?;

    // Auto-register on SessionStart if not already known. This is how a claude
    // session connects itself to the daemon: its first hook event registers it
    // using the incoming UUID, so discovery and `POST /sessions` are not the
    // only ways a session enters state. The workdir is left empty here and
    // enriched later by a snapshot or subsequent events.
    if post.event == HookEvent::SessionStart && state.session(session).is_none() {
        let mut new_session = Session::new(session, String::new(), ControlModel::Tmux, None);
        new_session.status = SessionStatus::Active;
        state.register_session(new_session);
        tracing::info!("auto-registered session on SessionStart: {session:?}");
    }

    match HookService::new(&state).process(session, post.event, post.payload) {
        HookDecision::Block { reason } => Err(DaemonError::OverseerBlocked { reason }),
        _ => Ok(Json(HookAcceptedResponse {
            accepted: post.event,
        })),
    }
}

/// `GET /overseer` — current session-overseer configuration and status.
///
/// Why: the CLI and dashboard surface whether oversight is active and which
/// strategy is in force.
/// What: returns `{ "overseer": { "enabled": <bool>, "handler": <str> } }`,
/// where `handler` is the active strategy name reported by the overseer.
/// Test: `get_overseer_returns_status`.
#[utoipa::path(
    get,
    path = "/overseer",
    tag = "config",
    responses((status = 200, description = "Overseer enabled flag and handler type"))
)]
pub async fn get_overseer(State(state): State<Arc<DaemonState>>) -> Json<OverseerResponse> {
    Json(OverseerResponse {
        overseer: OverseerStatus {
            enabled: state.overseer().is_enabled(),
            handler: state.overseer_handler().to_string(),
        },
    })
}

/// `POST /llm/chat` — send a message to the LLM chat assistant.
///
/// Why: the Telegram bot routes free-text (non-command) messages here, and the
/// TUI's `/chat` command does the same; both want a conversational endpoint
/// that reuses the overseer's already-resolved OpenRouter credentials.
/// What: requires a configured LLM overseer (else `503`), runs
/// [`LlmOverseer::chat`] over the client-supplied history, and returns the
/// assistant reply plus the updated history. The daemon stays stateless about
/// chat sessions — the caller owns the history.
/// Test: `llm_chat_without_overseer_is_503`.
#[utoipa::path(
    post,
    path = "/llm/chat",
    tag = "config",
    request_body = LlmChatRequest,
    responses(
        (status = 200, description = "Assistant reply and updated history"),
        (status = 503, description = "LLM chat is not configured on this daemon"),
    )
)]
pub async fn llm_chat(
    State(state): State<Arc<DaemonState>>,
    Json(body): Json<LlmChatRequest>,
) -> Result<Json<LlmChatResponse>, DaemonError> {
    let overseer = state.llm_overseer().ok_or_else(|| {
        DaemonError::ServiceUnavailable(
            "LLM chat is not configured (no OpenRouter API key)".to_string(),
        )
    })?;
    let mut history = body.history;
    let reply = overseer
        .chat(&mut history, &body.message)
        .await
        .map_err(|e| DaemonError::Internal(e.to_string()))?;
    Ok(Json(LlmChatResponse { reply, history }))
}

/// `GET /optimizer` — current token-use optimizer configuration.
///
/// Why: the CLI and dashboard surface the active compression tuning. The
/// config is now framework-managed on disk (`optimizer.toml`); this endpoint
/// is read-only introspection of the daemon's in-memory copy of it.
/// What: returns `{ "optimizer": <OptimizerConfig> }`.
/// Test: `get_optimizer_returns_default`.
#[utoipa::path(
    get,
    path = "/optimizer",
    tag = "config",
    responses((status = 200, description = "Current token-use optimizer configuration"))
)]
pub async fn get_optimizer(State(state): State<Arc<DaemonState>>) -> Json<OptimizerResponse> {
    Json(OptimizerResponse {
        optimizer: state.optimizer_config(),
    })
}

/// JSON body for registering a project via `POST /projects`.
///
/// Why: `trusty-mpm project init` announces a working directory to the daemon
/// so sessions started there can be associated with it.
/// What: the absolute path of the project's working directory.
/// Test: `register_and_list_projects`.
#[derive(serde::Deserialize, utoipa::ToSchema)]
pub struct RegisterProject {
    /// Absolute path to the project's working directory.
    #[schema(value_type = String)]
    pub path: PathBuf,
}

/// `POST /projects` — register a project, returning its `ProjectInfo`.
///
/// Why: the daemon owns the project registry; `project init` posts the
/// resolved directory here.
/// What: delegates to [`DaemonState::register_project`] and returns the
/// stored info as JSON.
/// Test: `register_and_list_projects`.
#[utoipa::path(
    post,
    path = "/projects",
    tag = "projects",
    request_body = RegisterProject,
    responses((status = 201, description = "Project registered", body = ProjectInfo))
)]
pub async fn register_project(
    State(state): State<Arc<DaemonState>>,
    Json(body): Json<RegisterProject>,
) -> Json<ProjectInfo> {
    Json(state.register_project(body.path))
}

/// `GET /projects` — snapshot of every registered project.
#[utoipa::path(
    get,
    path = "/projects",
    tag = "projects",
    responses((status = 200, description = "Array of registered projects", body = [ProjectInfo]))
)]
pub async fn list_projects(State(state): State<Arc<DaemonState>>) -> Json<ProjectsResponse> {
    Json(ProjectsResponse {
        projects: state.list_projects(),
    })
}

/// Query parameters for `GET /projects/current`.
///
/// Why: the daemon cannot see the caller's cwd; the CLI passes the resolved
/// path so the daemon can look the project up.
/// What: the path to resolve a project for.
/// Test: `current_project_found_and_missing`.
#[derive(serde::Deserialize)]
pub struct CurrentProjectQuery {
    /// Path whose registered project should be returned.
    pub path: PathBuf,
}

/// `GET /projects/current?path=<dir>` — the project registered for `path`.
///
/// Why: `trusty-mpm project info` shows the current directory's project; the
/// daemon resolves the path against its registry.
/// What: returns the matching `ProjectInfo`, or `404` when `path` is not a
/// registered project.
/// Test: `current_project_found_and_missing`.
#[utoipa::path(
    get,
    path = "/projects/current",
    tag = "projects",
    params(("path" = String, Query, description = "Directory whose project to resolve")),
    responses(
        (status = 200, description = "The project registered for the path", body = ProjectInfo),
        (status = 404, description = "Path is not a registered project"),
    )
)]
pub async fn current_project(
    State(state): State<Arc<DaemonState>>,
    Query(query): Query<CurrentProjectQuery>,
) -> Result<Json<ProjectInfo>, DaemonError> {
    match state.project(&query.path) {
        Some(info) => Ok(Json(info)),
        None => Err(DaemonError::SessionNotFound {
            id: query.path.display().to_string(),
        }),
    }
}

/// `GET /projects/discover` — projects mined from `~/.claude/projects/`.
///
/// Why: rather than register every repo by hand, the operator wants trusty-mpm
/// to enumerate the projects Claude Code already knows about and offer them for
/// one-tap registration (the Telegram `/projects` command consumes this).
/// What: runs [`ProjectDiscovery::discover`], maps each row to a
/// [`DiscoveredProjectInfo`] (path as a string, last-session time as an
/// ISO-8601 string), and returns them newest-session-first. The discovery
/// itself never fails — an absent directory yields an empty list.
/// Test: `discover_projects_returns_array`.
#[utoipa::path(
    get,
    path = "/projects/discover",
    tag = "projects",
    responses((status = 200, description = "Projects discovered from Claude Code config"))
)]
pub async fn discover_projects(
    State(_state): State<Arc<DaemonState>>,
) -> Json<DiscoverProjectsResponse> {
    let projects = crate::core::project_discovery::ProjectDiscovery::discover()
        .into_iter()
        .map(|p| DiscoveredProjectInfo {
            path: p.path.display().to_string(),
            session_count: p.session_count,
            last_session: p.last_session.map(system_time_to_iso8601),
        })
        .collect();
    Json(DiscoverProjectsResponse { projects })
}

/// Render a `SystemTime` as an ISO-8601 / RFC3339 UTC string.
///
/// Why: the discovery endpoint reports session times as human- and
/// machine-readable strings; `SystemTime` has no wire-stable serde form.
/// What: converts via `chrono::DateTime<Utc>`, falling back to the Unix epoch
/// for the (unreachable in practice) pre-1970 case.
/// Test: covered by `discover_projects_returns_array`.
fn system_time_to_iso8601(time: std::time::SystemTime) -> String {
    let datetime: chrono::DateTime<chrono::Utc> = time.into();
    datetime.to_rfc3339()
}

// ---- universal tmux session management ---------------------------------

/// `GET /tmux/sessions` — every tmux session on the host, origin-tagged.
///
/// Why: trusty-mpm manages *all* tmux sessions, not just the ones it created;
/// the dashboard needs the full list with an origin label so it can offer to
/// adopt external sessions.
/// What: runs `TmuxDriver::list_all_sessions` and returns
/// `{ "sessions": [ExternalSession, ...] }`. tmux being unavailable yields an
/// empty array rather than an error.
/// Test: `list_tmux_sessions_returns_array`.
#[utoipa::path(
    get,
    path = "/tmux/sessions",
    tag = "tmux",
    responses((status = 200, description = "All tmux sessions with origin labels"))
)]
pub async fn list_tmux_sessions(
    State(_state): State<Arc<DaemonState>>,
) -> Json<TmuxSessionsResponse> {
    Json(TmuxSessionsResponse {
        sessions: TmuxService::list_all(),
    })
}

/// `GET /tmux/sessions/{name}/snapshot` — capture any session's current state.
///
/// Why: the dashboard inspects any session (internal or external) without
/// attaching to it.
/// What: runs `TmuxDriver::monitor_session` for the last 100 pane lines and
/// returns the [`SessionSnapshot`]. A missing session or absent tmux is `404`.
/// Test: `tmux_snapshot_unknown_session_is_404` (covers the no-tmux path).
#[utoipa::path(
    get,
    path = "/tmux/sessions/{name}/snapshot",
    tag = "tmux",
    params(("name" = String, Path, description = "tmux session name")),
    responses(
        (status = 200, description = "Session snapshot"),
        (status = 404, description = "Session not found or tmux unavailable"),
    )
)]
pub async fn tmux_snapshot(
    State(_state): State<Arc<DaemonState>>,
    Path(name): Path<String>,
) -> Result<Json<TmuxSnapshotResponse>, DaemonError> {
    let snapshot = TmuxService::snapshot(&name, 100)?;
    Ok(Json(TmuxSnapshotResponse { snapshot }))
}

/// JSON body for `POST /tmux/adopt`.
///
/// Why: adopting an external session needs only its name.
/// What: the tmux session name to bring under oversight.
/// Test: `adopt_tmux_session_handles_missing`.
#[derive(serde::Deserialize, utoipa::ToSchema)]
pub struct AdoptRequest {
    /// tmux session name to adopt.
    pub session: String,
}

/// `POST /tmux/adopt` — register an external tmux session for oversight.
///
/// Why: trusty-mpm should watch sessions it did not create; adoption is the
/// explicit, non-destructive opt-in for that.
/// What: runs `TmuxDriver::adopt_session` (which captures the session's shape
/// without modifying it) and returns the [`AdoptedSession`]. A missing session
/// or absent tmux is `404`.
/// Test: `adopt_tmux_session_handles_missing`.
#[utoipa::path(
    post,
    path = "/tmux/adopt",
    tag = "tmux",
    request_body = AdoptRequest,
    responses(
        (status = 200, description = "Session adopted; returns its captured state"),
        (status = 404, description = "Session not found or tmux unavailable"),
    )
)]
pub async fn adopt_tmux_session(
    State(_state): State<Arc<DaemonState>>,
    Json(body): Json<AdoptRequest>,
) -> Result<Json<AdoptResponse>, DaemonError> {
    let adopted = TmuxService::adopt(&body.session)?;
    Ok(Json(AdoptResponse { adopted }))
}

// ---- bot pairing --------------------------------------------------------

/// `POST /pair/request` — generate a one-time Telegram-bot pairing code.
///
/// Why: pairing the Telegram bot to this daemon needs an out-of-band shared
/// secret; `tm pair` calls this on the local daemon to obtain a short code the
/// operator then types into the bot.
/// What: generates a six-character code (stored with a five-minute TTL) and
/// returns `{ "code", "expires_in_seconds" }`.
/// Test: `pair_request_returns_code`.
#[utoipa::path(
    post,
    path = "/pair/request",
    tag = "config",
    responses((status = 200, description = "A one-time pairing code and its TTL"))
)]
pub async fn pair_request(
    State(state): State<Arc<DaemonState>>,
) -> Json<super::services::PairCode> {
    Json(PairingService::new(&state).request_code())
}

/// JSON body for `POST /pair/confirm`.
///
/// Why: confirming a pairing needs the operator's code and the Telegram chat id
/// to bind.
/// What: the six-character code and the chat id.
/// Test: `pair_confirm_validates_code`.
#[derive(serde::Deserialize, utoipa::ToSchema)]
pub struct PairConfirmRequest {
    /// The one-time pairing code issued by `POST /pair/request`.
    pub code: String,
    /// The Telegram chat id to pair with this daemon.
    pub chat_id: i64,
}

/// `POST /pair/confirm` — confirm a pairing code and register the chat.
///
/// Why: the Telegram bot's `/pair <code>` flow validates the operator's code so
/// push alerts have an authenticated destination.
/// What: validates `code` against the outstanding code within its TTL; on
/// success stores `chat_id` and returns `{ "success": true, "chat_id" }`,
/// otherwise `{ "success": false, "error": "invalid or expired code" }`.
/// Test: `pair_confirm_validates_code`, `pair_confirm_rejects_bad_code`.
#[utoipa::path(
    post,
    path = "/pair/confirm",
    tag = "config",
    request_body = PairConfirmRequest,
    responses((status = 200, description = "Pairing result (success flag and chat id or error)"))
)]
pub async fn pair_confirm(
    State(state): State<Arc<DaemonState>>,
    Json(body): Json<PairConfirmRequest>,
) -> Json<PairConfirmResponse> {
    match PairingService::new(&state).confirm(&body.code, body.chat_id) {
        Ok(()) => Json(PairConfirmResponse {
            success: true,
            chat_id: Some(body.chat_id),
            error: None,
        }),
        Err(_) => Json(PairConfirmResponse {
            success: false,
            chat_id: None,
            error: Some("invalid or expired code".to_string()),
        }),
    }
}

/// `GET /pair/status` — report whether a Telegram chat is paired.
///
/// Why: the bot's `/start` command branches on whether the daemon is already
/// paired so it shows either a welcome-and-pair prompt or a ready message.
/// What: returns `{ "paired": <bool>, "chat_id": <i64 or null> }`.
/// Test: `pair_status_reports_unpaired`.
#[utoipa::path(
    get,
    path = "/pair/status",
    tag = "config",
    responses((status = 200, description = "Pairing status (paired flag and chat id)"))
)]
pub async fn pair_status(
    State(state): State<Arc<DaemonState>>,
) -> Json<super::services::PairStatus> {
    Json(PairingService::new(&state).status())
}

/// `POST /pair/reset` — clear the Telegram pairing.
///
/// Why: an operator unpairing the bot must drop the binding both in memory and
/// on disk so a daemon restart does not restore it from `pairing.json`.
/// What: delegates to [`PairingService::reset`] and returns `{ "reset": true }`.
/// Test: `pair_reset_clears_pairing` in `api_tests.rs`.
#[utoipa::path(
    post,
    path = "/pair/reset",
    tag = "config",
    responses((status = 200, description = "Pairing cleared"))
)]
pub async fn pair_reset(State(state): State<Arc<DaemonState>>) -> Json<PairResetResponse> {
    PairingService::new(&state).reset();
    Json(PairResetResponse { reset: true })
}

// ---- diagnostics --------------------------------------------------------

/// Query parameters for `GET /api/v1/doctor`.
///
/// Why: the instruction-pipeline probe must be scoped to a specific project
/// (`<project>/.trusty-mpm/last-instructions.md`); the daemon cannot see the
/// caller's cwd, so the CLI passes the resolved path here.
/// What: an optional `project` path; when absent the instruction probe reports
/// a warning rather than a definitive result.
/// Test: `doctor_endpoint_returns_report`.
#[derive(serde::Deserialize, Default)]
pub struct DoctorQuery {
    /// Optional project directory to scope the instruction-pipeline probe to.
    pub project: Option<PathBuf>,
}

/// `GET /api/v1/doctor` — run the full trusty-mpm stack diagnostic.
///
/// Why: a misconfigured stack fails in confusing ways; one endpoint that runs
/// every "is this wired correctly?" probe gives operators (and the `tm doctor`
/// CLI / Telegram `/doctor` command) a single actionable verdict.
/// What: delegates to [`super::doctor::run_doctor`], which probes the
/// instruction pipeline, agent and skill deployment, and the trusty-memory /
/// trusty-search sidecars, and returns the assembled [`DoctorReport`] as JSON.
/// The endpoint always returns `200` — individual failures live in the report's
/// per-check statuses, not the HTTP status.
/// Test: `doctor_endpoint_returns_report`.
#[utoipa::path(
    get,
    path = "/api/v1/doctor",
    tag = "config",
    params(("project" = Option<String>, Query, description = "Project directory to scope the instruction probe to")),
    responses((status = 200, description = "Full diagnostic report"))
)]
pub async fn doctor(
    State(_state): State<Arc<DaemonState>>,
    Query(query): Query<DoctorQuery>,
) -> Json<crate::core::doctor::DoctorReport> {
    Json(super::doctor::run_doctor(query.project.as_deref()).await)
}

// ── Bug-reporting HTTP endpoints (Phase 2 surface + Phase 3 filing) ──────────

/// Query parameters for `GET /api/v1/errors`.
///
/// Why: the caller controls how many errors to return; the daemon caps it at
///      100 to prevent oversized responses.
/// What: optional `limit` field; defaults to 20 when absent.
/// Test: `list_errors_returns_array` in `api_tests.rs`.
#[derive(serde::Deserialize, Default)]
pub struct ErrorsQuery {
    /// Maximum number of errors to return (default 20, max 100).
    #[serde(default)]
    pub limit: Option<u64>,
}

/// `GET /api/v1/errors` — list recently captured errors from all daemon stores.
///
/// Why: sub-agents that cannot use MCP tools directly can still browse captured
///      errors over plain HTTP before deciding to file a bug report.
/// What: aggregates and deduplicates errors from all known daemon JSONL stores,
///       returns them sorted by most-recent occurrence. Always returns `200`.
/// Test: `list_errors_returns_array` in `api_tests.rs`.
#[utoipa::path(
    get,
    path = "/api/v1/errors",
    tag = "bug-reporting",
    params(("limit" = Option<u64>, Query, description = "Max errors to return (default 20, max 100)")),
    responses((status = 200, description = "Deduplicated error list"))
)]
pub async fn list_errors(
    State(_state): State<Arc<DaemonState>>,
    Query(query): Query<ErrorsQuery>,
) -> Json<ErrorsResponse> {
    let limit = query.limit.unwrap_or(20).min(100) as usize;
    let errors = super::bug_report::aggregate_errors(limit);
    let summaries: Vec<ErrorSummary> = errors
        .iter()
        .map(|e| ErrorSummary {
            fingerprint: e.record.fingerprint.clone(),
            crate_target: e.record.crate_target.clone(),
            crate_version: e.record.crate_version.clone(),
            summary: e.record.summary(),
            occurrences: e.occurrences,
            timestamp_secs: e.record.timestamp_secs,
        })
        .collect();
    let total = summaries.len();
    Json(ErrorsResponse {
        errors: summaries,
        total,
        limit,
    })
}

/// JSON body for `POST /api/v1/report-bug`.
///
/// Why: a local request struct with `utoipa::ToSchema` lets the handler slot
///      into the utoipa-axum dispatch without special traits on the shared type.
/// What: the fingerprint and confirm flag; mirrors `bug_report::types::ReportBugRequest`.
/// Test: `report_bug_no_confirm_returns_preview` in `api_tests.rs`.
#[derive(serde::Deserialize, utoipa::ToSchema)]
pub struct ReportBugApiRequest {
    /// SHA-256 hex fingerprint (64 chars) from `GET /api/v1/errors`.
    pub fingerprint: String,
    /// Must be `true` to actually file; `false` or absent → preview only.
    #[serde(default)]
    pub confirm: bool,
}

/// Build a [`BugReportPreview`] from a scrubbed [`IssuePreview`].
///
/// Why: both the `confirm:false` path and the rate-limited path must return the
///      same preview shape so HTTP clients can inspect before (or after a blocked)
///      filing.
/// What: maps `IssuePreview` fields to [`BugReportPreview`] (the wire type).
/// Test: exercised transitively by `report_bug_no_confirm_includes_preview`.
fn to_wire_preview(p: &super::bug_report::IssuePreview) -> BugReportPreview {
    BugReportPreview {
        title: p.title.clone(),
        body: p.body.clone(),
        labels: p.labels.clone(),
        scrub_changes: p
            .scrub_changes
            .iter()
            .map(|c| ScrubChangeSummary {
                pattern: c.pattern.to_string(),
                hint: c.hint.to_string(),
            })
            .collect(),
    }
}

/// `POST /api/v1/report-bug` — file or preview a bug report via HTTP.
///
/// Why: sub-agents spawned by Claude Code's Agent tool do not inherit MCP
///      connections; they need an HTTP fallback to file bug reports. This
///      endpoint mirrors the `report_bug` MCP tool's consent gate.
///
/// Fixes implemented here:
///   - Fix 1 (#498, P0): token resolved via the full chain (`ResolvedProvider`
///     — PAT env → token file → GitHub App → NoToken) instead of the narrower
///     `EnvFileTokenProvider`, so the GitHub App path is now reachable.
///   - Fix 2 (P1): `confirm:false` now includes the scrubbed preview
///     (title/body/labels/scrub_changes) in the response so HTTP clients can
///     inspect before consenting.
///   - Fix 3 (P2): the `RateLimitGuard` is checked before filing; a blocked
///     call returns `{ filed:false, rate_limited:true, note:… }` without
///     hitting the GitHub API.  After a successful filing `record_filed` is
///     called.  Rate-limit state-file failures are non-fatal (logged only).
///
/// What: when `confirm:false` (or absent), returns a preview-only response
///       (`filed:false`, `preview:{…}`). When `confirm:true`, resolves the
///       token via the full provider chain, checks the rate-limit guard, then
///       calls the GitHub filing client. A missing token returns
///       `filed:false` with an actionable note; a blocked rate-limit returns
///       `filed:false, rate_limited:true`; a missing fingerprint returns
///       `filed:false` with a "not found" note.
/// Test: `report_bug_no_confirm_includes_preview`,
///       `report_bug_rate_limited_returns_not_filed` in `api_tests.rs`.
#[utoipa::path(
    post,
    path = "/api/v1/report-bug",
    tag = "bug-reporting",
    request_body = ReportBugApiRequest,
    responses(
        (status = 200, description = "Filing result or graceful failure; always 200"),
    )
)]
pub async fn report_bug_http(
    State(_state): State<Arc<DaemonState>>,
    Json(body): Json<ReportBugApiRequest>,
) -> Json<ReportBugHttpResponse> {
    // Load errors and find the requested fingerprint.
    let errors = super::bug_report::aggregate_errors(500);
    let found = errors
        .into_iter()
        .find(|e| e.record.fingerprint == body.fingerprint);

    let Some(agg) = found else {
        return Json(ReportBugHttpResponse {
            filed: false,
            deduped: None,
            issue_url: None,
            issue_number: None,
            note: Some(format!(
                "fingerprint `{}` not found in local error stores; \
                 run GET /api/v1/errors to see available fingerprints",
                body.fingerprint
            )),
            preview: None,
            rate_limited: None,
        });
    };

    let preview = super::bug_report::build_preview(&agg);

    // Fix 2 (P1): include scrubbed preview in confirm:false response.
    if !body.confirm {
        return Json(ReportBugHttpResponse {
            filed: false,
            deduped: None,
            issue_url: None,
            issue_number: None,
            note: Some("confirm:false — preview only. POST with confirm:true to file.".to_string()),
            preview: Some(to_wire_preview(&preview)),
            rate_limited: None,
        });
    }

    // Fix 3 (P2): check the rate-limit guard before calling GitHub.
    let guard = super::bug_report::RateLimitGuard::production();
    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let rl_decision = guard.check(&body.fingerprint, now_secs);
    if !rl_decision.is_allowed() {
        return Json(ReportBugHttpResponse {
            filed: false,
            deduped: None,
            issue_url: None,
            issue_number: None,
            note: Some(rl_decision.block_reason()),
            preview: None,
            rate_limited: Some(true),
        });
    }

    // Fix 1 (P0): use the full resolution chain — PAT → file → GitHub App → NoToken.
    let fingerprint = body.fingerprint.clone();
    let provider = super::bug_report::ResolvedProvider;
    let result =
        tokio::task::spawn_blocking(move || super::bug_report::file_issue(&preview, &provider))
            .await;

    match result {
        Ok(Ok(filing)) => {
            // Fix 3 (P2): record the successful filing in the rate-limit store.
            // State-file failures are non-fatal — log and allow.
            guard.record_filed(&fingerprint, now_secs);
            Json(ReportBugHttpResponse {
                filed: true,
                deduped: Some(filing.deduped),
                issue_url: Some(filing.issue_url),
                issue_number: Some(filing.issue_number),
                note: None,
                preview: None,
                rate_limited: None,
            })
        }
        Ok(Err(super::bug_report::GithubFilingError::NoToken)) => Json(ReportBugHttpResponse {
            filed: false,
            deduped: None,
            issue_url: None,
            issue_number: None,
            note: Some(super::bug_report::GithubFilingError::NoToken.to_string()),
            preview: None,
            rate_limited: None,
        }),
        Ok(Err(e)) => Json(ReportBugHttpResponse {
            filed: false,
            deduped: None,
            issue_url: None,
            issue_number: None,
            note: Some(format!("GitHub filing failed: {e}")),
            preview: None,
            rate_limited: None,
        }),
        Err(e) => Json(ReportBugHttpResponse {
            filed: false,
            deduped: None,
            issue_url: None,
            issue_number: None,
            note: Some(format!("internal error: {e}")),
            preview: None,
            rate_limited: None,
        }),
    }
}

/// Parse a UUID string into a `SessionId`, mapping failure to a `400`-mapped
/// [`DaemonError::InvalidRequest`].
fn parse_id(raw: &str) -> Result<SessionId, DaemonError> {
    uuid::Uuid::parse_str(raw)
        .map(SessionId)
        .map_err(|_| DaemonError::InvalidRequest(format!("malformed session id: {raw}")))
}

/// Handler unit tests.
///
/// Why: the suite is large enough that keeping it in `api.rs` pushed the file
/// past a maintainable size; a `#[path]`-linked sibling keeps the handler
/// surface readable while the tests still see the private helpers via
/// `super::*`.
/// Test: this *is* the test module.
#[cfg(test)]
#[path = "api_tests.rs"]
mod tests;
