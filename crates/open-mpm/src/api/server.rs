//! HTTP API server (#151 phase-2).
//!
//! Why: Exposes `PmResponse` over HTTP so external clients — the `ompm` thin
//! CLI, a future GUI, CI pipelines — can submit workflow tasks and poll for
//! results without spawning the orchestrator CLI themselves. Keeping the
//! server in-process with the workflow engine avoids double-spawn overhead
//! and shares the canonical response envelope.
//! What: Axum-based HTTP API with four routes:
//!   POST /api/task     → kick off a background workflow run, return
//!                        `{ "id": <uuid>, "status": "running" }`.
//!   GET  /api/task/:id → return the cached `PmResponse` (or a `running`
//!                        placeholder if the background task hasn't finished).
//!   GET  /api/tasks    → return up to 20 recent responses.
//!   GET  /api/health   → return `{ "status": "ok", "version": ... }`.
//! Task execution: the server spawns `open-mpm --workflow ... --json
//! --task <text> [--out-dir ...]` as a child process and parses its stdout
//! as a `PmResponse`. This reuses the entire workflow setup path (build
//! counter, tracing, run_id, registries) without duplicating it.
//! Test: `health_returns_ok_and_version`, `submit_task_returns_running`,
//! `unknown_task_id_is_error`.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Result;
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{Method, Request, StatusCode, header},
    middleware::{self, Next},
    response::{
        IntoResponse, Response,
        sse::{Event as SseEvent, KeepAlive, Sse},
    },
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use std::convert::Infallible;
use std::time::Duration;
use tokio::sync::Mutex;
use tokio_stream::Stream;
use tower_http::compression::CompressionLayer;
use tower_http::cors::{Any, CorsLayer};
use tower_http::trace::TraceLayer;

use crate::api::types::{PhaseProgress, PmResponse, PmStatus};
use crate::ctrl_session::{
    Session as CtrlSession, SessionStatus as CtrlSessionStatus, SessionStore,
};
use crate::events::{self, EVENT_LINE_PREFIX, Event};
use crate::recap::{self, RecapConfig, RecapPhase, RecapTask, RecapTracker};
use crate::registry::{ProjectEntry, ProjectRegistry, discover_active_projects};
use crate::tm::TmManager;
use crate::tm::project::{AdapterType, TmProject};
use uuid::Uuid;

// -------- embedded web UI ----------

/// Embed the Vite-built `ui/dist/` directory directly into the binary.
///
/// Why: Shipping a single self-contained binary simplifies deployment — users
/// run `open-mpm --serve` and immediately get both the REST API and the web UI
/// without managing a separate static-file server or CDN.
/// What: `rust-embed` walks `ui/dist/` at compile time and bakes every file
/// into the binary. At runtime `UiAssets::get(path)` returns the bytes.
/// Test: After `cargo build && ./target/debug/open-mpm --serve --port 7654 &`,
/// `curl -s http://localhost:7654/ | grep -c 'app'` should return > 0.
#[derive(rust_embed::RustEmbed)]
#[folder = "ui/dist/"]
struct UiAssets;

/// Serve `index.html` for the root path.
///
/// Why: SPA entry point — the browser loads this and the JS router takes over.
/// What: Fetches `index.html` from the embedded bundle and returns it with the
/// correct `text/html` content-type. Returns 404 with a plain-text message
/// when the UI was not compiled (i.e. `pnpm build` was skipped).
/// Test: GET `/` must return 200 with HTML containing the app mount point.
async fn serve_index() -> impl IntoResponse {
    match UiAssets::get("index.html") {
        Some(f) => {
            let mime = mime_guess::from_path("index.html").first_or_octet_stream();
            // `index.html` references content-hashed asset URLs. Use
            // `no-store` (not just `no-cache`) so browsers never serve a
            // stale entry point pointing at asset hashes that no longer exist
            // after a redeploy — the root cause of persistent blank-screen
            // regressions when the Rust binary is rebuilt with a new UI dist.
            (
                [
                    (header::CONTENT_TYPE, mime.as_ref().to_owned()),
                    (header::CACHE_CONTROL, "no-store".to_owned()),
                ],
                f.data.into_owned(),
            )
                .into_response()
        }
        None => (StatusCode::NOT_FOUND, "UI not built").into_response(),
    }
}

/// Serve a static asset by path, falling back to `index.html` for unknown
/// paths so client-side routing works correctly.
///
/// Why: Vite emits hashed assets under `assets/`; the SPA also uses
/// client-side routing, so any unrecognised path should return `index.html`
/// and let the JS router resolve it — the standard SPA fallback pattern.
/// What: Looks up `path` in the embedded bundle; if found, returns the file
/// with a guessed MIME type; otherwise delegates to `serve_index`.
/// Test: GET `/assets/index-<hash>.js` should return 200 with
/// `content-type: text/javascript`. GET `/unknown-route` should return the
/// `index.html` content.
async fn serve_asset(
    axum::extract::Path(path): axum::extract::Path<String>,
) -> axum::response::Response {
    // Axum's `/*path` catch-all includes the leading slash; strip it so the
    // path matches the keys stored by rust-embed (e.g. "assets/index.js").
    let path = path.trim_start_matches('/');
    match UiAssets::get(path) {
        Some(f) => {
            let mime = mime_guess::from_path(path).first_or_octet_stream();
            // Vite emits content-hashed filenames under `assets/` (e.g.
            // `assets/index-abc123.js`), so the bytes at a given URL never
            // change — safe to cache forever. Other top-level assets (favicon,
            // manifest, robots.txt) keep the default no-explicit-cache policy
            // so they pick up updates on the next request.
            if path.starts_with("assets/") {
                (
                    [
                        (header::CONTENT_TYPE, mime.as_ref().to_owned()),
                        (
                            header::CACHE_CONTROL,
                            "public, max-age=31536000, immutable".to_owned(),
                        ),
                    ],
                    f.data.into_owned(),
                )
                    .into_response()
            } else {
                (
                    [(header::CONTENT_TYPE, mime.as_ref().to_owned())],
                    f.data.into_owned(),
                )
                    .into_response()
            }
        }
        None => serve_index().await.into_response(),
    }
}

/// Maximum number of terminal responses retained in memory.
const MAX_RETAINED: usize = 20;

/// Server configuration. (#181)
///
/// Why: Bearer-token auth must be configurable per-launch so users can run
/// the server unauthenticated for local-only dev or token-protected when
/// exposing the bound port over a LAN. Keeping this as a struct (instead of
/// extra bare args to `serve`) gives us a clean place to grow more knobs
/// (TLS, allowed origins, …) without breaking callers.
/// What: `port` is the TCP port to bind on `0.0.0.0`. `token`, when `Some`,
/// makes every `/api/*` route (except `GET /api/health`) require an
/// `Authorization: Bearer <token>` header that exactly matches `token`.
/// Test: `auth_middleware_rejects_missing_token`, `auth_middleware_allows_health`.
#[derive(Clone, Debug)]
pub struct ApiConfig {
    pub port: u16,
    pub token: Option<String>,
}

impl ApiConfig {
    /// Convenience constructor mirroring the previous bare-port API.
    #[allow(dead_code)]
    pub fn unauthenticated(port: u16) -> Self {
        Self { port, token: None }
    }
}

/// Wrapper used by the auth middleware so axum can extract the optional
/// configured token from request state.
#[derive(Clone)]
struct AuthState {
    token: String,
}

/// Bearer-token authentication middleware. (#181)
///
/// Why: The server binds `0.0.0.0`, so any process on the LAN can reach the
/// REST API + UI. When the operator sets a token, we reject requests that
/// don't present it. We deliberately exempt `GET /api/health` so probes from
/// load balancers or healthchecks don't need credentials, and exempt the
/// embedded UI's static assets so a browser can load `index.html` and obtain
/// the token via `/api/config` before issuing authenticated requests.
/// What: For requests under `/api/*` (other than `/api/health`), checks
/// `Authorization: Bearer <token>` and returns 401 JSON
/// `{"error":"unauthorized"}` on mismatch.
/// Test: `auth_middleware_rejects_missing_token`,
/// `auth_middleware_accepts_valid_token`, `auth_middleware_allows_health`.
async fn auth_middleware(
    State(auth): State<AuthState>,
    req: Request<axum::body::Body>,
    next: Next,
) -> Response {
    let path = req.uri().path();

    // Public endpoints — never require auth:
    //   - GET /api/health  (health probes)
    //   - GET /api/config  (UI bootstrap: tells client whether auth is needed)
    //   - any non-/api path (UI static assets at "/" and "/*path")
    // #192 Phase B: `/api/events` is exempt from Bearer auth because the
    // browser EventSource API cannot attach custom Authorization headers.
    // Auth-sensitive deployments should front the server with a reverse proxy
    // and gate `/api/events` there (e.g. mTLS or cookie-based auth) since the
    // event stream itself contains only telemetry, not actionable controls.
    if path == "/api/health"
        || path == "/api/config"
        || path == "/api/events"
        || !path.starts_with("/api/")
    {
        return next.run(req).await;
    }

    let header_val = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
        .map(str::trim);

    match header_val {
        Some(t) if t == auth.token => next.run(req).await,
        _ => (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({ "error": "unauthorized" })),
        )
            .into_response(),
    }
}

/// Body returned by `GET /api/config`. (#181)
///
/// Why: The browser-served UI needs to know whether to attach a bearer token
/// to its requests. Rather than embedding the token into HTML (which would
/// leak via view-source), we publish only a boolean flag. The token itself
/// is provided to the user out-of-band and pasted into the UI.
#[derive(Debug, Clone, Serialize)]
struct ApiClientConfig {
    auth_required: bool,
}

/// Shared server state.
///
/// Why: Background workflow tasks need somewhere to deposit their results so
/// polling handlers can read them. A simple `HashMap` behind a `Mutex` is
/// ample for a single-node dev server; revisit with sled/redb if persistence
/// becomes a requirement.
#[derive(Clone)]
pub struct AppState {
    inner: Arc<Mutex<TaskStore>>,
    /// #187: Optional in-memory TF-IDF index over project documentation.
    /// `None` when the server starts without a docs corpus (tests, etc.).
    docs_index: Option<Arc<crate::docs_index::DocsIndex>>,
    /// #371: Per-session task counter driving recap generation. Wrapped in
    /// `Arc<Mutex>` so background `run_task` futures can tick the counter
    /// without taking ownership of the tracker.
    recap_tracker: Arc<Mutex<RecapTracker>>,
    /// #450: Optional TM (tmux) manager for live session management. `None`
    /// when tmux is not available on the host or initialization failed; the
    /// `/api/tm/*` routes return 503 in that case so the UI can degrade
    /// gracefully without crashing the server.
    tm_manager: Option<Arc<Mutex<TmManager>>>,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            inner: Arc::default(),
            docs_index: None,
            recap_tracker: Arc::new(Mutex::new(RecapTracker::new(RecapConfig::default()))),
            tm_manager: None,
        }
    }
}

/// Filesystem location for runtime state (recaps, tasks.json, etc.).
///
/// Why: Centralised so production code, tests, and `load_recap` agree on the
/// directory. Mirrors `tasks_persistence_path()` which is hard-coded to the
/// same root.
fn state_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(".open-mpm/state")
}

impl AppState {
    /// #187: Construct an `AppState` with a docs index attached.
    ///
    /// Why: `--api` mode builds the index at startup and threads it into
    /// the server so `GET /api/docs/search` can query it. Tests use the
    /// `Default::default` path (no index) and the search route falls back
    /// to "not ready".
    pub fn with_docs_index(index: Arc<crate::docs_index::DocsIndex>) -> Self {
        Self {
            inner: Arc::default(),
            docs_index: Some(index),
            recap_tracker: Arc::new(Mutex::new(RecapTracker::new(RecapConfig::default()))),
            tm_manager: None,
        }
    }

    /// #212: Construct an `AppState` pre-populated from `tasks.json` if present.
    ///
    /// Why: When the launchd-managed API server is restarted (deploy, reboot,
    /// crash), in-flight task results held only in `Arc<Mutex<HashMap>>` are
    /// lost — clients polling `GET /api/task/:id` see a 404 forever. Loading
    /// the persisted snapshot at startup lets the UI continue showing prior
    /// task history across restarts.
    /// What: Reads `.open-mpm/state/tasks.json` (relative to cwd) and seeds
    /// the in-memory map. Missing/unreadable file is non-fatal — we start
    /// empty. Subsequent `upsert` calls write the file atomically (temp +
    /// rename) so a crash mid-write can't corrupt the snapshot.
    /// Test: `app_state_persists_and_reloads_tasks` — upsert a task, drop
    /// the AppState, call `with_docs_index_and_persistence`, assert the
    /// task is present.
    pub async fn with_persistence(index: Option<Arc<crate::docs_index::DocsIndex>>) -> Self {
        let store = load_persisted_tasks().await.unwrap_or_default();
        // #450: Best-effort TmManager init. tmux may not be installed (CI,
        // minimal Docker images); in that case TmManager::new fails and the
        // `/api/tm/*` routes return 503 rather than crashing the server.
        let tm_manager = TmManager::new(&state_dir())
            .map(|m| Arc::new(Mutex::new(m)))
            .map_err(|e| {
                tracing::warn!(error = %e, "TmManager init failed; /api/tm/* will return 503");
                e
            })
            .ok();
        Self {
            inner: Arc::new(Mutex::new(store)),
            docs_index: index,
            recap_tracker: Arc::new(Mutex::new(RecapTracker::new(RecapConfig::default()))),
            tm_manager,
        }
    }
}

/// Path where the task snapshot is persisted.
///
/// Why: Centralized so production code and tests agree on location.
/// Located under `.open-mpm/state/` to colocate with other runtime state
/// (build.json, processes.json) and stay outside committed config.
fn tasks_persistence_path() -> std::path::PathBuf {
    std::path::PathBuf::from(".open-mpm/state/tasks.json")
}

/// Load persisted tasks from disk, if the file exists and is valid JSON.
///
/// Why: Non-fatal — a missing or malformed file should not prevent the
/// server from starting; we just begin with an empty store.
/// What: Reads the JSON file as `HashMap<String, PmResponse>`, then
/// reconstructs a `TaskStore` (responses + insertion order). Order is
/// rebuilt by sorting keys; the exact original order is not preserved
/// across restarts but newest-first listing remains stable thereafter.
/// Test: Persist a known map, call this fn, assert keys round-trip.
async fn load_persisted_tasks() -> Option<TaskStore> {
    let path = tasks_persistence_path();
    let bytes = tokio::fs::read(&path).await.ok()?;
    let responses: HashMap<String, PmResponse> = serde_json::from_slice(&bytes).ok()?;
    let mut order: Vec<String> = responses.keys().cloned().collect();
    order.sort(); // deterministic, even if not original order
    Some(TaskStore { responses, order })
}

/// Persist the given task map to disk atomically.
///
/// Why: A naive `write` to the live file risks readers (or a crash) seeing
/// a half-written file. Writing to a sibling temp path and renaming is
/// atomic on the same filesystem on POSIX, so observers either see the old
/// snapshot or the new one — never a corrupt one.
/// What: Ensures the parent directory exists, writes JSON to
/// `tasks.json.tmp`, then `rename`s onto `tasks.json`. Logs (but does not
/// fail) on I/O errors — losing a snapshot is preferable to crashing the
/// running server.
/// Test: Call with a sample map, assert the target file parses back to the
/// same map; force the parent dir to be missing and assert no panic.
async fn persist_tasks(responses: &HashMap<String, PmResponse>) {
    let path = tasks_persistence_path();
    let tmp = path.with_extension("json.tmp");
    if let Some(parent) = path.parent()
        && let Err(e) = tokio::fs::create_dir_all(parent).await
    {
        tracing::warn!(?e, "failed to create state dir for tasks.json");
        return;
    }
    let json = match serde_json::to_vec(responses) {
        Ok(j) => j,
        Err(e) => {
            tracing::warn!(?e, "failed to serialize tasks for persistence");
            return;
        }
    };
    if let Err(e) = tokio::fs::write(&tmp, &json).await {
        tracing::warn!(?e, "failed to write tasks.json.tmp");
        return;
    }
    if let Err(e) = tokio::fs::rename(&tmp, &path).await {
        tracing::warn!(?e, "failed to rename tasks.json.tmp -> tasks.json");
    }
}

#[derive(Default)]
struct TaskStore {
    /// task_id -> response (may be a `running` placeholder).
    responses: HashMap<String, PmResponse>,
    /// Insertion order for eviction; newest last.
    order: Vec<String>,
}

impl AppState {
    /// Insert or update the response for `id`. When a response transitions
    /// to terminal state we record its position for LRU trimming.
    async fn upsert(&self, id: String, resp: PmResponse) {
        let snapshot = {
            let mut store = self.inner.lock().await;
            let was_absent = !store.responses.contains_key(&id);
            store.responses.insert(id.clone(), resp);
            if was_absent {
                store.order.push(id);
            }
            // Trim to MAX_RETAINED by dropping the oldest entries.
            while store.order.len() > MAX_RETAINED {
                let old = store.order.remove(0);
                store.responses.remove(&old);
            }
            store.responses.clone()
        };
        // #212: Persist outside the lock — disk I/O shouldn't block readers.
        persist_tasks(&snapshot).await;
    }

    async fn get(&self, id: &str) -> Option<PmResponse> {
        let store = self.inner.lock().await;
        store.responses.get(id).cloned()
    }

    /// #149: Append (or replace by `name`) a phase progress event into the
    /// stored response so the polling client sees real-time updates.
    ///
    /// Why: While a workflow runs in a child subprocess, the server reads the
    /// child's stderr for `__OMPM_PROGRESS__ {…}` lines and forwards each one
    /// here. The Tauri UI poller then renders a live phase timeline without
    /// waiting for the workflow to finish.
    /// What: Looks up the response by `id`; if a progress entry with the same
    /// `name` already exists it's overwritten (so `running → done` collapses
    /// into the latest state); otherwise it's appended.
    /// Test: Unit-tested via `app_state_append_progress_replaces_by_name`.
    async fn append_progress(&self, id: &str, ev: PhaseProgress) {
        let mut store = self.inner.lock().await;
        if let Some(resp) = store.responses.get_mut(id) {
            if let Some(slot) = resp.phases_completed.iter_mut().find(|p| p.name == ev.name) {
                *slot = ev;
            } else {
                resp.phases_completed.push(ev);
            }
        }
    }

    async fn list(&self) -> Vec<PmResponse> {
        let store = self.inner.lock().await;
        // Newest first.
        store
            .order
            .iter()
            .rev()
            .filter_map(|id| store.responses.get(id).cloned())
            .collect()
    }

    /// Clear all tasks and return the count of tasks that were cancelled.
    ///
    /// Why: `POST /api/clear-context` lets the UI offer a one-click "start
    /// fresh" action without restarting the server. Callers that had running
    /// sessions receive a `SessionCancelled` event so SSE subscribers can
    /// update their UI state before the page reloads.
    /// What: Drains the task store, emits `SessionCancelled` for every task
    /// that was still in `Running` state, and returns the cancellation count.
    /// Test: Submit a task (status=running), call clear_tasks, assert list
    /// returns empty and the count matches.
    async fn clear_tasks(&self) -> usize {
        let mut store = self.inner.lock().await;
        let running_ids: Vec<String> = store
            .responses
            .iter()
            .filter(|(_, r)| r.status == PmStatus::Running)
            .map(|(id, _)| id.clone())
            .collect();
        let cancelled = running_ids.len();
        for id in running_ids {
            events::publish(Event::SessionCancelled { session_id: id });
        }
        store.responses.clear();
        store.order.clear();
        cancelled
    }
}

/// Request body for `POST /api/task`.
#[derive(Debug, Clone, Deserialize)]
pub struct TaskRequest {
    pub task: String,
    #[serde(default)]
    pub workflow: Option<String>,
    #[serde(default)]
    pub out_dir: Option<String>,
    #[serde(default)]
    pub task_file: Option<String>,
    /// #151 phase-4: when set, dispatch to a single sub-agent instead of a
    /// full workflow (via `open-mpm --direct <agent>`).
    #[serde(default)]
    pub agent: Option<String>,
    /// Tauri GUI: when set, run the spawned `open-mpm` subprocess with this
    /// directory as its working directory so project-scoped PMs can operate
    /// on a specific project without the caller having to `cd` first.
    ///
    /// Why: The desktop chat interface allows users to register multiple
    /// project paths and chat with a per-project PM; each task must execute
    /// in that project's root so `.open-mpm/`, file paths, and shell tools
    /// resolve relative to the correct codebase.
    /// What: Optional absolute path. When present and pointing at a directory,
    /// `run_task` sets it as the child process's `current_dir`.
    /// Test: Submit a task with `project_path: "/tmp"`, assert the spawned
    /// subprocess inherits that cwd (observable via child tracing or stdout
    /// from a task that prints `std::env::current_dir()`).
    #[serde(default)]
    pub project_path: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct TaskSubmittedBody {
    id: String,
    status: &'static str,
}

#[derive(Debug, Clone, Serialize)]
struct HealthBody {
    status: &'static str,
    version: &'static str,
}

/// Build the axum router.
///
/// Why: A permissive CORS layer lets the dual-mode UI (`pnpm dev` browser
/// build) talk to the API directly without a Vite proxy when desired, and
/// also unblocks `curl`/Postman from any origin during local development.
/// In production-style deploys the server is fronted by a same-origin
/// reverse proxy (or the Tauri webview) so the wide-open policy is
/// acceptable for our local-dev threat model. Tighten if/when we expose the
/// API publicly.
/// What: Wraps the route table in a `CorsLayer` that allows any origin,
/// method, and header.
/// Test: `curl -i -H 'Origin: http://localhost:5173' http://localhost:7654/api/health`
/// returns `access-control-allow-origin: *`.
//
// Used by unit tests (see `test_router` below) and kept `pub` for future
// callers that want an unauthenticated router without going through
// `ApiConfig`. Note: `#[allow(dead_code)]` is required because this is a
// `bin` crate — `pub` only suppresses dead-code warnings for library crates
// exposing items as public API, not for binaries with no external consumers.
#[allow(dead_code)]
pub fn build_router(state: AppState) -> Router {
    build_router_with_config(state, None)
}

/// Build the axum router, optionally with bearer-token auth. (#181)
///
/// Why: Splitting this from `build_router` keeps the call-sites that don't
/// care about auth (most tests) ergonomic while letting `serve()` thread an
/// `ApiConfig` through. The auth layer is only attached when `token` is
/// `Some` so the unauthenticated path remains identical to before.
/// What: Builds the same routes as `build_router`, adds `/api/config` for UI
/// bootstrap, and conditionally wraps `/api/*` with `auth_middleware`.
/// Test: `auth_middleware_*` tests cover both with-token and without-token
/// branches via `oneshot` requests.
pub fn build_router_with_config(state: AppState, token: Option<String>) -> Router {
    // CORS: keep `allow_origin(Any)` because the server is reachable over
    // Tailscale / LAN from the operator's other devices and from the Tauri
    // webview, and we don't know those origins ahead of time. We tighten the
    // method/header allowlists to the minimum the API actually uses so a
    // hostile LAN page can't, e.g., issue DELETE/PUT preflight or smuggle
    // exotic headers. Bearer-token auth (when configured) remains the real
    // gate on `POST /api/task` — CORS is defence-in-depth, not the lock.
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods([Method::GET, Method::POST, Method::OPTIONS])
        .allow_headers([header::AUTHORIZATION, header::CONTENT_TYPE]);

    let auth_required = token.is_some();
    let config_route = get(move || async move { Json(ApiClientConfig { auth_required }) });

    let mut router = Router::new()
        .route("/api/task", post(submit_task))
        .route("/api/task/{id}", get(get_task))
        .route("/api/tasks", get(list_tasks))
        .route("/api/clear-context", post(clear_context))
        .route("/api/health", get(health))
        .route("/api/config", config_route)
        .route("/api/docs/search", get(docs_search))
        .route("/api/projects", get(list_projects).post(connect_project))
        // #451: per-project TOML config lookup (mirrors the on-disk shape of
        // `.open-mpm/projects/<name>.toml` rather than the global registry).
        .route("/api/projects/{name}", get(get_project_config))
        // #407: agent + session listing for the web UI / CLI clients.
        .route("/api/agents", get(list_agents_route))
        .route("/api/sessions", get(list_sessions_route))
        // #371: session recap retrieval
        .route("/api/sessions/{id}/recap", get(get_session_recap))
        // #406: CTRL sessions (interactive REPL sessions, optional worktree).
        .route(
            "/api/ctrl/sessions",
            get(list_ctrl_sessions_handler).post(create_ctrl_session_handler),
        )
        .route(
            "/api/ctrl/sessions/{id}",
            get(get_ctrl_session_handler).delete(terminate_ctrl_session_handler),
        )
        .route(
            "/api/ctrl/sessions/{id}/attach",
            post(attach_ctrl_session_handler),
        )
        // #450: TM (tmux) session management — live tmux state, lifecycle,
        // and I/O for the web UI. All routes return 503 if TmManager isn't
        // available (tmux missing or init failed).
        .route(
            "/api/tm/sessions",
            get(tm_list_sessions).post(tm_create_session),
        )
        .route(
            "/api/tm/sessions/{name}",
            axum::routing::delete(tm_kill_session),
        )
        .route("/api/tm/sessions/{name}/pause", post(tm_pause_session))
        .route("/api/tm/sessions/{name}/resume", post(tm_resume_session))
        .route("/api/tm/sessions/{name}/send", post(tm_send_message))
        .route("/api/tm/sessions/{name}/pane", get(tm_capture_pane))
        // Favorite toggle — POST sets favorite=true, DELETE sets favorite=false.
        // Used by the WebUI star button (#450 spec refinement).
        .route(
            "/api/tm/sessions/{name}/favorite",
            post(tm_set_favorite).delete(tm_unset_favorite),
        )
        // `tell` routing — `POST /api/tm/tell` with `{project, message,
        // harness?}`. Routes through the project's declared default_harness
        // (or the explicit `harness`) to the active session for that
        // (project, harness) pair.
        .route("/api/tm/tell", post(tm_tell))
        // #192 Phase B: SSE event stream — replaces 2s stderr polling.
        .route("/api/events", get(events_handler))
        // #460: unified rpc.discover from linked ServiceDescriptor impls.
        // JSON-RPC POST endpoint that returns the merged OpenRPC manifest
        // covering every in-process MCP service (trusty-memory linked,
        // trusty-search mirrored — see src/rpc/mod.rs).
        .route("/rpc", post(crate::rpc::rpc_handler))
        // Web UI: root serves index.html; all other non-API paths serve a
        // static asset from the embedded bundle, falling back to index.html
        // for client-side routing (SPA pattern).
        .route("/", get(serve_index))
        .route("/{*path}", get(serve_asset))
        .with_state(state);

    if let Some(tok) = token {
        let auth_state = AuthState { token: tok };
        router = router.layer(middleware::from_fn_with_state(auth_state, auth_middleware));
    }

    router
        .layer(cors)
        .layer(CompressionLayer::new())
        .layer(TraceLayer::new_for_http())
}

/// Serve the HTTP API and embedded web UI on `0.0.0.0:<port>` until killed.
///
/// Why: Single-binary deployment — one process handles both API requests and
/// serves the web frontend so users don't need a separate static-file server.
/// What: Binds `0.0.0.0:<port>`, mounts the Axum router (API + embedded UI),
/// prints startup URLs to stdout, then runs until the process is killed.
/// Test: `cargo run -- --serve --port 7654 &` followed by
/// `curl -s http://localhost:7654/ | grep -c 'app'` should return > 0.
//
// Convenience entry point for callers that want to start an unauthenticated
// server without constructing an `ApiConfig`. Kept `pub` for tests and any
// future direct embedding of the server in another binary. Note:
// `#[allow(dead_code)]` is required because this is a `bin` crate — see the
// comment on `build_router` above for why `pub` alone isn't enough here.
#[allow(dead_code)]
pub async fn serve(port: u16) -> Result<()> {
    serve_with_config(ApiConfig::unauthenticated(port)).await
}

/// Serve the HTTP API and embedded web UI, honoring `ApiConfig`. (#181)
///
/// Why: Bound on `0.0.0.0`, the server is reachable from any host on the
/// LAN. We surface the LAN IP at startup so the operator can copy/paste the
/// URL to other devices, and we loudly warn if no auth token is configured
/// because the API can spawn arbitrary subprocesses.
/// What: Resolves the LAN IP via the standard "connect a UDP socket to
/// 8.8.8.8 and ask the kernel for the local addr" trick (no packet is
/// actually sent — UDP is connectionless), prints localhost + LAN URLs,
/// optionally warns on missing token, then serves until killed.
/// Test: Manual — run `--api --port 7654` with and without `--api-token`
/// and confirm the warning + LAN URL print as documented.
pub async fn serve_with_config(cfg: ApiConfig) -> Result<()> {
    // #364: Don't block server startup on docs indexing. For projects with
    // many docs files, `DocsIndex::build` can take 5–15s, which pushes us
    // past the Tauri sidecar's 20s health-check budget and the user sees
    // "API server did not become healthy within 20s". Spawn the build as
    // fire-and-forget instead — the server starts answering /api/health in
    // milliseconds; docs search degrades gracefully (returns "not ready")
    // until a future change wires the completed index back into AppState.
    let docs_dir = std::env::current_dir()
        .unwrap_or_else(|_| std::path::PathBuf::from("."))
        .join("docs");
    let docs_dir_for_log = docs_dir.clone();
    tokio::task::spawn(async move {
        let built =
            tokio::task::spawn_blocking(move || crate::docs_index::DocsIndex::build(&docs_dir))
                .await;
        match built {
            Ok(idx) if !idx.is_empty() => {
                println!(
                    "[open-mpm] Docs index: {} documents indexed from {} (background)",
                    idx.len(),
                    docs_dir_for_log.display()
                );
                // Note: the live AppState was constructed without this index.
                // Hot-swapping it in is a follow-up; for now docs search
                // remains "not ready" for the lifetime of this process when
                // the cwd has a docs/ corpus.
            }
            Ok(_) => {
                tracing::debug!(
                    docs_dir = %docs_dir_for_log.display(),
                    "docs index built but empty; skipping wire-up"
                );
            }
            Err(e) => {
                tracing::warn!(?e, "docs index build task panicked");
            }
        }
    });
    // #212: Load persisted task snapshot so restarts don't lose history.
    let state = AppState::with_persistence(None).await;
    let app = build_router_with_config(state, cfg.token.clone());
    let addr = std::net::SocketAddr::from(([0, 0, 0, 0], cfg.port));
    tracing::info!(%addr, "open-mpm api server listening");
    let listener = tokio::net::TcpListener::bind(addr).await?;

    let port = cfg.port;
    println!("[open-mpm] API:    http://localhost:{port}/api");
    println!("[open-mpm] Web UI: http://localhost:{port}/");
    if let Some(lan_ip) = detect_lan_ip() {
        println!("[open-mpm] Web UI (LAN): http://{lan_ip}:{port}/");
    }
    if cfg.token.is_none() {
        eprintln!("\u{26A0}  No API token set — server is unauthenticated");
    } else {
        eprintln!("[open-mpm] API token authentication: enabled");
    }

    axum::serve(listener, app).await?;
    Ok(())
}

/// Best-effort LAN IP detection. (#181)
///
/// Why: Printing `localhost` alone hides the URL another device on the same
/// Wi-Fi would use. The classic UDP trick — bind a UDP socket and "connect"
/// it to a public address — doesn't transmit anything but lets the OS pick
/// the outbound interface, giving us its IP. Any failure is non-fatal.
/// What: Returns `Some(IpAddr)` on success, `None` if no usable interface.
/// Test: Manually verified on macOS; in CI / unit tests we don't assert a
/// specific value (the function is best-effort).
fn detect_lan_ip() -> Option<std::net::IpAddr> {
    // Try the dependency-free UDP trick first.
    if let Ok(socket) = std::net::UdpSocket::bind("0.0.0.0:0")
        && socket.connect("8.8.8.8:80").is_ok()
        && let Ok(addr) = socket.local_addr()
    {
        let ip = addr.ip();
        if !ip.is_unspecified() && !ip.is_loopback() {
            return Some(ip);
        }
    }
    // Fallback: ask the local-ip-address crate.
    local_ip_address::local_ip().ok()
}

// -------- handlers ----------

async fn health() -> Json<HealthBody> {
    Json(HealthBody {
        status: "ok",
        version: env!("CARGO_PKG_VERSION"),
    })
}

async fn submit_task(
    State(state): State<AppState>,
    Json(req): Json<TaskRequest>,
) -> impl IntoResponse {
    let id = uuid::Uuid::new_v4().to_string();
    let placeholder = PmResponse::running(&id);
    state.upsert(id.clone(), placeholder).await;

    // #192 Phase B: announce the new session immediately on the event bus so
    // SSE subscribers (Sidebar task list, ChatView session bootstrap) update
    // before the child subprocess has even spawned.
    let project = req
        .project_path
        .clone()
        .unwrap_or_else(|| "(default)".to_string());
    events::publish(Event::SessionStarted {
        session_id: id.clone(),
        project,
    });

    // #199 / #203: Intent-based workflow inference. Three routes:
    //   - Conversational ("Hello", "Thanks") — in-process, no tools (~1-3s).
    //   - Research ("explain X", "what does Y do") — in-process tool-armed
    //     PM loop (`run_pm_task_with_session` falls through past its own
    //     Conversational fast-path since the input is Research, not
    //     Conversational). Lets `delegate_to_agent` fire when needed without
    //     paying for the prescriptive subprocess pipeline.
    //   - Implementation ("fix X", "build Y", slash commands) — full
    //     subprocess prescriptive workflow (~60-90s).
    use crate::intent::{IntentClass, classify_intent};

    // #208: CTRL management commands must short-circuit before intent
    // classification. Verbs like "add" and "remove" are in ACTION_VERBS
    // (correctly — "add authentication" is Implementation), but
    // "add project /path" needs CTRL's in-process tool registry
    // (AddProjectTool, RemoveProjectTool, …) which only exists in
    // `run_pm_task_with_session`. Routing these to the prescriptive
    // subprocess pipeline would lose access to those tools.
    let normalized = req.task.trim().to_lowercase();
    let is_ctrl_command = normalized.starts_with("add project ")
        || normalized.starts_with("remove project ")
        || normalized.starts_with("stop task ")
        || normalized.starts_with("set active ")
        || normalized == "list projects"
        || normalized == "list tasks";

    let intent = if is_ctrl_command {
        // Force the in-process Research path so CTRL tools are available.
        IntentClass::Research
    } else {
        classify_intent(&req.task)
    };

    match intent {
        IntentClass::Conversational | IntentClass::Research => {
            // Both run in-process via run_pm_task_with_session. The function
            // re-classifies internally: Conversational hits the no-tools fast
            // path; Research falls through to the tool-armed PM loop.
            let state_bg = state.clone();
            let id_bg = id.clone();
            let task_text = req.task.clone();
            let project_path = req
                .project_path
                .clone()
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
            let intent_label = match intent {
                IntentClass::Conversational => "conversational",
                IntentClass::Research => "research",
                IntentClass::Implementation => "implementation",
            };

            tokio::spawn(async move {
                let result = crate::ctrl::run_pm_task_with_session(
                    &project_path,
                    &task_text,
                    Some(id_bg.clone()),
                )
                .await;

                let resp = match result {
                    Ok(content) => {
                        let mut r = PmResponse::running(&id_bg);
                        r.response_type = crate::api::types::PmResponseType::AgentResponse;
                        r.status = PmStatus::Success;
                        r.narrative = content;
                        r
                    }
                    Err(e) => {
                        PmResponse::error(&id_bg, format!("{intent_label} handler failed: {e:#}"))
                    }
                };

                let status_str = match resp.status {
                    PmStatus::Success => "success",
                    PmStatus::Failed => "error",
                    PmStatus::Partial => "partial",
                    PmStatus::Running => "running",
                }
                .to_string();
                state_bg.upsert(id_bg.clone(), resp).await;
                maybe_emit_recap(&state_bg, &id_bg).await;
                events::publish(Event::SessionDone {
                    session_id: id_bg,
                    status: status_str,
                });
            });
        }
        IntentClass::Implementation => {
            // Spawn the workflow in the background. We reuse the current binary so
            // the child inherits full env/init (build counter, tracing, run_id).
            let state_bg = state.clone();
            let id_bg = id.clone();
            tokio::spawn(async move {
                let resp = run_task(&id_bg, req, state_bg.clone())
                    .await
                    .unwrap_or_else(|e| {
                        PmResponse::error(&id_bg, format!("server failed to run task: {e:#}"))
                    });
                let status_str = match resp.status {
                    PmStatus::Success => "success",
                    PmStatus::Failed => "error",
                    PmStatus::Partial => "partial",
                    PmStatus::Running => "running",
                }
                .to_string();
                state_bg.upsert(id_bg.clone(), resp).await;
                maybe_emit_recap(&state_bg, &id_bg).await;
                events::publish(Event::SessionDone {
                    session_id: id_bg,
                    status: status_str,
                });
            });
        }
    }

    (
        StatusCode::ACCEPTED,
        Json(TaskSubmittedBody {
            id,
            status: "running",
        }),
    )
}

async fn get_task(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<PmResponse>, (StatusCode, Json<serde_json::Value>)> {
    match state.get(&id).await {
        Some(r) => Ok(Json(r)),
        None => Err((
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": "unknown task id", "id": id })),
        )),
    }
}

/// `GET /api/sessions/:id/recap` — return the most recent stored recap for a
/// session, or 404 if none exists yet (#371).
///
/// Why: The GUI's `RecapPanel` polls this endpoint when a session loads so it
/// can render the latest summary + table without waiting for the next
/// `RecapGenerated` SSE event. 404 is the correct shape for "no recap yet"
/// since the resource genuinely doesn't exist on disk.
/// What: Reads `.open-mpm/state/recaps/{id}.json` via `recap::load_recap`.
/// Test: Save a recap then `curl /api/sessions/<id>/recap` → 200 + JSON;
/// missing session → 404.
async fn get_session_recap(
    Path(session_id): Path<String>,
    State(_state): State<AppState>,
) -> Response {
    let dir = state_dir();
    match recap::load_recap(&dir, &session_id) {
        Some(r) => Json(r).into_response(),
        None => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": "no recap for session", "id": session_id })),
        )
            .into_response(),
    }
}

async fn list_tasks(State(state): State<AppState>) -> Json<Vec<PmResponse>> {
    Json(state.list().await)
}

/// Response body for `POST /api/clear-context`.
#[derive(Debug, Clone, Serialize)]
struct ClearContextBody {
    cleared: bool,
    tasks_cancelled: usize,
}

/// `POST /api/clear-context` — wipe all in-memory task state.
///
/// Why: Provides a clean-slate action for the UI without restarting the
/// server process. Useful during development and when accumulated task
/// history causes the sidebar to become cluttered.
/// What: Clears the task store, emits `SessionCancelled` for running tasks,
/// and returns `{"cleared":true,"tasks_cancelled":<N>}`.
/// Test: POST /api/clear-context after submitting a task; assert 200, cleared
/// is true, then GET /api/tasks returns empty array.
async fn clear_context(State(state): State<AppState>) -> Json<ClearContextBody> {
    let tasks_cancelled = state.clear_tasks().await;
    Json(ClearContextBody {
        cleared: true,
        tasks_cancelled,
    })
}

/// Query string for `GET /api/docs/search`. (#187)
#[derive(Debug, Deserialize)]
struct DocsSearchQuery {
    q: Option<String>,
    /// Optional override for top-N (default 5, capped at 20).
    n: Option<usize>,
}

/// `GET /api/docs/search?q=<query>` — TF-IDF search over project docs. (#187)
///
/// Why: Lets the web UI add a "search docs" feature without spawning the
/// CLI or hitting an LLM. Backed by the same `DocsIndex` instance used by
/// the CTRL `search_docs` tool.
/// What: Returns `{"results":[{path,title,snippet,score}, …]}`. When the
/// index isn't attached (e.g. server started without `--api` wiring), the
/// route returns `{"results":[], "status":"no_index"}` with a 200 status so
/// clients can render a graceful empty state.
/// Test: `docs_search_returns_results_when_index_present`,
/// `docs_search_falls_back_when_index_missing`.
async fn docs_search(
    State(state): State<AppState>,
    axum::extract::Query(params): axum::extract::Query<DocsSearchQuery>,
) -> Json<serde_json::Value> {
    let q = params.q.unwrap_or_default();
    let n = params.n.unwrap_or(5).clamp(1, 20);
    let Some(idx) = state.docs_index.as_ref() else {
        return Json(serde_json::json!({
            "results": [],
            "status": "no_index",
        }));
    };
    if q.trim().is_empty() {
        return Json(serde_json::json!({
            "results": [],
            "status": "empty_query",
        }));
    }
    let hits = idx.search(&q, n);
    Json(serde_json::json!({
        "results": hits,
        "status": "ok",
    }))
}

/// Session summary for the projects API response. (#341)
///
/// Why: Lighter shape than the internal `SessionSummary`/`AdapterType`
/// structs — keeps the wire format flat and stable for the WebUI.
/// What: Mirrors the `name`, `adapter_type`, and `status` fields from
/// `crate::tm::project::SessionSummary` as plain strings.
/// Test: `list_projects_returns_empty_when_registry_unavailable` (smoke).
#[derive(Debug, Clone, Serialize)]
struct ProjectSessionSummary {
    name: String,
    adapter_type: String,
    status: String,
}

/// Response body shape for `GET /api/projects`. (#341)
///
/// Why: Joins the global `ProjectRegistry` (lifecycle + git/issue counts)
/// with the TM session registry (framework detection + live tmux sessions)
/// into a single record the UI can render without cross-referencing two
/// endpoints.
/// What: Each entry carries an id (the registry path string), human name,
/// path, optional git origin and issue/PR counts, an ISO-8601
/// `last_active` timestamp, optional framework label, and the list of
/// associated sessions.
/// Test: `list_projects_returns_array` exercises the empty/no-registry path.
#[derive(Debug, Clone, Serialize)]
struct ProjectResponse {
    id: String,
    name: String,
    path: String,
    git_origin: Option<String>,
    last_active: Option<String>,
    open_issues_count: Option<u32>,
    open_prs_count: Option<u32>,
    framework: Option<String>,
    sessions: Vec<ProjectSessionSummary>,
}

/// `GET /api/projects` — list known projects with session + GitHub metadata. (#341)
///
/// Why: The WebUI needs a single endpoint that surfaces every project the
/// user has touched, with enough context (origin, issue/PR counts, live
/// sessions) to navigate without shelling out. We default to "active" (last
/// 14 days OR has a tmux session) so the list stays focused; `?all=true`
/// returns every registry entry for power users / debugging.
/// What: Loads `ProjectRegistry` (best-effort; empty list on failure), reads
/// the TM project list from `.open-mpm/state/tm_sessions.json` directly
/// (we don't currently hold a `TmManager` in `AppState`), filters via
/// `discover_active_projects` unless `all=true`, and joins by canonical
/// path to attach framework + sessions.
/// Test: `list_projects_returns_array` — smoke-test the empty path.
async fn list_projects(
    State(_state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
) -> Json<Vec<ProjectResponse>> {
    let show_all = matches!(
        params.get("all").map(String::as_str),
        Some("true") | Some("1") | Some("yes")
    );

    // Load registry entries; on any error, fall back to an empty list so the
    // UI renders a graceful empty state instead of a 500.
    let mut registry_entries: Vec<ProjectEntry> = match ProjectRegistry::new() {
        Ok(reg) => match reg.load().await {
            Ok(map) => map.into_values().collect(),
            Err(e) => {
                tracing::debug!(error = %e, "list_projects: registry load failed");
                Vec::new()
            }
        },
        Err(e) => {
            tracing::debug!(error = %e, "list_projects: registry init failed");
            Vec::new()
        }
    };

    // #465: Merge in per-project TOML configs (`.open-mpm/projects/*.toml`)
    // for any project paths not already in the global registry. Projects
    // created via the REPL `/connect` slash command land only in the TOML
    // store; without this merge they vanish from `GET /api/projects` after
    // a restart even though they are valid registrations.
    match crate::tm::ProjectConfigStore::open(&projects_config_dir()) {
        Ok(store) => match store.list() {
            Ok(toml_configs) => {
                let known_paths: std::collections::HashSet<std::path::PathBuf> =
                    registry_entries.iter().map(|e| e.path.clone()).collect();
                for cfg in toml_configs {
                    if known_paths.contains(&cfg.project.path) {
                        continue;
                    }
                    registry_entries.push(ProjectEntry {
                        path: cfg.project.path.clone(),
                        name: cfg.project.name.clone(),
                        last_run: None,
                        status: crate::registry::ProjectStatus::Active,
                        last_connected: None,
                        pm_count: 0,
                        is_self: false,
                        git_origin: None,
                        open_issues_count: None,
                        open_prs_count: None,
                    });
                }
            }
            Err(e) => {
                tracing::debug!(error = %e, "list_projects: TOML store list failed");
            }
        },
        Err(e) => {
            tracing::debug!(error = %e, "list_projects: TOML store open failed");
        }
    }

    // Load TM projects from the on-disk JSON. Best-effort; missing/corrupt
    // file → empty.
    let tm_projects: Vec<TmProject> = load_tm_projects_from_disk().await;
    let tm_session_paths: Vec<std::path::PathBuf> =
        tm_projects.iter().map(|p| p.path.clone()).collect();

    // Filter to active (14 days OR has tmux session) unless ?all=true.
    // Always exclude temp directories regardless of the show_all flag.
    let filtered: Vec<ProjectEntry> = if show_all {
        let mut v: Vec<ProjectEntry> = registry_entries
            .into_iter()
            .filter(|e| e.is_real_project())
            .collect();
        v.sort_by_key(|b| std::cmp::Reverse(b.last_active()));
        v
    } else {
        let window = chrono::Duration::days(14);
        // discover_active_projects already applies is_real_project filtering.
        discover_active_projects(&registry_entries, &tm_session_paths, window)
            .into_iter()
            .cloned()
            .collect()
    };

    // Build the response, joining TM data by canonical path.
    let out: Vec<ProjectResponse> = filtered
        .into_iter()
        .map(|entry| {
            let path_str = entry.path.to_string_lossy().to_string();
            let tm_match = tm_projects.iter().find(|tp| tp.path == entry.path);
            let framework = tm_match.and_then(|tp| {
                if tp.framework.is_known() {
                    Some(tp.framework.display())
                } else {
                    None
                }
            });
            let sessions: Vec<ProjectSessionSummary> = tm_match
                .map(|tp| {
                    tp.sessions
                        .iter()
                        .map(|s| ProjectSessionSummary {
                            name: s.name.clone(),
                            adapter_type: s.adapter_type.as_str().to_string(),
                            status: s.status.to_string(),
                        })
                        .collect()
                })
                .unwrap_or_default();
            ProjectResponse {
                id: path_str.clone(),
                name: entry.name.clone(),
                path: path_str,
                git_origin: entry.git_origin.clone(),
                last_active: entry
                    .last_active()
                    .map(|t| t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)),
                open_issues_count: entry.open_issues_count,
                open_prs_count: entry.open_prs_count,
                framework,
                sessions,
            }
        })
        .collect();

    Json(out)
}

/// Load TM projects from `.open-mpm/state/tm_sessions.json` directly.
///
/// Why: `AppState` does not hold a `TmManager`; rather than threading one
/// through, we read the same on-disk JSON the manager owns. This is the
/// single source of truth so the snapshot can never disagree.
/// What: Reads the file, deserializes the envelope, returns the projects
/// vector. Any I/O or parse error yields an empty list (logged at debug).
/// Test: `load_tm_projects_from_disk_returns_empty_on_missing_file`.
async fn load_tm_projects_from_disk() -> Vec<TmProject> {
    #[derive(Deserialize)]
    struct Envelope {
        #[serde(default)]
        projects: Vec<TmProject>,
    }
    let path = std::path::PathBuf::from(".open-mpm/state/tm_sessions.json");
    let bytes = match tokio::fs::read(&path).await {
        Ok(b) => b,
        Err(e) => {
            tracing::debug!(error = %e, "tm_sessions.json: read failed");
            return Vec::new();
        }
    };
    match serde_json::from_slice::<Envelope>(&bytes) {
        Ok(env) => env.projects,
        Err(e) => {
            tracing::debug!(error = %e, "tm_sessions.json: parse failed");
            Vec::new()
        }
    }
}

/// Query string for `GET /api/events`. (#192 Phase B)
///
/// Why: Lets a single-task UI subscribe only to events for its session
/// without filtering N other concurrent tasks client-side. When omitted,
/// the stream emits every event the server's bus produces.
#[derive(Debug, Deserialize)]
struct EventsQuery {
    session_id: Option<String>,
}

/// `GET /api/events?session_id=<optional>` — Server-Sent Events stream of
/// real-time PM/agent/workflow telemetry. (#192 Phase B)
///
/// Why: Replaces the 2-second `setInterval` poll the UI used to hit
/// `/api/tasks` with. SSE keeps a single long-lived HTTP connection open and
/// pushes events the instant the back-end emits them, cutting perceived
/// latency from "up to 2 s" to "≤ network RTT" while reducing request load.
/// What: Subscribes to the process-global `events::bus`, optionally filters
/// to a single `session_id`, and yields each event as `event: event\ndata:
/// <json>\n\n`. Emits `event: ping\ndata: {}` every 15 s as a keepalive so
/// reverse proxies and mobile networks don't reap idle connections. On
/// `RecvError::Lagged(n)` (slow subscriber), yields one `event: lag\ndata:
/// {"skipped":<n>}` notice and resumes — never silently drops the stream.
/// Test: After `cargo run -- --api --port 7654 &`, run
/// `curl -N http://localhost:7654/api/events` and watch events stream as
/// tasks execute. The connection is exempt from Bearer auth (see
/// `auth_middleware`).
/// `GET /api/agents` (#407) — list discovered agents.
///
/// Why: The web UI and `om` CLI need to display which agent personas are
/// available without spawning a sub-agent. Reading TOML directly avoids
/// pulling the full `AgentRegistry` machinery into the request path. Wrapped
/// in a `{"agents": [...]}` envelope so future fields (e.g. pagination,
/// catalog version) can be added without breaking clients.
/// What: Scans `.open-mpm/agents/*.toml` (relative to cwd), parses each as a
/// minimal `[agent]` table, and returns `{"agents": [{name, role, model,
/// runner, description}, ...]}`. Any individual file failure is logged and
/// skipped — we never return 500 for a bad TOML so the UI keeps working with
/// a partial catalogue.
/// Test: `list_agents_returns_agents_envelope` — write a fixture TOML in a
/// temp cwd, hit the route, assert the envelope shape and field values.
async fn list_agents_route(State(_state): State<AppState>) -> Json<serde_json::Value> {
    Json(
        serde_json::json!({ "agents": scan_agents_dir(&std::path::PathBuf::from(".open-mpm/agents")).await }),
    )
}

/// Scan an agents directory and return a parsed JSON array.
///
/// Why: Extracted from `list_agents_route` so unit tests can drive it
/// against a `tempfile::TempDir` without juggling process cwd.
/// What: Reads `*.toml` files, extracts `[agent]` fields (name, role, model,
/// runner, description), sorts by name. Skips unreadable / unparseable files.
/// Test: `scan_agents_dir_parses_toml` — see tests module.
async fn scan_agents_dir(dir: &std::path::Path) -> Vec<serde_json::Value> {
    let mut out: Vec<serde_json::Value> = Vec::new();
    let entries = match tokio::fs::read_dir(dir).await {
        Ok(e) => e,
        Err(e) => {
            tracing::debug!(?e, dir = %dir.display(), "list_agents: dir read failed");
            return out;
        }
    };
    let mut entries = entries;
    while let Ok(Some(entry)) = entries.next_entry().await {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("toml") {
            continue;
        }
        let raw = match tokio::fs::read_to_string(&path).await {
            Ok(r) => r,
            Err(e) => {
                tracing::debug!(?e, path = %path.display(), "list_agents: read failed");
                continue;
            }
        };
        let parsed: toml::Value = match toml::from_str(&raw) {
            Ok(v) => v,
            Err(e) => {
                tracing::debug!(?e, path = %path.display(), "list_agents: parse failed");
                continue;
            }
        };
        let agent = parsed.get("agent");
        let get_str = |key: &str| -> Option<String> {
            agent
                .and_then(|a| a.get(key))
                .and_then(|v| v.as_str())
                .map(str::to_string)
        };
        let name = get_str("name").unwrap_or_else(|| {
            path.file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("unknown")
                .to_string()
        });
        out.push(serde_json::json!({
            "name": name,
            "role": get_str("role").unwrap_or_default(),
            "model": get_str("model").unwrap_or_default(),
            "runner": get_str("runner").unwrap_or_default(),
            "description": get_str("description").unwrap_or_default(),
        }));
    }
    out.sort_by(|a, b| {
        a.get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .cmp(b.get("name").and_then(|v| v.as_str()).unwrap_or(""))
    });
    out
}

/// Query parameters for `GET /api/sessions` (#407).
#[derive(Debug, Deserialize)]
struct SessionsQuery {
    /// Optional path filter: when provided, only sessions whose `project`
    /// (or `path`) field matches this value are returned.
    project: Option<String>,
}

/// `GET /api/sessions` (#407) — list recorded sessions.
///
/// Why: The web UI shows a session history sidebar; it needs a stable JSON
/// endpoint instead of falling through to the SPA catch-all (which returns
/// HTML and breaks JSON parsers).
/// What: Reads `.open-mpm/state/sessions.json` (an envelope with a
/// `sessions` array), optionally filters by `?project=<path>`, and returns
/// the array under a `sessions` key. Missing file → empty array.
/// Test: With `sessions.json` present, the route returns its `sessions`;
/// with no file, it returns `{"sessions": []}`.
async fn list_sessions_route(
    State(_state): State<AppState>,
    Query(q): Query<SessionsQuery>,
) -> Json<serde_json::Value> {
    let path = std::path::PathBuf::from(".open-mpm/state/sessions.json");
    Json(serde_json::json!({
        "sessions": load_sessions_from(&path, q.project.as_deref()).await,
    }))
}

/// Read sessions from a file path and apply optional project filter.
///
/// Why: Extracted from `list_sessions_route` so unit tests can drive it
/// against a `tempfile::TempDir` without changing the process-global cwd
/// (which races with sibling tests in the same binary).
/// What: Reads the file (missing → empty), parses `{"sessions": [...]}`,
/// optionally filters by `project`/`path` field equality. Errors degrade
/// to an empty list — never propagated as 500 to the route.
/// Test: `list_sessions_empty_returns_envelope`,
/// `list_sessions_filters_by_project` in the tests module.
async fn load_sessions_from(
    path: &std::path::Path,
    project_filter: Option<&str>,
) -> Vec<serde_json::Value> {
    let bytes = match tokio::fs::read(path).await {
        Ok(b) => b,
        Err(_) => return Vec::new(),
    };
    let parsed: serde_json::Value = match serde_json::from_slice(&bytes) {
        Ok(v) => v,
        Err(e) => {
            tracing::debug!(?e, "list_sessions: parse failed");
            return Vec::new();
        }
    };
    let sessions = parsed
        .get("sessions")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    if let Some(want) = project_filter {
        sessions
            .into_iter()
            .filter(|s| {
                s.get("project")
                    .or_else(|| s.get("path"))
                    .and_then(|v| v.as_str())
                    .map(|p| p == want)
                    .unwrap_or(false)
            })
            .collect()
    } else {
        sessions
    }
}

// -------- CTRL sessions (#406) ----------

/// Query parameters for `GET /api/ctrl/sessions`.
///
/// Why: Lets the CLI / UI scope the listing to one project or one status
/// without pulling the full list and filtering client-side.
#[derive(Debug, Deserialize)]
struct CtrlSessionsQuery {
    project: Option<String>,
    status: Option<String>,
}

/// `GET /api/ctrl/sessions` — list CTRL sessions from `SessionStore`.
///
/// Why: Backs `om session list`. Kept distinct from the workflow-flavoured
/// `/api/sessions` so the existing UI sidebar isn't disturbed.
/// What: Loads from `SessionStore`, optionally filters by `?project=<path>`
/// (matches `project_path`) and `?status=active|idle|terminated`.
/// Test: Smoke-tested via curl after `om session new`.
async fn list_ctrl_sessions_handler(Query(q): Query<CtrlSessionsQuery>) -> Json<serde_json::Value> {
    let mut sessions = SessionStore::load();
    if let Some(want) = q.project.as_deref() {
        sessions.retain(|s| s.project_path.to_string_lossy() == want || s.project_name == want);
    }
    if let Some(status) = q.status.as_deref() {
        sessions.retain(|s| {
            matches!(
                (status, &s.status),
                ("active", CtrlSessionStatus::Active)
                    | ("idle", CtrlSessionStatus::Idle)
                    | ("blocked", CtrlSessionStatus::Blocked)
                    | ("terminated", CtrlSessionStatus::Terminated)
            )
        });
    }
    let value = serde_json::to_value(&sessions).unwrap_or_default();
    Json(serde_json::json!({ "sessions": value }))
}

/// Request body for `POST /api/ctrl/sessions`.
#[derive(Debug, Deserialize)]
struct CreateCtrlSessionRequest {
    project_path: String,
    name: String,
    #[serde(default = "default_ctrl_agent")]
    agent: String,
    #[serde(default)]
    worktree: bool,
}

fn default_ctrl_agent() -> String {
    "pm".to_string()
}

/// `POST /api/ctrl/sessions` — create a CTRL session, optionally with worktree.
///
/// Why: Backs `om session new`. Worktree creation is best-effort: a git
/// failure logs and continues so users on non-git project trees still get a
/// session.
/// What: Validates the project path exists, optionally creates
/// `<parent>/<project>-<name>` worktree on a `session/<name>` branch off
/// `main`, persists via `SessionStore::upsert`, returns the saved record.
/// Test: Smoke-tested via `om session new` after the server is up.
async fn create_ctrl_session_handler(
    Json(req): Json<CreateCtrlSessionRequest>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let path = std::path::PathBuf::from(&req.project_path);
    if !path.exists() {
        return Err(StatusCode::BAD_REQUEST);
    }
    let abs = path.canonicalize().map_err(|_| StatusCode::BAD_REQUEST)?;

    // Default port: this handler isn't directly threaded the listening port,
    // so we record 8765 (the documented default). Callers that care about a
    // non-default port can override via a future `?port=` param.
    let mut session = CtrlSession::new(abs.clone(), req.name.clone(), req.agent.clone(), 8765);

    if req.worktree {
        let worktree_name = format!("{}-{}", session.project_name, req.name);
        let worktree_path = abs.parent().unwrap_or(&abs).join(&worktree_name);
        let branch_name = format!("session/{}", req.name);

        let result = std::process::Command::new("git")
            .args([
                "worktree",
                "add",
                "-b",
                &branch_name,
                worktree_path.to_str().unwrap_or(""),
                "main",
            ])
            .current_dir(&abs)
            .output();

        match result {
            Ok(out) if out.status.success() => {
                session.worktree_path = Some(worktree_path);
                session.worktree_branch = Some(branch_name);
            }
            Ok(out) => {
                let err = String::from_utf8_lossy(&out.stderr);
                tracing::warn!(stderr = %err, "git worktree add failed");
            }
            Err(e) => {
                tracing::warn!(error = %e, "git worktree add could not be invoked");
            }
        }
    }

    let saved = SessionStore::upsert(session).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(serde_json::to_value(&saved).unwrap_or_default()))
}

/// `GET /api/ctrl/sessions/:id` — fetch a single CTRL session by id.
///
/// Why: Lets clients inspect a session without listing.
/// What: Parses `id` as UUID, returns 400 / 404 / 200 as appropriate.
/// Test: Smoke-tested after `om session new`.
async fn get_ctrl_session_handler(
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let uuid = Uuid::parse_str(&id).map_err(|_| StatusCode::BAD_REQUEST)?;
    SessionStore::find(&uuid)
        .map(|s| Json(serde_json::to_value(&s).unwrap_or_default()))
        .ok_or(StatusCode::NOT_FOUND)
}

/// `DELETE /api/ctrl/sessions/:id` — terminate a CTRL session, removing its
/// git worktree if any.
///
/// Why: Backs `om session kill`. We force-remove the worktree so users don't
/// hit "worktree is dirty" errors on uncommitted scratch work.
/// What: Looks up the session, runs `git worktree remove --force <path>` if
/// applicable, then flips status to `Terminated`.
/// Test: Smoke-tested via `om session kill <id>`.
async fn terminate_ctrl_session_handler(
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let uuid = Uuid::parse_str(&id).map_err(|_| StatusCode::BAD_REQUEST)?;

    if let Some(session) = SessionStore::find(&uuid)
        && let Some(wt_path) = &session.worktree_path
        && wt_path.exists()
    {
        let _ = std::process::Command::new("git")
            .args([
                "worktree",
                "remove",
                "--force",
                wt_path.to_str().unwrap_or(""),
            ])
            .current_dir(&session.project_path)
            .output();
    }

    let found = SessionStore::terminate(&uuid).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    if found {
        Ok(Json(serde_json::json!({"status": "terminated"})))
    } else {
        Err(StatusCode::NOT_FOUND)
    }
}

/// `POST /api/ctrl/sessions/:id/attach` — return connection info for the CLI.
///
/// Why: `om session attach` needs to know which directory to cd into and
/// which agent to launch — a single round-trip avoids re-reading the store
/// in the CLI.
/// What: Returns `{session_id, project_path, working_dir, agent, name, port}`.
/// `working_dir` is the worktree path when present, otherwise `project_path`.
/// Test: Smoke-tested via `om session attach <id>` (which then re-execs).
async fn attach_ctrl_session_handler(
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let uuid = Uuid::parse_str(&id).map_err(|_| StatusCode::BAD_REQUEST)?;
    let session = SessionStore::find(&uuid).ok_or(StatusCode::NOT_FOUND)?;
    let working_dir = session
        .worktree_path
        .clone()
        .unwrap_or_else(|| session.project_path.clone());
    Ok(Json(serde_json::json!({
        "session_id": session.id,
        "project_path": session.project_path,
        "working_dir": working_dir,
        "agent": session.agent,
        "name": session.name,
        "port": session.port,
    })))
}

/// Request body for `POST /api/projects` (#405, extended in #451).
///
/// Why: The WebUI "Add Project" form posts `{path, adapter?, name?}`. `adapter`
/// is optional purely for backward compatibility with the original #405
/// callers (`om connect <path>`); when supplied, we route through
/// `ProjectConfigStore::find_or_create` so the on-disk shape matches `/connect`.
#[derive(Debug, Deserialize)]
struct ConnectProjectRequest {
    path: String,
    #[serde(default)]
    adapter: Option<String>,
    #[serde(default)]
    name: Option<String>,
}

/// `POST /api/projects` — register a project directory with the server.
///
/// Why: Two clients call this: `om connect <path>` (#405, no adapter) and the
/// WebUI "Add Project" form (#451, supplies adapter). Both want the project to
/// show up in `GET /api/projects` afterwards; the WebUI additionally wants the
/// per-project TOML under `.open-mpm/projects/<name>.toml` so subsequent
/// `tell <project>` calls and `/connect <path> <adapter>` invocations resolve.
/// Centralising both flows here means the on-disk shape can't drift between
/// CLI and HTTP entry points.
/// What:
///   1. Validate and canonicalize `path`.
///   2. Register in the global `~/.open-mpm/projects.json` registry so
///      `GET /api/projects` sees it.
///   3. If `adapter` is supplied, also call `ProjectConfigStore::find_or_create`
///      to materialize `.open-mpm/projects/<name>.toml` and return
///      `{name, path, default_harness, created}`.
///   4. If `adapter` is omitted, return the legacy `{id, path, name, created_at}`
///      shape for backward compatibility with #405 callers.
/// Test: `connect_project_persists_to_registry` (legacy shape) and
/// `connect_project_creates_tm_config_when_adapter_supplied` (new shape).
async fn connect_project(
    State(_state): State<AppState>,
    Json(req): Json<ConnectProjectRequest>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let path = std::path::PathBuf::from(&req.path);
    if !path.exists() {
        return Err(StatusCode::BAD_REQUEST);
    }
    let abs = path.canonicalize().unwrap_or(path);

    // Persist to the same registry GET reads from. Failures here surface
    // as 500 — the client expects success to mean "the project is now
    // visible to GET /api/projects", so silently swallowing would
    // reproduce the original bug.
    let registry = ProjectRegistry::new().map_err(|e| {
        tracing::warn!(error = %e, "connect_project: ProjectRegistry::new failed");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;
    registry.register_pm_start(&abs).await.map_err(|e| {
        tracing::warn!(error = %e, "connect_project: register_pm_start failed");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    // #451: New WebUI form supplies `adapter`; materialize the per-project
    // TOML so subsequent `tell`/`/connect` calls have something to resolve.
    if let Some(adapter) = req.adapter.as_deref() {
        let projects_dir = abs.join(".open-mpm").join("projects");
        let store = crate::tm::ProjectConfigStore::open(&projects_dir).map_err(|e| {
            tracing::warn!(error = %e, "connect_project: ProjectConfigStore::open failed");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
        // Detect create-vs-reuse: a pre-existing entry at this path means we
        // are reusing, not creating. The find_by_path probe is cheap and
        // avoids a save when the config already exists.
        let pre_existing = store.find_by_path(&abs).map_err(|e| {
            tracing::warn!(error = %e, "connect_project: find_by_path failed");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
        let cfg = store
            .find_or_create(&abs, adapter, req.name.as_deref())
            .map_err(|e| {
                tracing::warn!(error = %e, "connect_project: find_or_create failed");
                StatusCode::INTERNAL_SERVER_ERROR
            })?;
        return Ok(Json(serde_json::json!({
            "name": cfg.project.name,
            "path": cfg.project.path.to_string_lossy(),
            "default_harness": cfg.project.default_harness,
            "created": pre_existing.is_none(),
        })));
    }

    // Legacy #405 shape — no adapter, no TM config.
    let name = req.name.unwrap_or_else(|| {
        abs.file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| abs.to_string_lossy().to_string())
    });
    let id = abs.to_string_lossy().to_string();
    let created_at = chrono::Utc::now().to_rfc3339();
    Ok(Json(serde_json::json!({
        "id": id,
        "path": abs.to_string_lossy(),
        "name": name,
        "created_at": created_at,
    })))
}

/// `GET /api/projects/:name` (#451) — return one per-project TOML config.
///
/// Why: The WebUI project detail view needs to render the harness list and
/// default harness for a single project without scanning all of them. Reading
/// the matching `.open-mpm/projects/<name>.toml` is the source of truth.
/// What: Loads the config via `ProjectConfigStore::load`. 404 when the named
/// project has no TOML; 500 on filesystem/parse errors. Returns the raw
/// `ProjectConfig` JSON (serde already round-trips it).
/// Test: `get_project_config_returns_404_when_missing` and
/// `get_project_config_returns_existing`.
async fn get_project_config(
    State(_state): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let store = crate::tm::ProjectConfigStore::open(&projects_config_dir()).map_err(|e| {
        tracing::warn!(error = %e, "get_project_config: open failed");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;
    match store.load(&name) {
        Ok(Some(cfg)) => Ok(Json(serde_json::to_value(&cfg).map_err(|e| {
            tracing::warn!(error = %e, "get_project_config: serialize failed");
            StatusCode::INTERNAL_SERVER_ERROR
        })?)),
        Ok(None) => {
            // #465: Fall back to the global `ProjectRegistry`. Projects added
            // via `POST /api/projects` without an `adapter` only land in
            // `~/.open-mpm/projects.json`; without this fallback they appear
            // in `GET /api/projects` but 404 on the detail endpoint after a
            // restart. We match by entry name OR by path basename so both
            // `name`-keyed and dir-basename-keyed lookups resolve. Returns a
            // minimal `ProjectConfig` shape (no harnesses, no default).
            let registry = match ProjectRegistry::new() {
                Ok(r) => r,
                Err(e) => {
                    tracing::debug!(error = %e, "get_project_config: registry init failed");
                    return Err(StatusCode::NOT_FOUND);
                }
            };
            let entries = match registry.load().await {
                Ok(map) => map,
                Err(e) => {
                    tracing::debug!(error = %e, "get_project_config: registry load failed");
                    return Err(StatusCode::NOT_FOUND);
                }
            };
            let matched = entries.into_values().find(|e| {
                e.name == name
                    || e.path
                        .file_name()
                        .and_then(|n| n.to_str())
                        .map(|b| b == name)
                        .unwrap_or(false)
            });
            match matched {
                Some(entry) => {
                    let cfg = crate::tm::ProjectConfig {
                        project: crate::tm::ProjectMeta {
                            name: entry.name.clone(),
                            path: entry.path.clone(),
                            default_harness: None,
                        },
                        harnesses: Vec::new(),
                    };
                    Ok(Json(serde_json::to_value(&cfg).map_err(|e| {
                        tracing::warn!(error = %e, "get_project_config: serialize failed");
                        StatusCode::INTERNAL_SERVER_ERROR
                    })?))
                }
                None => Err(StatusCode::NOT_FOUND),
            }
        }
        Err(e) => {
            tracing::warn!(error = %e, "get_project_config: load failed");
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

// ==================== #450: TM (tmux) session management ====================
//
// Why: The web UI needs live tmux state — what sessions exist, their adapter
// type and status, and the ability to create, kill, pause, resume, send
// input, and capture pane output. Backed by `TmManager` which already
// coordinates the tmux orchestrator, adapter registry, and persistent
// session registry.
// What: Six routes mapped to `TmManager` methods. All return 503 when
// `state.tm_manager` is None (tmux not available); 500 on operational
// failures; 200/JSON on success.
// Test: `tm_routes_return_503_when_manager_missing` covers the no-tmux path.

/// Response payload for `GET /api/tm/sessions` — one entry per live session.
///
/// Why: The frontend renders these in a table; a flat, stable struct keeps
/// the JSON contract decoupled from the internal `TmSession` shape.
/// What: Mirrors the fields the UI consumes — name, project, adapter, status,
/// last-active timestamp (ISO 8601 UTC).
/// Test: Asserted via `TmSessionDto` JSON shape in routing tests.
#[derive(Debug, Serialize)]
struct TmSessionDto {
    name: String,
    project: String,
    adapter_type: String,
    status: String,
    last_active: String,
}

impl From<&crate::tm::TmSession> for TmSessionDto {
    fn from(s: &crate::tm::TmSession) -> Self {
        Self {
            name: s.name.clone(),
            project: s.project_path.to_string_lossy().to_string(),
            adapter_type: s.adapter_type.as_str().to_string(),
            status: s.status.to_string(),
            last_active: s.last_active.to_rfc3339(),
        }
    }
}

/// 503 response body used uniformly when `TmManager` is unavailable.
fn tm_unavailable() -> (StatusCode, Json<serde_json::Value>) {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(serde_json::json!({"error": "tmux not available"})),
    )
}

/// 500 response body for operational failures inside TmManager.
fn tm_internal_error(err: &anyhow::Error) -> (StatusCode, Json<serde_json::Value>) {
    tracing::warn!(error = %err, "tm handler failed");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(serde_json::json!({"error": err.to_string()})),
    )
}

/// `GET /api/tm/sessions` — list live tmux sessions (after reconcile).
async fn tm_list_sessions(
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let Some(mgr) = state.tm_manager.as_ref() else {
        return Err(tm_unavailable());
    };
    let mgr = mgr.lock().await;
    match mgr.list_sessions().await {
        Ok(sessions) => {
            let dtos: Vec<TmSessionDto> = sessions.iter().map(TmSessionDto::from).collect();
            Ok(Json(serde_json::json!({ "sessions": dtos })))
        }
        Err(e) => Err(tm_internal_error(&e)),
    }
}

/// Request body for `POST /api/tm/sessions` (#450).
#[derive(Debug, Deserialize)]
struct CreateTmSessionRequest {
    name: Option<String>,
    project_path: String,
    adapter: Option<String>,
}

/// `POST /api/tm/sessions` — create a new tmux session.
///
/// Why: The "New Session" button in the WebUI delegates here. We default the
/// session name to the project's basename when omitted so users don't have
/// to invent one, and default the adapter to Shell (matching `TmManager`'s
/// own behavior) when not specified.
/// What: Validates the path, resolves the adapter type, calls
/// `TmManager::new_session`, returns `{name, status}`.
/// Test: integration only — requires a running tmux server.
async fn tm_create_session(
    State(state): State<AppState>,
    Json(req): Json<CreateTmSessionRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let Some(mgr) = state.tm_manager.as_ref() else {
        return Err(tm_unavailable());
    };

    let path = std::path::PathBuf::from(&req.project_path);
    if !path.exists() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "project_path does not exist"})),
        ));
    }
    let abs = path.canonicalize().unwrap_or(path);

    // Default name = directory basename; safe ASCII fallback if missing.
    let name = req.name.unwrap_or_else(|| {
        abs.file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "session".to_string())
    });
    let adapter_type = req.adapter.as_deref().map(AdapterType::from_id);

    let mgr = mgr.lock().await;
    match mgr.new_session(&name, &abs, adapter_type).await {
        Ok(session) => Ok(Json(serde_json::json!({
            "name": session.name,
            "status": session.status.to_string(),
        }))),
        Err(e) => Err(tm_internal_error(&e)),
    }
}

/// `DELETE /api/tm/sessions/:name` — kill a tmux session.
async fn tm_kill_session(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let Some(mgr) = state.tm_manager.as_ref() else {
        return Err(tm_unavailable());
    };
    let mgr = mgr.lock().await;
    match mgr.kill_session(&name).await {
        Ok(()) => Ok(Json(serde_json::json!({"status": "killed"}))),
        Err(e) => Err(tm_internal_error(&e)),
    }
}

/// `POST /api/tm/sessions/:name/pause` — pause a tmux session.
async fn tm_pause_session(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let Some(mgr) = state.tm_manager.as_ref() else {
        return Err(tm_unavailable());
    };
    let mgr = mgr.lock().await;
    match mgr.pause_session(&name).await {
        Ok(()) => Ok(Json(serde_json::json!({"status": "paused"}))),
        Err(e) => Err(tm_internal_error(&e)),
    }
}

/// `POST /api/tm/sessions/:name/resume` — resume a tmux session.
async fn tm_resume_session(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let Some(mgr) = state.tm_manager.as_ref() else {
        return Err(tm_unavailable());
    };
    let mgr = mgr.lock().await;
    match mgr.resume_session(&name).await {
        Ok(()) => Ok(Json(serde_json::json!({"status": "running"}))),
        Err(e) => Err(tm_internal_error(&e)),
    }
}

/// Request body for `POST /api/tm/sessions/:name/send` (#450).
#[derive(Debug, Deserialize)]
struct SendMessageRequest {
    message: String,
}

/// `POST /api/tm/sessions/:name/send` — send a message to the session.
async fn tm_send_message(
    State(state): State<AppState>,
    Path(name): Path<String>,
    Json(req): Json<SendMessageRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let Some(mgr) = state.tm_manager.as_ref() else {
        return Err(tm_unavailable());
    };
    let mgr = mgr.lock().await;
    match mgr.send_message(&name, &req.message).await {
        Ok(()) => Ok(Json(serde_json::json!({"status": "sent"}))),
        Err(e) => Err(tm_internal_error(&e)),
    }
}

/// Query parameters for `GET /api/tm/sessions/:name/pane` (#450).
#[derive(Debug, Deserialize)]
struct CapturePaneQuery {
    /// Number of trailing lines to capture; defaults to 20.
    lines: Option<u32>,
}

/// `GET /api/tm/sessions/:name/pane` — capture last N lines of pane output.
async fn tm_capture_pane(
    State(state): State<AppState>,
    Path(name): Path<String>,
    Query(q): Query<CapturePaneQuery>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let Some(mgr) = state.tm_manager.as_ref() else {
        return Err(tm_unavailable());
    };
    let lines = q.lines.unwrap_or(20);
    let mgr = mgr.lock().await;
    match mgr.capture_pane(&name, lines).await {
        Ok(output) => Ok(Json(serde_json::json!({
            "output": output,
            "lines": lines,
        }))),
        Err(e) => Err(tm_internal_error(&e)),
    }
}

// ==================== #450: favorite + tell routing ====================
//
// Why: The refined `/connect` spec calls for per-session favorites and a
// project-aware `tell` that routes via `<project>.toml` -> default_harness
// (or an explicit `:<harness>` suffix) to the active session for that
// (project, harness) pair. Both endpoints share the same 503-on-no-manager
// pattern as the other `/api/tm/*` routes.
// What:
//   POST   /api/tm/sessions/:name/favorite   -> set favorite = true
//   DELETE /api/tm/sessions/:name/favorite   -> set favorite = false
//   POST   /api/tm/tell                      -> route `{project, message,
//                                               harness?}` to a session
// Test: `tm_favorite_*_returns_503_without_manager`, `tm_tell_returns_503_*`.

/// Directory under `.open-mpm/` holding per-project TOML configs.
///
/// Why: `tell` routing needs to load `<project>.toml` to resolve the harness.
/// Centralizing the path here so tests/CLI agree on the layout.
fn projects_config_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(".open-mpm/projects")
}

/// `POST /api/tm/sessions/:name/favorite` — pin a session in the UI.
async fn tm_set_favorite(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let Some(mgr) = state.tm_manager.as_ref() else {
        return Err(tm_unavailable());
    };
    let mgr = mgr.lock().await;
    match mgr.registry.set_favorite(&name, true) {
        Ok(true) => Ok(Json(serde_json::json!({
            "name": name,
            "favorite": true,
        }))),
        Ok(false) => Err((
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": "session not found"})),
        )),
        Err(e) => Err(tm_internal_error(&e)),
    }
}

/// `DELETE /api/tm/sessions/:name/favorite` — unpin a session.
async fn tm_unset_favorite(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let Some(mgr) = state.tm_manager.as_ref() else {
        return Err(tm_unavailable());
    };
    let mgr = mgr.lock().await;
    match mgr.registry.set_favorite(&name, false) {
        Ok(true) => Ok(Json(serde_json::json!({
            "name": name,
            "favorite": false,
        }))),
        Ok(false) => Err((
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": "session not found"})),
        )),
        Err(e) => Err(tm_internal_error(&e)),
    }
}

/// Request body for `POST /api/tm/tell`.
///
/// Why: `tell <project>` and `tell <project>:<harness>` are the two REPL
/// shapes; on the wire we split them into `project` + optional `harness`.
/// `message` is sent verbatim to the resolved session (the adapter formats
/// it).
/// What: All three fields are flat strings; `harness` defaults to the
/// project's `default_harness` when absent.
/// Test: `tm_tell_returns_503_without_manager` covers the unavailable path;
/// happy-path requires tmux and is exercised in integration.
#[derive(Debug, Deserialize)]
struct TellRequest {
    project: String,
    message: String,
    #[serde(default)]
    harness: Option<String>,
}

/// `POST /api/tm/tell` — route a message to a project's active session.
///
/// Why: Mirrors the REPL's `tell <project>[:<harness>] "msg"` shape over
/// HTTP so other clients (CLI tools, dashboards) can drive any project the
/// host knows about without first looking up the session name.
/// What:
///   1. Load `<.open-mpm/projects>/<project>.toml`; 404 if missing.
///   2. Resolve the harness via `ProjectConfig::resolve_harness(harness)`;
///      400 on unknown harness / missing default.
///   3. Look at the live session list, find the most recent (highest
///      `last_active`) session whose name starts with
///      `<project>-<harness>-`; 404 if none active.
///   4. `send_message(session.name, message)`.
/// Test: `tm_tell_returns_503_without_manager`; happy path = integration.
async fn tm_tell(
    State(state): State<AppState>,
    Json(req): Json<TellRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let Some(mgr) = state.tm_manager.as_ref() else {
        return Err(tm_unavailable());
    };

    // Load the project config first (no lock needed — pure filesystem read).
    let store = match crate::tm::ProjectConfigStore::open(&projects_config_dir()) {
        Ok(s) => s,
        Err(e) => return Err(tm_internal_error(&e)),
    };
    let cfg = match store.load(&req.project) {
        Ok(Some(c)) => c,
        Ok(None) => {
            return Err((
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({
                    "error": format!("project '{}' not configured", req.project),
                    "hint": "create .open-mpm/projects/<name>.toml"
                })),
            ));
        }
        Err(e) => return Err(tm_internal_error(&e)),
    };

    let harness = match cfg.resolve_harness(req.harness.as_deref()) {
        Ok(h) => h.name.clone(),
        Err(e) => {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": e.to_string()})),
            ));
        }
    };

    // Resolve the active session: most-recently-active session whose name
    // starts with `<project>-<harness>-`.
    let prefix = format!("{}-{}-", cfg.project.name, harness);
    let mgr = mgr.lock().await;
    let sessions = match mgr.list_sessions().await {
        Ok(s) => s,
        Err(e) => return Err(tm_internal_error(&e)),
    };
    let target = sessions
        .iter()
        .filter(|s| s.name.starts_with(&prefix))
        .max_by_key(|s| s.last_active);

    let target = match target {
        Some(s) => s.clone(),
        None => {
            return Err((
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({
                    "error": format!("no active session for {}:{}", cfg.project.name, harness),
                    "hint": format!("`/connect {} {}` to create one", cfg.project.path.display(), cfg.harnesses.iter().find(|h| h.name == harness).map(|h| h.adapter.as_str()).unwrap_or("shell")),
                })),
            ));
        }
    };

    match mgr.send_message(&target.name, &req.message).await {
        Ok(()) => Ok(Json(serde_json::json!({
            "project": cfg.project.name,
            "harness": harness,
            "session": target.name,
            "status": "sent",
        }))),
        Err(e) => Err(tm_internal_error(&e)),
    }
}

async fn events_handler(
    Query(params): Query<EventsQuery>,
) -> Sse<impl Stream<Item = Result<SseEvent, Infallible>>> {
    let mut rx = events::subscribe();
    let filter = params.session_id;

    // `async_stream::stream!` lets us write linear-looking code that yields
    // SSE events; the macro lowers it to a poll-based `Stream`.
    let s = async_stream::stream! {
        let mut keepalive = tokio::time::interval(Duration::from_secs(15));
        // Skip the immediate first tick — `interval` fires once at t=0.
        keepalive.tick().await;
        loop {
            tokio::select! {
                _ = keepalive.tick() => {
                    yield Ok(SseEvent::default().event("ping").data("{}"));
                }
                result = rx.recv() => {
                    match result {
                        Ok(event) => {
                            // Filter by session_id when the client requested one.
                            // `Ping` (session_id == None) always passes — keepalives
                            // must reach every subscriber.
                            if let Some(ref sid) = filter
                                && let Some(ev_sid) = event.session_id()
                                && ev_sid != sid.as_str()
                            {
                                continue;
                            }
                            let data = serde_json::to_string(&event)
                                .unwrap_or_else(|_| "{}".to_string());
                            yield Ok(SseEvent::default().event("event").data(data));
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                            yield Ok(SseEvent::default()
                                .event("lag")
                                .data(format!("{{\"skipped\":{n}}}")));
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    }
                }
            }
        }
    };

    // Axum's KeepAlive layer is redundant with our explicit ping but harmless
    // — it sends a comment line if no traffic flows for the default 15 s,
    // giving us double-protection against idle-connection reaping.
    Sse::new(s).keep_alive(KeepAlive::new().interval(Duration::from_secs(30)))
}

/// Execute a `TaskRequest` by invoking `open-mpm --workflow ... --json`
/// (or `--direct <agent>` when `agent` is set) as a subprocess.
///
/// Why: The orchestrator binary already wires build counters, tracing,
/// registries, skill discovery, etc. Re-using it avoids duplicating 200+
/// lines of setup and keeps the server self-contained.
/// What: Builds argv, spawns the child, parses stdout as JSON `PmResponse`.
/// Maps non-JSON stdout or non-zero exit to a `PmResponse::error`.
/// Test: Exercised via integration tests; unit-tested via `TaskRequest`
/// parsing.
async fn run_task(id: &str, req: TaskRequest, state: AppState) -> Result<PmResponse> {
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
    use tokio::process::Command;

    // #151 phase-4: direct agent dispatch. When `agent` is set, call
    // `open-mpm --direct <agent> --task <text>` which bypasses workflow
    // orchestration.
    let mut cmd = Command::new(current_exe()?);
    let is_direct = req.agent.is_some();
    if let Some(agent) = &req.agent {
        cmd.arg("--direct").arg(agent);
    } else {
        let workflow = req.workflow.as_deref().unwrap_or("prescriptive");
        cmd.arg("--workflow").arg(workflow);
        // `--json` only affects workflow mode (direct mode emits raw content).
        cmd.arg("--json");
    }
    cmd.arg("--task").arg(&req.task);
    if let Some(out_dir) = &req.out_dir {
        cmd.arg("--out-dir").arg(out_dir);
    }
    if let Some(task_file) = &req.task_file {
        cmd.arg("--task-file").arg(task_file);
    }

    // Tauri GUI: honour per-task working directory so project-scoped PMs run
    // in the user-selected project root.
    if let Some(project_path) = &req.project_path {
        let p = std::path::Path::new(project_path);
        if p.is_dir() {
            cmd.current_dir(p);
        } else {
            tracing::warn!(
                ?project_path,
                "project_path is not a directory; ignoring and using caller cwd"
            );
        }
    }

    // #149: Pipe stderr so we can sniff `__OMPM_PROGRESS__` lines and stream
    // them into the stored PmResponse. Other stderr lines pass through to our
    // own stderr (the original `inherit()` behavior, but with a parse layer).
    cmd.stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    tracing::info!(task_id = %id, "spawning workflow subprocess");
    let mut child = cmd.spawn()?;

    // #149: Drain stderr in a background task, parsing progress events.
    let stderr_handle = child.stderr.take();
    let id_for_stderr = id.to_string();
    let state_for_stderr = state.clone();
    let stderr_join = tokio::spawn(async move {
        if let Some(stderr) = stderr_handle {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                // #192 Phase B: relay structured Event JSON from the child
                // subprocess to the parent's process-global event bus. SSE
                // subscribers see them in real time. We deliberately check
                // EVENT_LINE_PREFIX BEFORE OMPM_PROGRESS so the new typed
                // protocol takes precedence; the legacy progress line stays
                // as a fallback for older child binaries.
                if let Some(rest) = line.strip_prefix(EVENT_LINE_PREFIX) {
                    match serde_json::from_str::<Event>(rest.trim()) {
                        Ok(ev) => events::publish(ev),
                        Err(e) => {
                            tracing::debug!(
                                error = %e,
                                line = %rest,
                                "failed to parse OMPM_EVENT line"
                            );
                            eprintln!("{line}");
                        }
                    }
                } else if let Some(rest) = line.strip_prefix("__OMPM_PROGRESS__ ") {
                    match serde_json::from_str::<PhaseProgress>(rest.trim()) {
                        Ok(ev) => {
                            // Fan out to BOTH the legacy in-memory store
                            // (still consumed by `GET /api/task/:id` polling
                            // clients) and the new event bus so SSE
                            // subscribers see phase transitions even when the
                            // child only emits the legacy line.
                            let phase = ev.name.clone();
                            let status = ev.status.clone();
                            state_for_stderr.append_progress(&id_for_stderr, ev).await;
                            if status == "running" {
                                events::publish(Event::PhaseStarted {
                                    session_id: id_for_stderr.clone(),
                                    phase,
                                });
                            } else {
                                events::publish(Event::PhaseDone {
                                    session_id: id_for_stderr.clone(),
                                    phase,
                                    status,
                                });
                            }
                        }
                        Err(e) => {
                            tracing::debug!(
                                error = %e,
                                line = %rest,
                                "failed to parse OMPM_PROGRESS event"
                            );
                            // Still forward the raw line to our stderr.
                            eprintln!("{line}");
                        }
                    }
                } else {
                    // Pass through non-progress lines so existing log output
                    // remains visible in the parent's stderr.
                    eprintln!("{line}");
                }
            }
        }
    });

    let mut stdout_buf = Vec::new();
    if let Some(mut so) = child.stdout.take() {
        so.read_to_end(&mut stdout_buf).await?;
    }
    let status = child.wait().await?;
    // Drain stderr task before returning so we don't drop progress events.
    let _ = stderr_join.await;

    if !status.success() {
        return Ok(PmResponse::error(
            id,
            format!("subprocess exited with status {:?}", status.code()),
        ));
    }

    let stdout = String::from_utf8_lossy(&stdout_buf);
    if is_direct {
        // Direct mode returns raw content; wrap it in an agent_response envelope.
        let mut resp = PmResponse::running(id);
        resp.response_type = crate::api::types::PmResponseType::AgentResponse;
        resp.status = PmStatus::Success;
        resp.narrative = stdout.trim().to_string();
        resp.timestamp = crate::api::types::now_iso8601();
        return Ok(resp);
    }

    match serde_json::from_str::<PmResponse>(&stdout) {
        Ok(mut r) => {
            // Preserve the server-assigned id so polling works.
            r.id = id.to_string();
            Ok(r)
        }
        Err(e) => Ok(PmResponse::error(
            id,
            format!("failed to parse workflow JSON output: {e}"),
        )),
    }
}

/// Resolve the current executable path (used for self-respawn).
fn current_exe() -> Result<std::path::PathBuf> {
    std::env::current_exe().map_err(Into::into)
}

/// #371: After a task completes, tick the recap tracker; if the configured
/// interval has been hit, assemble a recap from the last N task histories,
/// persist it, and emit a `RecapGenerated` event.
///
/// Why: Tasks complete on two distinct code paths (Conversational/Research
/// in-process branch, and the prescriptive subprocess branch). Centralising
/// recap dispatch keeps both call sites identical and ensures the GUI's
/// RecapPanel works regardless of which intent class produced the run.
/// What: Acquires the recap tracker lock, calls `tick`. On trigger, snapshots
/// the most recent N tasks from `AppState`, converts each `PhaseProgress`
/// into a `(name, status)` tuple, calls `assemble_recap`, saves to disk and
/// publishes `Event::RecapGenerated`. All disk + LLM-free path — safe to call
/// inside the tokio task that finalised the response.
/// Test: covered by integration; recap module unit tests cover the assembly
/// and persistence primitives.
async fn maybe_emit_recap(state: &AppState, session_id: &str) {
    let triggered = {
        let mut tracker = state.recap_tracker.lock().await;
        tracker.tick(session_id)
    };
    if !triggered {
        return;
    }

    // Snapshot the last N tasks from the response store. We pull from the
    // global `AppState.list()` since per-session task threading isn't tracked
    // here yet — the recap interval is small enough (default 5) that the
    // newest-first window approximates "last N completed in this session".
    let interval = state.recap_tracker.lock().await.config.interval.max(1);
    let recent = state.list().await;
    let tasks: Vec<RecapTask> = recent
        .into_iter()
        .take(interval)
        .map(|r| {
            let phases: Vec<RecapPhase> = r
                .phases_completed
                .iter()
                .map(|p| (p.name.clone(), p.status.clone()))
                .collect();
            // Use id as task prompt placeholder — TaskRequest text isn't
            // currently retained in PmResponse.
            (r.id.clone(), r.narrative.clone(), phases)
        })
        .collect();

    if tasks.is_empty() {
        return;
    }

    let recap = recap::assemble_recap(session_id, &tasks);
    let dir = state_dir();
    if let Err(e) = recap::save_recap(&dir, &recap) {
        tracing::warn!(?e, session_id, "failed to persist recap");
    }
    events::publish(Event::RecapGenerated {
        session_id: session_id.to_string(),
        summary: recap.summary.clone(),
        table_rows: recap
            .rows
            .iter()
            .map(|row| (row.step.clone(), row.result.clone()))
            .collect(),
    });
}

#[cfg(test)]
mod tests {
    // Why: These tests use `crate::test_env::HOME_LOCK` (a `std::sync::Mutex`)
    // to serialize cross-module mutation of `$HOME` while the test body
    // performs async I/O. Holding a sync mutex across `.await` would be a
    // bug in production code, but here the lock is held intentionally for
    // the full test body so two tests don't race on the global env var. The
    // tokio multi-threaded test runtime keeps the lock from causing deadlock.
    #![allow(clippy::await_holding_lock)]

    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt; // for `oneshot`

    fn test_router() -> Router {
        build_router(AppState::default())
    }

    #[tokio::test]
    async fn health_returns_ok_and_version() {
        let app = test_router();
        let req = Request::builder()
            .uri("/api/health")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body_bytes = axum::body::to_bytes(resp.into_body(), 1024).await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
        assert_eq!(body["status"], "ok");
        assert!(body["version"].is_string());
    }

    #[tokio::test]
    async fn unknown_task_id_returns_404() {
        let app = test_router();
        let req = Request::builder()
            .uri("/api/task/nope-does-not-exist")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn list_tasks_empty_store_returns_empty_array() {
        let app = test_router();
        let req = Request::builder()
            .uri("/api/tasks")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body_bytes = axum::body::to_bytes(resp.into_body(), 1024).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
        assert!(v.is_array());
        assert_eq!(v.as_array().unwrap().len(), 0);
    }

    // -- #450: TM session management routes --

    /// Why: All `/api/tm/*` routes must return 503 with `{"error":"tmux not
    /// available"}` when `AppState::tm_manager` is `None`, so the WebUI can
    /// degrade gracefully on hosts without tmux.
    /// What: For each route, build a default `AppState` (no manager), oneshot
    /// the request, assert 503 and the canonical error JSON.
    /// Test: This very function.
    async fn assert_tm_503(method: Method, path: &str, body: Body) {
        let app = build_router(AppState::default());
        let req = Request::builder()
            .method(method)
            .uri(path)
            .header(header::CONTENT_TYPE, "application/json")
            .body(body)
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::SERVICE_UNAVAILABLE,
            "expected 503 for {path}"
        );
        let bytes = axum::body::to_bytes(resp.into_body(), 256).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["error"], "tmux not available");
    }

    #[tokio::test]
    async fn tm_list_sessions_returns_503_without_manager() {
        assert_tm_503(Method::GET, "/api/tm/sessions", Body::empty()).await;
    }

    #[tokio::test]
    async fn tm_create_session_returns_503_without_manager() {
        let body = Body::from(r#"{"project_path":"/tmp"}"#);
        assert_tm_503(Method::POST, "/api/tm/sessions", body).await;
    }

    #[tokio::test]
    async fn tm_kill_session_returns_503_without_manager() {
        assert_tm_503(Method::DELETE, "/api/tm/sessions/foo", Body::empty()).await;
    }

    #[tokio::test]
    async fn tm_pause_session_returns_503_without_manager() {
        assert_tm_503(Method::POST, "/api/tm/sessions/foo/pause", Body::empty()).await;
    }

    #[tokio::test]
    async fn tm_resume_session_returns_503_without_manager() {
        assert_tm_503(Method::POST, "/api/tm/sessions/foo/resume", Body::empty()).await;
    }

    #[tokio::test]
    async fn tm_send_message_returns_503_without_manager() {
        let body = Body::from(r#"{"message":"hi"}"#);
        assert_tm_503(Method::POST, "/api/tm/sessions/foo/send", body).await;
    }

    #[tokio::test]
    async fn tm_capture_pane_returns_503_without_manager() {
        assert_tm_503(Method::GET, "/api/tm/sessions/foo/pane", Body::empty()).await;
    }

    #[tokio::test]
    async fn tm_set_favorite_returns_503_without_manager() {
        assert_tm_503(Method::POST, "/api/tm/sessions/foo/favorite", Body::empty()).await;
    }

    #[tokio::test]
    async fn tm_unset_favorite_returns_503_without_manager() {
        assert_tm_503(
            Method::DELETE,
            "/api/tm/sessions/foo/favorite",
            Body::empty(),
        )
        .await;
    }

    #[tokio::test]
    async fn tm_tell_returns_503_without_manager() {
        let body = Body::from(r#"{"project":"open-mpm","message":"hi"}"#);
        assert_tm_503(Method::POST, "/api/tm/tell", body).await;
    }

    #[tokio::test]
    async fn app_state_trims_to_max_retained() {
        let state = AppState::default();
        // Push MAX_RETAINED + 5.
        for i in 0..(MAX_RETAINED + 5) {
            let id = format!("id-{i}");
            state.upsert(id.clone(), PmResponse::running(&id)).await;
        }
        let list = state.list().await;
        assert_eq!(list.len(), MAX_RETAINED);
        // Newest should be at the front.
        assert_eq!(list[0].id, format!("id-{}", MAX_RETAINED + 4));
    }

    // -- #181: auth middleware --

    fn auth_router(token: &str) -> Router {
        build_router_with_config(AppState::default(), Some(token.to_string()))
    }

    #[tokio::test]
    async fn auth_middleware_rejects_missing_token() {
        let app = auth_router("secret");
        let req = Request::builder()
            .uri("/api/tasks")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        let body = axum::body::to_bytes(resp.into_body(), 256).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["error"], "unauthorized");
    }

    #[tokio::test]
    async fn auth_middleware_rejects_wrong_token() {
        let app = auth_router("secret");
        let req = Request::builder()
            .uri("/api/tasks")
            .header(header::AUTHORIZATION, "Bearer wrong")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn auth_middleware_accepts_valid_token() {
        let app = auth_router("secret");
        let req = Request::builder()
            .uri("/api/tasks")
            .header(header::AUTHORIZATION, "Bearer secret")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn auth_middleware_allows_health_without_token() {
        let app = auth_router("secret");
        let req = Request::builder()
            .uri("/api/health")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn config_endpoint_reports_auth_required_true() {
        let app = auth_router("secret");
        let req = Request::builder()
            .uri("/api/config")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), 256).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["auth_required"], true);
    }

    #[tokio::test]
    async fn config_endpoint_reports_auth_required_false() {
        let app = build_router(AppState::default());
        let req = Request::builder()
            .uri("/api/config")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), 256).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["auth_required"], false);
    }

    #[tokio::test]
    async fn list_projects_returns_array() {
        // Why: The handler must always return a JSON array (possibly empty)
        // so the WebUI can render without special-casing 500s.
        let app = test_router();
        let req = Request::builder()
            .uri("/api/projects")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), 8 * 1024 * 1024)
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(v.is_array(), "expected array, got {v}");
    }

    #[tokio::test]
    async fn connect_project_persists_to_registry() {
        // Why: #405 — POST /api/projects must write to the same
        // ~/.open-mpm/projects.json that GET /api/projects reads from,
        // otherwise `om connect <path>` silently no-ops.
        // What: Sandbox $HOME to a tempdir, POST a project path, then
        // load the registry directly and assert the entry is present.
        // Test guards against the regression where connect_project only
        // returned metadata without persisting.
        let _g = crate::test_env::HOME_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let prev_home = std::env::var_os("HOME");
        // Use the tempdir itself as both HOME and the project path — the
        // path must exist for connect_project to succeed.
        // SAFETY: HOME_LOCK held for entire test body; restored before drop.
        unsafe {
            std::env::set_var("HOME", tmp.path());
        }

        let project_dir = tmp.path().join("myproject");
        std::fs::create_dir(&project_dir).unwrap();

        let app = test_router();
        let body = serde_json::json!({
            "path": project_dir.to_string_lossy(),
            "name": "myproject",
        });
        let req = Request::builder()
            .method("POST")
            .uri("/api/projects")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "POST should succeed");

        // Read the registry directly (same source GET reads from).
        let registry = ProjectRegistry::new().unwrap();
        let entries = registry.load().await.unwrap();
        let canonical = project_dir
            .canonicalize()
            .unwrap()
            .to_string_lossy()
            .to_string();
        assert!(
            entries.contains_key(&canonical),
            "registry should contain {canonical} after POST; got keys {:?}",
            entries.keys().collect::<Vec<_>>()
        );

        // Restore HOME so other tests are unaffected.
        // SAFETY: HOME_LOCK still held.
        unsafe {
            match prev_home {
                Some(h) => std::env::set_var("HOME", h),
                None => std::env::remove_var("HOME"),
            }
        }
    }

    #[tokio::test]
    async fn get_project_config_falls_back_to_registry_when_toml_missing() {
        // Why: #465 — projects added via `POST /api/projects` without an
        // `adapter` only land in `~/.open-mpm/projects.json`. Before this fix,
        // `GET /api/projects/:name` returned 404 because it only inspected
        // `.open-mpm/projects/<name>.toml`, so projects registered through
        // the no-adapter path vanished from the detail endpoint after a
        // restart even though they were visible in the list endpoint.
        // What: Sandbox HOME, register a project via `ProjectRegistry`
        // directly (mirroring what `POST /api/projects` does), then
        // `GET /api/projects/<name>` and assert the synthesized config is
        // returned. Uses a project name that is guaranteed not to collide
        // with any file in the repo's `.open-mpm/projects/` directory.
        // Test: Asserts 200 + JSON with matching name/path.
        let _g = crate::test_env::HOME_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let prev_home = std::env::var_os("HOME");
        // SAFETY: HOME_LOCK held for the entire test body.
        unsafe {
            std::env::set_var("HOME", tmp.path());
        }

        let project_name = "restart-persistence-fallback-test-465";
        let project_dir = tmp.path().join(project_name);
        std::fs::create_dir(&project_dir).unwrap();

        let registry = ProjectRegistry::new().unwrap();
        registry.register_pm_start(&project_dir).await.unwrap();

        let app = test_router();
        let req = Request::builder()
            .uri(format!("/api/projects/{}", project_name))
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::OK,
            "registry-only project should resolve via fallback"
        );
        let body = axum::body::to_bytes(resp.into_body(), 64 * 1024)
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["project"]["name"], project_name);
        // Path may be canonicalized by `register_pm_start` (e.g. `/private/`
        // prefix on macOS); assert it ends with the project basename rather
        // than over-fitting to the canonical form.
        let returned_path = v["project"]["path"].as_str().unwrap_or("");
        assert!(
            returned_path.ends_with(project_name),
            "expected path to end with {project_name}, got {returned_path}"
        );

        // SAFETY: HOME_LOCK still held.
        unsafe {
            match prev_home {
                Some(h) => std::env::set_var("HOME", h),
                None => std::env::remove_var("HOME"),
            }
        }
    }

    #[tokio::test]
    async fn get_project_config_returns_404_when_neither_source_has_entry() {
        // Why: #465 — fallback to the registry must not turn unrelated 404s
        // into bogus 200s. When neither the TOML store nor the registry
        // knows about `:name`, the endpoint must still return 404.
        let _g = crate::test_env::HOME_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let prev_home = std::env::var_os("HOME");
        // SAFETY: HOME_LOCK held.
        unsafe {
            std::env::set_var("HOME", tmp.path());
        }

        let app = test_router();
        let req = Request::builder()
            .uri("/api/projects/this-name-does-not-exist-anywhere-465")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);

        // SAFETY: HOME_LOCK still held.
        unsafe {
            match prev_home {
                Some(h) => std::env::set_var("HOME", h),
                None => std::env::remove_var("HOME"),
            }
        }
    }

    #[tokio::test]
    async fn list_projects_accepts_all_query_param() {
        // Why: `?all=true` must not error and must still return an array.
        let app = test_router();
        let req = Request::builder()
            .uri("/api/projects?all=true")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), 8 * 1024 * 1024)
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(v.is_array());
    }

    #[tokio::test]
    async fn app_state_get_returns_stored() {
        let state = AppState::default();
        state.upsert("abc".into(), PmResponse::running("abc")).await;
        let r = state.get("abc").await.unwrap();
        assert_eq!(r.id, "abc");
        assert_eq!(r.status, PmStatus::Running);
    }

    // -------- CTRL session e2e tests (#406) ----------
    //
    // Why: The `om session` CLI workflow (v0.15.0) is wired through HTTP
    // handlers backed by `SessionStore`. Unit tests in `ctrl_session.rs`
    // cover the store in isolation; these tests drive the real axum router
    // end-to-end so regressions in route wiring, query-param filtering, or
    // status codes get caught.
    // What: Five tests covering full CRUD lifecycle, project filter, status
    // filter, store persistence across instances, and 404 on unknown id.
    // Test: This module IS the test — run via `cargo test session_e2e`.

    /// Sandbox HOME and run a closure with the api router.
    /// Why: Each session_e2e test must isolate `~/.open-mpm/sessions/...`
    /// from the developer's real home and from sibling tests. Mirrors the
    /// pattern in `connect_project_persists_to_registry`.
    async fn with_sandboxed_home<F, Fut, R>(f: F) -> R
    where
        F: FnOnce(tempfile::TempDir) -> Fut,
        Fut: std::future::Future<Output = R>,
    {
        // #409: This helper must hold the HOME override across the AWAIT of
        // the inner future, not just across construction. The previous
        // implementation took a closure returning an arbitrary `R` and reset
        // HOME before the caller could `.await` the future, so the actual
        // request handlers ran with the developer's real `~/.open-mpm` and
        // saw cross-test pollution. We now take an async closure, hold the
        // mutex guard for the lifetime of the future, and clean up after it
        // resolves.
        let _g = crate::test_env::HOME_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let prev_home = std::env::var_os("HOME");
        // SAFETY: HOME_LOCK held; restored before drop.
        unsafe {
            std::env::set_var("HOME", tmp.path());
        }
        let result = f(tmp).await;
        // SAFETY: HOME_LOCK still held (guard dropped at function return).
        unsafe {
            match prev_home {
                Some(h) => std::env::set_var("HOME", h),
                None => std::env::remove_var("HOME"),
            }
        }
        result
    }

    /// Issue a request through the test router and return (status, body json).
    async fn send_json(
        app: Router,
        method: &str,
        uri: &str,
        body: Option<serde_json::Value>,
    ) -> (StatusCode, serde_json::Value) {
        let mut builder = Request::builder().method(method).uri(uri);
        let body = if let Some(b) = body {
            builder = builder.header("content-type", "application/json");
            Body::from(serde_json::to_vec(&b).unwrap())
        } else {
            Body::empty()
        };
        let req = builder.body(body).unwrap();
        let resp = app.oneshot(req).await.unwrap();
        let status = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let json: serde_json::Value =
            serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
        (status, json)
    }

    #[tokio::test]
    async fn session_e2e_full_crud_lifecycle() {
        // Why: Verifies create → list → get → kill → get-after-kill round trip.
        // What: Drives every CTRL session route with a single project path.
        with_sandboxed_home(|tmp| async move {
            let project_dir = tmp.path().join("crud-proj");
            std::fs::create_dir(&project_dir).unwrap();

            // 1. Create.
            let (status, created) = send_json(
                test_router(),
                "POST",
                "/api/ctrl/sessions",
                Some(serde_json::json!({
                    "project_path": project_dir.to_string_lossy(),
                    "name": "feature-x",
                    "agent": "pm",
                    "worktree": false,
                })),
            )
            .await;
            assert_eq!(status, StatusCode::OK, "create: {created}");
            let id = created["id"].as_str().expect("id present").to_string();
            assert_eq!(created["name"], "feature-x");
            assert_eq!(created["agent"], "pm");
            assert_eq!(created["status"], "idle");

            // 2. List shows it.
            let (status, listed) =
                send_json(test_router(), "GET", "/api/ctrl/sessions", None).await;
            assert_eq!(status, StatusCode::OK);
            let arr = listed["sessions"].as_array().unwrap();
            assert_eq!(arr.len(), 1);
            assert_eq!(arr[0]["id"], id);

            // 3. Get by id.
            let (status, got) = send_json(
                test_router(),
                "GET",
                &format!("/api/ctrl/sessions/{id}"),
                None,
            )
            .await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(got["id"], id);
            assert_eq!(got["name"], "feature-x");
            assert_eq!(got["status"], "idle");

            // 4. Kill.
            let (status, killed) = send_json(
                test_router(),
                "DELETE",
                &format!("/api/ctrl/sessions/{id}"),
                None,
            )
            .await;
            assert_eq!(status, StatusCode::OK, "delete: {killed}");
            assert_eq!(killed["status"], "terminated");

            // 5. Get after kill — record kept, status flipped.
            let (status, after) = send_json(
                test_router(),
                "GET",
                &format!("/api/ctrl/sessions/{id}"),
                None,
            )
            .await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(after["status"], "terminated");
        })
        .await
    }

    #[tokio::test]
    async fn session_e2e_filter_by_project() {
        // Why: `?project=<path>` lets the CLI scope the listing — must not
        // leak sessions from sibling projects.
        with_sandboxed_home(|tmp| async move {
            let proj_a = tmp.path().join("proj-a");
            let proj_b = tmp.path().join("proj-b");
            std::fs::create_dir(&proj_a).unwrap();
            std::fs::create_dir(&proj_b).unwrap();

            for (path, name) in [(&proj_a, "a-sess"), (&proj_b, "b-sess")] {
                let (status, _) = send_json(
                    test_router(),
                    "POST",
                    "/api/ctrl/sessions",
                    Some(serde_json::json!({
                        "project_path": path.to_string_lossy(),
                        "name": name,
                        "agent": "pm",
                        "worktree": false,
                    })),
                )
                .await;
                assert_eq!(status, StatusCode::OK);
            }

            // Handler matches against canonicalized project_path.
            let canonical_a = proj_a.canonicalize().unwrap().to_string_lossy().to_string();
            // Tempdir paths on macOS/Linux contain only `/` and alphanumerics,
            // safe to embed in a query string without percent-encoding.
            let uri = format!("/api/ctrl/sessions?project={canonical_a}");
            let (status, listed) = send_json(test_router(), "GET", &uri, None).await;
            assert_eq!(status, StatusCode::OK);
            let arr = listed["sessions"].as_array().unwrap();
            assert_eq!(arr.len(), 1, "expected only proj-a, got {arr:?}");
            assert_eq!(arr[0]["name"], "a-sess");
        })
        .await
    }

    #[tokio::test]
    async fn session_e2e_filter_by_status() {
        // Why: `?status=<idle|terminated>` powers `om session list --status`.
        with_sandboxed_home(|tmp| async move {
            let project_dir = tmp.path().join("status-proj");
            std::fs::create_dir(&project_dir).unwrap();

            let (_, created) = send_json(
                test_router(),
                "POST",
                "/api/ctrl/sessions",
                Some(serde_json::json!({
                    "project_path": project_dir.to_string_lossy(),
                    "name": "to-kill",
                    "agent": "pm",
                    "worktree": false,
                })),
            )
            .await;
            let id = created["id"].as_str().unwrap().to_string();

            // Before kill: idle filter has 1, terminated has 0.
            let (_, idle_list) =
                send_json(test_router(), "GET", "/api/ctrl/sessions?status=idle", None).await;
            assert_eq!(idle_list["sessions"].as_array().unwrap().len(), 1);
            let (_, term_list) = send_json(
                test_router(),
                "GET",
                "/api/ctrl/sessions?status=terminated",
                None,
            )
            .await;
            assert_eq!(term_list["sessions"].as_array().unwrap().len(), 0);

            // Kill it.
            let (status, _) = send_json(
                test_router(),
                "DELETE",
                &format!("/api/ctrl/sessions/{id}"),
                None,
            )
            .await;
            assert_eq!(status, StatusCode::OK);

            // After kill: filters flip.
            let (_, idle_list) =
                send_json(test_router(), "GET", "/api/ctrl/sessions?status=idle", None).await;
            assert_eq!(
                idle_list["sessions"].as_array().unwrap().len(),
                0,
                "no idle sessions after kill"
            );
            let (_, term_list) = send_json(
                test_router(),
                "GET",
                "/api/ctrl/sessions?status=terminated",
                None,
            )
            .await;
            let arr = term_list["sessions"].as_array().unwrap();
            assert_eq!(arr.len(), 1);
            assert_eq!(arr[0]["id"], id);
        })
        .await
    }

    #[tokio::test]
    async fn session_e2e_store_persists_across_instances() {
        // Why: SessionStore is a stateless type that re-reads the JSON file on
        // every call. This test confirms a session written by one logical
        // "instance" (load + upsert + save) is observable by a subsequent
        // `load` that simulates a fresh process.
        with_sandboxed_home(|_tmp| async move {
            let session = CtrlSession::new(
                std::path::PathBuf::from("/tmp/persisted-proj"),
                "persisted".to_string(),
                "pm".to_string(),
                9999,
            );
            let id = session.id;
            SessionStore::upsert(session).unwrap();

            // Simulated fresh process: drop nothing, just call `load` again.
            let reloaded = SessionStore::load();
            assert_eq!(reloaded.len(), 1);
            assert_eq!(reloaded[0].id, id);
            assert_eq!(reloaded[0].name, "persisted");
            assert_eq!(reloaded[0].port, 9999);

            let by_id = SessionStore::find(&id).expect("findable after reload");
            assert_eq!(by_id.id, id);
            assert!(matches!(by_id.status, CtrlSessionStatus::Idle));
        })
        .await
    }

    #[tokio::test]
    async fn session_e2e_kill_unknown_id_returns_404() {
        // Why: DELETE on a nonexistent id must 404 (not 200) so the CLI can
        // distinguish "killed" from "no such session" without parsing bodies.
        with_sandboxed_home(|_tmp| async move {
            // Use a well-formed UUID that doesn't exist in the store.
            let bogus = "00000000-0000-0000-0000-000000000000";
            let (status, _) = send_json(
                test_router(),
                "DELETE",
                &format!("/api/ctrl/sessions/{bogus}"),
                None,
            )
            .await;
            assert_eq!(status, StatusCode::NOT_FOUND);

            // GET on the same id also 404s.
            let (status, _) = send_json(
                test_router(),
                "GET",
                &format!("/api/ctrl/sessions/{bogus}"),
                None,
            )
            .await;
            assert_eq!(status, StatusCode::NOT_FOUND);
        })
        .await
    }

    /// Why: Confirms `/api/agents` returns the `{"agents": [...]}` envelope
    /// with the spec-required fields (name, role, model, runner) parsed from
    /// agent TOML, sorted alphabetically. Drives `scan_agents_dir` directly
    /// against a tempdir so the test does not depend on process cwd.
    /// What: Writes two TOML fixtures, scans, asserts envelope shape and
    /// content.
    /// Test: Self-explanatory — run via `cargo test list_agents_returns`.
    #[tokio::test]
    async fn list_agents_returns_agents_envelope() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("pm.toml"),
            "[agent]\nname = \"pm\"\nrole = \"orchestrator\"\nmodel = \"claude-sonnet-4-6\"\nrunner = \"claude-code\"\n",
        )
        .unwrap();
        std::fs::write(
            tmp.path().join("engineer.toml"),
            "[agent]\nname = \"engineer\"\nrole = \"engineer\"\nmodel = \"claude-opus-4-6\"\nrunner = \"claude-code\"\n",
        )
        .unwrap();

        let agents = scan_agents_dir(tmp.path()).await;
        let envelope = serde_json::json!({ "agents": &agents });

        assert!(envelope["agents"].is_array(), "envelope shape: {envelope}");
        let arr = envelope["agents"].as_array().unwrap();
        assert_eq!(arr.len(), 2);
        // Sorted by name alphabetically: engineer < pm.
        assert_eq!(arr[0]["name"], "engineer");
        assert_eq!(arr[0]["role"], "engineer");
        assert_eq!(arr[0]["model"], "claude-opus-4-6");
        assert_eq!(arr[0]["runner"], "claude-code");
        assert_eq!(arr[1]["name"], "pm");
        assert_eq!(arr[1]["runner"], "claude-code");
    }

    /// Why: When the agents directory is missing, the route must still
    /// return a valid empty envelope so the UI does not crash.
    /// What: Points scan at a nonexistent path, asserts empty vec.
    #[tokio::test]
    async fn list_agents_missing_dir_returns_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let missing = tmp.path().join("does-not-exist");
        let agents = scan_agents_dir(&missing).await;
        assert!(agents.is_empty());
    }

    /// Why: `/api/sessions` must return `{"sessions": []}` (not HTML, not 404)
    /// when no sessions.json exists. Falling through to the SPA catch-all
    /// would break clients that parse the body as JSON (#407 root cause).
    /// Drives `load_sessions_from` directly against a temp path so the test
    /// does not race with sibling tests on the process-global cwd.
    /// What: Points the loader at a nonexistent path, asserts empty list, then
    /// wraps in the same envelope the route produces and asserts shape.
    /// Test: Self-explanatory.
    #[tokio::test]
    async fn list_sessions_empty_returns_envelope() {
        let tmp = tempfile::tempdir().unwrap();
        let missing = tmp.path().join("does-not-exist.json");
        let sessions = load_sessions_from(&missing, None).await;
        let envelope = serde_json::json!({ "sessions": &sessions });
        assert!(
            envelope["sessions"].is_array(),
            "envelope shape: {envelope}"
        );
        assert_eq!(envelope["sessions"].as_array().unwrap().len(), 0);
    }

    /// Why: When sessions.json contains entries, the loader must return them
    /// untouched and the optional `project` filter must select by `project`
    /// or `path` field equality.
    /// What: Writes a fixture sessions.json, loads with and without filter,
    /// asserts shape and counts.
    /// Test: Self-explanatory.
    #[tokio::test]
    async fn list_sessions_filters_by_project() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("sessions.json");
        std::fs::write(
            &path,
            br#"{"sessions":[
                {"id":"a","project":"/p1","status":"idle"},
                {"id":"b","project":"/p2","status":"idle"},
                {"id":"c","path":"/p1","status":"idle"}
            ]}"#,
        )
        .unwrap();

        let all = load_sessions_from(&path, None).await;
        assert_eq!(all.len(), 3);

        let p1 = load_sessions_from(&path, Some("/p1")).await;
        assert_eq!(p1.len(), 2, "both `project` and `path` should match");
        assert_eq!(p1[0]["id"], "a");
        assert_eq!(p1[1]["id"], "c");
    }
}
