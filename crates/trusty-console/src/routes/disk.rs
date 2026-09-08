//! The two read routes behind the console's Disk view (#6929, DOC-73 §16.5).
//!
//! Why: DOC-73 §16.5 puts `GET /api/console/disk/tree` and
//! `GET /api/console/disk/worktrees/{id}` in the console and says each one
//! proxies trusty-mpm's `disk_survey` MCP tool (#6927) — never the daemon's
//! HTTP port, per §16.4 and #1104. They live in their own module rather than in
//! `sessions.rs` because that file is already near the 500-SLOC production cap.
//!
//! **Why a budget is mandatory here, and why it is 20 seconds.** The console
//! reaches trusty-mpm over a stdio MCP child, and every `tools/call` on that
//! transport is bounded by `CALL_TIMEOUT` — 30 seconds
//! (`trusty-common/src/stdio_mcp_client/mod.rs:164`, applied at
//! `client.rs:390`). An unbudgeted `disk_survey` does not fit inside it: the
//! #6929 live check ran one against a 48-worktree fleet and it passed 60 s
//! without answering, while a 60 s-budgeted single-project call took 46.9 s.
//! Classification is what costs — one `git status` and one `gh` lookup per
//! worktree. So these routes always send `budget_seconds`, defaulting to
//! [`DEFAULT_BUDGET_SECONDS`] and clamped at [`MAX_BUDGET_SECONDS`], which
//! leaves at least five of the transport's thirty seconds for the byte
//! measurement and the JSON serialization that run OUTSIDE the classification
//! deadline. Worktrees the budget was reached before are still listed — the
//! tool reports them as `review` with an `unknown-branch-state` reason, never
//! as `stale` and never omitted — so a truncated pass renders as caution rather
//! than as a shorter fleet.
//!
//! **READ-ONLY.** `disk_survey` removes nothing and neither does this module.
//! The clear action is #6930 (DOC-73 §16.6 item 4), behind its own confirm.
//!
//! What: [`tree_handler`] returns the survey verbatim; [`worktree_handler`]
//! returns one worktree out of the same survey, for a deep link or a
//! programmatic caller. The Disk view itself opens its detail panel from the
//! tree payload it already holds and never calls the second route (§16.3's
//! "a direct render of the `ReclaimCandidate` struct, not a new computation").
//! Test: the `tests` module below covers the argument shape, the clamp and the
//! selection; `tests/disk_mcp_bridge.rs` drives both routes through the real
//! router against a stub trusty-mpm that speaks stdio MCP.

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::mcp_handle::McpHandleError;
use crate::routes::sessions::{map_tool_result, mpm_handle};
use crate::server::AppState;

/// The trusty-mpm MCP tool both routes proxy (#6927).
pub(crate) const DISK_SURVEY_TOOL: &str = "disk_survey";

/// The classification budget the console sends when the caller names none.
///
/// Why this number: see the module docs — the stdio MCP transport cuts a call
/// off at 30 s, and the survey's byte walk and serialization run after the
/// classification deadline, so the budget has to leave room under it.
pub(crate) const DEFAULT_BUDGET_SECONDS: u64 = 20;

/// The largest budget a caller may ask for, whatever it sends.
///
/// A request for 60 s would be answered by the transport's own timeout instead
/// — a 502 with no survey at all, where a truncated survey was available.
pub(crate) const MAX_BUDGET_SECONDS: u64 = 25;

/// Query parameters both routes accept.
///
/// `project` is passed through to the tool, which matches it against a
/// `<owner>/<repo>` label, a bare directory name, or an absolute path prefix.
/// `budget_seconds` overrides [`DEFAULT_BUDGET_SECONDS`] within the clamp.
#[derive(Debug, Default, Deserialize)]
pub struct SurveyQuery {
    project: Option<String>,
    budget_seconds: Option<u64>,
}

/// Build the `disk_survey` arguments, with the budget always present.
///
/// Why its own function: the budget is the one thing about this call that must
/// not vary between the two routes, and a pure function is what lets the clamp
/// be asserted without a daemon.
/// What: `budget_seconds` is the caller's value clamped to
/// `1..=MAX_BUDGET_SECONDS`, or the default when absent; `project` is omitted
/// entirely when unset, because the tool's schema is closed and a `null` there
/// would be an unknown argument shape rather than "survey everything".
/// Test: `survey_args_always_carries_a_budget`, `survey_args_clamps_the_budget`,
/// `survey_args_omits_an_absent_project`.
pub(crate) fn survey_args(query: &SurveyQuery) -> Value {
    let budget = query
        .budget_seconds
        .unwrap_or(DEFAULT_BUDGET_SECONDS)
        .clamp(1, MAX_BUDGET_SECONDS);
    let mut args = json!({ "budget_seconds": budget });
    if let Some(project) = query.project.as_deref().filter(|p| !p.is_empty()) {
        args["project"] = Value::String(project.to_string());
    }
    args
}

/// `GET /api/console/disk/tree` — the whole survey, for the sunburst.
///
/// Why: the Disk view draws the centre total, ring 1 (projects) and ring 2
/// (worktrees, coloured by tier) from ONE payload, and opens its detail panel
/// from that same payload. Splitting it would make the panel a second round
/// trip against a survey that had moved on.
/// What: calls `disk_survey` through the capability-gated handle and returns
/// its JSON unchanged — the console reshapes nothing, so the view and the tool
/// cannot disagree about a tier.
/// Test: `tree_absent_binary_does_not_500`; `tests/disk_mcp_bridge.rs` asserts
/// the live shape and the budget that went out with it.
pub async fn tree_handler(
    State(state): State<AppState>,
    Query(query): Query<SurveyQuery>,
) -> axum::response::Response {
    let Some(handle) = mpm_handle(&state) else {
        return unreachable_daemon();
    };
    map_survey_result(
        handle
            .call_tool_checked(DISK_SURVEY_TOOL, survey_args(&query))
            .await,
    )
}

/// The 503 both routes return when trusty-mpm has no MCP handle at all.
///
/// Why a body: the bare `StatusCode` this used to be serialized to
/// `content-length: 0`, so the Disk view had nothing to render but the number
/// and printed `HTTP 502` / `HTTP 503` at the operator. A status code is not a
/// diagnosis (#6929).
fn unreachable_daemon() -> axum::response::Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        axum::Json(json!({
            "status": "unreachable",
            "hint": "trusty-mpm is not reachable — the disk survey needs its MCP bridge",
        })),
    )
        .into_response()
}

/// [`map_tool_result`], with a body on the transport-failure arm.
///
/// Why: `map_tool_result` answers a failed `tools/call` with a bare 502 and no
/// body. That is the response the #6929 live check got — 30.1 s, then
/// `HTTP/1.1 502` with `content-length: 0` — and it is why the Disk view could
/// say nothing more useful than the number. The overrun itself is fixed in the
/// survey; this is what an operator sees on the day something else goes wrong.
/// What: `Ok` and every 503 arm are `map_tool_result`'s verbatim; a transport
/// failure keeps its 502 and gains `{status, hint}` naming the survey and the
/// transport's own 30-second bound. The error text is NOT forwarded — it can
/// carry a path — so the hint is a fixed sentence.
/// Test: `a_transport_failure_carries_a_body`, `an_absent_handle_carries_a_body`.
fn map_survey_result(result: Result<Value, McpHandleError>) -> axum::response::Response {
    match result {
        Err(McpHandleError::Absent | McpHandleError::Backoff { .. }) => unreachable_daemon(),
        Err(e @ McpHandleError::Other(_)) => {
            tracing::warn!("disk route: the survey call failed: {e:#}");
            (
                StatusCode::BAD_GATEWAY,
                axum::Json(json!({
                    "status": "survey_failed",
                    "hint": "trusty-mpm did not answer the disk survey — it is bounded by \
                             the console's 30-second MCP call timeout; check the daemon's log",
                })),
            )
                .into_response()
        }
        other => map_tool_result(other),
    }
}

/// `GET /api/console/disk/worktrees/{id}` — one worktree out of the survey.
///
/// Why: §16.5 names this route for the detail panel, and a deep link or a
/// script wanting one worktree should not have to parse the whole tree. The
/// Disk view does NOT call it — its panel renders from the tree it already
/// fetched — so this path costs nothing on the hot page.
/// What: `{id}` is the tool's own worktree id, which is the worktree's absolute
/// path, so it arrives percent-encoded as a single path segment (axum decodes
/// it). Scope the survey with `?project=` to keep it cheap. A miss is a 404
/// naming the id, never an empty 200.
/// Test: `worktree_absent_binary_does_not_500`, `select_worktree_*`;
/// `tests/disk_mcp_bridge.rs` covers the hit and the miss end to end.
pub async fn worktree_handler(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<SurveyQuery>,
) -> axum::response::Response {
    let Some(handle) = mpm_handle(&state) else {
        return unreachable_daemon();
    };
    let survey = match handle
        .call_tool_checked(DISK_SURVEY_TOOL, survey_args(&query))
        .await
    {
        Ok(survey) => survey,
        Err(e) => return map_survey_result(Err(e)),
    };
    match select_worktree(&survey, &id) {
        Some(detail) => axum::Json(detail).into_response(),
        None => (
            StatusCode::NOT_FOUND,
            axum::Json(json!({
                "status": "not_found",
                "id": id,
                "hint": "no worktree with that id is in the survey — it may have been \
                         removed, or the `project` scope may exclude it",
            })),
        )
            .into_response(),
    }
}

/// Pull one worktree, with its project, out of a `disk_survey` payload.
///
/// Why a pure function: the selection is the only logic this route adds, and
/// keeping it out of the handler is what lets it be tested against a fixture
/// rather than a spawned daemon.
/// What: walks `root.projects[].worktrees[]` for a matching `id` and returns
/// the worktree verbatim beside the survey's `generated_at`, its `keep_list`
/// (so a caller reading one worktree still sees an unreadable keep-list config)
/// and the enclosing project's name and path.
/// Test: `select_worktree_finds_a_row_and_names_its_project`,
/// `select_worktree_misses_an_unknown_id`.
pub(crate) fn select_worktree(survey: &Value, id: &str) -> Option<Value> {
    let projects = survey.get("root")?.get("projects")?.as_array()?;
    for project in projects {
        let Some(worktrees) = project.get("worktrees").and_then(Value::as_array) else {
            continue;
        };
        for worktree in worktrees {
            if worktree.get("id").and_then(Value::as_str) != Some(id) {
                continue;
            }
            return Some(json!({
                "generated_at": survey.get("generated_at").cloned().unwrap_or(Value::Null),
                "keep_list": survey.get("keep_list").cloned().unwrap_or(Value::Null),
                "project": {
                    "name": project.get("name").cloned().unwrap_or(Value::Null),
                    "path": project.get("path").cloned().unwrap_or(Value::Null),
                },
                "worktree": worktree.clone(),
            }));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connector::ServiceConnector;

    fn query(project: Option<&str>, budget: Option<u64>) -> SurveyQuery {
        SurveyQuery {
            project: project.map(str::to_string),
            budget_seconds: budget,
        }
    }

    fn fixture() -> Value {
        json!({
            "generated_at": "2026-09-07T00:00:00Z",
            "keep_list": { "patterns": ["keep-me"], "invalid": [], },
            "root": {
                "path": "/w",
                "projects": [
                    { "name": "acme/one", "path": "/w/one", "worktrees": [
                        { "id": "/w/one/.worktrees/a", "tier": "stale" },
                    ]},
                    { "name": "acme/two", "path": "/w/two", "worktrees": [
                        { "id": "/w/two/.worktrees/b", "tier": "keep" },
                    ]},
                ],
            },
        })
    }

    #[test]
    fn survey_args_always_carries_a_budget() {
        // The transport cuts the call off at 30s; an unbudgeted survey outruns
        // that on a real fleet (#6929 live check), so the absence of a caller
        // budget must not become the absence of a budget.
        let args = survey_args(&query(None, None));
        assert_eq!(args["budget_seconds"], json!(DEFAULT_BUDGET_SECONDS));
        // The transport bound is a compile-time constant, so this is too:
        // a future edit that raises the default past it fails to build.
        const { assert!(DEFAULT_BUDGET_SECONDS < 30) };
    }

    #[test]
    fn survey_args_clamps_the_budget() {
        assert_eq!(
            survey_args(&query(None, Some(600)))["budget_seconds"],
            json!(MAX_BUDGET_SECONDS)
        );
        assert_eq!(
            survey_args(&query(None, Some(0)))["budget_seconds"],
            json!(1)
        );
        assert_eq!(
            survey_args(&query(None, Some(5)))["budget_seconds"],
            json!(5)
        );
    }

    #[test]
    fn survey_args_omits_an_absent_project() {
        let args = survey_args(&query(None, None));
        assert!(args.get("project").is_none(), "closed schema: {args}");
        let scoped = survey_args(&query(Some(""), None));
        assert!(scoped.get("project").is_none(), "empty is not a scope");
        let named = survey_args(&query(Some("acme/one"), None));
        assert_eq!(named["project"], json!("acme/one"));
    }

    #[test]
    fn select_worktree_finds_a_row_and_names_its_project() {
        let picked = select_worktree(&fixture(), "/w/two/.worktrees/b").expect("row");
        assert_eq!(picked["worktree"]["tier"], json!("keep"));
        assert_eq!(picked["project"]["name"], json!("acme/two"));
        // The keep-list travels with a single row too, so a detail read still
        // shows an unreadable config rather than implying there is none.
        assert_eq!(picked["keep_list"]["patterns"], json!(["keep-me"]));
    }

    #[test]
    fn select_worktree_misses_an_unknown_id() {
        assert!(select_worktree(&fixture(), "/w/nope").is_none());
        assert!(select_worktree(&json!({}), "/w/one/.worktrees/a").is_none());
    }

    /// CI has no `trusty-mpm` on PATH, so both routes take the absent-binary
    /// path here. A 503 is the contract; a 500 would be the bug.
    async fn state() -> AppState {
        let connectors: Vec<Box<dyn ServiceConnector>> = Vec::new();
        AppState::new(connectors)
    }

    /// Read a response's body as JSON, for the body assertions below.
    async fn body(resp: axum::response::Response) -> Value {
        let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
            .await
            .expect("body");
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    }

    /// A failed survey says what failed, not just that something did (#6929).
    ///
    /// Why: the live check got `502` with `content-length: 0` after 30.1 s, so
    /// the Disk view had nothing to show but `HTTP 502`. A status code alone is
    /// not a diagnosis.
    #[tokio::test]
    async fn a_transport_failure_carries_a_body() {
        let resp = map_survey_result(Err(McpHandleError::Other(anyhow::anyhow!(
            "MCP request timed out after 30s"
        ))));
        assert_eq!(resp.status(), StatusCode::BAD_GATEWAY);
        let json = body(resp).await;
        assert_eq!(json["status"], json!("survey_failed"), "{json:#}");
        assert!(
            json["hint"]
                .as_str()
                .is_some_and(|h| h.contains("30-second")),
            "{json:#}"
        );
        // The daemon's own error text can name a path; it stays in the log.
        assert!(
            !json.to_string().contains("MCP request timed out"),
            "{json:#}"
        );
    }

    #[tokio::test]
    async fn an_absent_handle_carries_a_body() {
        let resp = map_survey_result(Err(McpHandleError::Absent));
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(body(resp).await["status"], json!("unreachable"));
    }

    #[tokio::test]
    async fn tree_absent_binary_does_not_500() {
        let resp = tree_handler(State(state().await), Query(query(None, None))).await;
        assert_ne!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[tokio::test]
    async fn worktree_absent_binary_does_not_500() {
        let resp = worktree_handler(
            State(state().await),
            Path("/w/one/.worktrees/a".to_string()),
            Query(query(None, None)),
        )
        .await;
        // A miss must be a 404, never a 200 carrying nothing: on a machine with
        // a real trusty-mpm this id is not in the fleet, and on CI (no binary)
        // the handle is Absent and the route is a 503 before it ever selects.
        assert!(
            matches!(
                resp.status(),
                StatusCode::NOT_FOUND | StatusCode::SERVICE_UNAVAILABLE | StatusCode::BAD_GATEWAY
            ),
            "unexpected status {}",
            resp.status()
        );
    }
}
