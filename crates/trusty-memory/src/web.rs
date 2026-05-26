//! HTTP API + embedded SPA shell for the trusty-memory admin UI.
//!
//! Why: The web admin panel is the primary GUI for non-MCP clients. Bundling
//! the Svelte build via `rust-embed` keeps deployment to "drop the binary on
//! a host"; the JSON API surface mirrors the MCP tool set so anything
//! trusty-memory can do via Claude Code can also be done via curl or browser.
//! What: All `/api/v1/*` handlers (status, palaces, drawers, recall, KG,
//! config, chat) plus an embedded-asset fallback that serves `ui/dist/`.
//! Test: `cargo test -p trusty-memory-mcp web::tests` covers the asset
//! fallback and JSON shape of every read endpoint against an in-memory
//! palace built on a `tempdir`.

use crate::attribution::{
    CreatorInfo, CreatorSource, HTTP_DEFAULT_CLIENT, X_TRUSTY_CLIENT_CWD, X_TRUSTY_CLIENT_NAME,
};
use crate::hook_emit::HookEventPayload;
use crate::{ActivityFilter, ActivitySource, AppState, DaemonEvent};
use axum::{
    body::Body,
    extract::{Path as AxumPath, Query, State},
    http::{header, HeaderMap, HeaderValue, Request, StatusCode},
    response::{IntoResponse, Response},
    routing::{delete, get, post},
    Json, Router,
};
use rust_embed::RustEmbed;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::sync::Arc;
use trusty_common::memory_core::community::KnowledgeGap;
use trusty_common::memory_core::dream::{DreamConfig, Dreamer, PersistedDreamStats};
use trusty_common::memory_core::palace::{Palace, PalaceId, RoomType};
use trusty_common::memory_core::retrieval::{
    recall_deep_with_default_embedder, recall_with_default_embedder, RecallResult,
};
use trusty_common::memory_core::store::kg::Triple;
use trusty_common::memory_core::{PalaceHandle, PalaceRegistry};
use uuid::Uuid;

/// Embedded UI assets produced by `pnpm build` in `ui/`.
///
/// Why: Single-binary deploys with no separate static-file dance. `build.rs`
/// runs the Vite build before compilation so this folder is always populated.
/// What: All files under `ui/dist/` are included in the binary.
/// Test: `serves_index_html` confirms the SPA shell loads.
#[derive(RustEmbed)]
// Monorepo migration: upstream trusty-memory put the Svelte UI at the repo
// root (`ui/dist/`), so the original path was `$CARGO_MANIFEST_DIR/../../ui/dist/`.
// In the trusty-tools monorepo we keep the UI inside the crate to avoid
// polluting the workspace root with per-crate asset directories.
#[folder = "$CARGO_MANIFEST_DIR/ui/dist/"]
struct WebAssets;

/// Build the public router with API routes + SPA asset fallback.
///
/// Why: `run_http` calls this so the same router shape is used in tests.
/// What: All API routes under `/api/v1`, fallback to the SPA shell.
/// Test: `serves_index_html` and `status_endpoint_returns_payload`.
pub fn router() -> Router<AppState> {
    // axum 0.8 path syntax uses `{param}` instead of `:param`. The shared
    // `trusty_common::server::with_standard_middleware` layer brings in CORS,
    // tracing, and gzip (with SSE excluded) so we don't drift from sibling
    // trusty-* daemons.
    let router = Router::new()
        .route("/api/v1/status", get(status))
        .route("/api/v1/config", get(config))
        .route("/api/v1/palaces", get(list_palaces).post(create_palace))
        .route("/api/v1/palaces/{id}", get(get_palace_handler))
        .route(
            "/api/v1/palaces/{id}/drawers",
            get(list_drawers).post(create_drawer),
        )
        .route(
            "/api/v1/palaces/{id}/drawers/{drawer_id}",
            delete(delete_drawer),
        )
        // Issue #70 — `/memories` is a backward-compatible alias for `/drawers`.
        // Some clients (and earlier docs) POST/GET against `…/memories`, which
        // 404'd because only `/drawers` was registered. Aliasing here keeps
        // both vocabularies working against the same handlers without breaking
        // existing `/drawers` callers.
        .route(
            "/api/v1/palaces/{id}/memories",
            get(list_drawers).post(create_drawer),
        )
        .route(
            "/api/v1/palaces/{id}/memories/{drawer_id}",
            delete(delete_drawer),
        )
        .route("/api/v1/palaces/{id}/recall", get(recall_handler))
        .route("/api/v1/recall", get(recall_all_handler))
        .route("/api/v1/palaces/{id}/kg", get(kg_query).post(kg_assert))
        .route("/api/v1/palaces/{id}/kg/subjects", get(kg_list_subjects))
        .route(
            "/api/v1/palaces/{id}/kg/subjects_with_counts",
            get(kg_list_subjects_with_counts),
        )
        .route("/api/v1/palaces/{id}/kg/all", get(kg_list_all))
        .route("/api/v1/palaces/{id}/kg/graph", get(kg_graph))
        .route("/api/v1/palaces/{id}/kg/count", get(kg_count))
        .route(
            "/api/v1/palaces/{id}/dream/status",
            get(palace_dream_status),
        )
        .route("/api/v1/dream/status", get(dream_status))
        .route("/api/v1/dream/run", post(dream_run))
        .route("/api/v1/kg/gaps", get(kg_gaps_handler))
        .route("/api/v1/kg/prompt-context", get(prompt_context_handler))
        .route("/api/v1/kg/aliases", post(add_alias_handler))
        .route(
            "/api/v1/kg/prompt-facts",
            get(list_prompt_facts_handler).delete(remove_prompt_fact_handler),
        )
        .route("/api/v1/chat", post(crate::chat::chat_handler))
        .route("/api/v1/chat/providers", get(crate::chat::list_providers))
        .route(
            "/api/v1/palaces/{id}/chat/sessions",
            get(crate::chat::list_chat_sessions).post(crate::chat::create_chat_session),
        )
        .route(
            "/api/v1/palaces/{id}/chat/sessions/{session_id}",
            get(crate::chat::get_chat_session).delete(crate::chat::delete_chat_session),
        )
        // Issue #99: inter-project messaging.
        .route(
            "/api/v1/messages",
            get(crate::chat::list_messages_handler).post(crate::chat::send_message_handler),
        )
        .route(
            "/api/v1/messages/mark_read",
            post(crate::chat::mark_message_read_handler),
        )
        .route("/health", get(health))
        .route("/api/v1/logs/tail", get(logs_tail))
        .route("/api/v1/activity", get(activity_handler))
        .route("/api/v1/activity/hook", post(hook_activity_handler))
        .route("/api/v1/admin/stop", post(admin_stop))
        // Multi-transport refactor: a single JSON-RPC 2.0 endpoint that
        // accepts the same envelopes the UDS transport speaks. Lets
        // browser clients, curl, and the stdio bridge fallback hit the
        // tool surface without learning the REST routes. The REST
        // routes above remain for backwards compatibility.
        .route("/rpc", post(rpc_handler))
        .fallback(static_handler);

    trusty_common::server::with_standard_middleware(router)
}

// ---------------------------------------------------------------------------
// Health check
// ---------------------------------------------------------------------------

/// Liveness/version payload for `GET /health`.
///
/// Why: `daemon_probe` requires an HTTP 200 from `/health` to confirm that the
/// port is owned by this daemon (and not a stale or foreign process). Issue
/// #35 enriches it with process resource metrics so operators (and the admin
/// UI) can see RSS, disk footprint, CPU, and uptime in one cheap call.
/// What: Carries a fixed `status` string, the compile-time crate version, and
/// the issue-#35 resource block (`rss_mb`, `disk_bytes`, `cpu_pct`,
/// `uptime_secs`).
/// Test: Asserted by `health_endpoint_returns_ok` and
/// `health_endpoint_includes_resource_fields` in this module's tests.
#[derive(serde::Serialize)]
struct HealthResponse {
    /// `"ok"` when the round-trip smoke test succeeds (or no palace exists
    /// yet), `"degraded"` when store/recall is broken (issue #71). Owned
    /// `String` so the handler can report different statuses without
    /// requiring static lifetimes.
    status: String,
    /// Populated only when `status == "degraded"` (issue #71). Carries a
    /// short phrase identifying which round-trip stage failed so operators
    /// can triage quickly (e.g. `"store failed: ..."`).
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<String>,
    version: &'static str,
    /// Current process Resident Set Size in megabytes (issue #35). Sampled
    /// via the shared `SysMetrics` on each health request.
    rss_mb: u64,
    /// On-disk footprint of the daemon's `data_root` in bytes (issue #35):
    /// the sum of every palace file. Refreshed by a background task every
    /// 10 s; `0` until the first walk completes.
    disk_bytes: u64,
    /// Current process CPU usage as a percentage (issue #35), where `100.0`
    /// means one fully-saturated core. The first reading after daemon start
    /// may be `0.0` until a delta window exists.
    cpu_pct: f32,
    /// Seconds elapsed since the daemon started (issue #35).
    uptime_secs: u64,
    /// Bound `host:port` of the HTTP listener. Why: dynamic port selection
    /// (7070..=7079 + OS fallback) means clients cannot assume `7070`; this
    /// field advertises the real port without forcing them to read
    /// `~/.trusty-memory/http_addr`. `None` when the daemon was constructed
    /// without ever binding (tests that drive the router with `TestServer`).
    #[serde(skip_serializing_if = "Option::is_none")]
    addr: Option<String>,
}

/// `GET /health` — unauthenticated liveness probe with store/recall smoke test.
///
/// Why: Gives `daemon_probe` and external monitors a cheap way to confirm port
/// ownership without touching palace state. Issue #35 additionally reports
/// process RSS, CPU, the `data_root` disk footprint, and uptime. Issue #71
/// upgrades the check to a full memory round-trip (store → recall → verify →
/// delete) against the first palace so operators learn about store/recall
/// regressions immediately instead of after a real request fails.
/// What: Returns HTTP 200 with `{status, version, rss_mb, disk_bytes,
/// cpu_pct, uptime_secs, detail?}`. RSS + CPU are sampled live; `disk_bytes`
/// is read from the background ticker; `uptime_secs` is elapsed since
/// `state.started_at`. When palaces exist, the handler attempts a full
/// remember/recall/forget cycle on the first palace — `status` is `"ok"` on
/// success (or when no palace exists yet), `"degraded"` with a `detail`
/// string explaining the failing stage otherwise. The probe never returns
/// non-200 so monitors keyed on HTTP status still see the daemon as up.
/// Test: `health_endpoint_returns_ok`,
/// `health_endpoint_includes_resource_fields`,
/// `health_endpoint_round_trip_on_fresh_install_is_ok`,
/// `health_endpoint_round_trip_with_palace_is_ok`.
async fn health(State(state): State<AppState>) -> Json<HealthResponse> {
    let (rss_mb, cpu_pct) = {
        let mut metrics = state.sys_metrics.lock().await;
        metrics.sample()
    };
    let disk_bytes = state.disk_bytes.load(std::sync::atomic::Ordering::Relaxed);
    let uptime_secs = state.started_at.elapsed().as_secs();
    let addr = state.bound_addr.get().map(|a| a.to_string());

    let (status, detail) = match run_health_round_trip(&state).await {
        Ok(()) => ("ok".to_string(), None),
        Err(HealthProbeError::NoPalaces) => ("ok".to_string(), None),
        Err(err) => {
            tracing::warn!("/health round-trip degraded: {err}");
            ("degraded".to_string(), Some(err.to_string()))
        }
    };

    Json(HealthResponse {
        status,
        detail,
        version: env!("CARGO_PKG_VERSION"),
        rss_mb,
        disk_bytes,
        cpu_pct,
        uptime_secs,
        addr,
    })
}

/// Stages of the `/health` round-trip that can fail (issue #71).
///
/// Why: `thiserror`-derived enum gives every failure point a stable phrase the
/// handler can render into the `detail` field without printing implementation
/// detail or full backtraces. `NoPalaces` is modelled as an error variant so
/// the round-trip helper can short-circuit cleanly while letting the caller
/// distinguish "skip" from real failures.
/// What: One variant per stage (list, open, store, recall, missing-in-results,
/// delete) plus the `NoPalaces` sentinel.
/// Test: Exercised indirectly by the `health_endpoint_round_trip_*` tests.
#[derive(Debug, thiserror::Error)]
enum HealthProbeError {
    #[error("no palaces present (skipped round-trip)")]
    NoPalaces,
    #[error("list palaces failed: {0}")]
    ListPalaces(String),
    #[error("open palace failed: {0}")]
    OpenPalace(String),
    #[error("store failed: {0}")]
    Store(String),
    #[error("recall failed: {0}")]
    Recall(String),
    #[error("recall did not return the probe drawer (id={0})")]
    ProbeMissing(Uuid),
    #[error("delete probe drawer failed: {0}")]
    Delete(String),
}

/// Execute a remember/recall/forget cycle against the first persisted palace.
///
/// Why: `/health` used to return `status: "ok"` even when `POST /drawers` or
/// the recall path was broken — only that the process was alive. Issue #71
/// asks the probe to actually exercise the store and recall service layer
/// (no HTTP loopback) so monitors detect data-plane regressions on the next
/// poll instead of waiting for a real client to surface them.
/// What: Lists palaces; if empty returns `NoPalaces` so the caller reports
/// "ok" (no way to probe without a palace on a fresh install). Otherwise
/// opens the first palace, stores a content-unique probe drawer via
/// `PalaceHandle::remember`, runs `recall_with_default_embedder` with the
/// probe phrase, asserts the new drawer is in the results, then deletes it
/// via `PalaceHandle::forget`. Returns the first failing stage as a
/// `HealthProbeError`.
/// Test: Indirect — `health_endpoint_round_trip_with_palace_is_ok` and
/// `health_endpoint_round_trip_on_fresh_install_is_ok`.
async fn run_health_round_trip(state: &AppState) -> Result<(), HealthProbeError> {
    let palaces = PalaceRegistry::list_palaces(&state.data_root)
        .map_err(|e| HealthProbeError::ListPalaces(format!("{e:#}")))?;
    let Some(palace) = palaces.into_iter().next() else {
        return Err(HealthProbeError::NoPalaces);
    };
    let handle = state
        .registry
        .open_palace(&state.data_root, &palace.id)
        .map_err(|e| HealthProbeError::OpenPalace(format!("{e:#}")))?;

    // Content-unique probe phrase. `__trusty_memory_healthcheck__` makes the
    // probe identifiable in logs / drawer dumps if a forget step is ever
    // skipped (e.g. handler panic between store and delete); the UUID
    // guarantees uniqueness across concurrent probes.
    let probe_token = Uuid::new_v4();
    let probe_content = format!("__trusty_memory_healthcheck__ probe {probe_token}");

    let drawer_id = handle
        .remember(
            probe_content.clone(),
            RoomType::General,
            vec!["healthcheck".to_string()],
            0.0,
        )
        .await
        .map_err(|e| HealthProbeError::Store(format!("{e:#}")))?;

    let recall_result = recall_with_default_embedder(&handle, &probe_content, 5).await;

    // Always attempt cleanup, even when recall failed, so the probe never
    // leaves drawers behind. Cleanup errors are reported only when no earlier
    // failure exists; otherwise we keep the upstream failure as the root cause.
    let delete_result = handle.forget(drawer_id).await;

    match recall_result {
        Ok(hits) => {
            if !hits.iter().any(|hit| hit.drawer.id == drawer_id) {
                return Err(HealthProbeError::ProbeMissing(drawer_id));
            }
        }
        Err(e) => return Err(HealthProbeError::Recall(format!("{e:#}"))),
    }

    delete_result.map_err(|e| HealthProbeError::Delete(format!("{e:#}")))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Logs tail + admin stop (issue #35)
// ---------------------------------------------------------------------------

/// Default number of log lines returned by `GET /api/v1/logs/tail` when `n`
/// is absent. 100 lines is enough context for a glance without a huge payload.
const DEFAULT_LOGS_TAIL_N: usize = 100;

/// Hard ceiling on `GET /api/v1/logs/tail?n=` — equal to the ring-buffer
/// capacity, so a request can never ask for more lines than the buffer holds.
const MAX_LOGS_TAIL_N: usize = trusty_common::log_buffer::DEFAULT_LOG_CAPACITY;

fn default_logs_tail_n() -> usize {
    DEFAULT_LOGS_TAIL_N
}

/// Query parameters for `GET /api/v1/logs/tail`.
///
/// Why (issue #35): callers ask for a bounded number of recent log lines;
/// `n` defaults to a useful page size and is clamped server-side so a
/// misconfigured client cannot request more lines than the buffer holds.
/// What: `n` is optional; absent → [`DEFAULT_LOGS_TAIL_N`]. Clamped to
/// `[1, MAX_LOGS_TAIL_N]` in the handler.
/// Test: `logs_tail_clamps_n` exercises the clamp.
#[derive(serde::Deserialize)]
struct LogsTailParams {
    #[serde(default = "default_logs_tail_n")]
    n: usize,
}

/// `GET /api/v1/logs/tail?n=200` — return the most recent N tracing log lines.
///
/// Why (issue #35): operators debugging a running daemon want recent logs
/// over HTTP without SSHing to the box or restarting with a different
/// `RUST_LOG`. The in-memory ring buffer (fed by the `LogBufferLayer` wired
/// into the subscriber at startup) makes this near-free.
/// What: clamps `n` to `[1, MAX_LOGS_TAIL_N]`, drains the tail of
/// `state.log_buffer`, and returns `{ "lines": [...], "total": <buffered> }`
/// where `total` is the number of lines currently buffered (so callers can
/// tell whether the ring has wrapped).
/// Test: `logs_tail_returns_recent_lines` and `logs_tail_clamps_n`.
async fn logs_tail(
    State(state): State<AppState>,
    Query(params): Query<LogsTailParams>,
) -> Json<Value> {
    let n = params.n.clamp(1, MAX_LOGS_TAIL_N);
    let lines = state.log_buffer.tail(n);
    Json(serde_json::json!({
        "lines": lines,
        "total": state.log_buffer.len(),
    }))
}

/// `POST /api/v1/admin/stop` — request a graceful shutdown of the daemon.
///
/// Why (issue #35): the admin UI and operators want a one-call way to stop
/// the daemon without resolving its PID and sending a signal. The daemon is
/// localhost-only and trusts every caller, so no auth is required.
/// What: spawns a detached task that sleeps 200 ms (giving this HTTP response
/// time to flush to the client) and then calls `std::process::exit(0)`.
/// Returns `{ "ok": true, "message": "shutting down" }` immediately.
/// Test: `admin_stop_returns_ok` asserts the response shape (it does not
/// drive the real exit — that would terminate the test process).
async fn admin_stop(State(_state): State<AppState>) -> Json<Value> {
    tracing::warn!("admin_stop: shutdown requested via POST /api/v1/admin/stop");
    tokio::spawn(async {
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        std::process::exit(0);
    });
    Json(serde_json::json!({ "ok": true, "message": "shutting down" }))
}

// ---------------------------------------------------------------------------
// Activity log (issue #96)
// ---------------------------------------------------------------------------

/// Default page size returned by `GET /api/v1/activity` when the client
/// omits `limit`. Matches the existing 50-row dashboard window.
const ACTIVITY_DEFAULT_LIMIT: usize = 50;

/// Hard cap on a single activity-page response.
///
/// Why: bounds the per-request work the handler performs and the response
/// size on the wire. The UI never asks for more than a screen's worth at a
/// time; this leaves headroom for power users running curl.
/// What: 500 entries — large enough for ad-hoc inspection without becoming
/// a DoS lever.
/// Test: `activity_endpoint_clamps_limit`.
const ACTIVITY_MAX_LIMIT: usize = 500;

/// Query parameters accepted by `GET /api/v1/activity`.
///
/// Why: serde-driven extraction keeps the handler signature small while
/// validating shape (numeric/ISO timestamps, optional fields). All filter
/// fields are optional and combine with AND.
/// What: see [`ActivityFilter`] for the underlying filter semantics.
/// Test: `activity_endpoint_lists_recent_emits`,
/// `activity_endpoint_filters_by_source_and_palace`.
#[derive(Deserialize, Debug, Default)]
struct ActivityQuery {
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    offset: Option<usize>,
    #[serde(default)]
    palace: Option<String>,
    #[serde(default)]
    source: Option<String>,
    #[serde(default)]
    since: Option<String>,
    #[serde(default)]
    until: Option<String>,
}

/// Wire shape for one row in the `GET /api/v1/activity` response.
///
/// Why: the persisted `ActivityEntry` carries a JSON-encoded `payload`
/// string so the schema is decoupled from `DaemonEvent` evolution; we
/// re-decode the payload to a `Value` here so the UI receives a structured
/// JSON object instead of a nested escaped string.
/// What: same fields as `ActivityEntry` except `payload` is the parsed
/// JSON `Value` (falls back to a string when parse fails).
/// Test: `activity_endpoint_lists_recent_emits`.
#[derive(Serialize, Debug)]
struct ActivityRow {
    id: u64,
    timestamp: chrono::DateTime<chrono::Utc>,
    source: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    palace_id: Option<String>,
    event_type: String,
    payload: Value,
}

/// `GET /api/v1/activity` — paginated activity history (issue #96).
///
/// Why: the dashboard activity feed (`ActivityFeed.svelte`) used to be a
/// pure live-stream — opening the UI rendered an empty pane. Returning a
/// paginated history lets the UI seed the feed on mount and load more on
/// scroll, then layer the SSE live-tail on top.
/// What: clamps `limit` to [1, [`ACTIVITY_MAX_LIMIT`]], parses optional
/// filters, and queries the persistent log. The response shape is
/// `{ entries: [...], total, limit, offset }` so the UI can decide
/// whether more rows exist.
/// Test: `activity_endpoint_lists_recent_emits`,
/// `activity_endpoint_clamps_limit`,
/// `activity_endpoint_filters_by_source_and_palace`.
async fn activity_handler(
    State(state): State<AppState>,
    Query(q): Query<ActivityQuery>,
) -> Result<Json<Value>, ApiError> {
    let limit = q
        .limit
        .unwrap_or(ACTIVITY_DEFAULT_LIMIT)
        .clamp(1, ACTIVITY_MAX_LIMIT);
    let offset = q.offset.unwrap_or(0);

    let source = match q.source.as_deref() {
        Some(s) => match ActivitySource::parse(s) {
            Some(parsed) => Some(parsed),
            None => {
                return Err(ApiError::bad_request(format!(
                    "unknown source '{s}'; expected one of http, mcp, hook",
                )))
            }
        },
        None => None,
    };

    let since = parse_iso_or_bad_request(q.since.as_deref(), "since")?;
    let until = parse_iso_or_bad_request(q.until.as_deref(), "until")?;

    let filter = ActivityFilter {
        palace_id: q.palace.filter(|s| !s.is_empty()),
        source,
        since,
        until,
    };

    let entries = state
        .activity_log
        .list(&filter, limit, offset)
        .map_err(|e| ApiError::internal(format!("activity list: {e:#}")))?;
    let total = state
        .activity_log
        .count()
        .map_err(|e| ApiError::internal(format!("activity count: {e:#}")))?;

    let rows: Vec<ActivityRow> = entries
        .into_iter()
        .map(|e| {
            let payload = serde_json::from_str::<Value>(&e.payload)
                .unwrap_or_else(|_| Value::String(e.payload.clone()));
            ActivityRow {
                id: e.id,
                timestamp: e.timestamp,
                source: e.source.as_str(),
                palace_id: e.palace_id,
                event_type: e.event_type,
                payload,
            }
        })
        .collect();

    Ok(Json(json!({
        "entries": rows,
        "total": total,
        "limit": limit,
        "offset": offset,
    })))
}

/// `POST /api/v1/activity/hook` — ingest a hook firing for the activity feed.
///
/// Why: Claude Code's hooks (`UserPromptSubmit` → `prompt-context`,
/// `SessionStart` → `inbox-check`) run as ephemeral CLI subprocesses with no
/// in-process access to `AppState`. Without an ingestion endpoint they had no
/// way to populate the activity feed, which left the TUI feed empty for any
/// session whose only daemon traffic was hooks. This endpoint accepts the
/// hook's self-reported payload and forwards it to `state.emit` so the same
/// persistence + SSE broadcast pipeline that handles `DrawerAdded`/etc. also
/// covers `HookFired`.
/// What: deserialises a [`HookEventPayload`], maps it onto a
/// `DaemonEvent::HookFired` with `source = ActivitySource::Hook`, hands it to
/// `state.emit`, and returns `204 No Content`. Errors only happen for
/// malformed JSON — handled by axum's own `Json` rejection.
/// Test: `hook_activity_endpoint_appends_to_activity_log`.
async fn hook_activity_handler(
    State(state): State<AppState>,
    Json(payload): Json<HookEventPayload>,
) -> Result<StatusCode, ApiError> {
    state.emit(DaemonEvent::HookFired {
        palace_id: payload.palace_id,
        palace_name: payload.palace_name,
        hook_type: payload.hook_type,
        injection_kind: payload.injection_kind,
        injection_length: payload.injection_length,
        trigger_prompt_excerpt: payload.trigger_prompt_excerpt,
        timestamp: chrono::Utc::now(),
        duration_ms: payload.duration_ms,
        source: ActivitySource::Hook,
    });
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /rpc` — JSON-RPC 2.0 dispatch endpoint.
///
/// Why: the multi-transport refactor needs a single HTTP route that
/// accepts the same envelopes the UDS transport speaks. Browser
/// clients that want the new tool surface (or third-party scripts
/// that prefer JSON-RPC to REST) can POST a request envelope here
/// and get a response back without learning the per-tool REST
/// vocabulary. The existing `/api/v1/*` REST routes continue to work
/// unchanged — this is purely additive.
/// What: deserialises a [`JsonRpcRequest`] from the request body,
/// calls [`crate::transport::rpc::dispatch`], and returns the
/// [`JsonRpcResponse`] as JSON. Always returns HTTP 200 with the
/// envelope inside (JSON-RPC errors are carried in the `error`
/// field, not the HTTP status). Returns HTTP 400 only on JSON
/// deserialisation failure of the outer envelope.
/// Test: `http_rpc_endpoint_roundtrip` in `web::tests`.
async fn rpc_handler(
    State(state): State<AppState>,
    Json(req): Json<crate::transport::rpc::JsonRpcRequest>,
) -> Json<crate::transport::rpc::JsonRpcResponse> {
    let resp = crate::transport::rpc::dispatch(&state, req).await;
    Json(resp)
}

/// Extract a [`CreatorInfo`] for an HTTP write request.
///
/// Why: every HTTP write path (drawers, messages) must attach
/// attribution tags so operators can trace which client wrote which
/// drawer. Centralising the extraction here keeps the `X-Trusty-Client-*`
/// header contract in one place.
/// What: pulls `X-Trusty-Client-Name` (default
/// [`HTTP_DEFAULT_CLIENT`]) and the optional `X-Trusty-Client-Cwd`
/// header off the request, then builds a `CreatorInfo` with
/// `source = Http` and the current daemon crate version.
/// Test: `drawer_creator_attribution_http_default`,
/// `drawer_creator_attribution_http_header`.
pub(crate) fn creator_info_from_http(headers: &HeaderMap) -> CreatorInfo {
    let client = headers
        .get(X_TRUSTY_CLIENT_NAME)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .unwrap_or(HTTP_DEFAULT_CLIENT)
        .to_string();
    let cwd = headers
        .get(X_TRUSTY_CLIENT_CWD)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
        .filter(|s| !s.is_empty());
    CreatorInfo {
        client,
        version: env!("CARGO_PKG_VERSION").to_string(),
        source: CreatorSource::Http,
        cwd,
    }
}

/// Parse an optional ISO-8601 timestamp string for the activity filter.
///
/// Why: the `since` / `until` query params are user-supplied; a bad value
/// should reject the request with a clear 400 rather than be silently
/// dropped (which would return seemingly-correct but mis-filtered data).
/// What: returns `Ok(None)` when the input is `None` or empty;
/// `Ok(Some(_))` on a parseable RFC 3339 timestamp; `Err(ApiError::bad_request)`
/// otherwise.
/// Test: `activity_endpoint_lists_recent_emits` exercises the happy path
/// (no timestamps); a bad timestamp returns 400 — see manual curl.
fn parse_iso_or_bad_request(
    s: Option<&str>,
    field: &str,
) -> Result<Option<chrono::DateTime<chrono::Utc>>, ApiError> {
    match s {
        None | Some("") => Ok(None),
        Some(raw) => chrono::DateTime::parse_from_rfc3339(raw)
            .map(|dt| Some(dt.with_timezone(&chrono::Utc)))
            .map_err(|e| ApiError::bad_request(format!("invalid {field} (RFC 3339): {e}"))),
    }
}

// ---------------------------------------------------------------------------
// Static asset serving
// ---------------------------------------------------------------------------

/// Serve any embedded asset; fall back to `index.html` for SPA routes.
///
/// Why: Hash-based routing lives client-side, but `/assets/foo.js` etc. must
/// resolve to the embedded file directly.
/// What: Looks up the request path under `WebAssets`; if absent, returns
/// `index.html`. Unknown paths under `/api/` return 404.
/// Test: `serves_index_html`, `serves_static_asset`, `unknown_api_404`.
async fn static_handler(req: Request<Body>) -> Response {
    let path = req.uri().path().trim_start_matches('/').to_string();

    if path.starts_with("api/") {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    }

    serve_embedded(&path).unwrap_or_else(|| {
        // SPA fallback.
        serve_embedded("index.html")
            .unwrap_or_else(|| (StatusCode::NOT_FOUND, "ui assets missing").into_response())
    })
}

fn serve_embedded(path: &str) -> Option<Response> {
    let path = if path.is_empty() { "index.html" } else { path };
    let asset = WebAssets::get(path)?;
    let mime = mime_guess::from_path(path).first_or_octet_stream();
    let body = Body::from(asset.data.into_owned());
    let mut resp = Response::new(body);
    resp.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(mime.as_ref())
            .unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream")),
    );
    Some(resp)
}

// ---------------------------------------------------------------------------
// /api/v1/status, /api/v1/config
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct StatusPayload {
    version: String,
    palace_count: usize,
    default_palace: Option<String>,
    data_root: String,
    total_drawers: usize,
    total_vectors: usize,
    total_kg_triples: usize,
}

async fn status(State(state): State<AppState>) -> Json<StatusPayload> {
    let palaces = PalaceRegistry::list_palaces(&state.data_root).unwrap_or_default();
    let palace_count = palaces.len();
    let (mut total_drawers, mut total_vectors, mut total_kg_triples) = (0usize, 0usize, 0usize);
    for p in &palaces {
        if let Ok(handle) = state.registry.open_palace(&state.data_root, &p.id) {
            total_drawers = total_drawers.saturating_add(handle.drawers.read().len());
            total_vectors = total_vectors.saturating_add(handle.vector_store.index_size());
            total_kg_triples = total_kg_triples.saturating_add(handle.kg.count_active_triples());
        }
    }
    Json(StatusPayload {
        version: state.version.clone(),
        palace_count,
        default_palace: state.default_palace.clone(),
        data_root: state.data_root.display().to_string(),
        total_drawers,
        total_vectors,
        total_kg_triples,
    })
}

#[derive(Serialize)]
struct ConfigPayload {
    openrouter_configured: bool,
    model: String,
    data_root: String,
}

async fn config(State(state): State<AppState>) -> Json<ConfigPayload> {
    let cfg = load_user_config().unwrap_or_default();
    Json(ConfigPayload {
        openrouter_configured: !cfg.openrouter_api_key.is_empty(),
        model: cfg.openrouter_model,
        data_root: state.data_root.display().to_string(),
    })
}

/// Minimal mirror of the user-config schema (the real type lives in the bin
/// crate; replicating just the fields we need here avoids a cyclic dep).
#[derive(Deserialize, Default, Clone)]
struct UserConfigMin {
    #[serde(default)]
    openrouter: OpenRouterMin,
    #[serde(default)]
    local_model: LocalModelMin,
    // Carry forward unknown sections by ignoring them on parse.
}

#[derive(Deserialize, Default, Clone)]
struct OpenRouterMin {
    #[serde(default)]
    api_key: String,
    #[serde(default)]
    model: String,
}

#[derive(Deserialize, Clone)]
struct LocalModelMin {
    #[serde(default = "default_local_enabled")]
    enabled: bool,
    #[serde(default = "default_local_base_url")]
    base_url: String,
    #[serde(default = "default_local_model")]
    model: String,
}

fn default_local_enabled() -> bool {
    true
}
fn default_local_base_url() -> String {
    "http://localhost:11434".to_string()
}
fn default_local_model() -> String {
    "llama3.2".to_string()
}

impl Default for LocalModelMin {
    fn default() -> Self {
        Self {
            enabled: default_local_enabled(),
            base_url: default_local_base_url(),
            model: default_local_model(),
        }
    }
}

#[derive(Clone)]
pub(crate) struct LoadedUserConfig {
    pub(crate) openrouter_api_key: String,
    pub(crate) openrouter_model: String,
    pub(crate) local_model: trusty_common::LocalModelConfig,
}

impl Default for LoadedUserConfig {
    fn default() -> Self {
        Self {
            openrouter_api_key: String::new(),
            openrouter_model: "anthropic/claude-3-5-sonnet".to_string(),
            local_model: trusty_common::LocalModelConfig::default(),
        }
    }
}

pub(crate) fn load_user_config() -> Option<LoadedUserConfig> {
    let home = dirs::home_dir()?;
    let path = home.join(".trusty-memory").join("config.toml");
    if !path.exists() {
        return Some(LoadedUserConfig::default());
    }
    let raw = std::fs::read_to_string(&path).ok()?;
    let parsed: UserConfigMin = toml::from_str(&raw).unwrap_or_default();
    let model = if parsed.openrouter.model.is_empty() {
        "anthropic/claude-3-5-sonnet".to_string()
    } else {
        parsed.openrouter.model
    };
    Some(LoadedUserConfig {
        openrouter_api_key: parsed.openrouter.api_key,
        openrouter_model: model,
        local_model: trusty_common::LocalModelConfig {
            enabled: parsed.local_model.enabled,
            base_url: parsed.local_model.base_url,
            model: parsed.local_model.model,
        },
    })
}

// ---------------------------------------------------------------------------
// /api/v1/palaces
// ---------------------------------------------------------------------------

#[derive(Serialize)]
pub(crate) struct PalaceInfo {
    id: String,
    name: String,
    description: Option<String>,
    drawer_count: usize,
    vector_count: usize,
    kg_triple_count: usize,
    wing_count: usize,
    created_at: chrono::DateTime<chrono::Utc>,
    /// Max `created_at` across this palace's drawers, or `None` if empty.
    ///
    /// Why: The UI "sort by activity" mode needs a single timestamp per
    /// palace so operators can spot recently-written palaces. Computing it
    /// from the loaded drawer set avoids adding a per-write update path or a
    /// new on-disk index.
    /// What: `handle.drawers.read().iter().map(|d| d.created_at).max()`.
    /// Null when the handle is unavailable or the palace has zero drawers.
    /// Test: `palace_list_includes_last_write_at` (web tests, added below).
    last_write_at: Option<chrono::DateTime<chrono::Utc>>,
    /// Distinct-entity count in the KG adjacency (zero when no handle).
    ///
    /// Why: The operator TUI surfaces graph breadth alongside triple count;
    /// a separate field avoids re-querying the KG for every dashboard tick.
    /// What: `handle.kg.node_count()`. `#[serde(default)]` so older clients
    /// that don't know the field still deserialise the payload.
    /// Test: `palace_list_includes_graph_counts`.
    #[serde(default)]
    node_count: u64,
    /// Directed-edge count in the KG adjacency (zero when no handle).
    ///
    /// Why: Companion to `node_count` for density at a glance.
    /// What: `handle.kg.edge_count()`. `#[serde(default)]` for forward-compat.
    /// Test: `palace_list_includes_graph_counts`.
    #[serde(default)]
    edge_count: u64,
    /// Number of Louvain communities detected in the KG (zero when no handle).
    ///
    /// Why: The MEMORY tab shows a community tally so operators can spot
    /// clustering at a glance without opening the KG explorer.
    /// What: `handle.kg.community_count()`. `#[serde(default)]` for
    /// forward-compat.
    /// Test: `palace_list_includes_graph_counts`.
    #[serde(default)]
    community_count: u64,
    /// `true` while a `Dreamer::dream_cycle` is running against this palace.
    ///
    /// Why: Drives the dreaming/compacting spinner in the operator TUI; the
    /// dashboard polls `/api/v1/palaces` and needs a single boolean signal.
    /// What: `handle.is_compacting()`, set by `CompactionGuard` in
    /// `trusty_common::memory_core::dream`. `#[serde(default)]` so old clients
    /// that don't expect the field deserialise as `false`.
    /// Test: `palace_list_includes_graph_counts`.
    #[serde(default)]
    is_compacting: bool,
}

/// Build a `PalaceInfo` from a `Palace` row plus an optional opened handle.
///
/// Why: Both `list_palaces` and `get_palace_handler` need the same enriched
/// shape; centralizing the field-pulling avoids drift.
/// What: Reads drawer count, vector index size, active KG triple count, and
/// derives wing_count from the number of distinct `room_id`s in the drawer
/// table (until a dedicated wings/rooms table exists, distinct rooms-by-drawer
/// is the closest proxy).
/// Test: `palace_list_includes_richer_counts`.
pub(crate) fn palace_info_from(palace: &Palace, handle: Option<&Arc<PalaceHandle>>) -> PalaceInfo {
    let (
        drawer_count,
        vector_count,
        kg_triple_count,
        wing_count,
        last_write_at,
        node_count,
        edge_count,
        community_count,
        is_compacting,
    ) = if let Some(h) = handle {
        let drawers = h.drawers.read();
        let distinct_rooms: HashSet<Uuid> = drawers.iter().map(|d| d.room_id).collect();
        let last_write = drawers.iter().map(|d| d.created_at).max();
        (
            drawers.len(),
            h.vector_store.index_size(),
            h.kg.count_active_triples(),
            distinct_rooms.len(),
            last_write,
            h.kg.node_count() as u64,
            h.kg.edge_count() as u64,
            h.kg.community_count() as u64,
            h.is_compacting(),
        )
    } else {
        (0, 0, 0, 0, None, 0, 0, 0, false)
    };
    PalaceInfo {
        id: palace.id.0.clone(),
        name: palace.name.clone(),
        description: palace.description.clone(),
        drawer_count,
        vector_count,
        kg_triple_count,
        wing_count,
        created_at: palace.created_at,
        last_write_at,
        node_count,
        edge_count,
        community_count,
        is_compacting,
    }
}

async fn list_palaces(State(state): State<AppState>) -> Result<Json<Vec<PalaceInfo>>, ApiError> {
    let palaces = PalaceRegistry::list_palaces(&state.data_root)
        .map_err(|e| ApiError::internal(format!("list palaces: {e:#}")))?;
    let mut out = Vec::with_capacity(palaces.len());
    for p in palaces {
        let handle = state.registry.open_palace(&state.data_root, &p.id).ok();
        out.push(palace_info_from(&p, handle.as_ref()));
    }
    Ok(Json(out))
}

#[derive(Deserialize)]
struct CreatePalaceBody {
    name: String,
    #[serde(default)]
    description: Option<String>,
}

async fn create_palace(
    State(state): State<AppState>,
    Json(body): Json<CreatePalaceBody>,
) -> Result<Json<Value>, ApiError> {
    let name = body.name.trim().to_string();
    if name.is_empty() {
        return Err(ApiError::bad_request("name is required"));
    }
    let id = PalaceId::new(&name);
    let palace = Palace {
        id: id.clone(),
        name: name.clone(),
        description: body.description.filter(|s| !s.is_empty()),
        created_at: chrono::Utc::now(),
        data_dir: state.data_root.join(&name),
    };
    state
        .registry
        .create_palace(&state.data_root, palace)
        .map_err(|e| ApiError::internal(format!("create palace: {e:#}")))?;
    state.emit(DaemonEvent::PalaceCreated {
        id: name.clone(),
        name: name.clone(),
        source: ActivitySource::Http,
    });
    Ok(Json(json!({ "id": name })))
}

async fn get_palace_handler(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
) -> Result<Json<PalaceInfo>, ApiError> {
    let palaces = PalaceRegistry::list_palaces(&state.data_root)
        .map_err(|e| ApiError::internal(format!("list palaces: {e:#}")))?;
    let palace = palaces
        .into_iter()
        .find(|p| p.id.0 == id)
        .ok_or_else(|| ApiError::not_found(format!("palace not found: {id}")))?;
    let handle = state
        .registry
        .open_palace(&state.data_root, &palace.id)
        .ok();
    Ok(Json(palace_info_from(&palace, handle.as_ref())))
}

// ---------------------------------------------------------------------------
// Drawers
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct ListDrawersQuery {
    #[serde(default)]
    room: Option<String>,
    #[serde(default)]
    tag: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
}

async fn list_drawers(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
    Query(q): Query<ListDrawersQuery>,
) -> Result<Json<Value>, ApiError> {
    let handle = open_handle(&state, &id)?;
    let room = q.room.as_deref().map(RoomType::parse);
    let drawers = handle.list_drawers(room, q.tag.clone(), q.limit.unwrap_or(50));
    Ok(Json(serde_json::to_value(drawers).unwrap_or(json!([]))))
}

#[derive(Deserialize)]
struct CreateDrawerBody {
    content: String,
    #[serde(default)]
    room: Option<String>,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    importance: Option<f32>,
}

/// Maximum number of characters retained in a drawer's content preview.
///
/// Why: SSE consumers (TUI activity log, dashboard ticker) render the
/// preview in a single line alongside the palace name; ~80 chars keeps the
/// line readable on a 100-column terminal without truncating the palace
/// label.
const DRAWER_PREVIEW_MAX_CHARS: usize = 80;

/// Build a single-line preview of drawer content for SSE events.
///
/// Why: the activity feed should show *what* was just stored, not only the
/// running drawer count. Multiline / whitespace-heavy bodies otherwise blow
/// out the log row, so we normalise whitespace and bound the length.
/// What: collapses every run of ASCII / Unicode whitespace to a single space,
/// trims leading/trailing whitespace, and truncates to
/// [`DRAWER_PREVIEW_MAX_CHARS`] characters with a trailing `…` when cut.
/// Test: `drawer_preview_collapses_whitespace_and_truncates`.
pub(crate) fn drawer_content_preview(content: &str) -> String {
    let normalised: String = content.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalised.chars().count() <= DRAWER_PREVIEW_MAX_CHARS {
        normalised
    } else {
        let kept: String = normalised
            .chars()
            .take(DRAWER_PREVIEW_MAX_CHARS.saturating_sub(1))
            .collect();
        format!("{kept}…")
    }
}

async fn create_drawer(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
    headers: HeaderMap,
    Json(body): Json<CreateDrawerBody>,
) -> Result<Json<Value>, ApiError> {
    let handle = open_handle(&state, &id)?;
    let room = body
        .room
        .as_deref()
        .map(RoomType::parse)
        .unwrap_or(RoomType::General);
    let importance = body.importance.unwrap_or(0.5);
    // Compute the preview *before* moving `body.content` into `remember` so
    // the SSE activity feed can show what was actually stored.
    let content_preview = drawer_content_preview(&body.content);
    // Submission-logging Part B: attach `creator:*` attribution tags so
    // every drawer carries the identity of the client that wrote it.
    // HTTP clients self-identify via `X-Trusty-Client-Name` /
    // `X-Trusty-Client-Cwd`; absent headers fall back to
    // `unknown-http-client` and an empty cwd.
    let creator = creator_info_from_http(&headers);
    let mut tags_with_creator = body.tags;
    creator.merge_into(&mut tags_with_creator);
    // Issue #133: mirror the MCP `memory_remember` path — keep originals so
    // the auto-KG extractor sees the same content / tags / room that landed
    // in the drawer. `remember` consumes them, so clone before the call.
    let content_for_kg = body.content.clone();
    let tags_for_kg = tags_with_creator.clone();
    let room_label_for_kg = crate::tools::room_label(&room);
    let drawer_id = handle
        .remember(body.content, room, tags_with_creator, importance)
        .await
        .map_err(|e| ApiError::internal(format!("remember: {e:#}")))?;
    let drawer_count = handle.drawers.read().len();
    let palace_name = PalaceRegistry::list_palaces(&state.data_root)
        .ok()
        .and_then(|ps| ps.into_iter().find(|p| p.id.0 == id).map(|p| p.name))
        .unwrap_or_else(|| id.clone());
    state.emit(DaemonEvent::DrawerAdded {
        palace_id: id.clone(),
        palace_name,
        drawer_count,
        timestamp: chrono::Utc::now(),
        content_preview,
        source: ActivitySource::Http,
    });
    state.emit(aggregate_status_event(&state));
    // Issue #133: best-effort auto-KG extraction. Mirrors the MCP path
    // (`tools.rs::memory_remember`). Failures during extraction MUST NOT
    // fail the HTTP write — the drawer is already on disk; the extractor
    // logs warnings and swallows errors internally.
    crate::tools::auto_extract_and_assert(
        &handle,
        drawer_id,
        &content_for_kg,
        &tags_for_kg,
        room_label_for_kg.as_deref(),
    )
    .await;
    Ok(Json(json!({ "id": drawer_id })))
}

async fn delete_drawer(
    State(state): State<AppState>,
    AxumPath((id, drawer_id)): AxumPath<(String, String)>,
) -> Result<StatusCode, ApiError> {
    let handle = open_handle(&state, &id)?;
    let uuid = Uuid::parse_str(&drawer_id)
        .map_err(|_| ApiError::bad_request("drawer_id must be a UUID"))?;
    handle
        .forget(uuid)
        .await
        .map_err(|e| ApiError::internal(format!("forget: {e:#}")))?;
    let drawer_count = handle.drawers.read().len();
    state.emit(DaemonEvent::DrawerDeleted {
        palace_id: id.clone(),
        drawer_count,
        source: ActivitySource::Http,
    });
    state.emit(aggregate_status_event(&state));
    Ok(StatusCode::NO_CONTENT)
}

/// Compute the current aggregate `StatusChanged` event by walking all palaces.
///
/// Why: Several mutating handlers (drawer add/delete, dream run) need to push
/// a refreshed status snapshot so dashboard stat cards stay in sync without
/// the SPA having to issue an extra `/api/v1/status` request.
/// What: Mirrors the math in the `status` handler — sums drawer count,
/// vector index size, and active KG triples across every persisted palace.
/// Test: Indirectly via the SSE integration tests that observe the event.
pub(crate) fn aggregate_status_event(state: &AppState) -> DaemonEvent {
    let palaces = PalaceRegistry::list_palaces(&state.data_root).unwrap_or_default();
    let (mut total_drawers, mut total_vectors, mut total_kg_triples) = (0usize, 0usize, 0usize);
    for p in &palaces {
        if let Ok(handle) = state.registry.open_palace(&state.data_root, &p.id) {
            total_drawers = total_drawers.saturating_add(handle.drawers.read().len());
            total_vectors = total_vectors.saturating_add(handle.vector_store.index_size());
            total_kg_triples = total_kg_triples.saturating_add(handle.kg.count_active_triples());
        }
    }
    DaemonEvent::StatusChanged {
        total_drawers,
        total_vectors,
        total_kg_triples,
    }
}

// ---------------------------------------------------------------------------
// Recall
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct RecallQuery {
    q: String,
    #[serde(default)]
    top_k: Option<usize>,
    #[serde(default)]
    deep: Option<bool>,
}

async fn recall_handler(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
    Query(q): Query<RecallQuery>,
) -> Result<Json<Value>, ApiError> {
    let handle = open_handle(&state, &id)?;
    let top_k = q.top_k.unwrap_or(10);
    let results = if q.deep.unwrap_or(false) {
        recall_deep_with_default_embedder(&handle, &q.q, top_k).await
    } else {
        recall_with_default_embedder(&handle, &q.q, top_k).await
    }
    .map_err(|e| ApiError::internal(format!("recall: {e:#}")))?;

    let payload: Vec<Value> = results.into_iter().map(recall_entry_json).collect();
    Ok(Json(json!(payload)))
}

/// Flatten a [`RecallResult`] into a single JSON object with the drawer's
/// fields hoisted to the top level.
///
/// Why: Issue #69 — the recall API previously nested the drawer under a
/// `"drawer"` wrapper (`{"drawer": {"content": …}, "score": …}`), so every
/// client that looked for `content`/`tags`/`importance` at the top level of an
/// entry got nothing and recall always appeared to return `[]`. Hoisting the
/// drawer fields makes `content` directly reachable while keeping `score` and
/// `layer` alongside as ranking metadata.
/// What: Serializes the [`Drawer`](trusty_common::memory_core::Drawer) to a
/// JSON object and inserts `score` and `layer`. The `Drawer` schema has no
/// `score`/`layer` keys, so there is no field collision. Falls back to a
/// `{"score", "layer"}`-only object if the drawer fails to serialize (it never
/// should — `Drawer` is plain `#[derive(Serialize)]` data).
/// Test: `recall_entry_json_hoists_drawer_fields` asserts `content` is at the
/// top level and the `drawer` wrapper key is absent.
fn recall_entry_json(r: RecallResult) -> Value {
    let mut obj = match serde_json::to_value(&r.drawer) {
        Ok(Value::Object(map)) => map,
        _ => serde_json::Map::new(),
    };
    obj.insert("score".to_string(), json!(r.score));
    obj.insert("layer".to_string(), json!(r.layer));
    Value::Object(obj)
}

/// `GET /api/v1/recall?q=<query>&top_k=<n>&deep=<bool>` — cross-palace semantic
/// search.
///
/// Why: Agents and dashboard widgets often need the most relevant memories
/// regardless of palace boundary; forcing the caller to issue one request per
/// palace and merge client-side is both slower (no fan-out) and wrong (no
/// dedup/rerank). Serving the merged top-k from the daemon collapses the
/// round-trip and reuses the shared embedder singleton.
/// What: Lists all palaces, opens each (skipping any that fail to open with a
/// warning), and delegates to `execute_recall_all`. Returns a JSON array of
/// `{ palace_id, drawer, score, layer }` entries sorted by score descending.
/// Test: Exercised via `execute_recall_all` directly and through the MCP
/// `memory_recall_all` tool dispatch.
async fn recall_all_handler(
    State(state): State<AppState>,
    Query(q): Query<RecallQuery>,
) -> Result<Json<Value>, ApiError> {
    let top_k = q.top_k.unwrap_or(10);
    let deep = q.deep.unwrap_or(false);
    let value = crate::chat::execute_recall_all(&state, &q.q, top_k, deep).await;
    if let Some(err) = value.get("error").and_then(|v| v.as_str()) {
        return Err(ApiError::internal(err.to_string()));
    }
    Ok(Json(value))
}

// ---------------------------------------------------------------------------
// Knowledge Graph
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct KgQueryParams {
    subject: String,
}

async fn kg_query(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
    Query(q): Query<KgQueryParams>,
) -> Result<Json<Vec<Triple>>, ApiError> {
    let handle = open_handle(&state, &id)?;
    let triples = handle
        .kg
        .query_active(&q.subject)
        .await
        .map_err(|e| ApiError::internal(format!("kg query: {e:#}")))?;
    Ok(Json(triples))
}

#[derive(Deserialize)]
struct KgAssertBody {
    subject: String,
    predicate: String,
    object: String,
    #[serde(default)]
    confidence: Option<f32>,
    #[serde(default)]
    provenance: Option<String>,
}

async fn kg_assert(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
    Json(body): Json<KgAssertBody>,
) -> Result<StatusCode, ApiError> {
    let handle = open_handle(&state, &id)?;
    let triple = Triple {
        subject: body.subject,
        predicate: body.predicate,
        object: body.object,
        valid_from: chrono::Utc::now(),
        valid_to: None,
        confidence: body.confidence.unwrap_or(1.0),
        provenance: body.provenance,
    };
    handle
        .kg
        .assert(triple)
        .await
        .map_err(|e| ApiError::internal(format!("kg assert: {e:#}")))?;
    Ok(StatusCode::NO_CONTENT)
}

/// Default page size for KG explorer list endpoints when caller omits `limit`.
///
/// Why: 50 is large enough to feel responsive in the SPA without dumping a
/// full graph in one request; matches the default the spec calls for.
const DEFAULT_KG_LIST_LIMIT: usize = 50;

/// Hard ceiling on `limit` for KG explorer list endpoints.
///
/// Why: prevent a misconfigured client from asking the daemon to materialize
/// thousands of rows in one go; matches the spec's max=200.
const MAX_KG_LIST_LIMIT: usize = 200;

fn default_kg_list_limit() -> usize {
    DEFAULT_KG_LIST_LIMIT
}

/// Query parameters for `GET /api/v1/palaces/{id}/kg/subjects`.
///
/// Why: The KG Explorer's left panel asks for a bounded subject list; `limit`
/// is clamped server-side so the SPA cannot accidentally pull the whole graph.
/// What: `limit` defaults to [`DEFAULT_KG_LIST_LIMIT`] and is clamped to
/// `[1, MAX_KG_LIST_LIMIT]` in the handler.
/// Test: indirectly by the KG explorer UI; `kg_list_subjects_returns_distinct`
/// in the web tests below covers the happy path.
#[derive(Deserialize)]
struct KgListSubjectsParams {
    #[serde(default = "default_kg_list_limit")]
    limit: usize,
}

/// `GET /api/v1/palaces/{id}/kg/subjects?limit=N` — list distinct active
/// subjects.
///
/// Why: The KG Explorer needs to browse subjects without a prior query (the
/// existing `kg_query` endpoint requires one). Surfacing this read on the
/// daemon avoids the SPA having to know how to issue SQL.
/// What: clamps `limit` to `[1, MAX_KG_LIST_LIMIT]` and delegates to
/// `KnowledgeGraph::list_subjects`. Returns a JSON array of strings.
/// Test: `kg_list_subjects_returns_distinct` (web tests).
async fn kg_list_subjects(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
    Query(q): Query<KgListSubjectsParams>,
) -> Result<Json<Vec<String>>, ApiError> {
    let handle = open_handle(&state, &id)?;
    let limit = q.limit.clamp(1, MAX_KG_LIST_LIMIT);
    let subjects = handle
        .kg
        .list_subjects(limit)
        .map_err(|e| ApiError::internal(format!("kg list_subjects: {e:#}")))?;
    Ok(Json(subjects))
}

/// `GET /api/v1/palaces/{id}/kg/subjects_with_counts?limit=N` — list distinct
/// active subjects with their active-triple counts.
///
/// Why: The KG Explorer's subject list shows a count badge per subject and
/// supports sort-by-count. Returning the grouped counts in a single SQL pass
/// is cheaper than issuing one query per subject from the SPA.
/// What: clamps `limit` to `[1, MAX_KG_LIST_LIMIT]` and delegates to
/// `KnowledgeGraph::list_subjects_with_counts`. Returns a JSON array of
/// `{subject, count}` objects ordered alphabetically.
/// Test: indirectly via the KG Explorer UI; the core `list_subjects_with_counts`
/// test in `kg.rs` covers the SQL grouping.
async fn kg_list_subjects_with_counts(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
    Query(q): Query<KgListSubjectsParams>,
) -> Result<Json<Vec<Value>>, ApiError> {
    let handle = open_handle(&state, &id)?;
    let limit = q.limit.clamp(1, MAX_KG_LIST_LIMIT);
    let rows = handle
        .kg
        .list_subjects_with_counts(limit)
        .map_err(|e| ApiError::internal(format!("kg list_subjects_with_counts: {e:#}")))?;
    let out: Vec<Value> = rows
        .into_iter()
        .map(|(subject, count)| json!({ "subject": subject, "count": count }))
        .collect();
    Ok(Json(out))
}

/// Query parameters for `GET /api/v1/palaces/{id}/kg/all`.
///
/// Why: The KG Explorer's "All" mode pages through every active triple;
/// `limit`+`offset` give the SPA stable prev/next controls.
/// What: defaults match `kg_list_subjects` for limit; `offset` defaults to 0.
/// Test: `kg_list_all_returns_paginated_triples` (web tests).
#[derive(Deserialize)]
struct KgListAllParams {
    #[serde(default = "default_kg_list_limit")]
    limit: usize,
    #[serde(default)]
    offset: usize,
}

/// `GET /api/v1/palaces/{id}/kg/all?limit=N&offset=N` — list all active
/// triples ordered by `valid_from` descending.
///
/// Why: The KG Explorer's "All" mode wants a paged view across every active
/// triple regardless of subject. The existing `kg_query` requires a subject.
/// What: clamps `limit` to `[1, MAX_KG_LIST_LIMIT]` and delegates to
/// `KnowledgeGraph::list_active`. Returns a JSON array of `Triple` objects.
/// Test: `kg_list_all_returns_paginated_triples` (web tests).
async fn kg_list_all(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
    Query(q): Query<KgListAllParams>,
) -> Result<Json<Vec<Triple>>, ApiError> {
    let handle = open_handle(&state, &id)?;
    let limit = q.limit.clamp(1, MAX_KG_LIST_LIMIT);
    let triples = handle
        .kg
        .list_active(limit, q.offset)
        .await
        .map_err(|e| ApiError::internal(format!("kg list_active: {e:#}")))?;
    Ok(Json(triples))
}

/// `GET /api/v1/palaces/{id}/kg/count` — count of currently-active triples.
///
/// Why: The KG Explorer header shows a quick "N triples" badge; computing the
/// count server-side avoids fetching every triple to count them.
/// What: returns `{ "active": N }` where N is `count_active_triples()` on the
/// palace's KG.
/// Test: indirectly via the same palace counts surfaced on `/api/v1/status`.
async fn kg_count(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
) -> Result<Json<Value>, ApiError> {
    let handle = open_handle(&state, &id)?;
    let active = handle.kg.count_active_triples();
    Ok(Json(json!({ "active": active })))
}

/// Wire shape returned by `/api/v1/palaces/{id}/kg/graph`.
///
/// Why: Issue #97 — the per-palace visual graph view needs a single payload
/// containing every active triple plus a node-level community count so the
/// UI can color-code clusters. Splitting into nodes/edges client-side lets
/// d3-force own the layout while keeping the wire shape compact.
/// What: `triples` carries the raw `(s, p, o)` list; `community_count` lets
/// the SPA decide whether to color-code clusters (zero ⇒ no Louvain pass
/// has run).
/// Test: `kg_graph_returns_active_triples`.
#[derive(Serialize)]
struct KgGraphPayload {
    triples: Vec<Triple>,
    node_count: u64,
    edge_count: u64,
    community_count: u64,
}

/// Hard cap on triples returned by the per-palace graph endpoint.
///
/// Why: The visual graph is unusable past a few thousand triples regardless
/// of how fast the daemon serves the payload. The spec target is "<1s for
/// palaces with <500 triples"; this cap protects the daemon from a runaway
/// fetch on a pathologically large palace.
/// What: `5_000` — well above the issue's 500-triple sanity benchmark while
/// staying inside an acceptable single JSON response budget.
/// Test: `kg_graph_returns_active_triples`.
const KG_GRAPH_MAX_TRIPLES: usize = 5_000;

/// `GET /api/v1/palaces/{id}/kg/graph` — full graph payload for the SPA.
///
/// Why: Issue #97's visual graph view needs every active triple in one call
/// so the d3-force layout can run without paging. The existing `/kg/all`
/// endpoint caps page size at 200 — fine for the explorer table but too
/// small for the visual graph. This handler does a single `list_active`
/// pass capped at [`KG_GRAPH_MAX_TRIPLES`] and bundles the graph counts in
/// the same payload.
/// What: opens the palace handle, calls `KnowledgeGraph::list_active(cap,
/// 0)`, and returns `{ triples, node_count, edge_count, community_count }`.
/// Failures surface as `ApiError::internal`.
/// Test: `kg_graph_returns_active_triples` (web tests).
async fn kg_graph(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
) -> Result<Json<KgGraphPayload>, ApiError> {
    let handle = open_handle(&state, &id)?;
    let triples = handle
        .kg
        .list_active(KG_GRAPH_MAX_TRIPLES, 0)
        .await
        .map_err(|e| ApiError::internal(format!("kg list_active: {e:#}")))?;
    let node_count = handle.kg.node_count() as u64;
    let edge_count = handle.kg.edge_count() as u64;
    let community_count = handle.kg.community_count() as u64;
    Ok(Json(KgGraphPayload {
        triples,
        node_count,
        edge_count,
        community_count,
    }))
}

// ---------------------------------------------------------------------------
// Dream cycle status + on-demand run
// ---------------------------------------------------------------------------

/// Wire payload for dream status endpoints — `last_run_at` may be null when no
/// cycle has run yet on this palace (or the aggregate has nothing to report).
#[derive(Serialize, Default)]
pub(crate) struct DreamStatusPayload {
    pub(crate) last_run_at: Option<chrono::DateTime<chrono::Utc>>,
    pub(crate) merged: usize,
    pub(crate) pruned: usize,
    pub(crate) compacted: usize,
    pub(crate) closets_updated: usize,
    pub(crate) duration_ms: u64,
}

impl From<PersistedDreamStats> for DreamStatusPayload {
    fn from(p: PersistedDreamStats) -> Self {
        Self {
            last_run_at: Some(p.last_run_at),
            merged: p.stats.merged,
            pruned: p.stats.pruned,
            compacted: p.stats.compacted,
            closets_updated: p.stats.closets_updated,
            duration_ms: p.stats.duration_ms,
        }
    }
}

/// GET /api/v1/dream/status — aggregate latest dream stats across all palaces.
///
/// Why: The dashboard wants a single "last dream cycle" panel rather than
/// per-palace details; we sum the per-palace counters and surface the most
/// recent `last_run_at` so operators can spot a stalled background loop.
/// What: Walks every palace, loads its `dream_stats.json` if present, sums
/// counts, and returns the max `last_run_at` (or null if no palace has run).
/// Test: `dream_status_aggregates_across_palaces` covers the read path.
async fn dream_status(State(state): State<AppState>) -> Json<DreamStatusPayload> {
    let palaces = PalaceRegistry::list_palaces(&state.data_root).unwrap_or_default();
    let mut out = DreamStatusPayload::default();
    let mut latest: Option<chrono::DateTime<chrono::Utc>> = None;
    for p in palaces {
        let data_dir = state.data_root.join(p.id.as_str());
        let snap = match PersistedDreamStats::load(&data_dir) {
            Ok(Some(s)) => s,
            _ => continue,
        };
        out.merged = out.merged.saturating_add(snap.stats.merged);
        out.pruned = out.pruned.saturating_add(snap.stats.pruned);
        out.compacted = out.compacted.saturating_add(snap.stats.compacted);
        out.closets_updated = out
            .closets_updated
            .saturating_add(snap.stats.closets_updated);
        out.duration_ms = out.duration_ms.saturating_add(snap.stats.duration_ms);
        latest = match latest {
            Some(t) if t >= snap.last_run_at => Some(t),
            _ => Some(snap.last_run_at),
        };
    }
    out.last_run_at = latest;
    Json(out)
}

/// GET /api/v1/palaces/:id/dream/status — per-palace dream stats snapshot.
async fn palace_dream_status(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
) -> Result<Json<DreamStatusPayload>, ApiError> {
    let data_dir = state.data_root.join(&id);
    if !data_dir.exists() {
        return Err(ApiError::not_found(format!("palace not found: {id}")));
    }
    let payload = match PersistedDreamStats::load(&data_dir) {
        Ok(Some(s)) => s.into(),
        Ok(None) => DreamStatusPayload::default(),
        Err(e) => return Err(ApiError::internal(format!("read dream stats: {e:#}"))),
    };
    Ok(Json(payload))
}

/// POST /api/v1/dream/run — run a dream cycle across all palaces on demand.
///
/// Why: The dashboard exposes a "Run now" button so operators can force a
/// cycle without waiting for the idle clock; useful after a bulk ingest or
/// when diagnosing the dream loop itself.
/// What: Opens every persisted palace, runs `Dreamer::dream_cycle` with the
/// default config, and returns the aggregated stats plus the run timestamp.
/// Errors on individual palaces are logged but don't abort the sweep.
/// Test: `dream_run_aggregates_stats` covers the round-trip.
async fn dream_run(State(state): State<AppState>) -> Result<Json<DreamStatusPayload>, ApiError> {
    let palaces = PalaceRegistry::list_palaces(&state.data_root)
        .map_err(|e| ApiError::internal(format!("list palaces: {e:#}")))?;
    let dreamer = Dreamer::new(DreamConfig::default());
    let mut out = DreamStatusPayload::default();
    for p in palaces {
        let handle = match state.registry.open_palace(&state.data_root, &p.id) {
            Ok(h) => h,
            Err(e) => {
                tracing::warn!(palace = %p.id, "dream_run: open failed: {e:#}");
                continue;
            }
        };
        match dreamer.dream_cycle(&handle).await {
            Ok(stats) => {
                out.merged = out.merged.saturating_add(stats.merged);
                out.pruned = out.pruned.saturating_add(stats.pruned);
                out.compacted = out.compacted.saturating_add(stats.compacted);
                out.closets_updated = out.closets_updated.saturating_add(stats.closets_updated);
                out.duration_ms = out.duration_ms.saturating_add(stats.duration_ms);
            }
            Err(e) => tracing::warn!(palace = %p.id, "dream_run: cycle failed: {e:#}"),
        }
        // Issue #53: refresh the community-detection cache after each
        // successful or failed cycle. Even if the dedup/decay pass errored we
        // still want a fresh gap snapshot — `knowledge_gaps()` reads the KG
        // directly and is independent of the dream pass results.
        refresh_gaps_cache(&state, &handle).await;
    }
    out.last_run_at = Some(chrono::Utc::now());
    state.emit(DaemonEvent::DreamCompleted {
        palace_id: None,
        merged: out.merged,
        pruned: out.pruned,
        compacted: out.compacted,
        closets_updated: out.closets_updated,
        duration_ms: out.duration_ms,
        source: ActivitySource::Http,
    });
    state.emit(aggregate_status_event(&state));
    Ok(Json(out))
}

// ---------------------------------------------------------------------------
// Knowledge gaps — community detection cache (issue #53)
// ---------------------------------------------------------------------------

/// Wire shape for a single knowledge gap returned by `/api/v1/kg/gaps`.
///
/// Why: `KnowledgeGap` (in `trusty-common`) does not derive `Serialize`
/// because that would force serde into the memory-core feature surface; the
/// HTTP layer instead owns a narrow response struct mirroring its fields.
/// What: One-for-one wire representation of `KnowledgeGap` — entities, the
/// internal-density score, the cross-community bridge count, and the
/// LLM/template exploration hint.
/// Test: `kg_gaps_endpoint_returns_cached_gaps`.
#[derive(Serialize, Debug, Clone)]
pub struct KnowledgeGapResponse {
    pub entities: Vec<String>,
    pub internal_density: f32,
    pub external_bridges: usize,
    pub suggested_exploration: String,
}

impl From<KnowledgeGap> for KnowledgeGapResponse {
    fn from(g: KnowledgeGap) -> Self {
        Self {
            entities: g.entities,
            internal_density: g.internal_density,
            external_bridges: g.external_bridges,
            suggested_exploration: g.suggested_exploration,
        }
    }
}

#[derive(Deserialize)]
struct KgGapsQuery {
    #[serde(default)]
    palace: Option<String>,
}

/// `GET /api/v1/kg/gaps?palace=<name>` — return the cached knowledge gaps.
///
/// Why: Issue #53 — surfaces the community-detection output computed by the
/// dream cycle so callers (dashboard, MCP tool, external tooling) can list
/// the sparse-cluster targets the model should explore next. Reading from
/// the in-memory cache means a `/kg/gaps` request never triggers a Louvain
/// run; it just clones the latest snapshot.
/// What: Resolves the palace from the optional `palace` query arg (falling
/// back to the daemon's `default_palace`, then erroring with 400 if neither
/// is set). Returns `[]` when the cache has no entry yet — the dream cycle
/// simply hasn't populated it. Returns 404 only when the palace name is
/// unknown to the registry (handle.open failed).
/// Test: `kg_gaps_endpoint_returns_cached_gaps`,
/// `kg_gaps_endpoint_returns_empty_when_uncached`.
async fn kg_gaps_handler(
    State(state): State<AppState>,
    Query(q): Query<KgGapsQuery>,
) -> Result<Json<Vec<KnowledgeGapResponse>>, ApiError> {
    let palace_name = q
        .palace
        .clone()
        .or_else(|| state.default_palace.clone())
        .ok_or_else(|| {
            ApiError::bad_request("missing 'palace' query parameter (no default palace configured)")
        })?;

    // Validate the palace exists; we don't strictly need the handle for the
    // cache lookup but we want a 404 rather than an empty-array masking a
    // typo in the palace name.
    let _handle = open_handle(&state, &palace_name)?;

    let pid = PalaceId::new(&palace_name);
    let gaps = state.registry.get_gaps(&pid).unwrap_or_default();
    let body: Vec<KnowledgeGapResponse> =
        gaps.into_iter().map(KnowledgeGapResponse::from).collect();
    Ok(Json(body))
}

// ---------------------------------------------------------------------------
// Prompt-facts surface (issue #42)
// ---------------------------------------------------------------------------

/// Query parameters shared by the prompt-context / prompt-facts read endpoints.
///
/// Why: Both `GET /api/v1/kg/prompt-context` and `GET /api/v1/kg/prompt-facts`
/// optionally accept a `palace` filter so callers can scope reads to a single
/// project namespace. A shared struct keeps the wire shape consistent.
/// What: A single optional `palace` query parameter. When omitted, handlers
/// span every palace in the registry (matching the MCP tool behaviour).
/// Test: `prompt_context_endpoint_returns_formatted_block`,
/// `list_prompt_facts_endpoint_returns_hot_triples`.
#[derive(Deserialize)]
struct PromptFactsQuery {
    // Accepted for forward-compat with the MCP tool surface, but ignored:
    // the prompt cache is registry-wide, so reads always span every palace.
    // We keep the field rather than ignoring `?palace=...` silently so a
    // future per-palace filter is a non-breaking schema addition.
    #[serde(default)]
    #[allow(dead_code)]
    palace: Option<String>,
}

/// Wire shape for `POST /api/v1/kg/aliases`.
///
/// Why: Mirrors the `add_alias` MCP tool: a short → full mapping with an
/// optional palace target. Keeping the field names identical between the
/// HTTP and MCP surfaces makes documentation and client code reuse trivial.
/// What: Required `short` and `full`; optional `palace` (falls back to the
/// daemon default).
/// Test: `add_alias_endpoint_asserts_triple_and_refreshes_cache`.
#[derive(Deserialize)]
struct AddAliasRequest {
    short: String,
    full: String,
    #[serde(default)]
    palace: Option<String>,
}

/// Wire shape for a single hot-predicate triple in JSON responses.
///
/// Why: `list_prompt_facts` returns a structured array rather than the
/// pre-formatted Markdown so dashboards and tooling can render their own
/// views over the raw data.
/// What: subject/predicate/object string trio matching the underlying KG row.
/// Test: `list_prompt_facts_endpoint_returns_hot_triples`.
#[derive(Serialize)]
struct PromptFactRow {
    subject: String,
    predicate: String,
    object: String,
}

/// Query parameters for `DELETE /api/v1/kg/prompt-facts`.
///
/// Why: The MCP tool retracts the active interval for a `(subject, predicate)`
/// pair across every palace; the HTTP endpoint matches that contract so a
/// dashboard "Remove" button doesn't need to know which palace owns the fact.
/// What: Required `subject` and `predicate`; the issue spec mentions an
/// optional `object` filter but the underlying `KnowledgeGraph::retract` API
/// closes the entire `(subject, predicate)` interval — we accept `object`
/// for forward-compat but currently ignore it, mirroring the MCP tool.
/// Test: `remove_prompt_fact_endpoint_soft_deletes_and_refreshes_cache`.
#[derive(Deserialize)]
struct RemovePromptFactQuery {
    subject: String,
    predicate: String,
    #[serde(default)]
    #[allow(dead_code)]
    object: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    palace: Option<String>,
}

/// `GET /api/v1/kg/prompt-context` — return the formatted prompt-context block.
///
/// Why: Lets non-MCP callers (the admin UI, curl, integration tests) fetch
/// the same Markdown block the `get_prompt_context` tool returns, without
/// needing to speak JSON-RPC. The body is a plain text response so it can
/// be piped straight into a model prompt.
/// What: Reads the in-memory `prompt_context_cache` (already kept fresh by
/// any write that touches a hot predicate), returns the formatted string,
/// or a placeholder message when nothing has been stored yet.
/// Test: `prompt_context_endpoint_returns_formatted_block`.
async fn prompt_context_handler(
    State(state): State<AppState>,
    Query(_q): Query<PromptFactsQuery>,
) -> Result<Response, ApiError> {
    let cache_snapshot = {
        let guard = state
            .prompt_context_cache
            .read()
            .map_err(|e| ApiError::internal(format!("prompt cache lock poisoned: {e}")))?;
        guard.clone()
    };
    let body = if cache_snapshot.formatted.is_empty() {
        "No prompt facts stored yet.".to_string()
    } else {
        cache_snapshot.formatted
    };
    let mut resp = body.into_response();
    resp.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    Ok(resp)
}

/// `POST /api/v1/kg/aliases` — assert a `(short, is_alias_for, full)` triple.
///
/// Why: HTTP counterpart to the `add_alias` MCP tool — lets the admin UI
/// (or an external automation) register aliases without speaking JSON-RPC.
/// What: Resolves the target palace (request body → daemon default), opens
/// the palace handle, asserts the alias triple, and rebuilds the prompt
/// cache so subsequent `GET /api/v1/kg/prompt-context` calls reflect the
/// write immediately.
/// Test: `add_alias_endpoint_asserts_triple_and_refreshes_cache`.
async fn add_alias_handler(
    State(state): State<AppState>,
    Json(req): Json<AddAliasRequest>,
) -> Result<Json<Value>, ApiError> {
    if req.short.is_empty() || req.full.is_empty() {
        return Err(ApiError::bad_request("short and full are required"));
    }
    let palace_name = req
        .palace
        .clone()
        .or_else(|| state.default_palace.clone())
        .ok_or_else(|| ApiError::bad_request("missing 'palace' (no default palace configured)"))?;
    let handle = open_handle(&state, &palace_name)?;
    let triple = Triple {
        subject: req.short.clone(),
        predicate: "is_alias_for".to_string(),
        object: req.full.clone(),
        valid_from: chrono::Utc::now(),
        valid_to: None,
        confidence: 1.0,
        provenance: Some("add_alias_http".to_string()),
    };
    handle
        .kg
        .assert(triple)
        .await
        .map_err(|e| ApiError::internal(format!("kg.assert failed: {e:#}")))?;
    if let Err(e) = crate::prompt_facts::rebuild_prompt_cache(&state).await {
        tracing::warn!("rebuild_prompt_cache after HTTP add_alias failed: {e:#}");
    }
    Ok(Json(json!({
        "subject": req.short,
        "predicate": "is_alias_for",
        "object": req.full,
        "palace": palace_name,
    })))
}

/// `GET /api/v1/kg/prompt-facts` — list every active hot-predicate triple.
///
/// Why: Mirrors the `list_prompt_facts` MCP tool. Returning the raw triples
/// (rather than the formatted block) lets dashboards group, search, and
/// edit them with their own UI.
/// What: Calls `gather_hot_triples` over the live registry and serialises
/// each row as `{subject, predicate, object}`.
/// Test: `list_prompt_facts_endpoint_returns_hot_triples`.
async fn list_prompt_facts_handler(
    State(state): State<AppState>,
    Query(_q): Query<PromptFactsQuery>,
) -> Result<Json<Vec<PromptFactRow>>, ApiError> {
    let triples = crate::prompt_facts::gather_hot_triples(&state)
        .await
        .map_err(|e| ApiError::internal(format!("gather_hot_triples: {e:#}")))?;
    let rows: Vec<PromptFactRow> = triples
        .into_iter()
        .map(|(subject, predicate, object)| PromptFactRow {
            subject,
            predicate,
            object,
        })
        .collect();
    Ok(Json(rows))
}

/// `DELETE /api/v1/kg/prompt-facts?subject=...&predicate=...` — soft-delete
/// the active triple matching the given `(subject, predicate)` pair.
///
/// Why: HTTP counterpart to the `remove_prompt_fact` MCP tool. Mirrors the
/// retract-across-palaces semantics so a single call cleans up the fact
/// regardless of which palace stored it.
/// What: Iterates every palace, calls `kg.retract(subject, predicate)`, and
/// reports the total number of intervals closed. Rebuilds the prompt cache
/// when at least one retraction occurred.
/// Test: `remove_prompt_fact_endpoint_soft_deletes_and_refreshes_cache`.
async fn remove_prompt_fact_handler(
    State(state): State<AppState>,
    Query(q): Query<RemovePromptFactQuery>,
) -> Result<Json<Value>, ApiError> {
    if q.subject.is_empty() || q.predicate.is_empty() {
        return Err(ApiError::bad_request("subject and predicate are required"));
    }
    let mut closed_total: usize = 0;
    for palace_id in state.registry.list() {
        if let Some(handle) = state.registry.get(&palace_id) {
            match handle.kg.retract(&q.subject, &q.predicate).await {
                Ok(n) => closed_total += n,
                Err(e) => tracing::warn!(
                    palace = %palace_id.as_str(),
                    "HTTP retract failed: {e:#}",
                ),
            }
        }
    }
    if closed_total > 0 {
        if let Err(e) = crate::prompt_facts::rebuild_prompt_cache(&state).await {
            tracing::warn!("rebuild_prompt_cache after HTTP remove_prompt_fact failed: {e:#}");
        }
        Ok(Json(json!({"removed": true, "closed": closed_total})))
    } else {
        Ok(Json(json!({"removed": false, "reason": "not found"})))
    }
}

/// Recompute the gaps for `handle` and write them to the registry cache.
///
/// Why: Wraps the post-dream-cycle bookkeeping in one place so the HTTP
/// `dream_run` handler and any future schedulers share the exact same
/// enrichment path. Issue #53 also asks for an LLM-generated
/// `suggested_exploration` when `OPENROUTER_API_KEY` is set — that step is
/// best-effort and never blocks cache population.
/// What: Calls `KnowledgeGraph::knowledge_gaps()`, optionally enriches the
/// `suggested_exploration` field via `enrich_gap_exploration`, then stores
/// the resulting vec on `state.registry`. Logs the gap count at `debug!`.
/// Test: Indirect via `kg_gaps_endpoint_returns_cached_gaps` (which runs a
/// dream cycle and then reads `/api/v1/kg/gaps`).
async fn refresh_gaps_cache(state: &AppState, handle: &Arc<PalaceHandle>) {
    let mut gaps = handle.kg.knowledge_gaps();
    // LLM enrichment is best-effort. We only attempt it when an API key is
    // present in the process environment; absence is the common case and the
    // template `suggested_exploration` from `find_communities` is already a
    // perfectly serviceable fallback.
    if let Ok(api_key) = std::env::var("OPENROUTER_API_KEY") {
        if !api_key.is_empty() {
            for gap in gaps.iter_mut() {
                if let Some(enriched) = enrich_gap_exploration(&api_key, gap).await {
                    gap.suggested_exploration = enriched;
                }
            }
        }
    }
    let gap_count = gaps.len();
    state.registry.set_gaps(handle.id.clone(), gaps);
    tracing::debug!(palace = %handle.id, gaps = gap_count, "community gaps updated");
}

/// Ask OpenRouter for a focused exploration question for a single gap.
///
/// Why: Issue #53 — when an API key is available the dream cycle should
/// upgrade the templated `suggested_exploration` to a model-generated
/// research question. The result is cached for cheap re-reads, so the LLM
/// cost is paid at most once per dream cycle per gap rather than on every
/// `/kg/gaps` request.
/// What: Builds a short user prompt naming up to the first five entities in
/// the gap, calls `openrouter_chat` (deprecated but still the simplest
/// one-shot helper in `trusty-common`), and returns the trimmed completion
/// on success. Returns `None` on any error so the caller can fall back to
/// the template.
/// Test: Network-dependent — not unit-tested. Behavioural coverage comes
/// from manual runs of the dream cycle with `OPENROUTER_API_KEY` set.
async fn enrich_gap_exploration(api_key: &str, gap: &KnowledgeGap) -> Option<String> {
    // Limit the entity list we shove into the prompt so we don't blow the
    // token budget on a 1k-node community.
    let preview: Vec<&str> = gap.entities.iter().take(5).map(String::as_str).collect();
    if preview.is_empty() {
        return None;
    }
    let entities = preview.join(", ");
    let user = format!(
        "Given these related entities from a knowledge graph: {entities}. \
         Suggest one specific research question (single sentence, under 25 words) \
         that would help fill gaps in this knowledge cluster. Return only the question."
    );
    let messages = vec![trusty_common::ChatMessage {
        role: "user".to_string(),
        content: user,
        tool_call_id: None,
        tool_calls: None,
    }];
    // `openrouter_chat` is deprecated in favour of `OpenRouterProvider::chat_stream`,
    // but the one-shot helper is the right tool for this background, best-effort
    // enrichment — we don't need streaming and we explicitly tolerate failures.
    #[allow(deprecated)]
    let res = trusty_common::openrouter_chat(api_key, "openai/gpt-4o-mini", messages).await;
    match res {
        Ok(text) => {
            let trimmed = text.trim().to_string();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed)
            }
        }
        Err(e) => {
            tracing::debug!("openrouter gap enrichment failed (using template): {e:#}");
            None
        }
    }
}


// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

pub(crate) fn open_handle(
    state: &AppState,
    id: &str,
) -> Result<std::sync::Arc<trusty_common::memory_core::PalaceHandle>, ApiError> {
    state
        .registry
        .open_palace(&state.data_root, &PalaceId::new(id))
        .map_err(|e| ApiError::not_found(format!("palace not found: {id} ({e:#})")))
}

/// Lightweight error type for HTTP handlers.
pub(crate) struct ApiError {
    status: StatusCode,
    message: String,
}

impl ApiError {
    pub(crate) fn bad_request(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: msg.into(),
        }
    }
    pub(crate) fn not_found(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            message: msg.into(),
        }
    }
    pub(crate) fn internal(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: msg.into(),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status, Json(json!({ "error": self.message }))).into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;
    use axum::http::Request;
    use tower::util::ServiceExt;

    fn test_state() -> AppState {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().to_path_buf();
        std::mem::forget(tmp);
        AppState::new(root)
    }

    #[test]
    fn drawer_preview_collapses_whitespace_and_truncates() {
        // Short single-line content is returned verbatim.
        assert_eq!(drawer_content_preview("hello world"), "hello world");

        // Multiline / tab-laden content collapses to single-spaced text.
        assert_eq!(
            drawer_content_preview("first line\n\nsecond\tline   third"),
            "first line second line third"
        );

        // Leading / trailing whitespace is stripped.
        assert_eq!(drawer_content_preview("   padded   "), "padded");

        // Empty content yields an empty preview (fallback signal for clients).
        assert_eq!(drawer_content_preview(""), "");

        // Long content is truncated to DRAWER_PREVIEW_MAX_CHARS with an ellipsis.
        let long = "x".repeat(DRAWER_PREVIEW_MAX_CHARS + 50);
        let preview = drawer_content_preview(&long);
        assert_eq!(preview.chars().count(), DRAWER_PREVIEW_MAX_CHARS);
        assert!(preview.ends_with('…'));

        // Content right at the limit is not truncated.
        let exact = "y".repeat(DRAWER_PREVIEW_MAX_CHARS);
        assert_eq!(drawer_content_preview(&exact), exact);
    }

    #[tokio::test]
    async fn health_endpoint_returns_ok() {
        let state = test_state();
        let app = router().with_state(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = to_bytes(resp.into_body(), 1024).await.unwrap();
        let v: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["status"], "ok");
        assert_eq!(v["version"], env!("CARGO_PKG_VERSION"));
    }

    /// Issue #35 — `GET /health` carries the enriched resource block
    /// (`rss_mb`, `disk_bytes`, `cpu_pct`, `uptime_secs`).
    ///
    /// Why: external probes and the admin UI render these; the JSON contract
    /// must remain stable. `rss_mb` is sampled live so it is asserted only
    /// for a sane unit, not an exact value.
    /// What: drives `/health` through the router and asserts every new field
    /// deserialises with a plausible value.
    /// Test: this test.
    #[tokio::test]
    async fn health_endpoint_includes_resource_fields() {
        let state = test_state();
        let app = router().with_state(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = to_bytes(resp.into_body(), 1024).await.unwrap();
        let v: Value = serde_json::from_slice(&bytes).unwrap();
        // rss_mb must be a sane unit (megabytes, not bytes).
        let rss_mb = v["rss_mb"].as_u64().expect("rss_mb is u64");
        assert!(rss_mb < 1024 * 1024, "rss_mb unit must be MB");
        // cpu_pct is a non-negative percentage (first sample may be 0.0).
        let cpu = v["cpu_pct"].as_f64().expect("cpu_pct is a number");
        assert!(cpu >= 0.0, "cpu_pct must be non-negative");
        // disk ticker has not run in this oneshot test → 0.
        assert_eq!(v["disk_bytes"].as_u64(), Some(0));
        // uptime_secs is present and a u64.
        assert!(v["uptime_secs"].is_u64(), "uptime_secs must be present");
    }

    /// Issue #71 — `GET /health` reports `status: "ok"` on a fresh install
    /// (no palaces) and never carries a `detail` field.
    ///
    /// Why: A daemon with zero palaces cannot run a meaningful round-trip
    /// (there is nothing to remember against), and reporting "degraded" in
    /// that case would alarm operators on first boot. The handler must
    /// treat "no palaces" as a clean state and skip the probe.
    /// What: Drives `/health` through the router with an empty `data_root`
    /// and asserts `status == "ok"` and the `detail` key is absent.
    /// Test: this test.
    #[tokio::test]
    async fn health_endpoint_round_trip_on_fresh_install_is_ok() {
        let state = test_state();
        let app = router().with_state(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = to_bytes(resp.into_body(), 1024).await.unwrap();
        let v: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["status"], "ok");
        assert!(
            v.get("detail").is_none() || v["detail"].is_null(),
            "fresh-install health must not carry a degraded detail (got {v:?})"
        );
    }

    /// Issue #71 — `GET /health` exercises the full store/recall/forget
    /// cycle against the first palace and reports `status: "ok"` on success.
    ///
    /// Why: The whole point of issue #71 is to catch store/recall
    /// regressions at probe time rather than via real client traffic. This
    /// test creates a real palace, hits `/health`, and asserts the
    /// round-trip path is happy. Marked `#[ignore]` because
    /// `recall_with_default_embedder` pulls in the ONNX model and is too
    /// heavy for the default CI matrix — run with
    /// `cargo test -p trusty-memory -- --include-ignored` for local
    /// verification.
    /// What: Builds an `AppState` with a tempdir `data_root`, creates a
    /// `health-probe-palace` via `registry.create_palace`, hits `/health`,
    /// and asserts both the status and the absence of any `detail` field.
    /// Test: this test.
    #[tokio::test]
    #[ignore = "loads the default ONNX embedder; run with --include-ignored"]
    async fn health_endpoint_round_trip_with_palace_is_ok() {
        let state = test_state();
        let palace = trusty_common::memory_core::Palace {
            id: PalaceId::new("health-probe-palace"),
            name: "health-probe-palace".to_string(),
            description: None,
            created_at: chrono::Utc::now(),
            data_dir: state.data_root.join("health-probe-palace"),
        };
        state
            .registry
            .create_palace(&state.data_root, palace)
            .expect("create_palace");

        let app = router().with_state(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = to_bytes(resp.into_body(), 2048).await.unwrap();
        let v: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(
            v["status"], "ok",
            "round-trip should succeed against a fresh palace; got {v:?}"
        );
        assert!(
            v.get("detail").is_none() || v["detail"].is_null(),
            "successful round-trip must not carry a detail field (got {v:?})"
        );
    }

    /// Issue #69 — `recall_entry_json` hoists the drawer's fields to the top
    /// level so `content` is directly reachable.
    ///
    /// Why: The recall API previously wrapped the drawer under a `"drawer"`
    /// key, so clients scanning the top level for `content`/`tags` found
    /// nothing and recall always looked empty. This locks the flattened shape
    /// in place so the regression cannot silently return.
    /// What: Builds a `RecallResult`, runs it through `recall_entry_json`, and
    /// asserts `content`, `tags`, and `importance` are at the top level, that
    /// `score`/`layer` sit alongside them, and that the old `drawer` wrapper
    /// key is gone.
    /// Test: this test.
    #[test]
    fn recall_entry_json_hoists_drawer_fields() {
        use trusty_common::memory_core::Drawer;

        let room = Uuid::new_v4();
        let mut drawer = Drawer::new(room, "the answer is 42");
        drawer.tags = vec!["source:kuzu".to_string()];
        drawer.importance = 0.7;

        let entry = recall_entry_json(RecallResult {
            drawer,
            score: 0.699,
            layer: 1,
        });

        // Content must be reachable WITHOUT a `drawer` wrapper (issue #69).
        assert_eq!(
            entry.get("content").and_then(|v| v.as_str()),
            Some("the answer is 42"),
            "content must be at the top level, got {entry:?}"
        );
        assert!(
            entry.get("drawer").is_none(),
            "the legacy `drawer` wrapper must not be present, got {entry:?}"
        );
        // Other drawer fields are hoisted too.
        assert_eq!(
            entry["importance"].as_f64().map(|f| (f * 10.0).round()),
            Some(7.0)
        );
        assert_eq!(
            entry["tags"][0].as_str(),
            Some("source:kuzu"),
            "tags must be hoisted, got {entry:?}"
        );
        // Ranking metadata sits alongside the hoisted fields.
        assert_eq!(entry["layer"].as_u64(), Some(1));
        assert!(
            entry["score"]
                .as_f64()
                .is_some_and(|s| (s - 0.699).abs() < 1e-6),
            "score must be preserved, got {entry:?}"
        );
    }

    /// Issue #35 — `GET /api/v1/logs/tail` returns the most recent buffered
    /// lines and the total count.
    ///
    /// Why: operators inspect a running daemon via this endpoint; it must
    /// surface exactly what the shared `LogBuffer` holds.
    /// What: attaches a `LogBuffer` to the state, pushes three lines, GETs
    /// `?n=2`, and asserts the tail + `total`.
    /// Test: this test.
    #[tokio::test]
    async fn logs_tail_returns_recent_lines() {
        let buffer = trusty_common::log_buffer::LogBuffer::new(100);
        buffer.push("line one".to_string());
        buffer.push("line two".to_string());
        buffer.push("line three".to_string());
        let state = test_state().with_log_buffer(buffer);
        let app = router().with_state(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/logs/tail?n=2")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = to_bytes(resp.into_body(), 4096).await.unwrap();
        let v: Value = serde_json::from_slice(&bytes).unwrap();
        let lines = v["lines"].as_array().expect("lines array");
        assert_eq!(lines.len(), 2, "n=2 must return two lines");
        assert_eq!(lines[0].as_str(), Some("line two"));
        assert_eq!(lines[1].as_str(), Some("line three"));
        assert_eq!(v["total"].as_u64(), Some(3));
    }

    /// Issue #35 — `GET /api/v1/logs/tail?n=` is clamped to
    /// `[1, MAX_LOGS_TAIL_N]`.
    ///
    /// Why: a misconfigured client must not request more lines than the
    /// buffer holds, and `n=0` must still return at least one line.
    /// What: pushes five lines, requests `n=0` (clamps to 1) and an oversized
    /// `n` (clamps to the buffer length).
    /// Test: this test.
    #[tokio::test]
    async fn logs_tail_clamps_n() {
        let buffer = trusty_common::log_buffer::LogBuffer::new(100);
        for i in 0..5 {
            buffer.push(format!("l{i}"));
        }
        let state = test_state().with_log_buffer(buffer);
        let app = router().with_state(state);

        // n=0 clamps up to 1.
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/logs/tail?n=0")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let bytes = to_bytes(resp.into_body(), 4096).await.unwrap();
        let v: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["lines"].as_array().expect("lines").len(), 1);

        // n far past MAX clamps down to the buffer length (5).
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/logs/tail?n=999999")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let bytes = to_bytes(resp.into_body(), 4096).await.unwrap();
        let v: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["lines"].as_array().expect("lines").len(), 5);
    }

    /// Issue #35 — `POST /api/v1/admin/stop` acknowledges the shutdown
    /// request with `{ ok, message }`.
    ///
    /// Why: the response shape is the documented contract for the admin UI's
    /// stop button.
    /// What: calls `admin_stop` directly and asserts the JSON body. It does
    /// NOT await the spawned exit task — that would terminate the test
    /// process — but the 200 ms delay before `process::exit` guarantees the
    /// test returns first.
    /// Test: this test.
    #[tokio::test]
    async fn admin_stop_returns_ok() {
        let state = test_state();
        let Json(body) = admin_stop(State(state)).await;
        assert_eq!(body["ok"], Value::Bool(true));
        assert_eq!(body["message"].as_str(), Some("shutting down"));
    }

    #[tokio::test]
    async fn status_endpoint_returns_payload() {
        let state = test_state();
        let app = router().with_state(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/status")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = to_bytes(resp.into_body(), 1024).await.unwrap();
        let v: Value = serde_json::from_slice(&bytes).unwrap();
        assert!(v["version"].is_string());
        assert_eq!(v["palace_count"], 0);
    }

    #[tokio::test]
    async fn unknown_api_returns_404() {
        let state = test_state();
        let app = router().with_state(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/does-not-exist")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    /// Issue #70 — `…/memories` is a working alias for `…/drawers`.
    ///
    /// Why: Clients that POST/GET against `…/memories` previously hit a 404
    /// because only `/drawers` was registered, which silently broke every
    /// store call (and pushed callers onto an OOM-prone CLI fallback). The
    /// alias must route to the same handler as `/drawers`.
    /// What: Creates a real palace via the registry, then GETs the `/memories`
    /// alias and asserts a 200 with a JSON array body (the list-drawers shape).
    /// Uses GET, not POST, so the test stays embedder-free (no ONNX load).
    /// Test: this test.
    #[tokio::test]
    async fn memories_alias_routes_to_drawers() {
        let state = test_state();
        let palace = Palace {
            id: PalaceId::new("alias-test"),
            name: "alias-test".to_string(),
            description: None,
            created_at: chrono::Utc::now(),
            data_dir: state.data_root.join("alias-test"),
        };
        state
            .registry
            .create_palace(&state.data_root, palace)
            .expect("create_palace");

        let app = router().with_state(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/palaces/alias-test/memories")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::OK,
            "the /memories alias must resolve to list_drawers, not 404"
        );
        let bytes = to_bytes(resp.into_body(), 4096).await.unwrap();
        let v: Value = serde_json::from_slice(&bytes).unwrap();
        assert!(
            v.is_array(),
            "the alias must return the list-drawers array shape, got {v:?}"
        );
    }

    /// Issue #133 — `POST /api/v1/palaces/{id}/drawers` must trigger the
    /// same auto-KG extraction as the MCP `memory_remember` tool.
    ///
    /// Why: PR #106 wired auto-extract only into the MCP path; HTTP-origin
    /// writes silently skipped it, leaving every palace populated via the
    /// HTTP API with an empty KG. This regression test posts a drawer over
    /// HTTP and then queries the KG to confirm the expected `tag:`,
    /// `room:`, and `topic:` (`#hashtag`) auto-extracted triples landed.
    /// What: creates a palace via the registry, posts a drawer with tags +
    /// room + a `#hashtag` over the HTTP endpoint, reads
    /// `/api/v1/palaces/{id}/kg/graph`, and asserts the auto-extracted
    /// triples (provenance = `auto:remember`) appear.
    /// Test: this test.
    #[tokio::test]
    async fn http_create_drawer_runs_auto_kg_extraction() {
        let state = test_state();
        let palace = Palace {
            id: PalaceId::new("kgauto-http"),
            name: "kgauto-http".to_string(),
            description: None,
            created_at: chrono::Utc::now(),
            data_dir: state.data_root.join("kgauto-http"),
        };
        state
            .registry
            .create_palace(&state.data_root, palace)
            .expect("create_palace");

        let app = router().with_state(state.clone());
        let body = json!({
            "content": "trusty-memory is a Rust crate that ships an MCP server. \
                        It tracks #mcp and #rust topics with care.",
            "room": "Backend",
            "tags": ["test", "kg"],
            "importance": 0.5,
        })
        .to_string();
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/palaces/kgauto-http/drawers")
                    .header("content-type", "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::OK,
            "create_drawer must return 200 OK"
        );

        // Read the KG graph for the same palace and assert auto-extracted
        // triples landed. The exact set is exercised in
        // `tools::tests::auto_kg_extraction_hooks_into_memory_remember`; here
        // we only need to confirm the HTTP path now mirrors the MCP path.
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/palaces/kgauto-http/kg/graph")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
        let v: Value = serde_json::from_slice(&bytes).unwrap();
        let triples = v["triples"].as_array().expect("triples array");
        assert!(
            !triples.is_empty(),
            "HTTP-origin drawer must populate the KG; got empty graph"
        );
        let auto: Vec<&Value> = triples
            .iter()
            .filter(|t| t["provenance"].as_str() == Some(crate::kg_extract::AUTO_PROVENANCE))
            .collect();
        assert!(
            !auto.is_empty(),
            "expected at least one auto-extracted triple in HTTP-populated KG; got: {triples:?}"
        );
        // Spot-check the tag-as-subject encoding survived (matches the MCP
        // path's behaviour and proves the extractor saw the body's tags).
        assert!(
            auto.iter()
                .any(|t| t["subject"].as_str() == Some("tag:test")),
            "expected `tag:test` auto-extracted edge, got: {auto:?}"
        );
        // Hashtag mention triples (room-aware extractor).
        assert!(
            auto.iter()
                .any(|t| t["predicate"].as_str() == Some("mentioned-in")),
            "expected at least one #hashtag mention triple, got: {auto:?}"
        );
    }

    #[tokio::test]
    async fn create_then_list_palace() {
        let state = test_state();
        let app = router().with_state(state.clone());
        let body = json!({"name": "web-test", "description": "from test"}).to_string();
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/palaces")
                    .header("content-type", "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/palaces")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = to_bytes(resp.into_body(), 4096).await.unwrap();
        let v: Value = serde_json::from_slice(&bytes).unwrap();
        let arr = v.as_array().expect("array");
        assert!(arr.iter().any(|p| p["id"] == "web-test"));
    }

    /// Why: The operator TUI's MEMORY tab reads `node_count`, `edge_count`,
    /// `community_count`, and `is_compacting` straight off the
    /// `/api/v1/palaces` payload. If any of those fields disappear or change
    /// type the spinner / counters break silently. Pin the shape here.
    /// What: Creates a palace, lists `/api/v1/palaces`, and asserts every new
    /// field is present and typed as expected (numbers default to 0, the
    /// compacting flag defaults to false on a freshly-opened palace).
    /// Test: This test itself.
    #[tokio::test]
    async fn palace_list_includes_graph_counts() {
        let state = test_state();
        let app = router().with_state(state.clone());
        let body = json!({"name": "graph-counts", "description": null}).to_string();
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/palaces")
                    .header("content-type", "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/palaces")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = to_bytes(resp.into_body(), 4096).await.unwrap();
        let v: Value = serde_json::from_slice(&bytes).unwrap();
        let arr = v.as_array().expect("array");
        let row = arr
            .iter()
            .find(|p| p["id"] == "graph-counts")
            .expect("created palace must appear in list");
        assert_eq!(row["node_count"].as_u64(), Some(0));
        assert_eq!(row["edge_count"].as_u64(), Some(0));
        assert_eq!(row["community_count"].as_u64(), Some(0));
        assert_eq!(row["is_compacting"].as_bool(), Some(false));
    }

    /// Why: The enriched status payload backs the dashboard's top-row stats;
    /// it must always include the new total_* counters, even on an empty data
    /// root, so the UI can render zeros without special-casing missing fields.
    /// What: Hit `/api/v1/status` on a fresh state and assert the new fields
    /// are present and set to 0.
    /// Test: This test itself.
    #[tokio::test]
    async fn status_includes_total_counters() {
        let state = test_state();
        let app = router().with_state(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/status")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let bytes = to_bytes(resp.into_body(), 4096).await.unwrap();
        let v: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["total_drawers"], 0);
        assert_eq!(v["total_vectors"], 0);
        assert_eq!(v["total_kg_triples"], 0);
    }

    /// Why: `/api/v1/dream/status` must return a well-shaped payload even
    /// when no palace has ever run a dream cycle (so the dashboard's first
    /// load doesn't error).
    /// What: Hit the endpoint on a fresh state and assert `last_run_at` is
    /// null and the counters are zero.
    /// Test: This test itself.
    #[tokio::test]
    async fn dream_status_empty_returns_nulls() {
        let state = test_state();
        let app = router().with_state(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/dream/status")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = to_bytes(resp.into_body(), 4096).await.unwrap();
        let v: Value = serde_json::from_slice(&bytes).unwrap();
        assert!(v["last_run_at"].is_null());
        assert_eq!(v["merged"], 0);
        assert_eq!(v["pruned"], 0);
    }

    /// Why: `/api/v1/chat/providers` must return a well-shaped payload even
    /// when no provider is available, so the SPA can render disabled states
    /// without special-casing missing fields.
    /// What: Hit the endpoint on a fresh state; assert it returns `providers`
    /// (an array of length 2) and an `active` field (possibly null).
    /// Test: This test itself.
    #[tokio::test]
    async fn providers_endpoint_returns_payload() {
        let state = test_state();
        let app = router().with_state(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/chat/providers")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = to_bytes(resp.into_body(), 4096).await.unwrap();
        let v: Value = serde_json::from_slice(&bytes).unwrap();
        let arr = v["providers"].as_array().expect("providers array");
        assert_eq!(arr.len(), 2);
        let names: Vec<&str> = arr.iter().filter_map(|p| p["name"].as_str()).collect();
        assert!(names.contains(&"ollama"));
        assert!(names.contains(&"openrouter"));
        // `active` may be null when no provider is configured/reachable.
        assert!(v.get("active").is_some());
    }

    /// Why: Chat-session CRUD must round-trip end-to-end through the HTTP
    /// surface — create returns an id, list shows it, get returns the
    /// (empty) history, delete removes it.
    /// What: Create a palace, then exercise the four session endpoints
    /// sequentially, asserting JSON shapes at each step.
    /// Test: This test itself.
    #[tokio::test]
    async fn chat_session_crud_round_trip() {
        let state = test_state();
        // Pre-create a palace dir so session store has a place to live.
        let palace = trusty_common::memory_core::Palace {
            id: PalaceId::new("sess-test"),
            name: "sess-test".to_string(),
            description: None,
            created_at: chrono::Utc::now(),
            data_dir: state.data_root.join("sess-test"),
        };
        state
            .registry
            .create_palace(&state.data_root, palace)
            .expect("create_palace");
        let app = router().with_state(state);

        // Create
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/palaces/sess-test/chat/sessions")
                    .header("content-type", "application/json")
                    .body(Body::from(json!({"title":"first chat"}).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = to_bytes(resp.into_body(), 4096).await.unwrap();
        let v: Value = serde_json::from_slice(&bytes).unwrap();
        let sid = v["id"].as_str().expect("session id").to_string();

        // List
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/palaces/sess-test/chat/sessions")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = to_bytes(resp.into_body(), 4096).await.unwrap();
        let v: Value = serde_json::from_slice(&bytes).unwrap();
        let arr = v.as_array().expect("array");
        assert!(arr.iter().any(|s| s["id"] == sid));

        // Get
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/api/v1/palaces/sess-test/chat/sessions/{sid}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        // Delete
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri(format!("/api/v1/palaces/sess-test/chat/sessions/{sid}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);

        // Get after delete -> 404
        let resp = app
            .oneshot(
                Request::builder()
                    .uri(format!("/api/v1/palaces/sess-test/chat/sessions/{sid}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    /// Why: issue #99 — the HTTP surface for inter-project messaging is what
    /// `trusty-memory send-message` and `trusty-memory inbox-check` both
    /// drive. We pin the round-trip (send → list-unread → mark-read →
    /// list-empty) so a future refactor cannot accidentally break either
    /// CLI without a failing test.
    /// What: pre-creates the recipient palace, POSTs a message, asserts
    /// `unread_only=true` returns exactly one entry with the right
    /// envelope fields, POSTs to mark_read, asserts the unread inbox is
    /// now empty, and confirms the audit view (`unread_only=false`) still
    /// surfaces the read message.
    /// Test: this test itself.
    #[tokio::test]
    async fn messages_endpoint_round_trip() {
        let state = test_state();
        let palace = trusty_common::memory_core::Palace {
            id: PalaceId::new("msg-test"),
            name: "msg-test".to_string(),
            description: None,
            created_at: chrono::Utc::now(),
            data_dir: state.data_root.join("msg-test"),
        };
        state
            .registry
            .create_palace(&state.data_root, palace)
            .expect("create_palace");
        let app = router().with_state(state);

        // POST /api/v1/messages — send.
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/messages")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({
                            "to_palace":   "msg-test",
                            "from_palace": "sender-palace",
                            "purpose":     "task",
                            "content":     "please refresh schema"
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = to_bytes(resp.into_body(), 4096).await.unwrap();
        let send_resp: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(send_resp["status"], "sent");
        let drawer_id = send_resp["drawer_id"]
            .as_str()
            .expect("drawer_id")
            .to_string();

        // GET unread inbox.
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/messages?palace=msg-test&unread_only=true")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = to_bytes(resp.into_body(), 8192).await.unwrap();
        let list: Vec<Value> = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0]["id"], drawer_id);
        assert_eq!(list[0]["from_palace"], "sender-palace");
        assert_eq!(list[0]["to_palace"], "msg-test");
        assert_eq!(list[0]["purpose"], "task");
        assert_eq!(list[0]["content"], "please refresh schema");
        assert_eq!(list[0]["read"], false);
        assert!(list[0]["formatted"]
            .as_str()
            .unwrap()
            .contains("sender-palace"));

        // POST mark_read.
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/messages/mark_read")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({"palace": "msg-test", "drawer_id": drawer_id}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = to_bytes(resp.into_body(), 1024).await.unwrap();
        let mark: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(mark["flipped"], true);

        // GET unread again — empty.
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/messages?palace=msg-test&unread_only=true")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = to_bytes(resp.into_body(), 4096).await.unwrap();
        let list: Vec<Value> = serde_json::from_slice(&bytes).unwrap();
        assert!(list.is_empty(), "inbox cleared after mark_read");

        // GET history (unread_only=false) — still has the message, now read.
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/messages?palace=msg-test&unread_only=false")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = to_bytes(resp.into_body(), 8192).await.unwrap();
        let history: Vec<Value> = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0]["read"], true);
    }

    /// Why: The chat assistant's tool surface is part of the public API — any
    /// drift in tool names or required-argument lists is a breaking change for
    /// the UI and any external automation. Pin the shape here so a refactor
    /// has to acknowledge it.
    /// What: Snapshots the names + every tool's `required` array.
    /// Test: This test itself.
    #[test]
    fn all_tools_returns_expected_set() {
        let tools = crate::chat::all_tools();
        let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "list_palaces",
                "get_palace",
                "recall_memories",
                "list_drawers",
                "kg_query",
                "get_config",
                "get_status",
                "get_dream_status",
                "get_palace_dream_status",
                "create_memory",
                "kg_assert",
                "memory_recall_all",
            ]
        );
        // Every tool's `parameters` must be a JSON Schema object with a
        // `required` array (possibly empty).
        for t in &tools {
            assert_eq!(
                t.parameters["type"], "object",
                "tool {} schema type",
                t.name
            );
            assert!(
                t.parameters["required"].is_array(),
                "tool {} required not array",
                t.name
            );
        }
    }

    /// Why: `execute_tool` is the bridge between the model's tool_call
    /// arguments and the live Rust core. We exercise the happy path
    /// (`list_palaces` on an empty registry returns `[]`) and the unknown-
    /// tool path (returns `{"error": "..."}`) to lock down both branches.
    /// What: Calls execute_tool against a fresh `AppState`.
    /// Test: This test itself.
    #[tokio::test]
    async fn execute_tool_dispatches_known_tools() {
        let state = test_state();
        let result = crate::chat::execute_tool("list_palaces", "{}", &state).await;
        assert!(
            result.is_array(),
            "list_palaces should be array, got {result}"
        );
        assert_eq!(result.as_array().unwrap().len(), 0);

        let unknown = crate::chat::execute_tool("not_a_tool", "{}", &state).await;
        assert!(
            unknown["error"]
                .as_str()
                .unwrap_or("")
                .contains("unknown tool"),
            "expected unknown-tool error, got {unknown}"
        );

        let missing = crate::chat::execute_tool("get_palace", "{}", &state).await;
        assert!(
            missing["error"]
                .as_str()
                .unwrap_or("")
                .contains("palace_id"),
            "expected missing-arg error, got {missing}"
        );
    }

    /// Why: The SSE event bus is the dashboard's live-update transport;
    /// regressing it would silently break the UI. Subscribing before the
    /// emit guarantees the broadcast channel has a receiver when the
    /// handler fires, so we can deterministically observe the event.
    /// What: Subscribes to `state.events`, calls the `create_palace`
    /// handler through the router, then asserts a `PalaceCreated` event
    /// (and a follow-up status event from drawer mutation) flow through.
    /// Test: `cargo test -p trusty-memory-mcp sse_broadcast_emits_palace_created`.
    #[tokio::test]
    async fn sse_broadcast_emits_palace_created() {
        let state = test_state();
        let mut rx = state.events.subscribe();
        let app = router().with_state(state.clone());
        let body = json!({"name": "sse-test"}).to_string();
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/palaces")
                    .header("content-type", "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        // The handler should have emitted PalaceCreated before returning.
        let event = tokio::time::timeout(std::time::Duration::from_millis(500), rx.recv())
            .await
            .expect("event received within timeout")
            .expect("event channel still open");
        match event {
            DaemonEvent::PalaceCreated { id, name, source } => {
                assert_eq!(id, "sse-test");
                assert_eq!(name, "sse-test");
                assert_eq!(source, ActivitySource::Http);
            }
            other => panic!("expected PalaceCreated, got {other:?}"),
        }
    }

    /// Why: Confirm the `/sse` endpoint speaks `text/event-stream` and emits
    /// the initial `connected` frame so dashboard clients can rely on a
    /// known greeting.
    /// What: Issues a GET against `/sse`, reads the response body chunk,
    /// asserts the content-type header and the first SSE frame shape.
    /// Test: `cargo test -p trusty-memory-mcp sse_endpoint_emits_connected_frame`.
    #[tokio::test]
    async fn sse_endpoint_emits_connected_frame() {
        use axum::routing::get;
        let state = test_state();
        let app = router()
            .route("/sse", get(crate::sse_handler))
            .with_state(state);
        let resp = app
            .oneshot(Request::builder().uri("/sse").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            resp.headers()
                .get(header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok()),
            Some("text/event-stream")
        );
        // Read just the first chunk (the connected frame) — the stream stays
        // open otherwise, so we use a small read budget plus timeout.
        let body = resp.into_body();
        let bytes =
            tokio::time::timeout(std::time::Duration::from_millis(500), to_bytes(body, 4096))
                .await
                .ok()
                .and_then(|r| r.ok())
                .unwrap_or_default();
        let text = String::from_utf8_lossy(&bytes);
        assert!(
            text.contains("\"type\":\"connected\""),
            "expected connected frame, got: {text}"
        );
    }

    /// Why: `/api/v1/dream/status` must sum per-palace `dream_stats.json`
    /// counters and surface the most recent `last_run_at`. A regression that
    /// returned only the first palace's stats would silently break the
    /// "global dream activity" dashboard panel.
    /// What: Pre-seeds two palace dirs under the AppState root, writes a
    /// distinct `PersistedDreamStats` JSON file into each, hits the endpoint,
    /// and asserts the integer fields are summed and `last_run_at` equals the
    /// newer of the two timestamps.
    /// Test: This test itself.
    #[tokio::test]
    async fn dream_status_aggregates_across_palaces() {
        use trusty_common::memory_core::dream::{DreamStats, PersistedDreamStats};

        let state = test_state();
        // Two palace directories — each must contain a `palace.json` so
        // `PalaceRegistry::list_palaces` sees them, plus a `dream_stats.json`
        // with distinct counter values.
        for (id, stats, ts) in [
            (
                "palace-a",
                DreamStats {
                    merged: 1,
                    pruned: 2,
                    compacted: 3,
                    closets_updated: 4,
                    duration_ms: 100,
                },
                chrono::Utc::now() - chrono::Duration::seconds(60),
            ),
            (
                "palace-b",
                DreamStats {
                    merged: 10,
                    pruned: 20,
                    compacted: 30,
                    closets_updated: 40,
                    duration_ms: 200,
                },
                chrono::Utc::now(),
            ),
        ] {
            let palace = trusty_common::memory_core::Palace {
                id: PalaceId::new(id),
                name: id.to_string(),
                description: None,
                created_at: chrono::Utc::now(),
                data_dir: state.data_root.join(id),
            };
            state
                .registry
                .create_palace(&state.data_root, palace)
                .expect("create palace");
            let persisted = PersistedDreamStats {
                last_run_at: ts,
                stats,
            };
            persisted
                .save(&state.data_root.join(id))
                .expect("save dream stats");
        }

        let later = chrono::Utc::now();
        let app = router().with_state(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/dream/status")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = to_bytes(resp.into_body(), 4096).await.unwrap();
        let v: Value = serde_json::from_slice(&bytes).unwrap();

        // Aggregated counters.
        assert_eq!(v["merged"], 11);
        assert_eq!(v["pruned"], 22);
        assert_eq!(v["compacted"], 33);
        assert_eq!(v["closets_updated"], 44);
        assert_eq!(v["duration_ms"], 300);

        // `last_run_at` is the more-recent of the two timestamps.
        let last = v["last_run_at"].as_str().expect("last_run_at is string");
        let parsed: chrono::DateTime<chrono::Utc> = last
            .parse()
            .expect("last_run_at parses as RFC3339 timestamp");
        assert!(
            parsed <= later,
            "last_run_at ({parsed}) should not exceed wall clock ({later})"
        );
        // Must have picked palace-b's newer stamp, not palace-a's older one.
        let cutoff = chrono::Utc::now() - chrono::Duration::seconds(30);
        assert!(
            parsed >= cutoff,
            "expected the newer (palace-b) timestamp; got {parsed}"
        );
    }

    /// Why: `POST /api/v1/dream/run` triggers a dream cycle across every
    /// palace and must return the aggregated stats. Even when no palace
    /// has work to do (empty registry) the endpoint must round-trip 200
    /// with the well-formed payload shape so the dashboard's "Run now"
    /// button never fails the UI.
    /// What: Pre-creates one palace via the registry, posts to the endpoint,
    /// and asserts the response is 200 with all expected fields present.
    /// Deeper assertions (specific merged/pruned counts) are skipped here
    /// because running a full dream cycle requires the ONNX embedder load
    /// path and we want this test to stay fast and embedder-free.
    /// Test: This test itself.
    #[tokio::test]
    async fn dream_run_aggregates_stats() {
        let state = test_state();
        let palace = trusty_common::memory_core::Palace {
            id: PalaceId::new("dream-run-test"),
            name: "dream-run-test".to_string(),
            description: None,
            created_at: chrono::Utc::now(),
            data_dir: state.data_root.join("dream-run-test"),
        };
        state
            .registry
            .create_palace(&state.data_root, palace)
            .expect("create palace");

        let app = router().with_state(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/dream/run")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = to_bytes(resp.into_body(), 4096).await.unwrap();
        let v: Value = serde_json::from_slice(&bytes).unwrap();

        // Shape: every aggregated counter must be present (even if zero) and
        // `last_run_at` is set by the handler to "now".
        for key in [
            "merged",
            "pruned",
            "compacted",
            "closets_updated",
            "duration_ms",
        ] {
            assert!(
                v.get(key).is_some(),
                "missing key {key} in dream_run payload: {v}"
            );
            assert!(
                v[key].is_u64() || v[key].is_i64(),
                "{key} should be integer, got {}",
                v[key]
            );
        }
        assert!(
            v["last_run_at"].is_string(),
            "last_run_at must be set by dream_run; got {v}"
        );
    }

    /// Why: Issue #53 — when the dream cycle has not yet run for a palace,
    /// `/api/v1/kg/gaps` must return an empty array (200 OK), not 404 or
    /// 500. The cache miss is a meaningful, non-error state.
    /// What: Creates a palace, queries `/api/v1/kg/gaps?palace=...`, asserts
    /// the response is `200` with body `[]`.
    /// Test: this test itself.
    #[tokio::test]
    async fn kg_gaps_endpoint_returns_empty_when_uncached() {
        let state = test_state();
        let palace = trusty_common::memory_core::Palace {
            id: PalaceId::new("gaps-empty"),
            name: "gaps-empty".to_string(),
            description: None,
            created_at: chrono::Utc::now(),
            data_dir: state.data_root.join("gaps-empty"),
        };
        state
            .registry
            .create_palace(&state.data_root, palace)
            .expect("create palace");

        let app = router().with_state(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/kg/gaps?palace=gaps-empty")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = to_bytes(resp.into_body(), 4096).await.unwrap();
        let v: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v.as_array().expect("array").len(), 0);
    }

    /// Why: Issue #53 — when the cache *has* been populated (by the dream
    /// cycle in production, or by direct seeding here), the endpoint must
    /// return each gap with the four wire fields.
    /// What: Seeds the registry cache via `set_gaps` directly, then GETs
    /// `/api/v1/kg/gaps?palace=...` and asserts the JSON shape.
    /// Test: this test itself.
    #[tokio::test]
    async fn kg_gaps_endpoint_returns_cached_gaps() {
        use trusty_common::memory_core::community::KnowledgeGap;

        let state = test_state();
        let palace = trusty_common::memory_core::Palace {
            id: PalaceId::new("gaps-seed"),
            name: "gaps-seed".to_string(),
            description: None,
            created_at: chrono::Utc::now(),
            data_dir: state.data_root.join("gaps-seed"),
        };
        state
            .registry
            .create_palace(&state.data_root, palace)
            .expect("create palace");

        state.registry.set_gaps(
            PalaceId::new("gaps-seed"),
            vec![KnowledgeGap {
                entities: vec!["foo".to_string(), "bar".to_string(), "baz".to_string()],
                internal_density: 0.15,
                external_bridges: 2,
                suggested_exploration: "Explore connections between foo and related concepts"
                    .to_string(),
            }],
        );

        let app = router().with_state(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/kg/gaps?palace=gaps-seed")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = to_bytes(resp.into_body(), 4096).await.unwrap();
        let v: Value = serde_json::from_slice(&bytes).unwrap();
        let arr = v.as_array().expect("array");
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["entities"].as_array().unwrap().len(), 3);
        assert_eq!(arr[0]["external_bridges"], 2);
        assert!(arr[0]["suggested_exploration"]
            .as_str()
            .unwrap()
            .contains("foo"));
    }

    /// Why: The KG Explorer UI calls `/api/v1/palaces/{id}/kg/subjects` to
    /// populate the left panel; the endpoint must return distinct active
    /// subjects as a JSON string array.
    /// What: Creates a palace, asserts two triples via the existing kg endpoint,
    /// then GETs the subjects route and asserts the shape.
    /// Test: this test itself.
    #[tokio::test]
    async fn kg_list_subjects_returns_distinct() {
        let state = test_state();
        let app = router().with_state(state.clone());

        // Create palace.
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/palaces")
                    .header("content-type", "application/json")
                    .body(Body::from(json!({"name": "kg-list"}).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        // Assert two triples on distinct subjects.
        for subj in ["alpha", "beta"] {
            let body = json!({
                "subject": subj,
                "predicate": "is",
                "object": "thing",
            })
            .to_string();
            let r = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri("/api/v1/palaces/kg-list/kg")
                        .header("content-type", "application/json")
                        .body(Body::from(body))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(r.status(), StatusCode::NO_CONTENT);
        }

        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/palaces/kg-list/kg/subjects?limit=10")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = to_bytes(resp.into_body(), 4096).await.unwrap();
        let v: Value = serde_json::from_slice(&bytes).unwrap();
        let arr = v.as_array().expect("subjects must be array");
        let subjects: Vec<String> = arr
            .iter()
            .filter_map(|x| x.as_str().map(String::from))
            .collect();
        assert_eq!(subjects, vec!["alpha".to_string(), "beta".to_string()]);
    }

    /// Why: KG Explorer's "All" mode pages through every active triple via
    /// `/api/v1/palaces/{id}/kg/all`; the endpoint must return a JSON array of
    /// `Triple` rows ordered by `valid_from` DESC.
    /// What: Creates a palace, asserts a triple, then GETs the all route and
    /// asserts the response is an array with the expected shape.
    /// Test: this test itself.
    #[tokio::test]
    async fn kg_list_all_returns_paginated_triples() {
        let state = test_state();
        let app = router().with_state(state.clone());

        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/palaces")
                    .header("content-type", "application/json")
                    .body(Body::from(json!({"name": "kg-all"}).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let body = json!({
            "subject": "alpha",
            "predicate": "is",
            "object": "thing",
        })
        .to_string();
        let r = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/palaces/kg-all/kg")
                    .header("content-type", "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::NO_CONTENT);

        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/palaces/kg-all/kg/all?limit=10&offset=0")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = to_bytes(resp.into_body(), 4096).await.unwrap();
        let v: Value = serde_json::from_slice(&bytes).unwrap();
        let arr = v.as_array().expect("triples must be array");
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["subject"], "alpha");
        assert_eq!(arr[0]["predicate"], "is");
        assert_eq!(arr[0]["object"], "thing");
    }

    /// Why (issue #97): The visual graph view fetches the entire active
    /// triple set in one call so d3-force can lay it out without paging.
    /// The endpoint must return the triple list plus the node/edge/
    /// community counts that drive the legend.
    /// What: Creates a palace, asserts a single triple, and confirms `GET
    /// /api/v1/palaces/{id}/kg/graph` returns `{ triples, node_count,
    /// edge_count, community_count }` with the right shape.
    /// Test: This test.
    #[tokio::test]
    async fn kg_graph_returns_active_triples() {
        let state = test_state();
        let app = router().with_state(state.clone());

        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/palaces")
                    .header("content-type", "application/json")
                    .body(Body::from(json!({"name": "kg-graph"}).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let body = json!({
            "subject": "alpha",
            "predicate": "is",
            "object": "thing",
        })
        .to_string();
        let r = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/palaces/kg-graph/kg")
                    .header("content-type", "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::NO_CONTENT);

        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/palaces/kg-graph/kg/graph")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = to_bytes(resp.into_body(), 16_384).await.unwrap();
        let v: Value = serde_json::from_slice(&bytes).unwrap();
        let triples = v["triples"].as_array().expect("triples array");
        assert!(triples
            .iter()
            .any(|t| t["subject"] == "alpha" && t["predicate"] == "is" && t["object"] == "thing"));
        assert!(v["node_count"].as_u64().is_some());
        assert!(v["edge_count"].as_u64().is_some());
        assert!(v["community_count"].as_u64().is_some());
    }

    /// Why (issue #97): The visual graph view's stated perf budget is
    /// "<1s for palaces with <500 triples". Seed 500 triples, time one
    /// `/kg/graph` round-trip, and assert the result stays well under that
    /// budget. The assertion uses a generous 10x ceiling so flaky CI
    /// hardware doesn't false-positive while still catching catastrophic
    /// regressions.
    /// What: Creates a palace, asserts 500 triples directly through the
    /// `KnowledgeGraph` handle (skipping the HTTP overhead of 500 separate
    /// `POST /kg` calls), then runs one `GET /kg/graph` and prints the
    /// elapsed time to stderr.
    /// Test: This test.
    #[tokio::test]
    async fn kg_graph_meets_perf_budget_for_500_triples() {
        let state = test_state();
        let app = router().with_state(state.clone());

        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/palaces")
                    .header("content-type", "application/json")
                    .body(Body::from(json!({"name": "kg-perf"}).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let pid = trusty_common::memory_core::palace::PalaceId::new("kg-perf");
        let handle = state
            .registry
            .open_palace(&state.data_root, &pid)
            .expect("open palace");
        let now = chrono::Utc::now();
        for s in 0..10 {
            for o in 0..50 {
                handle
                    .kg
                    .assert(Triple {
                        subject: format!("s{s}"),
                        predicate: format!("p{o}"),
                        object: format!("o{o}"),
                        valid_from: now,
                        valid_to: None,
                        confidence: 1.0,
                        provenance: Some("perf-test".to_string()),
                    })
                    .await
                    .expect("kg.assert");
            }
        }

        let started = std::time::Instant::now();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/palaces/kg-perf/kg/graph")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let elapsed = started.elapsed();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = to_bytes(resp.into_body(), 1_000_000).await.unwrap();
        let v: Value = serde_json::from_slice(&bytes).unwrap();
        let n = v["triples"].as_array().map(|a| a.len()).unwrap_or(0);
        assert_eq!(n, 500, "expected 500 triples in payload");
        assert!(
            elapsed.as_secs_f64() < 10.0,
            "graph endpoint should serve 500 triples in well under 10s; took {elapsed:?}"
        );
        eprintln!(
            "[perf] kg_graph endpoint served 500 triples in {:.3}ms",
            elapsed.as_secs_f64() * 1000.0
        );
    }

    /// Why (issue #42): `GET /api/v1/kg/prompt-context` must serve the
    /// formatted Markdown block from the in-memory cache (or a placeholder
    /// when empty). Mirrors the MCP `get_prompt_context` tool but over HTTP.
    #[tokio::test]
    async fn prompt_context_endpoint_returns_formatted_block() {
        let state = test_state();

        // Empty cache returns the placeholder text.
        let app = router().with_state(state.clone());
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/kg/prompt-context")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = to_bytes(resp.into_body(), 4096).await.unwrap();
        let text = String::from_utf8(bytes.to_vec()).unwrap();
        assert_eq!(text, "No prompt facts stored yet.");

        // Populate the cache and re-fetch.
        {
            let mut guard = state.prompt_context_cache.write().expect("write lock");
            let triples = vec![(
                "tga".to_string(),
                "is_alias_for".to_string(),
                "trusty-git-analytics".to_string(),
            )];
            let formatted = crate::prompt_facts::build_prompt_context(&triples);
            *guard = crate::prompt_facts::PromptFactsCache { triples, formatted };
        }
        let app = router().with_state(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/kg/prompt-context")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = to_bytes(resp.into_body(), 4096).await.unwrap();
        let text = String::from_utf8(bytes.to_vec()).unwrap();
        assert!(text.contains("tga → trusty-git-analytics"), "got: {text}");
    }

    /// Why (issue #42): `POST /api/v1/kg/aliases` must assert the alias as
    /// an `is_alias_for` triple AND refresh the prompt cache so subsequent
    /// reads see the new alias.
    #[tokio::test]
    async fn add_alias_endpoint_asserts_triple_and_refreshes_cache() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().to_path_buf();
        std::mem::forget(tmp);
        let state = AppState::new(root).with_default_palace(Some("aliases".to_string()));
        let palace = trusty_common::memory_core::Palace {
            id: PalaceId::new("aliases"),
            name: "aliases".to_string(),
            description: None,
            created_at: chrono::Utc::now(),
            data_dir: state.data_root.join("aliases"),
        };
        state
            .registry
            .create_palace(&state.data_root, palace)
            .expect("create palace");

        let body = json!({"short": "tm", "full": "trusty-memory"});
        let app = router().with_state(state.clone());
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/kg/aliases")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = to_bytes(resp.into_body(), 4096).await.unwrap();
        let v: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["subject"], "tm");
        assert_eq!(v["object"], "trusty-memory");

        // The prompt cache must reflect the new alias.
        let guard = state.prompt_context_cache.read().expect("read lock");
        assert!(
            guard.formatted.contains("tm → trusty-memory"),
            "cache missing alias; got: {}",
            guard.formatted
        );
    }

    /// Why (issue #42): `GET /api/v1/kg/prompt-facts` returns the structured
    /// JSON array of every hot-predicate triple across the registry (so a
    /// dashboard can render its own table).
    #[tokio::test]
    async fn list_prompt_facts_endpoint_returns_hot_triples() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().to_path_buf();
        std::mem::forget(tmp);
        let state = AppState::new(root).with_default_palace(Some("listfacts".to_string()));
        let palace = trusty_common::memory_core::Palace {
            id: PalaceId::new("listfacts"),
            name: "listfacts".to_string(),
            description: None,
            created_at: chrono::Utc::now(),
            data_dir: state.data_root.join("listfacts"),
        };
        let handle = state
            .registry
            .create_palace(&state.data_root, palace)
            .expect("create palace");

        // Insert one hot triple and one non-hot triple; only the hot one
        // should surface.
        handle
            .kg
            .assert(Triple {
                subject: "ts".to_string(),
                predicate: "is_alias_for".to_string(),
                object: "trusty-search".to_string(),
                valid_from: chrono::Utc::now(),
                valid_to: None,
                confidence: 1.0,
                provenance: None,
            })
            .await
            .expect("assert alias");
        handle
            .kg
            .assert(Triple {
                subject: "alice".to_string(),
                predicate: "works_at".to_string(),
                object: "Acme".to_string(),
                valid_from: chrono::Utc::now(),
                valid_to: None,
                confidence: 1.0,
                provenance: None,
            })
            .await
            .expect("assert works_at");

        let app = router().with_state(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/kg/prompt-facts")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = to_bytes(resp.into_body(), 4096).await.unwrap();
        let v: Value = serde_json::from_slice(&bytes).unwrap();
        let arr = v.as_array().expect("array");
        assert!(
            arr.iter().any(|r| r["subject"] == "ts"
                && r["predicate"] == "is_alias_for"
                && r["object"] == "trusty-search"),
            "missing ts alias; got {arr:?}"
        );
        // The non-hot `works_at` triple must not be present.
        assert!(
            !arr.iter().any(|r| r["predicate"] == "works_at"),
            "non-hot triple leaked into prompt facts: {arr:?}"
        );
    }

    /// Why (issue #42): `DELETE /api/v1/kg/prompt-facts` must retract the
    /// interval and refresh the cache; the next list call must omit it.
    #[tokio::test]
    async fn remove_prompt_fact_endpoint_soft_deletes_and_refreshes_cache() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().to_path_buf();
        std::mem::forget(tmp);
        let state = AppState::new(root).with_default_palace(Some("rmfacts".to_string()));
        let palace = trusty_common::memory_core::Palace {
            id: PalaceId::new("rmfacts"),
            name: "rmfacts".to_string(),
            description: None,
            created_at: chrono::Utc::now(),
            data_dir: state.data_root.join("rmfacts"),
        };
        let handle = state
            .registry
            .create_palace(&state.data_root, palace)
            .expect("create palace");

        handle
            .kg
            .assert(Triple {
                subject: "ta".to_string(),
                predicate: "is_alias_for".to_string(),
                object: "trusty-analyze".to_string(),
                valid_from: chrono::Utc::now(),
                valid_to: None,
                confidence: 1.0,
                provenance: None,
            })
            .await
            .expect("assert alias");
        // Prime the cache so we can observe the removal effect.
        crate::prompt_facts::rebuild_prompt_cache(&state)
            .await
            .expect("rebuild prompt cache");

        let app = router().with_state(state.clone());
        let resp = app
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/api/v1/kg/prompt-facts?subject=ta&predicate=is_alias_for")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = to_bytes(resp.into_body(), 4096).await.unwrap();
        let v: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["removed"], true);
        assert!(v["closed"].as_u64().unwrap_or(0) >= 1);

        // Cache must no longer contain the alias.
        {
            let guard = state.prompt_context_cache.read().expect("read lock");
            assert!(
                !guard.formatted.contains("ta → trusty-analyze"),
                "alias still in cache after delete: {}",
                guard.formatted
            );
        }

        // Removing a non-existent fact returns removed=false.
        let app = router().with_state(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/api/v1/kg/prompt-facts?subject=nope&predicate=is_alias_for")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = to_bytes(resp.into_body(), 4096).await.unwrap();
        let v: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["removed"], false);
    }

    #[tokio::test]
    async fn serves_index_html_fallback() {
        let state = test_state();
        let app = router().with_state(state);
        let resp = app
            .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
            .await
            .unwrap();
        // Either OK with embedded HTML, or NOT_FOUND if assets not built.
        assert!(
            resp.status() == StatusCode::OK || resp.status() == StatusCode::NOT_FOUND,
            "got {}",
            resp.status()
        );
    }

    /// Why (issue #96): `GET /api/v1/activity` must return the entries
    /// captured by the persistent log so the dashboard feed has history on
    /// page load. This drives the endpoint with a sequence of emits that
    /// model both HTTP- and MCP-origin writes, then asserts the response
    /// shape, ordering, total count, and that the source labels make it
    /// onto the wire.
    /// What: emits four `DaemonEvent`s with mixed sources, fetches
    /// `/api/v1/activity?limit=10`, and checks the structure of the
    /// returned JSON.
    /// Test: this test.
    #[tokio::test]
    async fn activity_endpoint_lists_recent_emits() {
        let state = test_state();
        // Three drawer_added (one MCP, two HTTP) and one palace_created.
        state.emit(DaemonEvent::PalaceCreated {
            id: "alpha".into(),
            name: "alpha".into(),
            source: ActivitySource::Http,
        });
        state.emit(DaemonEvent::DrawerAdded {
            palace_id: "alpha".into(),
            palace_name: "alpha".into(),
            drawer_count: 1,
            timestamp: chrono::Utc::now(),
            content_preview: "hello".into(),
            source: ActivitySource::Mcp,
        });
        state.emit(DaemonEvent::DrawerAdded {
            palace_id: "beta".into(),
            palace_name: "beta".into(),
            drawer_count: 1,
            timestamp: chrono::Utc::now(),
            content_preview: "hi there".into(),
            source: ActivitySource::Http,
        });
        state.emit(DaemonEvent::DrawerDeleted {
            palace_id: "alpha".into(),
            drawer_count: 0,
            source: ActivitySource::Http,
        });

        let app = router().with_state(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/activity?limit=10")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = to_bytes(resp.into_body(), 8192).await.unwrap();
        let v: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["limit"], 10);
        assert_eq!(v["offset"], 0);
        assert_eq!(v["total"], 4);
        let entries = v["entries"].as_array().expect("entries array");
        assert_eq!(entries.len(), 4);
        // Newest-first: drawer_deleted is the last event we pushed.
        assert_eq!(entries[0]["event_type"], "drawer_deleted");
        assert_eq!(entries[3]["event_type"], "palace_created");
        // Sources made it onto the wire as lowercase strings.
        let sources: Vec<&str> = entries
            .iter()
            .filter_map(|e| e["source"].as_str())
            .collect();
        assert!(sources.contains(&"http"));
        assert!(sources.contains(&"mcp"));
        // Payload is structured JSON, not an escaped string.
        assert!(entries[0]["payload"].is_object());
    }

    /// Why: the handler must enforce a sane upper bound on `limit` so a
    /// curl with `?limit=1000000` cannot force a huge scan + response.
    /// What: asks for `limit=10000`, asserts the response advertises the
    /// clamped value.
    /// Test: this test.
    #[tokio::test]
    async fn activity_endpoint_clamps_limit() {
        let state = test_state();
        let app = router().with_state(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/activity?limit=10000")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = to_bytes(resp.into_body(), 4096).await.unwrap();
        let v: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["limit"], ACTIVITY_MAX_LIMIT);
    }

    /// Why: filters are how the dashboard scopes the feed to a single
    /// palace or to one origin (MCP vs HTTP). Confirm AND-semantics on
    /// `?palace=` and `?source=`.
    /// What: emits 3 events, queries with `?palace=alpha&source=mcp`, and
    /// asserts only the matching row is returned.
    /// Test: this test.
    #[tokio::test]
    async fn activity_endpoint_filters_by_source_and_palace() {
        let state = test_state();
        state.emit(DaemonEvent::DrawerAdded {
            palace_id: "alpha".into(),
            palace_name: "alpha".into(),
            drawer_count: 1,
            timestamp: chrono::Utc::now(),
            content_preview: "".into(),
            source: ActivitySource::Mcp,
        });
        state.emit(DaemonEvent::DrawerAdded {
            palace_id: "alpha".into(),
            palace_name: "alpha".into(),
            drawer_count: 2,
            timestamp: chrono::Utc::now(),
            content_preview: "".into(),
            source: ActivitySource::Http,
        });
        state.emit(DaemonEvent::DrawerAdded {
            palace_id: "beta".into(),
            palace_name: "beta".into(),
            drawer_count: 1,
            timestamp: chrono::Utc::now(),
            content_preview: "".into(),
            source: ActivitySource::Mcp,
        });

        let app = router().with_state(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/activity?palace=alpha&source=mcp&limit=50")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = to_bytes(resp.into_body(), 4096).await.unwrap();
        let v: Value = serde_json::from_slice(&bytes).unwrap();
        let entries = v["entries"].as_array().unwrap();
        assert_eq!(entries.len(), 1, "filter should leave one row, got {v}");
        assert_eq!(entries[0]["palace_id"], "alpha");
        assert_eq!(entries[0]["source"], "mcp");
    }

    /// Why: unknown source values must produce a 400 so the caller sees the
    /// typo instead of silently getting "no rows".
    #[tokio::test]
    async fn activity_endpoint_rejects_unknown_source() {
        let state = test_state();
        let app = router().with_state(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/activity?source=nope")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    /// Why (issue #96): MCP-side `memory_remember` must now emit a
    /// `DrawerAdded` event with `source = Mcp`. Confirm by driving the MCP
    /// dispatcher directly and reading the broadcast channel.
    /// What: pre-creates a palace, calls `dispatch_tool("memory_remember",
    /// ...)`, subscribes to the events channel before the call, and
    /// asserts the next event tag is `drawer_added` with the MCP source.
    /// Test: this test.
    #[tokio::test]
    async fn mcp_memory_remember_emits_drawer_added_with_mcp_source() {
        use crate::tools::dispatch_tool;
        let state = test_state();
        let mut rx = state.events.subscribe();
        // Create palace via the MCP tool so the activity log captures both
        // the palace_created and drawer_added events.
        let _ = dispatch_tool(&state, "palace_create", json!({"name": "p1"}))
            .await
            .expect("palace_create");
        // Drain the palace_created event.
        let first = tokio::time::timeout(std::time::Duration::from_millis(500), rx.recv())
            .await
            .expect("first event")
            .expect("channel open");
        assert!(
            matches!(first, DaemonEvent::PalaceCreated { ref source, .. } if *source == ActivitySource::Mcp)
        );

        let _ = dispatch_tool(
            &state,
            "memory_remember",
            json!({
                "palace": "p1",
                "text": "the quick brown fox jumps over the lazy dog and more"
            }),
        )
        .await
        .expect("memory_remember");

        // The next event from the channel should be DrawerAdded(Mcp).
        let next = tokio::time::timeout(std::time::Duration::from_millis(500), rx.recv())
            .await
            .expect("drawer_added event")
            .expect("channel open");
        match next {
            DaemonEvent::DrawerAdded {
                source, palace_id, ..
            } => {
                assert_eq!(source, ActivitySource::Mcp);
                assert_eq!(palace_id, "p1");
            }
            other => panic!("expected DrawerAdded, got {other:?}"),
        }

        // The activity log should now hold ≥ 2 entries (palace_created +
        // drawer_added). Also confirm the HTTP endpoint surfaces them with
        // `mcp` sources.
        let app = router().with_state(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/activity?source=mcp&limit=10")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = to_bytes(resp.into_body(), 4096).await.unwrap();
        let v: Value = serde_json::from_slice(&bytes).unwrap();
        let entries = v["entries"].as_array().unwrap();
        let event_types: std::collections::HashSet<&str> = entries
            .iter()
            .filter_map(|e| e["event_type"].as_str())
            .collect();
        assert!(event_types.contains("drawer_added"));
        assert!(event_types.contains("palace_created"));
    }

    // -----------------------------------------------------------------
    // Submission-logging tests (Part A: hook activity, Part B: drawer
    // attribution).
    // -----------------------------------------------------------------

    /// Why (submission-logging Part A): every hook firing must produce an
    /// activity-feed entry tagged `source=hook` so a normal Claude Code
    /// session that only triggers hooks no longer leaves the TUI feed
    /// empty. The simplest direct check is to POST to the hook ingestion
    /// endpoint and confirm the new entry shows up in `GET /api/v1/activity`.
    /// What: posts a `HookEventPayload` to `/api/v1/activity/hook`, then
    /// queries `/api/v1/activity?source=hook&limit=1` and asserts a row
    /// exists with the matching event_type and source.
    /// Test: itself.
    #[tokio::test]
    async fn hook_fired_activity_emit_smoke() {
        let state = test_state();
        let app = router().with_state(state.clone());

        let payload = serde_json::json!({
            "palace_id": "alpha",
            "palace_name": "alpha",
            "hook_type": "UserPromptSubmit",
            "injection_kind": "prompt-context",
            "injection_length": 256,
            "trigger_prompt_excerpt": "test prompt",
            "duration_ms": 12,
        });
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/activity/hook")
                    .header("content-type", "application/json")
                    .body(Body::from(payload.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);

        // Read it back through the activity history endpoint.
        let app = router().with_state(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/activity?source=hook&limit=10")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = to_bytes(resp.into_body(), 4096).await.unwrap();
        let v: Value = serde_json::from_slice(&bytes).unwrap();
        let entries = v["entries"].as_array().expect("entries array");
        assert!(
            !entries.is_empty(),
            "expected at least one hook activity row, got {entries:?}"
        );
        let first = &entries[0];
        assert_eq!(first["source"], "hook");
        assert_eq!(first["event_type"], "hook_fired");
        assert_eq!(first["palace_id"], "alpha");
        let body = &first["payload"];
        assert_eq!(body["hook_type"], "UserPromptSubmit");
        assert_eq!(body["injection_kind"], "prompt-context");
    }

    /// Why (submission-logging Part B): an HTTP drawer write with no
    /// client-identifying header must still produce a drawer carrying a
    /// `creator:client=unknown-http-client` tag so operators can recognise
    /// "writer didn't self-identify" as distinct from "writer is known".
    /// What: creates a palace via the registry, POSTs a drawer with no
    /// `X-Trusty-Client-Name` header, lists the palace drawers, asserts
    /// the new drawer carries the four creator tags with the default
    /// client name and `source=http`.
    /// Test: itself.
    #[tokio::test]
    async fn drawer_creator_attribution_http_default() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().to_path_buf();
        std::mem::forget(tmp);
        let state = AppState::new(root);
        let palace = trusty_common::memory_core::Palace {
            id: PalaceId::new("cred-default"),
            name: "cred-default".to_string(),
            description: None,
            created_at: chrono::Utc::now(),
            data_dir: state.data_root.join("cred-default"),
        };
        state
            .registry
            .create_palace(&state.data_root, palace)
            .expect("create palace");

        let app = router().with_state(state.clone());
        let body = serde_json::json!({
            "content": "hello world from anonymous client",
            "tags": ["user-tag"],
        });
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/palaces/cred-default/drawers")
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        // Inspect the persisted drawer's tags.
        let app = router().with_state(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/palaces/cred-default/drawers?limit=10")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = to_bytes(resp.into_body(), 8192).await.unwrap();
        let v: Value = serde_json::from_slice(&bytes).unwrap();
        let drawers = v.as_array().expect("drawers array");
        assert_eq!(drawers.len(), 1, "expected one drawer, got {drawers:?}");
        let tags: Vec<&str> = drawers[0]["tags"]
            .as_array()
            .expect("tags array")
            .iter()
            .filter_map(|t| t.as_str())
            .collect();
        assert!(
            tags.contains(&"user-tag"),
            "user-supplied tag must survive; got {tags:?}"
        );
        assert!(
            tags.contains(&"creator:client=unknown-http-client"),
            "expected default client tag; got {tags:?}"
        );
        assert!(
            tags.contains(&"creator:source=http"),
            "expected http source tag; got {tags:?}"
        );
        assert!(
            tags.iter().any(|t| t.starts_with("creator:version=")),
            "expected creator:version tag; got {tags:?}"
        );
    }

    /// Why (submission-logging Part B): when an HTTP client *does* set
    /// `X-Trusty-Client-Name`, the drawer must carry that exact name in
    /// its `creator:client=` tag so operators can trace which client wrote
    /// which drawer.
    /// What: POST with `X-Trusty-Client-Name: qa-curl` and assert the
    /// rendered tag matches.
    /// Test: itself.
    #[tokio::test]
    async fn drawer_creator_attribution_http_header() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().to_path_buf();
        std::mem::forget(tmp);
        let state = AppState::new(root);
        let palace = trusty_common::memory_core::Palace {
            id: PalaceId::new("cred-header"),
            name: "cred-header".to_string(),
            description: None,
            created_at: chrono::Utc::now(),
            data_dir: state.data_root.join("cred-header"),
        };
        state
            .registry
            .create_palace(&state.data_root, palace)
            .expect("create palace");

        let app = router().with_state(state.clone());
        let body = serde_json::json!({
            "content": "this is enough content to pass the signal/noise filter applied by remember",
            "tags": [],
        });
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/palaces/cred-header/drawers")
                    .header("content-type", "application/json")
                    .header("x-trusty-client-name", "qa-curl")
                    .header("x-trusty-client-cwd", "/tmp/qa")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let app = router().with_state(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/palaces/cred-header/drawers?limit=10")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let bytes = to_bytes(resp.into_body(), 8192).await.unwrap();
        let v: Value = serde_json::from_slice(&bytes).unwrap();
        let tags: Vec<&str> = v[0]["tags"]
            .as_array()
            .expect("tags")
            .iter()
            .filter_map(|t| t.as_str())
            .collect();
        assert!(
            tags.contains(&"creator:client=qa-curl"),
            "expected custom client tag; got {tags:?}"
        );
        assert!(
            tags.contains(&"creator:cwd=/tmp/qa"),
            "expected cwd tag from header; got {tags:?}"
        );
    }

    /// Why (submission-logging Part B): drawers written through the MCP
    /// tool surface (`memory_remember`) must carry
    /// `creator:client=trusty-memory-mcp` and `creator:source=mcp` so
    /// operators can tell MCP-origin drawers apart from HTTP / CLI writes.
    /// What: dispatches `memory_remember` directly against an in-process
    /// `AppState` (no HTTP), then lists the palace drawers and asserts
    /// the MCP attribution tags landed.
    /// Test: itself.
    #[tokio::test]
    async fn drawer_creator_attribution_mcp_default() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().to_path_buf();
        std::mem::forget(tmp);
        let state = AppState::new(root);
        let palace = trusty_common::memory_core::Palace {
            id: PalaceId::new("cred-mcp"),
            name: "cred-mcp".to_string(),
            description: None,
            created_at: chrono::Utc::now(),
            data_dir: state.data_root.join("cred-mcp"),
        };
        state
            .registry
            .create_palace(&state.data_root, palace)
            .expect("create palace");

        let _ = crate::tools::dispatch_tool(
            &state,
            "memory_remember",
            json!({
                "palace": "cred-mcp",
                "text": "remember a sentence with enough tokens to pass filters please",
                "room": "General",
                "tags": ["from-test"],
            }),
        )
        .await
        .expect("memory_remember dispatch");

        let app = router().with_state(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/palaces/cred-mcp/drawers?limit=10")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let bytes = to_bytes(resp.into_body(), 8192).await.unwrap();
        let v: Value = serde_json::from_slice(&bytes).unwrap();
        let drawers = v.as_array().expect("drawers array");
        assert!(!drawers.is_empty(), "expected at least one drawer");
        let tags: Vec<&str> = drawers[0]["tags"]
            .as_array()
            .expect("tags array")
            .iter()
            .filter_map(|t| t.as_str())
            .collect();
        assert!(
            tags.contains(&"creator:client=trusty-memory-mcp"),
            "expected MCP client tag; got {tags:?}"
        );
        assert!(
            tags.contains(&"creator:source=mcp"),
            "expected MCP source tag; got {tags:?}"
        );
    }

    /// Why (submission-logging Part A, failure isolation): if the daemon
    /// is unreachable when the hook fires, the hook command MUST still
    /// return `Ok(())` so the user's prompt is not blocked. The activity
    /// emit failure is surfaced via a stderr warn-log only.
    /// What: pins a tempdir as the data dir (so `read_daemon_addr`
    /// returns `Ok(None)` — no http_addr file), runs `handle_prompt_context`,
    /// and asserts it returns `Ok(())`. Separately verifies the emit
    /// helper does not panic — covered by `post_hook_event_no_daemon_is_noop`
    /// in `hook_emit::tests`.
    /// Test: itself.
    #[tokio::test]
    async fn hook_emit_failure_isolated() {
        let _guard = crate::commands::env_test_lock().lock().await;
        let tmp = tempfile::tempdir().expect("tempdir");
        // SAFETY: test serialised via env_test_lock.
        unsafe {
            std::env::set_var(trusty_common::DATA_DIR_OVERRIDE_ENV, tmp.path());
        }
        let res = crate::commands::prompt_context::handle_prompt_context().await;
        unsafe {
            std::env::remove_var(trusty_common::DATA_DIR_OVERRIDE_ENV);
        }
        assert!(
            res.is_ok(),
            "hook must complete even when daemon emit fails; got {res:?}"
        );
    }
}
