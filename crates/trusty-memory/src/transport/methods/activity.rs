//! The paginated activity history (#6286).
//!
//! Why: `GET /api/v1/activity` seeded the console's activity feed on mount;
//! without it the pane rendered empty until the next live event. The hook
//! ingestion route it shared a file with is NOT folded — `hook_fired` was
//! already a dispatcher method, and the route was its duplicate.
//! What: `memory.activity`, with the same filters and the same clamp.
//! Test: `super::super::uds::tests` — `rpc_activity_*`.

use serde::{Deserialize, Serialize};
use serde_json::Value;

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
