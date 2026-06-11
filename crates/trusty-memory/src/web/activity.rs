//! Activity log and hook ingestion handlers.
//!
//! Why: The activity feed (issue #96) gives operators a paginated history of
//! what the daemon has been doing. The hook ingestion endpoint lets ephemeral
//! CLI hooks (Claude Code's `UserPromptSubmit` / `SessionStart`) write to the
//! same feed without a live MCP connection.
//! What: `GET /api/v1/activity` handler with filter/pagination, and
//! `POST /api/v1/activity/hook` ingestion endpoint.
//! Test: `activity_endpoint_*` and `hook_fired_activity_emit_smoke` tests in
//! `web::tests`.

use axum::{
    extract::{Query, State},
    http::StatusCode,
    Json,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::hook_emit::HookEventPayload;
use crate::{ActivityFilter, ActivitySource, AppState, DaemonEvent};

use super::error::ApiError;
use super::rpc::parse_iso_or_bad_request;

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
pub(super) struct ActivityQuery {
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
pub(super) struct ActivityRow {
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
pub(super) async fn activity_handler(
    State(state): State<AppState>,
    Query(q): Query<ActivityQuery>,
) -> Result<Json<Value>, ApiError> {
    let limit = q
        .limit
        .unwrap_or(ACTIVITY_DEFAULT_LIMIT)
        .clamp(1, ACTIVITY_MAX_LIMIT);
    let offset = q.offset.unwrap_or(0);

    // Validate + parse source filter.
    let source = match q.source.as_deref() {
        Some(s) => match ActivitySource::parse(s) {
            Some(parsed) => Some(parsed),
            None => {
                return Err(ApiError::bad_request(format!(
                    "unknown source '{s}'; expected one of http, mcp, hook",
                )));
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

    Ok(Json(serde_json::json!({
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
pub(super) async fn hook_activity_handler(
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
