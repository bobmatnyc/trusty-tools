//! `tm session prune-idle` — reclaim idle session-manager sessions (issue #1313).
//!
//! Why: paused orchestration sessions leave behind idle SM tmux sessions that
//! consume claude Max rate-limit slots and clutter the fleet. This command
//! enumerates managed sessions, reads each one's latest activity-monitor verdict,
//! and applies the locked teardown policy (idle → stop/resumable, done →
//! decommission, everything else → skip) by REUSING the existing managed
//! `runtime-stop` / `decommission` operations. It must no-op gracefully when the
//! Session Manager is disabled or the daemon is unreachable so the trusty-mpm
//! `/tm-session-management` flow never breaks when SM is off.
//! What: [`prune_idle`] is the async entry point (list → verdict → plan →
//! render → act); [`build_plan`] is the pure, synchronous core that turns a
//! fetched `Vec<SessionVerdict>` into a `Vec<PlannedAction>` via the
//! `core::sm::prune::decide` policy, so the plan is unit-testable without a live
//! daemon; rendering goes through [`render_plan_text`] / [`render_plan_json`].
//! Test: `build_plan_*` and `render_*` in the `tests` module below; the policy
//! mapping itself is covered in `core::sm::prune::tests`.

use std::sync::Arc;

use serde::Deserialize;
use serde::Serialize;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

use trusty_mpm::core::sm::prune::{PruneAction, decide};

/// Re-export the typed SM-unavailable error so `main` can downcast on it via
/// `commands::prune::PruneError` without reaching into the library crate path.
///
/// Why: keeps the top-level exit-code translation in `main` referring to the
/// same module that owns the prune command, rather than threading
/// `trusty_mpm::core::sm::prune` through the binary's call sites.
/// What: alias for the library's [`trusty_mpm::core::sm::prune::PruneError`].
/// Test: used by `cli_prune_idle_unreachable_exit_code`.
pub(crate) use trusty_mpm::core::sm::prune::PruneError;

/// Max concurrent per-session activity fetches against the daemon.
///
/// Why: prune fans out one `GET …/activity` per managed session; doing them
/// sequentially is slow for a large fleet, but unbounded concurrency would
/// hammer the daemon. A small semaphore bounds in-flight requests to a value
/// that parallelizes well without overwhelming the loopback server.
/// What: caps concurrent verdict fetches at `12`.
/// Test: `fetch_verdicts_preserves_order` exercises the fan-out join + sort.
const MAX_INFLIGHT_VERDICTS: usize = 12;

/// Exit code emitted when SM is off / the daemon is unreachable (graceful no-op).
///
/// Why: the claude-mpm pause skill calls this command and must distinguish "ran,
/// nothing to do" from "SM not available" without treating the latter as a hard
/// failure. A distinct, non-1 code lets the caller branch while a human reading
/// stderr still sees a clear message. `prune_idle` no longer exits the process
/// itself — it returns [`PruneError::SmUnavailable`]; `main` downcasts that error
/// and exits with this code at the top-level command boundary (no live async
/// resources), which is why the constant is `pub(crate)`. Sourced from
/// `trusty_mpm::core::exit_codes::EXIT_UNAVAILABLE`, the single shared
/// constant also used by `core::discovery::EXIT_DAEMON_URL_UNREACHABLE`
/// (issue #1737) — rather than redefining the literal `75` a second time, so
/// the two "target unavailable" conventions can never drift apart.
/// What: `75` (EX_TEMPFAIL-adjacent) signals "SM unavailable, not an error".
/// Test: `unavailable_exit_code_is_stable` (value) and
/// `cli_prune_idle_unreachable_exit_code` (end-to-end wiring).
pub(crate) const EXIT_SM_UNAVAILABLE: i32 = trusty_mpm::core::exit_codes::EXIT_UNAVAILABLE;

/// A session paired with its latest activity verdict (the prune input row).
///
/// Why: `build_plan` is pure over this shape so the orchestration can be tested
/// without HTTP; the fetch step lowers the daemon's list+activity responses into
/// these rows.
/// What: id, friendly name, and the latest verdict string (`None` = no verdict).
/// Test: constructed in `build_plan_*` tests.
#[derive(Debug, Clone)]
pub(crate) struct SessionVerdict {
    /// Managed session id (UUID string).
    pub(crate) id: String,
    /// Friendly tmux name (for display).
    pub(crate) name: String,
    /// Latest activity verdict, or `None` when the session has no verdict yet.
    pub(crate) verdict: Option<String>,
}

/// One planned action: a session plus the policy decision for it.
///
/// Why: the dry-run plan, the JSON output, and the live executor all consume the
/// same planned rows; one struct keeps them consistent.
/// What: carries the session identity, the observed verdict, and the chosen
/// [`PruneAction`].
/// Test: produced by `build_plan`; serialized form covered by `render_*` tests.
#[derive(Debug, Clone)]
pub(crate) struct PlannedAction {
    /// Managed session id.
    pub(crate) id: String,
    /// Friendly tmux name.
    pub(crate) name: String,
    /// Verdict that drove the decision (`"none"` when absent).
    pub(crate) verdict: String,
    /// The policy decision.
    pub(crate) action: PruneAction,
}

/// JSON row for the `--json` output mode (programmatic callers).
///
/// Why: the claude-mpm pause skill parses the plan; a stable, flat JSON shape
/// decouples it from the internal `PlannedAction` type.
/// What: `id`, `name`, `verdict`, `action` (`"stop"`/`"decommission"`/`"skip"`),
/// and `reason` (the skip rationale, empty for actionable rows).
/// Test: `render_plan_json_shape`.
#[derive(Debug, Serialize)]
struct JsonRow {
    id: String,
    name: String,
    verdict: String,
    action: String,
    reason: String,
}

/// Top-level JSON document for `--json`.
///
/// Why: callers want a single object with the dry-run flag and a counted summary
/// alongside the per-session rows. The `sm_available` flag lets the claude-mpm
/// pause skill distinguish a real (SM-up) empty plan from the SM-unavailable
/// no-op without parsing exit codes or stderr, and it keeps the unavailable
/// branch emitting the SAME serde-derived schema as the normal path rather than
/// a hand-rolled JSON string literal.
/// What: `dry_run`, `actionable` count, `total`, the `sessions` array, and
/// `sm_available` (`true` on the normal path, `false` on the SM-unavailable
/// no-op).
/// Test: `render_plan_json_shape` (available path) and
/// `render_unavailable_json_shape` (unavailable path).
#[derive(Debug, Serialize)]
struct JsonPlan {
    dry_run: bool,
    actionable: usize,
    total: usize,
    sessions: Vec<JsonRow>,
    sm_available: bool,
}

/// Turn fetched session/verdict rows into a concrete action plan (pure).
///
/// Why: this is the testable heart of the command — given the rows the daemon
/// returned, it deterministically computes what WOULD be done, with zero side
/// effects. `--dry-run` is therefore "build the plan and stop"; the live path is
/// "build the plan, then execute the actionable rows", guaranteeing the dry-run
/// and live plans are byte-identical.
/// What: maps each [`SessionVerdict`] through `core::sm::prune::decide`,
/// preserving input order; `None` verdicts surface as the literal `"none"` in
/// the rendered verdict column.
/// Test: `build_plan_maps_each_verdict`, `build_plan_preserves_order`,
/// `build_plan_dry_run_has_no_side_effects` (the type system: it returns a plan,
/// touches nothing).
pub(crate) fn build_plan(rows: &[SessionVerdict]) -> Vec<PlannedAction> {
    rows.iter()
        .map(|row| PlannedAction {
            id: row.id.clone(),
            name: row.name.clone(),
            verdict: row.verdict.clone().unwrap_or_else(|| "none".to_string()),
            action: decide(row.verdict.as_deref()),
        })
        .collect()
}

/// Count the rows in a plan that would mutate the fleet.
///
/// Why: both the human summary line and the JSON `actionable` field need the
/// count of stop/decommission rows (skips excluded).
/// What: returns the number of `PlannedAction`s whose action `is_actionable`.
/// Test: `actionable_count_excludes_skips`.
pub(crate) fn actionable_count(plan: &[PlannedAction]) -> usize {
    plan.iter().filter(|p| p.action.is_actionable()).count()
}

/// Render the plan as an operator-readable table + summary.
///
/// Why: the default (non-JSON) output must let an operator see each candidate
/// session, its verdict, and the action that would/will be taken.
/// What: one line per session (`ACTION  name (short-id)  verdict[: reason]`)
/// plus a trailing summary of the actionable count; an empty plan renders a
/// single "no managed sessions" line.
/// Test: `render_plan_text_lists_actions`, `render_plan_text_empty`.
pub(crate) fn render_plan_text(plan: &[PlannedAction], dry_run: bool) -> String {
    if plan.is_empty() {
        return "no managed sessions to prune\n".to_string();
    }
    let mut out = String::new();
    for p in plan {
        let reason = match &p.action {
            PruneAction::Skip(why) => format!(": {why}"),
            _ => String::new(),
        };
        out.push_str(&format!(
            "{:<13} {} ({})  verdict={}{}\n",
            p.action.label(),
            p.name,
            short_id(&p.id),
            p.verdict,
            reason,
        ));
    }
    let n = actionable_count(plan);
    let verb = if dry_run { "would act on" } else { "acted on" };
    out.push_str(&format!(
        "{verb} {n} of {} session(s){}\n",
        plan.len(),
        if dry_run { " (dry run)" } else { "" }
    ));
    out
}

/// Render the plan as a single JSON object for programmatic callers.
///
/// Why: the claude-mpm pause skill consumes `--json`; a stable document is the
/// integration contract.
/// What: serializes a [`JsonPlan`] with the dry-run flag, counts, and per-session
/// rows; the skip reason is flattened into `reason`.
/// Test: `render_plan_json_shape`.
pub(crate) fn render_plan_json(plan: &[PlannedAction], dry_run: bool) -> anyhow::Result<String> {
    let sessions = plan
        .iter()
        .map(|p| JsonRow {
            id: p.id.clone(),
            name: p.name.clone(),
            verdict: p.verdict.clone(),
            action: p.action.label().to_string(),
            reason: match &p.action {
                PruneAction::Skip(why) => (*why).to_string(),
                _ => String::new(),
            },
        })
        .collect::<Vec<_>>();
    let doc = JsonPlan {
        dry_run,
        actionable: actionable_count(plan),
        total: plan.len(),
        sessions,
        sm_available: true,
    };
    Ok(serde_json::to_string_pretty(&doc)?)
}

/// Render the SM-unavailable no-op as the same JSON schema as a real plan.
///
/// Why: when the daemon is unreachable / SM is disabled we still emit `--json`
/// output, but it must be the SAME serde-derived shape as the available path
/// (issue #1313 review) — never a hand-rolled string literal that can drift from
/// [`JsonPlan`]. The only signal that distinguishes it is `sm_available: false`.
/// What: serializes an empty [`JsonPlan`] (`actionable: 0`, `total: 0`, no
/// `sessions`) with `sm_available` set to `false`, preserving the requested
/// `dry_run` flag.
/// Test: `render_unavailable_json_shape`.
pub(crate) fn render_unavailable_json(dry_run: bool) -> anyhow::Result<String> {
    let doc = JsonPlan {
        dry_run,
        actionable: 0,
        total: 0,
        sessions: Vec::new(),
        sm_available: false,
    };
    Ok(serde_json::to_string_pretty(&doc)?)
}

/// Shorten a UUID to its first segment for compact display.
///
/// Why: full UUIDs make the table noisy; the first hyphen-delimited group is
/// enough to disambiguate at a glance.
/// What: returns the substring before the first `-`, or the whole string if none.
/// Test: covered indirectly by `render_plan_text_lists_actions`.
fn short_id(id: &str) -> &str {
    id.split('-').next().unwrap_or(id)
}

/// `tm session prune-idle` — enumerate idle SM sessions and reclaim them.
///
/// Why: the operator-facing (and pause-automated) entry point. It must reuse the
/// existing managed list/activity/runtime-stop/decommission surface, apply the
/// pure policy, and no-op gracefully when SM is unavailable.
/// What: fetches the managed session list; a *transport* error (daemon down / SM
/// off) surfaces as [`PruneError::SmUnavailable`] which propagates to `main` for
/// the graceful exit [`EXIT_SM_UNAVAILABLE`] — this function never calls
/// `process::exit` itself so no live async resource is leaked. A reachable daemon
/// returning a 4xx/5xx is a *real* failure and propagates as an ordinary `Err`
/// (exit 1). On success it reads each session's verdict concurrently (bounded
/// fan-out), builds the plan via [`build_plan`], renders it (text or JSON), and —
/// unless `dry_run` — executes the actionable rows by delegating to the existing
/// `runtime-stop` / `decommission` handlers (teardown is NOT reimplemented here).
/// Test: the pure plan/render logic is covered by `build_plan_*`/`render_*`;
/// CLI parsing by `cli_parses_session_prune_idle` in `tests.rs`; the
/// SM-unavailable → exit-75 wiring by `cli_prune_idle_unreachable_exit_code` in
/// `tests/session_manager_mvp.rs`. The HTTP round-trip reuses the `managed`
/// handlers already covered there.
pub(crate) async fn prune_idle(
    client: &reqwest::Client,
    url: &str,
    dry_run: bool,
    json: bool,
) -> anyhow::Result<()> {
    // 1) List managed sessions. `fetch_sessions` distinguishes the two failure
    //    modes: a transport error (daemon down / SM off) is the graceful no-op
    //    path → render the SM-unavailable document/message and return the typed
    //    `SmUnavailable` error so `main` can exit with the distinct code without
    //    treating "SM off" as a hard failure; a reachable-but-erroring daemon
    //    (4xx/5xx) propagates as an ordinary `Err` (a real failure).
    let sessions = match fetch_sessions(client, url).await {
        Ok(sessions) => sessions,
        Err(FetchSessionsError::Unreachable) => {
            if json {
                println!("{}", render_unavailable_json(dry_run)?);
            } else {
                eprintln!("{}", PruneError::SmUnavailable);
            }
            return Err(PruneError::SmUnavailable.into());
        }
        Err(FetchSessionsError::Http(e)) => return Err(e),
    };

    // 2) Read each session's latest verdict concurrently (bounded fan-out). A
    //    per-session activity failure surfaces as `None` → policy skips it. The
    //    result is re-sorted into the daemon's original list order so the plan
    //    (and its rendered output) stays deterministic regardless of completion
    //    order.
    let rows = fetch_verdicts(client, url, sessions).await;

    // 3) Build the plan (pure) and render it.
    let plan = build_plan(&rows);
    if json {
        println!("{}", render_plan_json(&plan, dry_run)?);
    } else {
        print!("{}", render_plan_text(&plan, dry_run));
    }

    // 4) Dry run stops here — by construction it has taken no action.
    if dry_run {
        return Ok(());
    }

    // 5) Execute the actionable rows by REUSING the existing managed operations,
    //    best-effort: a single session vanishing mid-sweep (raced by the idle
    //    reaper, a concurrent stop, or an overlapping prune) must not abort the
    //    REST of the sweep (#2521 review). `execute_plan` catches each row's
    //    error instead of `?`-propagating it, so every actionable row is
    //    attempted regardless of earlier failures.
    let executed = execute_plan(client, url, &plan).await;
    if json {
        println!("{}", render_execution_summary_json(&executed)?);
    } else {
        print!("{}", render_execution_summary_text(&executed));
    }

    // The command still fails overall (non-zero exit, matching #2457's
    // fail-closed spirit) when ANY row failed — but only after the sweep has
    // run to completion, and the summary above already reported which rows.
    let (_, failed, _) = summarize_execution(&executed);
    if failed > 0 {
        anyhow::bail!(
            "prune sweep completed with {failed} failure(s) out of {} session(s); see summary above",
            executed.len()
        );
    }
    Ok(())
}

/// Outcome of attempting one planned action against the daemon.
///
/// Why: the #2521 review found the live-execute loop `?`-propagated the FIRST
/// per-session failure, aborting the rest of the sweep while the
/// pre-execution "acted on N of M" line kept describing only the intention.
/// Tracking a per-row outcome lets the sweep run best-effort to completion and
/// lets the post-execution summary report what ACTUALLY happened.
/// What: `Succeeded`/`Failed(reason)` for attempted (stop/decommission) rows,
/// `Skipped` for rows the policy already excluded from execution.
/// Test: `execute_plan_is_best_effort_and_reports_accurately` in `prune_tests.rs`.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ExecOutcome {
    /// The stop/decommission call succeeded.
    Succeeded,
    /// The stop/decommission call failed; carries the error's `Display` text.
    Failed(String),
    /// The policy decided to skip this session — never attempted.
    Skipped,
}

/// One executed row: the session identity plus its actual outcome.
///
/// Why: the post-execution summary (text or JSON) needs the id/name alongside
/// the outcome so it can name exactly which sessions failed.
/// What: carries `id`, `name`, and the row's [`ExecOutcome`].
/// Test: produced by `execute_plan`; rendered by `render_execution_summary_*`.
struct ExecutedRow {
    id: String,
    name: String,
    outcome: ExecOutcome,
}

/// Execute every row in the plan, never aborting the sweep on a per-row error.
///
/// Why: this IS the fix for the #2521 review finding — a raced session
/// disappearing between planning and execution must not stop the sweep from
/// acting on the rest of the fleet. Each failure is (a) logged to stderr
/// immediately, for an operator watching live output, and (b) captured in the
/// returned row so the post-sweep summary is accurate rather than aspirational.
/// What: for each [`PlannedAction`], calls the matching
/// `managed::session_stop` / `managed::session_decommission` handler and
/// catches (does not propagate) its `Err`; `Skip` rows are recorded as
/// `Skipped` without an HTTP call.
/// Test: `execute_plan_is_best_effort_and_reports_accurately`.
async fn execute_plan(
    client: &reqwest::Client,
    url: &str,
    plan: &[PlannedAction],
) -> Vec<ExecutedRow> {
    let mut results = Vec::with_capacity(plan.len());
    for p in plan {
        let outcome = match &p.action {
            PruneAction::Stop => {
                match super::managed::session_stop(client, url, p.id.clone()).await {
                    Ok(()) => ExecOutcome::Succeeded,
                    Err(e) => {
                        eprintln!(
                            "prune: failed to stop session {} ({}): {e}",
                            p.name,
                            short_id(&p.id)
                        );
                        ExecOutcome::Failed(e.to_string())
                    }
                }
            }
            PruneAction::Decommission => {
                match super::managed::session_decommission(client, url, p.id.clone()).await {
                    Ok(()) => ExecOutcome::Succeeded,
                    Err(e) => {
                        eprintln!(
                            "prune: failed to decommission session {} ({}): {e}",
                            p.name,
                            short_id(&p.id)
                        );
                        ExecOutcome::Failed(e.to_string())
                    }
                }
            }
            PruneAction::Skip(_) => ExecOutcome::Skipped,
        };
        results.push(ExecutedRow {
            id: p.id.clone(),
            name: p.name.clone(),
            outcome,
        });
    }
    results
}

/// Count succeeded/failed/skipped rows (pure).
///
/// Why: shared by both the text and JSON post-execution renderers so the
/// counts can never drift between the two output modes, and by `prune_idle`'s
/// final exit-code decision.
/// What: returns `(succeeded, failed, skipped)`.
/// Test: `render_execution_summary_text_reports_failures`,
/// `render_execution_summary_json_reports_failures`.
fn summarize_execution(rows: &[ExecutedRow]) -> (usize, usize, usize) {
    let succeeded = rows
        .iter()
        .filter(|r| r.outcome == ExecOutcome::Succeeded)
        .count();
    let failed = rows
        .iter()
        .filter(|r| matches!(r.outcome, ExecOutcome::Failed(_)))
        .count();
    let skipped = rows
        .iter()
        .filter(|r| r.outcome == ExecOutcome::Skipped)
        .count();
    (succeeded, failed, skipped)
}

/// Render the post-execution outcome as an operator-readable summary.
///
/// Why: the pre-execution plan line ("acted on N of M") states an INTENTION;
/// this renders what ACTUALLY happened, naming every row that failed so a
/// partial sweep is never silently hidden behind the earlier optimistic line.
/// What: one `FAILED  name (short-id)  reason` line per failed row, followed
/// by a `succeeded/failed/skipped of total` summary line.
/// Test: `render_execution_summary_text_reports_failures`.
fn render_execution_summary_text(rows: &[ExecutedRow]) -> String {
    let mut out = String::new();
    for r in rows {
        if let ExecOutcome::Failed(reason) = &r.outcome {
            out.push_str(&format!(
                "FAILED        {} ({})  {reason}\n",
                r.name,
                short_id(&r.id)
            ));
        }
    }
    let (succeeded, failed, skipped) = summarize_execution(rows);
    out.push_str(&format!(
        "prune sweep complete: {succeeded} succeeded, {failed} failed, {skipped} skipped of {} session(s)\n",
        rows.len()
    ));
    out
}

/// JSON row for the post-execution `--json` summary.
///
/// Why: mirrors [`JsonRow`]'s flat shape so programmatic callers get a stable,
/// self-describing per-session outcome for the ACTUAL sweep result.
/// What: `id`, `name`, `outcome` (`"succeeded"`/`"failed"`/`"skipped"`), and
/// `error` (the failure reason, empty for non-failed rows).
/// Test: `render_execution_summary_json_reports_failures`.
#[derive(Debug, Serialize)]
struct ExecutionJsonRow {
    id: String,
    name: String,
    outcome: String,
    error: String,
}

/// Top-level JSON document for the post-execution `--json` summary.
///
/// Why: printed AFTER the pre-execution plan JSON, only for live (non-dry-run)
/// runs, so a programmatic caller (e.g. the claude-mpm pause skill) can tell
/// "what was planned" apart from "what actually happened".
/// What: `succeeded`/`failed`/`skipped`/`total` counts plus the per-session
/// `sessions` rows.
/// Test: `render_execution_summary_json_reports_failures`.
#[derive(Debug, Serialize)]
struct ExecutionSummaryJson {
    succeeded: usize,
    failed: usize,
    skipped: usize,
    total: usize,
    sessions: Vec<ExecutionJsonRow>,
}

/// Render the post-execution outcome as a single JSON object.
///
/// Why: JSON companion to [`render_execution_summary_text`] — programmatic
/// callers need the ACTUAL result of a live sweep, not just the pre-execution
/// plan that was already printed.
/// What: serializes an [`ExecutionSummaryJson`].
/// Test: `render_execution_summary_json_reports_failures`.
fn render_execution_summary_json(rows: &[ExecutedRow]) -> anyhow::Result<String> {
    let (succeeded, failed, skipped) = summarize_execution(rows);
    let sessions = rows
        .iter()
        .map(|r| {
            let (outcome, error) = match &r.outcome {
                ExecOutcome::Succeeded => ("succeeded", String::new()),
                ExecOutcome::Failed(e) => ("failed", e.clone()),
                ExecOutcome::Skipped => ("skipped", String::new()),
            };
            ExecutionJsonRow {
                id: r.id.clone(),
                name: r.name.clone(),
                outcome: outcome.to_string(),
                error,
            }
        })
        .collect::<Vec<_>>();
    let doc = ExecutionSummaryJson {
        succeeded,
        failed,
        skipped,
        total: rows.len(),
        sessions,
    };
    Ok(serde_json::to_string_pretty(&doc)?)
}

/// A managed-session id+name pair as read from the list endpoint.
///
/// Why: prune only needs the id (to act) and name (to display) from the richer
/// list response. `Clone` so each ref can be moved into a concurrent
/// verdict-fetch task.
/// What: deserializes the subset of `SessionSummary` it uses.
/// Test: exercised via `fetch_sessions` against the integration daemon.
#[derive(Debug, Clone, Deserialize)]
struct SessionRef {
    id: String,
    name: String,
}

/// The two distinct failure modes of listing managed sessions.
///
/// Why: review finding #3 — prune must NOT collapse a reachable-but-erroring
/// daemon (4xx/5xx) into the same "empty list / graceful no-op" path as a
/// genuinely unreachable daemon. A transport error means SM is off/down (exit
/// 75, no-op); a non-2xx HTTP status means the daemon answered with a real
/// failure that should surface as a hard error (exit 1), not a silent "0 of 0".
/// What: `Unreachable` wraps the connection/transport case; `Http` carries the
/// real error to propagate (non-2xx status or a body-decode failure).
/// Test: `fetch_sessions` behavior is covered via the integration daemon path
/// and the policy-level distinction by `cli_prune_idle_unreachable_exit_code`.
enum FetchSessionsError {
    /// The daemon could not be reached (connection refused, DNS, timeout, …).
    Unreachable,
    /// The daemon answered but with a non-2xx status or an undecodable body.
    Http(anyhow::Error),
}

/// Fetch the managed session list, lowered to id+name refs.
///
/// Why: the first step of prune; isolating it keeps `prune_idle` readable and
/// lets a *transport* error become the graceful no-op while a reachable-daemon
/// HTTP error stays a real failure (review finding #3).
/// What: GETs `/api/v1/sessions/managed`. A `reqwest` send error (daemon
/// unreachable) → `FetchSessionsError::Unreachable`. A non-success HTTP status
/// (daemon up but erroring) → `FetchSessionsError::Http` carrying the status —
/// it is NOT treated as an empty list. A 2xx with a decodable body → the
/// session vec (which may legitimately be empty when SM is enabled with no
/// sessions).
/// Test: covered via the integration daemon path; the unreachable branch by
/// `cli_prune_idle_unreachable_exit_code`.
async fn fetch_sessions(
    client: &reqwest::Client,
    url: &str,
) -> Result<Vec<SessionRef>, FetchSessionsError> {
    #[derive(Deserialize)]
    struct ListResp {
        sessions: Vec<SessionRef>,
    }
    // A send error is a transport failure → SM unavailable (graceful no-op).
    let resp = client
        .get(format!("{url}/api/v1/sessions/managed"))
        .send()
        .await
        .map_err(|_| FetchSessionsError::Unreachable)?;
    // A reachable daemon returning non-2xx is a REAL failure, not "no sessions".
    let status = resp.status();
    if !status.is_success() {
        return Err(FetchSessionsError::Http(anyhow::anyhow!(
            "session manager returned HTTP {status} listing managed sessions"
        )));
    }
    let body: ListResp = resp
        .json()
        .await
        .map_err(|e| FetchSessionsError::Http(e.into()))?;
    Ok(body.sessions)
}

/// Concurrently read every session's latest verdict, preserving list order.
///
/// Why: review finding #2 — fetching verdicts sequentially is slow for a large
/// fleet. Fanning the per-session activity reads out with a bounded
/// [`JoinSet`] + [`Semaphore`] parallelizes them without hammering the daemon,
/// while re-sorting by the original index keeps `build_plan`'s output (and the
/// rendered plan / tests) deterministic regardless of task completion order.
/// What: spawns one bounded task per session that calls [`fetch_verdict`]
/// (best-effort → `None` on failure), joins them all, then sorts the results
/// back into the input order and lowers them into [`SessionVerdict`] rows.
/// Test: `fetch_verdicts_preserves_order` covers the order-restoring reorder
/// (via [`reorder_by_index`]); the verdict→action mapping in
/// `core::sm::prune::tests`.
async fn fetch_verdicts(
    client: &reqwest::Client,
    url: &str,
    sessions: Vec<SessionRef>,
) -> Vec<SessionVerdict> {
    let semaphore = Arc::new(Semaphore::new(MAX_INFLIGHT_VERDICTS));
    let mut join_set: JoinSet<(usize, SessionVerdict)> = JoinSet::new();
    for (idx, s) in sessions.into_iter().enumerate() {
        let client = client.clone();
        let url = url.to_string();
        let semaphore = Arc::clone(&semaphore);
        join_set.spawn(async move {
            // The semaphore is never closed, so acquire cannot fail; the permit
            // is held for the duration of the request and dropped on return.
            let _permit = semaphore
                .acquire()
                .await
                .expect("prune verdict semaphore is never closed");
            let verdict = fetch_verdict(&client, &url, &s.id).await;
            (
                idx,
                SessionVerdict {
                    id: s.id,
                    name: s.name,
                    verdict,
                },
            )
        });
    }

    let mut indexed: Vec<(usize, SessionVerdict)> = Vec::new();
    while let Some(joined) = join_set.join_next().await {
        // A task panic is a programmer error; surface it loudly rather than
        // silently dropping a session from the plan.
        let row = joined.expect("prune verdict task panicked");
        indexed.push(row);
    }
    reorder_by_index(indexed)
}

/// Re-sort fan-out results back into the original list order (pure).
///
/// Why: `JoinSet` yields completed tasks in nondeterministic order, but the
/// plan — and the rendered output / dry-run-vs-live equality — must be stable.
/// Tagging each task with its input index and sorting on it restores the
/// daemon's list order deterministically. Extracted as a pure function so the
/// ordering guarantee is unit-testable without spawning tasks or a daemon.
/// What: sorts `(index, row)` pairs by index, then drops the index.
/// Test: `fetch_verdicts_preserves_order`.
fn reorder_by_index(mut indexed: Vec<(usize, SessionVerdict)>) -> Vec<SessionVerdict> {
    indexed.sort_by_key(|(idx, _)| *idx);
    indexed.into_iter().map(|(_, row)| row).collect()
}

/// Read one session's latest activity verdict, best-effort.
///
/// Why: the policy is driven by the activity-monitor verdict; a per-session read
/// failure (404, transport blip, missing classifier) must degrade to "no
/// verdict" so the policy safely skips rather than erroring the whole prune.
/// What: GETs `/api/v1/sessions/managed/{id}/activity` and returns the
/// `classification` field when present, else the `state` field, else `None`.
/// `classification` is preferred because it is `null` precisely when no LLM
/// classifier ran (no key) — and the policy treats that absence as a skip.
/// Test: the verdict→action mapping is covered in `core::sm::prune::tests`;
/// the HTTP shape matches the `managed::session_activity` response.
async fn fetch_verdict(client: &reqwest::Client, url: &str, id: &str) -> Option<String> {
    #[derive(Deserialize)]
    struct ActivityResp {
        #[serde(default)]
        state: Option<String>,
        #[serde(default)]
        classification: Option<String>,
    }
    let resp = client
        .get(format!("{url}/api/v1/sessions/managed/{id}/activity"))
        .send()
        .await
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let body: ActivityResp = resp.json().await.ok()?;
    body.classification.or(body.state)
}

// Unit tests live in prune_tests.rs (test-file budget: 1500 SLOC) — extracted
// from an inline `mod tests` (mirroring managed.rs/managed_tests.rs) so the
// #2521 best-effort-sweep HTTP-round-trip coverage doesn't push this
// production file toward the 500-SLOC cap.
#[cfg(test)]
#[path = "prune_tests.rs"]
mod tests;
