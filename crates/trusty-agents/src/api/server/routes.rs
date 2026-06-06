//! Router assembly + server bootstrap (#151, #181).
//!
//! Why: One place to wire every route to its handler, attach CORS /
//! compression / tracing / optional bearer-auth layers, and bind the listener.
//! Keeping route registration separate from the handlers themselves makes the
//! API surface auditable at a glance.
//! What: `build_router*` construct the axum `Router`; `serve*` bind
//! `0.0.0.0:<port>`, print startup URLs, and run until killed.
//! Test: `super::tests` build routers via `build_router` / `build_router_with_config`.

use anyhow::Result;
use axum::{
    Json, Router,
    http::{Method, header},
    middleware,
    routing::{get, post},
};
use tower_http::compression::CompressionLayer;
use tower_http::cors::{Any, CorsLayer};
use tower_http::trace::TraceLayer;

use super::auth::{ApiClientConfig, ApiConfig, AuthState, auth_middleware};
use super::ctrl_sessions::{
    attach_ctrl_session_handler, create_ctrl_session_handler, get_ctrl_session_handler,
    list_ctrl_sessions_handler, terminate_ctrl_session_handler,
};
use super::events_sse::events_handler;
use super::handlers::{
    clear_context, docs_search, get_session_recap, get_task, health, list_tasks, submit_task,
};
use super::project_registration::{connect_project, get_project_config};
use super::projects::{list_agents_route, list_projects, list_sessions_route};
use super::state::AppState;
use super::tm::{
    tm_capture_pane, tm_create_session, tm_kill_session, tm_list_sessions, tm_pause_session,
    tm_resume_session, tm_send_message, tm_set_favorite, tm_tell, tm_unset_favorite,
};
use super::ui::{serve_asset, serve_index};

/// Build the axum router.
///
/// Why: A permissive CORS layer lets the dual-mode UI (`pnpm dev` browser
/// build) talk to the API directly without a Vite proxy when desired, and
/// also unblocks `curl`/Postman from any origin during local development.
/// In production-style deploys the server is fronted by a same-origin
/// reverse proxy (or the Tauri webview) so the wide-open policy is
/// acceptable for our local-dev threat model. Tighten if/when we expose the
/// API publicly.
/// What: Builds the route table with a permissive `CorsLayer` and no auth.
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
        // `.trusty-agents/projects/<name>.toml` rather than the global registry).
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
/// What: Delegates to `serve_with_config` with an unauthenticated config.
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
                    "[trusty-agents] Docs index: {} documents indexed from {} (background)",
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
    tracing::info!(%addr, "trusty-agents api server listening");
    let listener = tokio::net::TcpListener::bind(addr).await?;

    let port = cfg.port;
    println!("[trusty-agents] API:    http://localhost:{port}/api");
    println!("[trusty-agents] Web UI: http://localhost:{port}/");
    if let Some(lan_ip) = detect_lan_ip() {
        println!("[trusty-agents] Web UI (LAN): http://{lan_ip}:{port}/");
    }
    if cfg.token.is_none() {
        eprintln!("\u{26A0}  No API token set — server is unauthenticated");
    } else {
        eprintln!("[trusty-agents] API token authentication: enabled");
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
