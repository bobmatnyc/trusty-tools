//! Administrative, config, log-tail, and status-stream handlers.
//!
//! Why: Groups operational endpoints (log tailing, live config, graceful stop,
//! and the SSE dashboard feed) separately from domain search logic.
//! What: `logs_tail_handler`, `admin_stop_handler`, `get_config_handler`,
//! `patch_config_handler`, `status_stream_handler`, `collect_status_counts`.
//! Test: `logs_tail_returns_recent_lines`, `admin_stop_returns_ok`, and
//! `patch_config_partial_update` in `super::tests`.
use axum::{
    body::Body,
    extract::{Query, State},
    response::{IntoResponse, Response},
    Json,
};
use futures::stream::{self, StreamExt};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;
use tokio_stream::wrappers::BroadcastStream;

use super::state::SearchAppState;

/// Query parameters for `GET /logs/tail`.
///
/// Why (issue #35): callers ask for a bounded number of recent log lines;
/// `n` defaults to a useful page size and is clamped server-side so a
/// misconfigured client cannot request more lines than the buffer holds.
/// What: `n` is optional; absent → [`DEFAULT_LOGS_TAIL_N`]. Clamped to
/// `[1, MAX_LOGS_TAIL_N]` in the handler.
/// Test: `logs_tail_clamps_n` exercises the clamp.
#[derive(Deserialize)]
pub struct LogsTailParams {
    #[serde(default = "default_logs_tail_n")]
    pub n: usize,
}

/// Default number of log lines returned by `GET /logs/tail` when `n` is
/// absent. 100 lines is enough context for a glance without a huge payload.
const DEFAULT_LOGS_TAIL_N: usize = 100;

/// Hard ceiling on `GET /logs/tail?n=` — equal to the ring-buffer capacity,
/// so a request can never ask for more lines than the buffer can hold.
pub(super) const MAX_LOGS_TAIL_N: usize = trusty_common::log_buffer::DEFAULT_LOG_CAPACITY;

fn default_logs_tail_n() -> usize {
    DEFAULT_LOGS_TAIL_N
}

/// `GET /logs/tail?n=200` — return the most recent N tracing log lines.
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
pub(super) async fn logs_tail_handler(
    State(state): State<Arc<SearchAppState>>,
    Query(params): Query<LogsTailParams>,
) -> Json<serde_json::Value> {
    let n = params.n.clamp(1, MAX_LOGS_TAIL_N);
    let lines = state.log_buffer.tail(n);
    Json(serde_json::json!({
        "lines": lines,
        "total": state.log_buffer.len(),
    }))
}

/// `POST /admin/stop` — request a graceful shutdown of the daemon.
///
/// Why (issue #35): the admin UI and operators want a one-call way to stop
/// the daemon without resolving its PID and sending a signal. The daemon is
/// localhost-only and trusts every caller, so no auth is required.
/// What: spawns a detached task that sleeps 200 ms (giving this HTTP response
/// time to flush to the client) and then calls `std::process::exit(0)`.
/// Returns `{ "ok": true, "message": "shutting down" }` immediately.
/// Test: `admin_stop_returns_ok` asserts the response shape (it does not
/// drive the real exit — that would terminate the test process).
pub(super) async fn admin_stop_handler(
    State(_state): State<Arc<SearchAppState>>,
) -> Json<serde_json::Value> {
    tracing::warn!("admin_stop: shutdown requested via POST /admin/stop");
    tokio::spawn(async {
        tokio::time::sleep(Duration::from_millis(200)).await;
        std::process::exit(0);
    });
    Json(serde_json::json!({ "ok": true, "message": "shutting down" }))
}

/// Request body for `PATCH /config`. Any field may be omitted to leave that
/// limit unchanged; an explicit `null` disables the limit. Unknown JSON keys
/// are tolerated (serde's default `deny_unknown_fields` is off) so future
/// versions can introduce new keys without breaking older clients.
///
/// Why: backs `trusty-search config set <key> <value>` — operators tune the
/// daemon's memory limits without dropping the embedder model or any indexes
/// (which a full restart would cost). Patch semantics are the right HTTP
/// shape because only the fields the client cares about are sent.
/// What: serde flags distinguish "absent" (`Option::None`, leave alone) from
/// "explicit null" (`Some(None)`, disable). We use the
/// `serde_with::rust::double_option` idiom by representing each field as
/// `Option<Option<u64>>`.
/// Test: `tests::patch_config_partial_update` exercises both arms.
#[derive(Debug, Deserialize, Default)]
pub(super) struct PatchConfigRequest {
    #[serde(default, deserialize_with = "deserialize_optional_option_u64")]
    memory_limit_mb: Option<Option<u64>>,
    #[serde(default, deserialize_with = "deserialize_optional_option_u64")]
    index_memory_limit_mb: Option<Option<u64>>,
}

/// Response body for `GET /config` and `PATCH /config`.
///
/// Why: always returns the resolved current values for both limits after any
/// changes have been applied. Lets the CLI print "before → after" without
/// issuing a follow-up GET.
/// What: `null` field means the limit is disabled (no cap). Field names match
/// the env-var-derived keys (`memory_limit_mb` / `index_memory_limit_mb`) for
/// symmetry with the request body.
#[derive(Debug, Serialize)]
pub(super) struct ConfigResponse {
    memory_limit_mb: Option<u64>,
    index_memory_limit_mb: Option<u64>,
}

/// Custom deserializer for `Option<Option<u64>>` so we can tell "field absent"
/// (no change) from "field present and null" (disable the limit). Serde's
/// default skips `null` for `Option<u64>`, collapsing both cases — we need to
/// preserve the distinction to support partial updates.
///
/// Why: PATCH semantics require this three-state encoding.
/// What: returns `Some(Some(n))` for a numeric value, `Some(None)` for null,
/// and the outer `Option::None` is supplied by `#[serde(default)]` when the
/// field is absent entirely.
/// Test: `tests::patch_config_partial_update`.
fn deserialize_optional_option_u64<'de, D>(deserializer: D) -> Result<Option<Option<u64>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let v = Option::<u64>::deserialize(deserializer)?;
    Ok(Some(v))
}

/// `GET /config` — return the daemon's current memory-limit configuration.
///
/// Why: `trusty-search config get` reads this to print the live values, which
/// may differ from what's in `daemon.env` if the operator has already issued
/// a `PATCH /config` call. Pure read; no side effects.
/// What: snapshots `memory_limit_mb()` and `index_memory_limit_mb()` and
/// returns them as JSON. `null` means "no limit configured".
/// Test: `tests::get_config_returns_current_values`.
pub(super) async fn get_config_handler(
    State(_state): State<Arc<SearchAppState>>,
) -> Json<ConfigResponse> {
    use crate::core::memguard::{index_memory_limit_mb, memory_limit_mb};
    Json(ConfigResponse {
        memory_limit_mb: memory_limit_mb(),
        index_memory_limit_mb: index_memory_limit_mb(),
    })
}

/// `PATCH /config` — update the daemon's runtime memory-limit configuration.
///
/// Why: lets `trusty-search config set <key> <value>` retune memory limits
/// without a daemon restart (preserves the 86 MB embedder session and all
/// loaded indexes). The `AtomicU64` cells in `core::memguard` mean the
/// background memory poller observes the change on its next tick.
/// What: applies `memory_limit_mb` and/or `index_memory_limit_mb` from the
/// request body, logs each change at `INFO`, and returns the resolved
/// post-update values. Omitted fields are not touched. `null` disables the
/// corresponding limit. Always returns `200 OK` with a `ConfigResponse`.
/// Test: `tests::patch_config_partial_update` and
/// `tests::patch_config_disables_limit_with_null`.
pub(super) async fn patch_config_handler(
    State(_state): State<Arc<SearchAppState>>,
    Json(req): Json<PatchConfigRequest>,
) -> Json<ConfigResponse> {
    use crate::core::memguard::{
        index_memory_limit_mb, memory_limit_mb, set_index_memory_limit_mb, set_memory_limit_mb,
    };

    let fmt = |v: Option<u64>| match v {
        Some(mb) => mb.to_string(),
        None => "unlimited".to_string(),
    };

    if let Some(new) = req.memory_limit_mb {
        let before = memory_limit_mb();
        set_memory_limit_mb(new);
        let after = memory_limit_mb();
        tracing::info!(
            "config updated: memory_limit_mb {} → {}",
            fmt(before),
            fmt(after)
        );
    }
    if let Some(new) = req.index_memory_limit_mb {
        let before = index_memory_limit_mb();
        set_index_memory_limit_mb(new);
        let after = index_memory_limit_mb();
        tracing::info!(
            "config updated: index_memory_limit_mb {} → {}",
            fmt(before),
            fmt(after)
        );
    }

    Json(ConfigResponse {
        memory_limit_mb: memory_limit_mb(),
        index_memory_limit_mb: index_memory_limit_mb(),
    })
}

/// Snapshot used by both `/health` (one-shot) and `/status/stream` (SSE tick).
///
/// Why: The dashboard needs live counts of registered indexes + total chunks
/// across the whole daemon. Computing this requires acquiring a read-lock on
/// every indexer, so the work is centralised here to keep the SSE loop tidy.
/// What: Returns `(indexes_count, total_chunks)` summed across the registry.
/// Test: Register two indexes seeded with one file each; the helper returns
/// `(2, chunks_in_file_a + chunks_in_file_b)`.
pub(super) async fn collect_status_counts(state: &SearchAppState) -> (usize, usize) {
    let ids = state.registry.list();
    let indexes_count = ids.len();
    let mut total_chunks: usize = 0;
    for id in ids {
        if let Some(handle) = state.registry.get(&id) {
            let indexer = handle.indexer.read().await;
            // Issue #681: prefer durable corpus count (accurate post-eviction).
            let count = indexer
                .corpus_arc()
                .and_then(|c| c.chunk_count().ok())
                .unwrap_or_else(|| indexer.chunk_count());
            total_chunks = total_chunks.saturating_add(count);
        }
    }
    (indexes_count, total_chunks)
}

/// `GET /status/stream` — Server-Sent Events stream of live daemon stats.
///
/// Why: The admin dashboard's headline stat cards (Indexes, Documents,
/// Uptime, Version) should update without a manual refresh. Mirrors the
/// trusty-memory `/sse` pattern — subscribers receive `DaemonEvent` frames
/// pushed via the shared `broadcast::Sender` on `SearchAppState`.
/// What: Subscribes to `state.events`, emits an initial `{"type":"connected"}`
/// frame, then forwards every `DaemonEvent` as `data: <json>\n\n`. Lagged
/// subscribers receive a `{"type":"lag","skipped":N}` frame. The 2s status
/// cadence is supplied by the background ticker spawned in `build_router`.
/// Test: `curl -N http://127.0.0.1:7878/status/stream` shows a `connected`
/// frame immediately and a `status_changed` frame every ~2s.
pub(super) async fn status_stream_handler(
    State(state): State<Arc<SearchAppState>>,
) -> impl IntoResponse {
    let rx = state.events.subscribe();
    let initial = stream::once(async {
        Ok::<axum::body::Bytes, std::io::Error>(axum::body::Bytes::from(
            "data: {\"type\":\"connected\"}\n\n",
        ))
    });
    let events = BroadcastStream::new(rx).map(|res| {
        let frame = match res {
            Ok(event) => match serde_json::to_string(&event) {
                Ok(json) => format!("data: {json}\n\n"),
                Err(e) => format!("data: {{\"type\":\"error\",\"message\":\"{e}\"}}\n\n"),
            },
            Err(tokio_stream::wrappers::errors::BroadcastStreamRecvError::Lagged(n)) => {
                format!("data: {{\"type\":\"lag\",\"skipped\":{n}}}\n\n")
            }
        };
        Ok::<axum::body::Bytes, std::io::Error>(axum::body::Bytes::from(frame))
    });
    let stream = initial.chain(events);

    Response::builder()
        .header("Content-Type", "text/event-stream")
        .header("Cache-Control", "no-cache")
        .header("X-Accel-Buffering", "no")
        .body(Body::from_stream(stream))
        .expect("valid SSE response")
}
