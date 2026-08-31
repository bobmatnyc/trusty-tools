//! Router assembly + server bootstrap (#151, #181, #3329).
//!
//! Why: One place to wire every route to its handler, attach the shared
//! same-origin write guard + standard middleware stack, the optional
//! bearer-auth layer, and bind the listener. Keeping route registration
//! separate from the handlers themselves makes the API surface auditable at a
//! glance.
//! What: `build_router*` construct the axum `Router` (router-wide guard applied
//! AFTER all route registration, #3329); `serve*` default to a loopback bind
//! (`127.0.0.1:<port>`), refuse an unauthenticated non-loopback bind, write the
//! `http_addr` discovery file so the console proxy can reach this surface, print
//! startup URLs, and run until killed.
//! Test: `super::tests` build routers via `build_router` / `build_router_with_config`;
//! `super::tests::guard` covers the router-wide write guard and the
//! non-loopback-without-token startup refusal.

use anyhow::Result;
use axum::{
    Extension, Json, Router, middleware,
    routing::{get, post},
};
use trusty_common::server::{SelfOrigins, with_guarded_middleware_same_origin_cors};

use super::agent_create::create_agent_route;
// #4290: read-only KG proxy routes backing the Knowledge Graph browser.
use super::agent_kg::{
    agent_kg_all_route, agent_kg_count_route, agent_kg_query_route, agent_kg_subjects_route,
};
use super::agent_knowledge::agent_knowledge_route;
use super::agent_patch::{get_agent_persona_route, get_agent_route, patch_agent_route};
use super::agent_permissions::agent_permissions_route;
use super::agent_skills::agent_skills_route;
use super::agent_stores::agent_stores_route;
use super::agent_subagents::agent_subagents_route;
use super::auth::{ApiClientConfig, ApiConfig, AuthState, auth_middleware};
use super::cancel::cancel_task;
use super::chat_history::agent_chat_history_route;
use super::costs::get_costs;
use super::ctrl_sessions::{
    attach_ctrl_session_handler, create_ctrl_session_handler, get_ctrl_session_handler,
    list_ctrl_sessions_handler, terminate_ctrl_session_handler,
};
use super::event_tickets::EventStreamAuth;
use super::events_sse::{events_handler, mint_event_ticket};
use super::handlers::{
    clear_context, clear_recent_tasks, docs_search, get_session_recap, get_task, health,
    list_tasks, submit_task,
};
use super::listener_events::{list_listener_events, set_listener_event_filter};
use super::models::get_models;
use super::project_registration::{connect_project, get_project_config};
use super::projects::{list_agents_route, list_projects, list_sessions_route};
use super::relay::relay_event_handler;
use super::state::AppState;
use super::tm::{
    tm_capture_pane, tm_create_session, tm_kill_session, tm_list_sessions, tm_pause_session,
    tm_resume_session, tm_send_message, tm_set_favorite, tm_tell, tm_unset_favorite,
};
use super::ui::{serve_asset, serve_index};
use super::workstreams::{list_workstreams_route, workstream_history_route};

/// Build the axum router.
///
/// Why: Kept `pub` for unit tests and future callers that want an
/// unauthenticated, loopback-only router without going through `ApiConfig`.
/// What: Delegates to `build_router_with_config` (no token, loopback-only
/// self-origins).
/// Test: `super::tests` build routers via this entry point.
//
// Note: `#[allow(dead_code)]` is required because this is a `bin` crate —
// `pub` only suppresses dead-code warnings for library crates exposing items
// as public API, not for binaries with no external consumers.
#[allow(dead_code)]
pub fn build_router(state: AppState) -> Router {
    build_router_with_config(state, None)
}

/// Build the axum router, optionally with bearer-token auth. (#181)
///
/// Why: The default entry point for tests and same-machine dev — trusts only
/// loopback for the router-wide write guard. `serve_with_config` uses
/// [`build_router_with_origins`] with the resolved (possibly non-loopback)
/// bind address.
/// What: Delegates to [`build_router_with_origins`] with a default
/// (loopback-only) `SelfOrigins`.
/// Test: `auth_middleware_*` and `super::tests::guard` cover both branches.
pub fn build_router_with_config(state: AppState, token: Option<String>) -> Router {
    build_router_with_origins(state, token, SelfOrigins::default())
}

/// Build the axum router with bearer-token auth AND the router-wide
/// same-origin write guard bound to `self_origins`. (#181, #3329)
///
/// Why: The trusty-agents API exposes DESTRUCTIVE write routes (`POST
/// /api/task` spawns arbitrary subprocesses, `DELETE /api/task/{id}` and the
/// `/api/tm/*`, `/api/ctrl/*`, `POST /rpc` surfaces mutate live sessions)
/// behind permissive CORS. Without a same-origin guard, any page the operator
/// visits could drive those endpoints cross-origin (CSRF). This adopts the
/// shared guard (`trusty_common::server::with_guarded_middleware`,
/// mirroring #3317) router-wide so every route — including any merged in
/// later — is covered (the #3268 lesson). The guard is method-gated
/// (POST/PUT/PATCH/DELETE only), so `GET` reads and the `/api/events` SSE
/// stream pass through untouched, and it fails open on a missing `Origin`
/// header so server-side callers (the console reverse proxy, `curl`, the Tauri
/// IPC path) keep working. `self_origins` additionally trusts the daemon's own
/// resolved non-loopback bind address so a token-guarded remote bind still
/// serves its own write UI (#3269).
/// What: Registers the same routes as before, conditionally wraps `/api/*`
/// with `auth_middleware` when a token is set (innermost), then applies
/// `with_guarded_middleware` (guard + standard CORS/trace/gzip stack)
/// router-wide.
/// Test: `super::tests::guard` — cross-origin write → 403; loopback (browser
/// same-origin) write → allowed; GET reads unaffected.
pub fn build_router_with_origins(
    state: AppState,
    token: Option<String>,
    self_origins: SelfOrigins,
) -> Router {
    let auth_required = token.is_some();
    let config_route = get(move || async move { Json(ApiClientConfig { auth_required }) });

    let mut router = Router::new()
        .route("/api/task", post(submit_task))
        // #3063: DELETE aborts an in-flight task (cancellation/retask
        // primitive — see `cancel::cancel_task` for the full contract).
        .route("/api/task/{id}", get(get_task).delete(cancel_task))
        // #3737: GET lists recent tasks; DELETE clears the finished-task
        // history while keeping any still-running task (distinct from
        // /api/clear-context, which also aborts in-flight work).
        .route("/api/tasks", get(list_tasks).delete(clear_recent_tasks))
        .route("/api/clear-context", post(clear_context))
        .route("/api/health", get(health))
        .route("/api/config", config_route)
        .route("/api/docs/search", get(docs_search))
        // #3243: inference provider catalog for the Assistant-MVP model
        // picker (epic #3052) — never returns credential values, only
        // whether one resolves.
        .route("/api/models", get(get_models))
        .route("/api/projects", get(list_projects).post(connect_project))
        // #451: per-project TOML config lookup (mirrors the on-disk shape of
        // `.trusty-agents/projects/<name>.toml` rather than the global registry).
        .route("/api/projects/{name}", get(get_project_config))
        // #407: agent + session listing for the web UI / CLI clients.
        // #3819: POST creates a new agent from the `assistant` template.
        .route(
            "/api/agents",
            get(list_agents_route).post(create_agent_route),
        )
        // #3246: persist a per-agent model/provider override, validated
        // against the inference registry + runner constraints.
        .route(
            "/api/agents/{name}",
            axum::routing::get(get_agent_route).patch(patch_agent_route),
        )
        // #3819: gear-panel Personality tab reads the agent's actual
        // persona.md prose (package agents only — see agent_patch's module
        // doc for the package-vs-flat distinction).
        .route(
            "/api/agents/{name}/persona",
            axum::routing::get(get_agent_persona_route),
        )
        // #3816/#3864: the gear-panel OKG Stores tab reads the agent's
        // `[[stores]]` bindings resolved LIVE against trusty-search /
        // trusty-memory (connected + chunk count, or not-connected + reason).
        // Read-only in this slice — bindings are edited in `agent.toml`.
        .route(
            "/api/agents/{name}/stores",
            axum::routing::get(agent_stores_route),
        )
        // #3933: the Skills pane reads the agent's resolved capability set —
        // one named skill per tool, with granted state, the tool each wraps,
        // and the credential it needs. Read-only in this slice; editing arrives
        // as `PATCH { skills_allow }` (DOC-57 §5.7, S-12).
        .route(
            "/api/agents/{name}/skills",
            axum::routing::get(agent_skills_route),
        )
        // #3936: the Permissions pane reads the agent's structured
        // permissions model — scopes (with declared/inherited provenance),
        // tiers, `user_authority`, autonomy posture, per-skill grants — with
        // an `enforced` flag on every field (DOC-57 §7). Read-only in every
        // phase (PM-4).
        .route(
            "/api/agents/{name}/permissions",
            axum::routing::get(agent_permissions_route),
        )
        // #3935: the Knowledge pane reads store bindings + knowledge-classified
        // tools + declared MCP knowledge connections in one call (DOC-57 §4).
        // Read-only; store editing remains #3890's contract.
        .route(
            "/api/agents/{name}/knowledge",
            axum::routing::get(agent_knowledge_route),
        )
        // #4290: the Knowledge Graph browser reads the agent's BOUND memory
        // palace's KG through a thin read-only proxy — trusty-memory owns
        // every one of these queries (`web/kg_routes.rs`), this crate only
        // maps agent → palace and passes the upstream JSON through. Assert /
        // delete are deliberately NOT proxied (owner decision: editability is
        // a separate ticket), so this whole family stays GET-only.
        .route(
            "/api/agents/{name}/kg",
            axum::routing::get(agent_kg_query_route),
        )
        .route(
            "/api/agents/{name}/kg/subjects",
            axum::routing::get(agent_kg_subjects_route),
        )
        .route(
            "/api/agents/{name}/kg/all",
            axum::routing::get(agent_kg_all_route),
        )
        .route(
            "/api/agents/{name}/kg/count",
            axum::routing::get(agent_kg_count_route),
        )
        // #4029 (epic #4021, OQ-5): the Sub-agents pane reads the UNION of the
        // two delegation mechanisms — in-product `delegate_to_agent` targets
        // (role allowlist + #4169's L0/L1 tier gate) and cross-product
        // `dispatch_task` specialists (`[subagents].allowed` ∩ the bridge's
        // `NON_CODING_TARGETS` floor) — labelled by kind, never flattened.
        // A dedicated route rather than an extension of `GET /api/agents/:name`:
        // see `agent_subagents`'s module doc. Read-only.
        .route(
            "/api/agents/{name}/subagents",
            axum::routing::get(agent_subagents_route),
        )
        // #4278: the durable persona chat log, so a page reload rehydrates the
        // chat view instead of discarding it. Reads the trusty-memory
        // `chat_session` projection keyed `persona-{agent}` — the only store
        // holding BOTH halves of a turn (`GET /api/tasks` carries no request
        // text). Bounded + cursored, because that session never rolls over.
        .route(
            "/api/agents/{name}/chat-history",
            axum::routing::get(agent_chat_history_route),
        )
        // #4098: aggregated usage cost for the Costs tab — totals plus
        // by-agent/by-model/by-date breakdowns, folded read-time from
        // `.trusty-agents/state/usage.jsonl` and priced through the single
        // pricing table (`perf::pricing`). Distinguishes "no log yet"
        // (200 + `available: false`) from "$0.00" — see `costs`'s module doc.
        .route("/api/costs", get(get_costs))
        .route("/api/workstreams", get(list_workstreams_route))
        .route(
            "/api/workstreams/{name}/history",
            get(workstream_history_route),
        )
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
        // #5052: authenticated by ticket or bearer token; see `events_sse`.
        .route("/api/events", get(events_handler))
        // #5052: mints the short-lived ticket a browser `EventSource` presents
        // on `/api/events`. A POST on purpose — that puts it behind the
        // router-wide same-origin write guard applied below, which is what
        // stops a cross-origin page minting one in the default, token-less
        // configuration.
        .route("/api/events/ticket", post(mint_event_ticket))
        // #3820: durable, persisted eventstream-listener event list + the
        // per-event-type include/exclude toggle (Events pane, #3818). Named
        // `/api/listener-events` — NOT `/api/events` — because that path is
        // already the SSE telemetry stream above; see `listener_events.rs`'s
        // module doc for the full naming rationale.
        .route("/api/listener-events", get(list_listener_events))
        .route(
            "/api/listener-events/filter",
            post(set_listener_event_filter),
        )
        // #3752: internal loopback relay — the separate `tagent --slack`
        // process POSTs its Slack-mirror events here so they reach this
        // process's bus (and the GUI's `/api/events` stream). Guarded by the
        // router-wide same-origin/bearer stack applied below; the handler
        // additionally whitelists only `Slack*` event kinds.
        .route("/api/internal/relay-event", post(relay_event_handler))
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

    // #5052: the SSE stream's own credential state — the ticket store plus the
    // configured bearer token — delivered to `/api/events` and
    // `/api/events/ticket` as a request extension. Built unconditionally: the
    // stream is gated whether or not an operator token is configured.
    router = router.layer(Extension(EventStreamAuth::new(token.clone())));

    if let Some(tok) = token {
        let auth_state = AuthState { token: tok };
        router = router.layer(middleware::from_fn_with_state(auth_state, auth_middleware));
    }

    // #3329: router-wide same-origin write guard + the shared standard
    // middleware stack (tracing, gzip), applied AFTER all route registration so
    // every destructive route is covered (#3268 lesson). Replaces the
    // crate-local CORS/compression/trace layers so trusty-agents no longer
    // drifts from the sibling trusty-* daemons' middleware.
    //
    // #5052: this daemon takes the SAME-ORIGIN CORS variant rather than the
    // permissive one its siblings use. Its GET surface is conversation content
    // — `/api/events`, `/api/tasks`, `/api/task/{id}`,
    // `/api/sessions/{id}/recap` — and `allow_origin(Any)` let any page in the
    // operator's browser read all of it from `127.0.0.1`. The write guard did
    // not help, because it is method-gated and reads are GET.
    with_guarded_middleware_same_origin_cors(router, self_origins)
}

/// Serve the HTTP API and embedded web UI on `127.0.0.1:<port>` until killed.
///
/// Why: Single-binary deployment — one process handles both API requests and
/// serves the web frontend so users don't need a separate static-file server.
/// What: Delegates to `serve_with_config` with an unauthenticated,
/// loopback-only config (#3329).
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

/// Serve the HTTP API and embedded web UI, honoring `ApiConfig`. (#181, #3329)
///
/// Why: Loopback-only doctrine (#3329) — the API can spawn arbitrary
/// subprocesses and mutate live sessions, so it binds `127.0.0.1` by default
/// and REFUSES to start on a non-loopback interface without a token. For
/// remote access the intended path is the trusty-console reverse proxy
/// (`/api/agents/*`), not exposing this port directly. We also write the
/// standard `http_addr` discovery file after bind so the console proxy can
/// resolve this surface, and remove it on shutdown.
/// What: Rejects a non-loopback `cfg.bind` when `cfg.token` is `None` with an
/// actionable error; binds `cfg.bind:cfg.port`; builds the router trusting the
/// resolved bind address as a self-origin for the write guard; writes/removes
/// the `http_addr` discovery file; prints startup URLs (LAN URL only for a
/// non-loopback bind); serves until killed.
/// Test: `super::tests::guard::non_loopback_without_token_refuses_start`;
/// manual — run `--api --port 7654` and confirm the discovery file appears.
pub async fn serve_with_config(cfg: ApiConfig) -> Result<()> {
    // #3329: loopback-only doctrine. An unauthenticated non-loopback bind would
    // expose an arbitrary-subprocess-spawning API to the whole LAN — refuse it
    // with an actionable error rather than binding silently.
    if !cfg.bind.is_loopback() && cfg.token.is_none() {
        anyhow::bail!(
            "refusing to start the trusty-agents API on non-loopback bind {bind} without an API \
             token: this API can spawn arbitrary subprocesses. Set --api-token <TOKEN> (or the \
             TAGENT_API_TOKEN env var), or bind loopback (omit --bind for the 127.0.0.1 default). \
             For remote access, front this surface with the trusty-console proxy (reachable as \
             /api/agents/*) instead of exposing this port directly.",
            bind = cfg.bind
        );
    }

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
    // #3820: start any enabled eventstream listeners (Gmail history-poll
    // today) as background tasks in this long-lived server process. Reads
    // `~/.trusty-agents/config.toml`'s `[[listeners]]` — every entry defaults
    // `enabled = false`, so an install with no listeners configured (the
    // common case today) spawns nothing. Never fails server startup: a
    // listener config problem is logged and that listener alone is skipped
    // (see `listeners::poll::spawn_listeners`).
    let listener_project_path =
        std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    let global_config = crate::mcp::config::GlobalConfig::load().await;
    crate::listeners::poll::spawn_listeners(global_config.listeners, listener_project_path);

    // #212: Load persisted task snapshot so restarts don't lose history.
    let state = AppState::with_persistence(None).await;
    let addr = std::net::SocketAddr::from((cfg.bind, cfg.port));
    tracing::info!(%addr, "trusty-agents api server listening");
    let listener = tokio::net::TcpListener::bind(addr).await?;

    // Resolve the actual bound address (port may have been requested as 0) and
    // trust it as a self-origin for the router-wide write guard so a
    // token-guarded non-loopback bind still serves its own write UI (#3269);
    // loopback binds are always trusted by the guard, so `from_bind_addrs`
    // drops them.
    let resolved = listener.local_addr().unwrap_or(addr);
    let self_origins = SelfOrigins::from_bind_addrs(&[resolved]);
    let app = build_router_with_origins(state, cfg.token.clone(), self_origins);

    // #3331: publish the bound address via the standard `http_addr` discovery
    // file so the trusty-console reverse proxy (and the connector poller) can
    // resolve `/api/agents/*` to this daemon — the same mechanism the sibling
    // daemons use. Best-effort: a missing $HOME / read-only fs is non-fatal.
    if let Err(e) = trusty_common::write_daemon_addr("trusty-agents", &resolved.to_string()) {
        tracing::warn!(?e, "could not write trusty-agents http_addr discovery file");
    }

    let port = cfg.port;
    println!("[trusty-agents] API:    http://localhost:{port}/api");
    println!("[trusty-agents] Web UI: http://localhost:{port}/");
    // Only advertise a LAN URL when the operator explicitly opted into a
    // non-loopback bind; the loopback default is not reachable off-host.
    if !cfg.bind.is_loopback()
        && let Some(lan_ip) = detect_lan_ip()
    {
        println!("[trusty-agents] Web UI (LAN): http://{lan_ip}:{port}/");
    }
    if cfg.token.is_none() {
        eprintln!("\u{26A0}  No API token set — server is unauthenticated (loopback-only)");
    } else {
        eprintln!("[trusty-agents] API token authentication: enabled");
    }

    let serve_result = axum::serve(listener, app)
        .with_graceful_shutdown(trusty_common::shutdown_signal())
        .await;

    // Remove the discovery file so stale clients fail fast instead of proxying
    // to a dead port.
    if let Err(e) = trusty_common::remove_daemon_addr("trusty-agents") {
        tracing::warn!(
            ?e,
            "could not remove trusty-agents http_addr discovery file"
        );
    }
    serve_result?;
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
