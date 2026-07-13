//! Unit tests for `commands::prune` — split out of `prune.rs` (test-file
//! budget: 1500 SLOC), mirroring the `managed.rs`/`managed_tests.rs` split, so
//! the #2521 best-effort-sweep HTTP-round-trip coverage below doesn't push the
//! production file toward the 500-SLOC cap.
//!
//! Why: `build_plan_*`/`render_plan_*`/`fetch_verdicts_preserves_order` were
//! carried over unchanged from the inline `mod tests` this file replaces. The
//! new tests below cover the #2521 review fix: `prune_idle`'s live-execute
//! loop used to `?`-propagate the FIRST per-session `session_stop`/
//! `session_decommission` failure, aborting the rest of the sweep while the
//! pre-execution "acted on N of M" line kept describing only the intention.
//! `execute_plan` is now the best-effort executor (never aborts on a per-row
//! error) and `render_execution_summary_*`/`summarize_execution` render what
//! ACTUALLY happened.
//! What: `execute_plan_is_best_effort_and_reports_accurately` drives
//! `execute_plan` against a real hermetic daemon (mirroring
//! `managed_tests.rs`'s `spawn_test_daemon` pattern) with a plan of THREE
//! `Stop` rows where the middle id was never seeded (a guaranteed 404,
//! standing in for a session that raced out from under the sweep) and asserts
//! the other two still get acted on; `render_execution_summary_text_*`/
//! `render_execution_summary_json_*` cover the pure renderers with synthetic
//! rows.
//! Test: this file IS the test module for `commands::prune`.

use std::future::IntoFuture as _;

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

/// Spawn the daemon's real HTTP API on a random loopback port, rooted in a
/// throwaway temp directory, with a FakeNoopTmuxDriver so no real tmux
/// sessions are created (#1790).
///
/// Why: mirrors `managed_tests.rs`'s `spawn_test_daemon` — returning the
/// `Arc<DaemonState>` (not just the URL) lets a test seed a real managed
/// session via `state.session_manager()` BEFORE serving, so `execute_plan`
/// exercises the actual HTTP round-trip for both the "session exists" and
/// "session was never seeded → 404" cases.
/// What: builds `daemon::api::router(...)`, binds an ephemeral port, serves it
/// on a background task, and returns `(base_url, state)`.
async fn spawn_test_daemon() -> (
    String,
    std::sync::Arc<trusty_mpm::daemon::state::DaemonState>,
) {
    use trusty_mpm::daemon::{api, state::DaemonState};
    let root = tempfile::tempdir().unwrap().keep();
    let state = std::sync::Arc::new(DaemonState::with_root_isolated_managed(root).await);
    let router = api::router(std::sync::Arc::clone(&state));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(axum::serve(listener, router).into_future());
    (format!("http://{addr}"), state)
}

/// Seed one real managed session record into `state`'s store so a
/// `session_stop`/`session_decommission` call against its id succeeds against
/// the real daemon route (rather than 404ing).
///
/// Why: `execute_plan`'s best-effort test needs a MIX of real and missing ids
/// in the same plan; this helper creates the "real" half via the same
/// `create_with_id` scaffolding `session_manager_mvp.rs` uses.
/// What: creates a session with a fresh id under a unique workspace path
/// (derived from the id, so parallel test runs never collide) and returns its
/// id as a string.
async fn seed_session(state: &trusty_mpm::daemon::state::DaemonState, name: &str) -> String {
    use trusty_mpm::runtime::RuntimeKind;
    use trusty_mpm::session_manager::ManagedSessionId;

    let id = ManagedSessionId::new();
    let ws = std::env::temp_dir().join(format!("{id}-{name}-ws"));
    state
        .session_manager()
        .await
        .create_with_id(
            id,
            format!("regression: #2521 best-effort sweep ({name})"),
            Some(ws.clone()),
            None,
            Some(ws),
            Some("https://example.com/r.git".to_string()),
            Some("main".to_string()),
            RuntimeKind::default(),
            false,
            false,
        )
        .await
        .expect("seed session");
    id.to_string()
}

/// #2521 review fix: `execute_plan` must be best-effort — a session that 404s
/// mid-sweep (raced out from under the prune, stood in for here by an id that
/// was simply never seeded) must NOT prevent the other rows from being acted
/// on, and the returned outcomes must accurately reflect the mixed result.
///
/// What: seeds two REAL sessions (`alpha`, `charlie`) and builds a 3-row plan
/// — `Stop(alpha)`, `Stop(<never-seeded id>)`, `Stop(charlie)` — with the
/// failing row in the MIDDLE. Asserts all three rows were attempted (not
/// short-circuited), the middle one failed while the other two succeeded, and
/// `summarize_execution`/`render_execution_summary_text`/
/// `render_execution_summary_json` all agree: 2 succeeded, 1 failed, 0
/// skipped.
/// Test: this function IS the test.
#[tokio::test]
async fn execute_plan_is_best_effort_and_reports_accurately() {
    let (url, state) = spawn_test_daemon().await;
    let client = reqwest::Client::new();

    let alpha_id = seed_session(&state, "alpha").await;
    let charlie_id = seed_session(&state, "charlie").await;
    // Never seeded — the daemon has no record for this id, so `session_stop`
    // against it is a guaranteed 404 (stands in for a session that raced out
    // from under the sweep between planning and execution).
    let missing_id = "nonexistent-middle-session".to_string();

    let plan = vec![
        PlannedAction {
            id: alpha_id.clone(),
            name: "alpha".to_string(),
            verdict: "idle".to_string(),
            action: PruneAction::Stop,
        },
        PlannedAction {
            id: missing_id.clone(),
            name: "bravo".to_string(),
            verdict: "idle".to_string(),
            action: PruneAction::Stop,
        },
        PlannedAction {
            id: charlie_id.clone(),
            name: "charlie".to_string(),
            verdict: "idle".to_string(),
            action: PruneAction::Stop,
        },
    ];

    let executed = execute_plan(&client, &url, &plan).await;

    // All three rows were attempted — the middle failure did not abort the
    // sweep and skip the row after it.
    assert_eq!(
        executed.len(),
        3,
        "the middle failure must not truncate the sweep"
    );
    assert_eq!(executed[0].id, alpha_id);
    assert_eq!(executed[0].outcome, ExecOutcome::Succeeded);
    assert_eq!(executed[1].id, missing_id);
    assert!(
        matches!(executed[1].outcome, ExecOutcome::Failed(_)),
        "the never-seeded middle session must be reported as a failure, not silently dropped: {:?}",
        executed[1].outcome
    );
    assert_eq!(executed[2].id, charlie_id);
    assert_eq!(
        executed[2].outcome,
        ExecOutcome::Succeeded,
        "the row AFTER the failing one must still have been attempted and succeeded"
    );

    // The accurate summary: 2 succeeded, 1 failed, 0 skipped of 3.
    assert_eq!(summarize_execution(&executed), (2, 1, 0));

    let text = render_execution_summary_text(&executed);
    assert!(text.contains("FAILED"));
    assert!(text.contains("bravo"));
    assert!(text.contains("2 succeeded, 1 failed, 0 skipped of 3 session(s)"));

    let json = render_execution_summary_json(&executed).expect("json");
    let v: serde_json::Value = serde_json::from_str(&json).expect("parse");
    assert_eq!(v["succeeded"], 2);
    assert_eq!(v["failed"], 1);
    assert_eq!(v["skipped"], 0);
    assert_eq!(v["total"], 3);
    assert_eq!(v["sessions"][0]["outcome"], "succeeded");
    assert_eq!(v["sessions"][1]["outcome"], "failed");
    assert!(
        v["sessions"][1]["error"]
            .as_str()
            .expect("error string")
            .contains(&missing_id)
    );
    assert_eq!(v["sessions"][2]["outcome"], "succeeded");
}

/// Why: `summarize_execution` must correctly tally a mix that includes skipped
/// rows too (not just succeeded/failed), since `PruneAction::Skip` rows never
/// reach the HTTP layer.
/// What: builds a synthetic 4-row outcome set and asserts the tally.
/// Test: this test.
#[test]
fn summarize_execution_counts_all_three_outcomes() {
    let rows = vec![
        ExecutedRow {
            id: "1".to_string(),
            name: "a".to_string(),
            outcome: ExecOutcome::Succeeded,
        },
        ExecutedRow {
            id: "2".to_string(),
            name: "b".to_string(),
            outcome: ExecOutcome::Failed("boom".to_string()),
        },
        ExecutedRow {
            id: "3".to_string(),
            name: "c".to_string(),
            outcome: ExecOutcome::Skipped,
        },
        ExecutedRow {
            id: "4".to_string(),
            name: "d".to_string(),
            outcome: ExecOutcome::Succeeded,
        },
    ];
    assert_eq!(summarize_execution(&rows), (2, 1, 1));
}

/// Why: the post-execution text summary must NAME every failed session (not
/// just count it), so an operator can see exactly which one needs attention.
/// What: asserts the `FAILED` line contains the name/id/reason and the
/// trailing summary line has the right counts.
/// Test: this test.
#[test]
fn render_execution_summary_text_reports_failures() {
    let rows = vec![
        ExecutedRow {
            id: "aaaa-1111".to_string(),
            name: "alpha".to_string(),
            outcome: ExecOutcome::Succeeded,
        },
        ExecutedRow {
            id: "bbbb-2222".to_string(),
            name: "bravo".to_string(),
            outcome: ExecOutcome::Failed("session not found".to_string()),
        },
    ];
    let out = render_execution_summary_text(&rows);
    assert!(out.contains("FAILED"));
    assert!(out.contains("bravo"));
    // The row is displayed with its SHORT id (first hyphen-delimited segment,
    // same convention as `render_plan_text`), not the full id.
    assert!(out.contains("bbbb"));
    assert!(out.contains("session not found"));
    assert!(out.contains("1 succeeded, 1 failed, 0 skipped of 2 session(s)"));
}

/// Why: the JSON summary is the programmatic-caller contract; it must expose
/// per-row outcome + error text, not just aggregate counts.
/// What: parses the rendered JSON and asserts the shape and a failed row.
/// Test: this test.
#[test]
fn render_execution_summary_json_reports_failures() {
    let rows = vec![
        ExecutedRow {
            id: "1".to_string(),
            name: "alpha".to_string(),
            outcome: ExecOutcome::Succeeded,
        },
        ExecutedRow {
            id: "2".to_string(),
            name: "bravo".to_string(),
            outcome: ExecOutcome::Failed("boom".to_string()),
        },
    ];
    let json = render_execution_summary_json(&rows).expect("json");
    let v: serde_json::Value = serde_json::from_str(&json).expect("parse");
    assert_eq!(v["succeeded"], 1);
    assert_eq!(v["failed"], 1);
    assert_eq!(v["skipped"], 0);
    assert_eq!(v["total"], 2);
    assert_eq!(v["sessions"][0]["outcome"], "succeeded");
    assert_eq!(v["sessions"][0]["error"], "");
    assert_eq!(v["sessions"][1]["outcome"], "failed");
    assert_eq!(v["sessions"][1]["error"], "boom");
}
