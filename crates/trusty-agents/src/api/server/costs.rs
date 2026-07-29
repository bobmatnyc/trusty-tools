//! `GET /api/costs` — aggregated usage cost for the Costs tab (#4098).
//!
//! Why: The Costs tab (COST-09, formerly #4108) needs totals plus a breakdown
//! by agent, model and day, and nothing exposed the per-dispatch usage log over
//! HTTP. This route is that surface. It owns no arithmetic of its own: the fold
//! is `usage::aggregate::aggregate_usage` and the rates are
//! `perf::pricing::cost_usd`, so the API can never report a number the CLI or a
//! future report would disagree with.
//!
//! **Honesty rules, and why each is load-bearing.** A cost view is only worth
//! having if a number on it is a claim you can act on, so every state that
//! ISN'T "here are your costs" gets its own distinguishable answer:
//!
//! - **No log yet → `200` with `available: false` and a `reason`, never a zero
//!   summary.** `$0.00` and "nothing has been recorded" are different claims,
//!   and the first one, made wrongly, is the failure mode that makes an
//!   operator stop trusting the whole tab. The GUI renders the reason.
//! - **A log that exists but cannot be read → `500`.** That is a real fault
//!   (permissions, a truncated mount) and is not degraded into "no data".
//! - **Unparseable lines → `200`, aggregated without them, with
//!   `malformed_lines > 0`.** Partial data is still useful; presenting a short
//!   total as a whole one is not. The GUI warns when the count is non-zero.
//! - **An empty-but-present log → `200`, `available: true`, `records: 0`.** The
//!   project has state and simply has not dispatched — a real, different answer
//!   from both of the above.
//!
//! Response shape (a single object, not COST-08's bare array):
//!
//! ```json
//! {
//!   "available": true,
//!   "source": "/proj/.trusty-agents/state/usage.jsonl",
//!   "records": 128, "malformed_lines": 0,
//!   "first_ts": "...", "last_ts": "...", "window_days": 7,
//!   "totals":   { "key": "total", "input_tokens": 0, "output_tokens": 0,
//!                 "cost_usd": 0.0, "dispatch_count": 0, "duration_ms": 0 },
//!   "by_agent": [ ... ], "by_model": [ ... ], "by_date": [ ... ]
//! }
//! ```
//!
//! Two deliberate deviations from COST-08's stated criteria, both consequences
//! of building on read-time aggregation rather than #4104's unimplemented
//! rollup tables (see `usage::aggregate`'s module doc and the PR body):
//!
//! - `?range=day|week|month` is not implemented — week/month rollups are
//!   COST-07 (#4105) and there is nothing to query. `?days=<n>` narrows the
//!   window instead, and `by_date` gives daily granularity.
//! - `?group_by=` is not implemented. All three breakdowns ship in one payload
//!   so the tab switches grouping without a refetch AND so the three can never
//!   disagree — they are folds of one pass over one set of rows.
//!
//! What: [`get_costs`] is the axum handler; [`costs_at`] is the testable core
//! that takes an explicit project dir.
//! Test: `super::tests::costs`.

use axum::{
    Json,
    extract::Query,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use serde_json::json;

use crate::usage::aggregate::{AggregateError, aggregate_usage};

/// Upper bound on `?days=`, so a hostile or fat-fingered value cannot ask for
/// a window wider than any plausible log. Purely defensive — the fold reads the
/// whole file regardless; this only bounds the reported `window_days`.
const MAX_WINDOW_DAYS: u32 = 3650;

/// Query string for `GET /api/costs`.
///
/// Why: One optional knob. See the module doc for why `range`/`group_by` from
/// COST-08 are absent rather than stubbed — a parameter that silently ignores
/// its value is worse than one that isn't there.
/// What: `days` — keep only rows within N days of the newest recorded row.
/// Omitted (or 0) means "everything".
/// Test: `costs_window_narrows_the_report`.
#[derive(Debug, Default, Deserialize)]
pub(super) struct CostsQuery {
    /// Window in days, anchored on the newest recorded row.
    #[serde(default)]
    days: Option<u32>,
}

/// `GET /api/costs` — HTTP entry point.
///
/// Why/What: see the module doc. The project root is the daemon's CWD, matching
/// `usage::project_dir`'s convention and the sibling routes' resolution.
/// Test: `super::tests::costs::costs_route_is_wired_into_router`.
pub(super) async fn get_costs(Query(q): Query<CostsQuery>) -> Response {
    costs_at(&crate::usage::project_dir(), q.days)
}

/// Aggregate `project_dir`'s usage log into an HTTP response.
///
/// Why: Split from the axum shim so the four states this route distinguishes
/// (recorded / not recorded / unreadable / malformed) are testable against a
/// tempdir without standing up a server or mutating the process CWD.
/// What: Calls `aggregate_usage`, maps `NotRecorded` to a `200` "no data"
/// envelope and `Read` to a `500`, and otherwise serializes the summary with
/// `available: true` spliced in.
/// Test: `costs_reports_no_data_for_missing_log`,
/// `costs_returns_totals_and_breakdowns`, `costs_surfaces_malformed_line_count`,
/// `costs_reports_an_unreadable_log_as_a_server_error`.
pub(super) fn costs_at(project_dir: &std::path::Path, days: Option<u32>) -> Response {
    let window = days.filter(|d| *d > 0).map(|d| d.min(MAX_WINDOW_DAYS));
    match aggregate_usage(project_dir, window) {
        Ok(summary) => {
            let mut body = match serde_json::to_value(&summary) {
                Ok(serde_json::Value::Object(map)) => map,
                _ => {
                    // Unreachable: `CostSummary` is a plain struct. Answering
                    // 500 rather than unwrapping keeps the daemon alive if it
                    // ever becomes reachable.
                    return (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Json(json!({ "error": "could not serialize cost summary" })),
                    )
                        .into_response();
                }
            };
            body.insert("available".to_string(), json!(true));
            (StatusCode::OK, Json(serde_json::Value::Object(body))).into_response()
        }
        // Not an error: a project that has never dispatched has no log. The
        // caller gets an explicit "nothing recorded", never a zero total.
        Err(AggregateError::NotRecorded { path }) => (
            StatusCode::OK,
            Json(json!({
                "available": false,
                "reason": "no usage has been recorded for this project yet",
                "source": path.display().to_string(),
                "records": 0,
                "malformed_lines": 0,
                "window_days": window,
                "totals": crate::usage::aggregate::CostRow {
                    key: "total".to_string(),
                    ..Default::default()
                },
                "by_agent": [], "by_model": [], "by_date": [],
            })),
        )
            .into_response(),
        // A log that exists but will not read is a real fault. Degrading it to
        // "no data" would hide a broken cost trail behind an innocent message.
        Err(e @ AggregateError::Read { .. }) => {
            tracing::warn!(error = %e, "costs: usage log unreadable");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "available": false, "error": e.to_string() })),
            )
                .into_response()
        }
    }
}
