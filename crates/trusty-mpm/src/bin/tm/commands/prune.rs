//! `tm session prune-idle` — reclaim idle session-manager sessions (issue #1313).
//!
//! Why: paused orchestration sessions leave behind idle SM tmux sessions that
//! consume claude Max rate-limit slots and clutter the fleet. This command
//! enumerates managed sessions, reads each one's latest activity-monitor verdict,
//! and applies the locked teardown policy (idle → stop/resumable, done →
//! decommission, everything else → skip) by REUSING the existing managed
//! `runtime-stop` / `decommission` operations. It must no-op gracefully when the
//! Session Manager is disabled or the daemon is unreachable so the claude-mpm
//! `/mpm-session-pause` flow never breaks when SM is off.
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
/// resources), which is why the constant is `pub(crate)`.
/// What: `75` (EX_TEMPFAIL-adjacent) signals "SM unavailable, not an error".
/// Test: `unavailable_exit_code_is_stable` (value) and
/// `cli_prune_idle_unreachable_exit_code` (end-to-end wiring).
pub(crate) const EXIT_SM_UNAVAILABLE: i32 = 75;

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

    // 5) Execute the actionable rows by REUSING the existing managed operations.
    for p in &plan {
        match p.action {
            PruneAction::Stop => {
                super::managed::session_stop(client, url, p.id.clone()).await?;
            }
            PruneAction::Decommission => {
                super::managed::session_decommission(client, url, p.id.clone()).await?;
            }
            PruneAction::Skip(_) => {}
        }
    }
    Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: &str, name: &str, verdict: Option<&str>) -> SessionVerdict {
        SessionVerdict {
            id: id.to_string(),
            name: name.to_string(),
            verdict: verdict.map(str::to_string),
        }
    }

    /// Why: prove the orchestration maps every verdict through the policy
    /// (idle→stop, done→decommission, working/none→skip) in one pass.
    /// What: builds a plan over mixed rows and asserts each chosen action.
    /// Test: this test.
    #[test]
    fn build_plan_maps_each_verdict() {
        let rows = vec![
            row("11111111-a", "alpha", Some("idle")),
            row("22222222-b", "bravo", Some("done")),
            row("33333333-c", "charlie", Some("working")),
            row("44444444-d", "delta", None),
        ];
        let plan = build_plan(&rows);
        assert_eq!(plan[0].action, PruneAction::Stop);
        assert_eq!(plan[1].action, PruneAction::Decommission);
        assert!(matches!(plan[2].action, PruneAction::Skip(_)));
        assert!(matches!(plan[3].action, PruneAction::Skip(_)));
        assert_eq!(plan[3].verdict, "none");
    }

    /// Why: the plan order must mirror the daemon's list order for stable output.
    /// What: asserts ids appear in input order.
    /// Test: this test.
    #[test]
    fn build_plan_preserves_order() {
        let rows = vec![
            row("aaaa-1", "a", Some("idle")),
            row("bbbb-2", "b", Some("idle")),
        ];
        let plan = build_plan(&rows);
        assert_eq!(plan[0].id, "aaaa-1");
        assert_eq!(plan[1].id, "bbbb-2");
    }

    /// Why: `--dry-run` correctness — building the plan is pure and yields the
    /// SAME actionable set the live path would execute, with no side effects.
    /// What: asserts the dry-run plan equals the live plan (same input → same
    /// decisions) and that only stop/decommission rows are counted actionable.
    /// Test: this test.
    #[test]
    fn build_plan_dry_run_matches_live_plan() {
        let rows = vec![
            row("1-a", "a", Some("idle")),
            row("2-b", "b", Some("done")),
            row("3-c", "c", Some("errored")),
        ];
        // The function is pure: calling it twice (as dry-run then live would)
        // produces identical decisions, proving dry-run previews the live action.
        let dry = build_plan(&rows);
        let live = build_plan(&rows);
        let labels = |p: &[PlannedAction]| p.iter().map(|x| x.action.label()).collect::<Vec<_>>();
        assert_eq!(labels(&dry), labels(&live));
        assert_eq!(actionable_count(&dry), 2);
    }

    /// Why: the summary counter must exclude skips.
    /// What: asserts the actionable count over a mixed plan.
    /// Test: this test.
    #[test]
    fn actionable_count_excludes_skips() {
        let rows = vec![
            row("1", "a", Some("idle")),
            row("2", "b", Some("working")),
            row("3", "c", Some("done")),
        ];
        assert_eq!(actionable_count(&build_plan(&rows)), 2);
    }

    /// Why: the text plan must show each session with its verdict and action.
    /// What: asserts the rendered table contains the names, verbs, and short id.
    /// Test: this test.
    #[test]
    fn render_plan_text_lists_actions() {
        let rows = vec![
            row("11111111-aaaa", "alpha", Some("idle")),
            row("22222222-bbbb", "bravo", Some("working")),
        ];
        let out = render_plan_text(&build_plan(&rows), true);
        assert!(out.contains("stop"));
        assert!(out.contains("alpha"));
        assert!(out.contains("11111111"));
        assert!(out.contains("skip"));
        assert!(out.contains("bravo"));
        assert!(out.contains("dry run"));
        assert!(out.contains("would act on 1 of 2"));
    }

    /// Why: an empty fleet must render a clear no-op line, not a blank.
    /// What: asserts the empty-plan message.
    /// Test: this test.
    #[test]
    fn render_plan_text_empty() {
        assert_eq!(
            render_plan_text(&[], true),
            "no managed sessions to prune\n"
        );
    }

    /// Why: the JSON contract for the claude-mpm pause skill must be stable.
    /// What: parses the rendered JSON and asserts the document shape and a row.
    /// Test: this test.
    #[test]
    fn render_plan_json_shape() {
        let rows = vec![
            row("1-a", "alpha", Some("idle")),
            row("2-b", "bravo", Some("working")),
        ];
        let json = render_plan_json(&build_plan(&rows), true).expect("json");
        let v: serde_json::Value = serde_json::from_str(&json).expect("parse");
        assert_eq!(v["dry_run"], true);
        assert_eq!(v["actionable"], 1);
        assert_eq!(v["total"], 2);
        assert_eq!(v["sessions"][0]["action"], "stop");
        assert_eq!(v["sessions"][0]["verdict"], "idle");
        assert_eq!(v["sessions"][1]["action"], "skip");
        assert_eq!(v["sessions"][1]["reason"], "working");
    }

    /// Why: the concurrent verdict fan-out (`JoinSet`) completes tasks in
    /// nondeterministic order, but the plan must mirror the daemon's list order.
    /// `reorder_by_index` is the pure piece that restores it; proving it sorts
    /// out-of-order completions back to input order guarantees deterministic
    /// plans (and byte-identical dry-run/live output) regardless of scheduling.
    /// What: feeds `(index, row)` pairs in shuffled order and asserts the result
    /// is in ascending-index (i.e. original list) order.
    /// Test: this test.
    #[test]
    fn fetch_verdicts_preserves_order() {
        // Tasks "completed" out of order: indices 2, 0, 1.
        let shuffled = vec![
            (2, row("c", "charlie", Some("done"))),
            (0, row("a", "alpha", Some("idle"))),
            (1, row("b", "bravo", Some("working"))),
        ];
        let ordered = reorder_by_index(shuffled);
        let ids: Vec<&str> = ordered.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, ["a", "b", "c"]);
    }

    /// Why: the SM-unavailable `--json` branch must emit the SAME serde schema as
    /// the available path (issue #1313 review finding #4) — never a hand-rolled
    /// literal. The only difference is `sm_available: false` and empty counts.
    /// What: parses `render_unavailable_json` and asserts every field the
    /// available-path `render_plan_json_shape` test checks, plus `sm_available`.
    /// Test: this test.
    #[test]
    fn render_unavailable_json_shape() {
        let json = render_unavailable_json(true).expect("json");
        let v: serde_json::Value = serde_json::from_str(&json).expect("parse");
        // Same schema/keys as the available path…
        assert_eq!(v["dry_run"], true);
        assert_eq!(v["actionable"], 0);
        assert_eq!(v["total"], 0);
        assert!(v["sessions"].is_array());
        assert_eq!(v["sessions"].as_array().expect("array").len(), 0);
        // …distinguished only by the availability flag.
        assert_eq!(v["sm_available"], false);
        // The available path sets the same flag to `true`.
        let avail = render_plan_json(&[], false).expect("json");
        let av: serde_json::Value = serde_json::from_str(&avail).expect("parse");
        assert_eq!(av["sm_available"], true);
    }

    /// Why: the graceful no-op exit code must stay the documented constant.
    /// What: asserts the value the pause skill branches on.
    /// Test: this test.
    #[test]
    fn unavailable_exit_code_is_stable() {
        assert_eq!(EXIT_SM_UNAVAILABLE, 75);
    }
}
