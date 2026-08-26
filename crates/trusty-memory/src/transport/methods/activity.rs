//! The activity history, and the live feed that replaces `/sse` (#6286).
//!
//! Why: `GET /api/v1/activity` seeded the console's activity feed on mount;
//! without it the pane rendered empty until the next live event. The hook
//! ingestion route it shared a file with is NOT folded — `hook_fired` was
//! already a dispatcher method, and the route was its duplicate.
//!
//! [`activity_stream`] is what `/sse` was. The first pass at this migration
//! retired that listener with nothing in its place, so the monitor TUI polled
//! `memory.activity` on a 2-second tick — an event appeared up to two seconds
//! late, and one evicted from the log between two ticks was never seen at all.
//! This streams from the SAME `broadcast::Sender<DaemonEvent>` the SSE handler
//! subscribed to, so an event reaches a reader as it is emitted.
//!
//! What: `memory.activity` (a page, with the same filters and the same clamp)
//! and `memory.activity_stream` (every event from now on, as it happens).
//! Test: `super::super::uds::tests` — `rpc_activity_*`.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::mpsc;
use trusty_common::uds::server::{RpcError, RpcStreamItems};

use crate::transport::api_error::ApiError;
use crate::{ActivityFilter, ActivitySource, AppState};

use super::parse_iso_or_bad_request;

/// Default page size — the console's 50-row window.
const ACTIVITY_DEFAULT_LIMIT: usize = 50;

/// Ceiling on one page.
///
/// Bounds both the per-request work and the frame size. 500 is large enough for
/// ad-hoc inspection without becoming a lever.
const ACTIVITY_MAX_LIMIT: usize = 500;

/// Params for `memory.activity`. Every filter is optional and they combine
/// with AND.
#[derive(Debug, Default, Deserialize)]
pub struct ActivityParams {
    /// Page size, clamped to `[1, ACTIVITY_MAX_LIMIT]`.
    #[serde(default)]
    pub limit: Option<usize>,
    /// Rows to skip.
    #[serde(default)]
    pub offset: Option<usize>,
    /// Restrict to one palace.
    #[serde(default)]
    pub palace: Option<String>,
    /// `http` | `mcp` | `hook`.
    #[serde(default)]
    pub source: Option<String>,
    /// RFC 3339 lower bound.
    #[serde(default)]
    pub since: Option<String>,
    /// RFC 3339 upper bound.
    #[serde(default)]
    pub until: Option<String>,
}

/// One row of the activity response.
///
/// The persisted entry carries `payload` as a JSON-encoded STRING so the stored
/// schema is decoupled from `DaemonEvent`'s evolution; it is re-decoded here so
/// the caller receives an object rather than an escaped string.
#[derive(Debug, Serialize)]
pub struct ActivityRow {
    /// Monotonic row id.
    pub id: u64,
    /// When the event was emitted.
    pub timestamp: chrono::DateTime<chrono::Utc>,
    /// Which transport produced it.
    pub source: &'static str,
    /// The palace it concerned, when it concerned one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub palace_id: Option<String>,
    /// The `DaemonEvent` variant name.
    pub event_type: String,
    /// The event's own body.
    pub payload: Value,
}

/// `memory.activity` — a page of activity history (#96).
///
/// Answers `{entries, total, limit, offset}` so the caller can tell whether
/// more rows exist without a second call.
pub async fn activity(state: &AppState, params: ActivityParams) -> Result<Value, ApiError> {
    let limit = params
        .limit
        .unwrap_or(ACTIVITY_DEFAULT_LIMIT)
        .clamp(1, ACTIVITY_MAX_LIMIT);
    let offset = params.offset.unwrap_or(0);

    let source = match params.source.as_deref() {
        Some(s) => match ActivitySource::parse(s) {
            Some(parsed) => Some(parsed),
            None => {
                return Err(ApiError::bad_request(format!(
                    "unknown source '{s}'; expected one of http, mcp, hook"
                )));
            }
        },
        None => None,
    };

    let filter = ActivityFilter {
        palace_id: params.palace.filter(|s| !s.is_empty()),
        source,
        since: parse_iso_or_bad_request(params.since.as_deref(), "since")?,
        until: parse_iso_or_bad_request(params.until.as_deref(), "until")?,
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

    super::to_value(serde_json::json!({
        "entries": rows,
        "total": total,
        "limit": limit,
        "offset": offset,
    }))
}

/// How many events the daemon buffers for one slow reader.
///
/// Why: the producer is a broadcast subscriber and the consumer is a socket
/// write, so a reader that stalls has to be bounded somewhere. 256 is four
/// times the broadcast channel's own capacity, which means the broadcast
/// channel's lag guard fires first for a reader that stops draining — and lag
/// is a frame this method can report, where a full mpsc would only block.
const STREAM_BUFFER: usize = 256;

/// `memory.activity_stream` — every event from now on, as it happens (#6286).
///
/// Why: this is what `/sse` was. Polling `memory.activity` on a tick — the
/// stopgap the first migration pass left the TUI with — shows an event up to
/// one tick late and never shows one evicted from the log between two ticks.
///
/// What: subscribes to the same `broadcast::Sender<DaemonEvent>` the SSE
/// handler used and forwards each event as one `"stream":"item"` frame,
/// carrying the same `type`-tagged body the SSE `data:` lines carried.
///
/// **History is NOT replayed.** The stream begins at the subscription, exactly
/// as `/sse` did; a reader that wants what came before asks `memory.activity`.
/// Saying so is the whole contract: a caller that assumed a replay would show
/// an empty pane and conclude the daemon is idle.
///
/// **A lagged reader gets a frame, not a silent gap.** A `broadcast` receiver
/// that falls behind drops the events it missed. Reporting that as
/// `{"lagged": N}` is what lets a reader say so rather than render a
/// continuous-looking feed with a hole in it.
///
/// The stream ends when the daemon shuts down (the broadcast sender is
/// dropped) or when the reader disconnects, which closes the mpsc and stops
/// the task.
///
/// # Errors
///
/// Never before the first item: the subscription cannot fail. A failure to
/// serialise one event becomes that event's terminal error frame.
///
/// Test: `rpc_activity_stream_delivers_an_event_without_polling`,
/// `rpc_activity_stream_does_not_replay_history`.
pub async fn activity_stream(state: &AppState) -> Result<RpcStreamItems, RpcError> {
    let mut events = state.events.subscribe();
    let (tx, items) = mpsc::channel(STREAM_BUFFER);

    tokio::spawn(async move {
        loop {
            let frame = match events.recv().await {
                Ok(event) => match serde_json::to_value(&event) {
                    Ok(value) => Ok(value),
                    Err(e) => Err(RpcError::internal(format!("serialize activity event: {e}"))),
                },
                // The reader missed `n` events. Telling it beats a gap it
                // cannot see.
                Err(RecvError::Lagged(n)) => {
                    tracing::warn!(
                        target: "trusty_memory::activity_stream",
                        "an activity_stream reader lagged {n} events"
                    );
                    Ok(serde_json::json!({ "type": "lagged", "lagged": n }))
                }
                // The daemon is shutting down.
                Err(RecvError::Closed) => break,
            };
            let terminal = frame.is_err();
            if tx.send(frame).await.is_err() {
                // The reader disconnected.
                break;
            }
            if terminal {
                break;
            }
        }
    });

    Ok(items)
}
