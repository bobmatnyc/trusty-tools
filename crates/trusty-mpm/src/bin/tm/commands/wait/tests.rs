//! Unit tests for `tm wait` (#5843).
//!
//! Why: the two outcomes that carry the whole contract — "still pending, re-run
//! me" and "hard timeout exhausted" — are defined by elapsed time, so both are
//! driven through a fake clock rather than a real sleep. The GitHub verb is
//! driven through canned `gh pr view` JSON, including the bucketed-complete
//! false-DONE shape and the empty rollup.
//! What: per-verb condition tests, budget persistence tests, loop tests, and
//! the status-line/rerun contract tests.

use std::cell::Cell;

use super::*;
use crate::cli::{WaitArgs, WaitFor};
use condition::{CheckCondition, ChecksProbe, Condition, FileCondition, Poll, RunCondition};

/// Build a `WaitArgs` with every field at its default, then let the caller
/// mutate what the case is about.
fn args_for(condition: WaitFor) -> WaitArgs {
    WaitArgs {
        condition,
        pid: None,
        handle: None,
        path: None,
        contains: None,
        pr: None,
        repo: None,
        allow_empty_checks: false,
        timeout: 1800,
        interval: None,
        slice: 100,
        state_dir: None,
        reset: false,
    }
}

/// A clock that never really sleeps — it just advances.
struct FakeClock {
    now: Cell<u64>,
}

impl FakeClock {
    fn at(now: u64) -> Self {
        Self {
            now: Cell::new(now),
        }
    }
}

impl Clock for FakeClock {
    fn now_unix(&self) -> u64 {
        self.now.get()
    }
    fn sleep(&self, secs: u64) {
        self.now.set(self.now.get() + secs);
    }
}

/// A condition that yields a scripted sequence, then repeats its last value.
struct FakeCondition {
    script: Vec<Result<Poll, String>>,
    calls: Cell<usize>,
}

impl FakeCondition {
    fn new(script: Vec<Result<Poll, String>>) -> Self {
        Self {
            script,
            calls: Cell::new(0),
        }
    }
    fn pending_forever() -> Self {
        Self::new(vec![Ok(Poll::Pending("not yet".into()))])
    }
}

impl Condition for FakeCondition {
    fn poll(&self) -> anyhow::Result<Poll> {
        let i = self.calls.get().min(self.script.len() - 1);
        self.calls.set(self.calls.get() + 1);
        match &self.script[i] {
            Ok(p) => Ok(p.clone()),
            Err(e) => Err(anyhow::anyhow!("{e}")),
        }
    }
}

/// A `gh` seam returning canned JSON.
struct FakeProbe {
    json: String,
}

impl ChecksProbe for FakeProbe {
    fn pr_view_json(&self, _pr: u64, _repo: Option<&str>) -> anyhow::Result<String> {
        Ok(self.json.clone())
    }
}

/// A `gh` seam that always fails, for the retry-budget test.
struct FailingProbe;

impl ChecksProbe for FailingProbe {
    fn pr_view_json(&self, _pr: u64, _repo: Option<&str>) -> anyhow::Result<String> {
        anyhow::bail!("gh: command not found")
    }
}

fn budget_at(now: u64, timeout_s: u64) -> Budget {
    Budget {
        spec: "file:/tmp/gate".into(),
        started_at: now,
        updated_at: now,
        polls: 0,
        timeout_s,
    }
}

// ── run verb ─────────────────────────────────────────────────────────────────

/// Why: the whole point of `--for run` — a PID that is gone means the
/// background job finished.
#[test]
fn run_condition_met_for_dead_pid() {
    // `u32::MAX` can never be a live pid (see `core::process::is_process_alive`).
    let c = RunCondition::new(Some(u32::MAX), None);
    assert!(matches!(c.poll().unwrap(), Poll::Met(_)));
}

/// Why: the inverse — a process we know is alive must never read as finished.
#[test]
fn run_condition_pending_for_self() {
    let c = RunCondition::new(Some(std::process::id()), None);
    assert!(matches!(c.poll().unwrap(), Poll::Pending(_)));
}

/// Why: a launcher writes the PID to a handle file; the wait has to read it.
#[test]
fn run_condition_reads_handle_file() {
    let dir = tempfile::tempdir().unwrap();
    let handle = dir.path().join("job.pid");
    std::fs::write(
        &handle,
        format!("pid={}\nstarted=now\n", std::process::id()),
    )
    .unwrap();
    let c = RunCondition::new(None, Some(handle));
    assert!(matches!(c.poll().unwrap(), Poll::Pending(_)));
}

/// Why: a handle file with no PID in it is a real error — silently treating it
/// as "met" would report a job finished that never started.
#[test]
fn run_condition_rejects_unparsable_handle() {
    let dir = tempfile::tempdir().unwrap();
    let handle = dir.path().join("job.pid");
    std::fs::write(&handle, "starting up\n").unwrap();
    let c = RunCondition::new(None, Some(handle));
    let err = c.poll().unwrap_err();
    assert!(format!("{err:#}").contains("no PID found"), "{err:#}");
}

/// Why: both handle shapes launchers actually write must parse.
#[test]
fn parse_handle_pid_accepts_bare_and_keyed() {
    assert_eq!(condition::parse_handle_pid("  4242\n"), Some(4242));
    assert_eq!(
        condition::parse_handle_pid("job=build\npid = 77\n"),
        Some(77)
    );
    assert_eq!(condition::parse_handle_pid("no pid here"), None);
}

// ── file verb ────────────────────────────────────────────────────────────────

/// Why: the sentinel has not been created yet — the normal state early in a wait.
#[test]
fn file_condition_pending_when_absent() {
    let dir = tempfile::tempdir().unwrap();
    let c = FileCondition::new(dir.path().join("gate.txt"), None);
    assert!(matches!(c.poll().unwrap(), Poll::Pending(_)));
}

/// Why: with no `--contains`, existence alone is the condition.
#[test]
fn file_condition_met_on_existence() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("gate.txt");
    std::fs::write(&p, "").unwrap();
    let c = FileCondition::new(p, None);
    assert!(matches!(c.poll().unwrap(), Poll::Met(_)));
}

/// Why: the guard-compliant shape is `<cmd> > gate.txt; echo EXIT=$? >> gate.txt`
/// — the file exists long before the command finishes, so existence alone would
/// report a false DONE. The substring is what actually settles it.
#[test]
fn file_condition_waits_for_substring() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("gate.txt");
    std::fs::write(&p, "compiling trusty-mpm...\n").unwrap();
    let c = FileCondition::new(p.clone(), Some("EXIT=".to_string()));
    assert!(matches!(c.poll().unwrap(), Poll::Pending(_)));

    std::fs::write(&p, "compiling trusty-mpm...\nEXIT=0\n").unwrap();
    assert!(matches!(c.poll().unwrap(), Poll::Met(_)));
}

/// Why: build logs carry non-UTF-8 bytes; a wait must not die on one.
#[test]
fn file_condition_tolerates_non_utf8() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("gate.bin");
    std::fs::write(&p, [0xff, 0xfe, b'D', b'O', b'N', b'E']).unwrap();
    let c = FileCondition::new(p, Some("DONE".to_string()));
    assert!(matches!(c.poll().unwrap(), Poll::Met(_)));
}

// ── check verb ───────────────────────────────────────────────────────────────

fn check_with(json: &str, allow_empty: bool) -> Poll {
    CheckCondition::new(
        FakeProbe {
            json: json.to_string(),
        },
        5843,
        None,
        allow_empty,
    )
    .poll()
    .unwrap()
}

/// Why: one unsettled check keeps the whole rollup unsettled; once every entry
/// carries its own terminal field, the wait is over.
#[test]
fn check_condition_pending_until_all_settled() {
    let running = r#"{"state":"OPEN","statusCheckRollup":[
        {"__typename":"CheckRun","name":"Clippy","status":"COMPLETED","conclusion":"SUCCESS"},
        {"__typename":"CheckRun","name":"Tests","status":"IN_PROGRESS","conclusion":""}
    ]}"#;
    let Poll::Pending(detail) = check_with(running, false) else {
        panic!("a running check must not read as settled");
    };
    assert!(detail.contains("Tests"), "{detail}");

    let done = r#"{"state":"OPEN","statusCheckRollup":[
        {"__typename":"CheckRun","name":"Clippy","status":"COMPLETED","conclusion":"SUCCESS"},
        {"__typename":"CheckRun","name":"Tests","status":"COMPLETED","conclusion":"FAILURE"}
    ]}"#;
    let Poll::Met(detail) = check_with(done, false) else {
        panic!("every check settled must read as met");
    };
    assert!(detail.contains("1 failing"), "{detail}");
}

/// Why: the recorded false-DONE trap. GitHub can report a bucketed-complete
/// value for a check whose own `status`/`conclusion` say it is still running,
/// so `bucket` must not be able to settle anything.
#[test]
fn check_condition_ignores_bucket() {
    let json = r#"{"state":"OPEN","statusCheckRollup":[
        {"__typename":"CheckRun","name":"Tests","bucket":"pass","status":"IN_PROGRESS"}
    ]}"#;
    assert!(
        matches!(check_with(json, false), Poll::Pending(_)),
        "a bucketed-complete entry with a non-terminal status is not settled"
    );
}

/// Why: GitHub reports zero check runs for a window after a push. Reading that
/// as "settled" is the same false-DONE in a different costume.
#[test]
fn check_condition_empty_rollup_is_pending() {
    for json in [
        r#"{"state":"OPEN","statusCheckRollup":[]}"#,
        r#"{"state":"OPEN","statusCheckRollup":null}"#,
        r#"{"state":"OPEN"}"#,
    ] {
        assert!(
            matches!(check_with(json, false), Poll::Pending(_)),
            "empty rollup must be pending: {json}"
        );
    }
    // …unless the caller explicitly opted in for a PR that runs no checks.
    assert!(matches!(
        check_with(r#"{"state":"OPEN","statusCheckRollup":[]}"#, true),
        Poll::Met(_)
    ));
}

/// Why: a legacy commit status carries `state`, not `status`/`conclusion`.
#[test]
fn check_condition_settles_status_contexts() {
    let json = r#"{"state":"OPEN","statusCheckRollup":[
        {"__typename":"StatusContext","context":"vercel","state":"SUCCESS"}
    ]}"#;
    assert!(matches!(check_with(json, false), Poll::Met(_)));

    let pending = r#"{"state":"OPEN","statusCheckRollup":[
        {"__typename":"StatusContext","context":"vercel","state":"PENDING"}
    ]}"#;
    assert!(matches!(check_with(pending, false), Poll::Pending(_)));
}

/// Why: an unrecognised entry shape must fail CLOSED — keep waiting rather than
/// declare a settle nobody proved.
#[test]
fn check_condition_unknown_shape_is_pending() {
    let json = r#"{"state":"OPEN","statusCheckRollup":[{"__typename":"Mystery"}]}"#;
    assert!(matches!(check_with(json, false), Poll::Pending(_)));
}

/// Why: once the PR is merged or closed there is nothing left to settle, and a
/// wait that kept polling would burn its whole budget.
#[test]
fn check_condition_merged_pr_is_met() {
    let json = r#"{"state":"MERGED","statusCheckRollup":[]}"#;
    assert!(matches!(check_with(json, false), Poll::Met(_)));
}

// ── selector validation ──────────────────────────────────────────────────────

/// Why: a missing selector is a usage error before the first poll, not a panic
/// during one.
#[test]
fn build_condition_requires_a_selector() {
    for verb in [WaitFor::Run, WaitFor::File, WaitFor::Check] {
        let Err(err) = condition::build(&args_for(verb)) else {
            panic!("a selector-less {verb:?} must not build");
        };
        assert!(format!("{err:#}").contains("needs"), "{err:#}");
    }
}

/// Why: `--pid` and `--handle` name two different processes; silently picking
/// one would make the wait watch something the agent did not ask for.
#[test]
fn build_condition_rejects_pid_and_handle_together() {
    let mut a = args_for(WaitFor::Run);
    a.pid = Some(1);
    a.handle = Some("/tmp/x.pid".into());
    let Err(err) = condition::build(&a) else {
        panic!("--pid together with --handle must not build");
    };
    assert!(format!("{err:#}").contains("not both"), "{err:#}");
}

/// Why: the budget key must be identical across re-runs of the same wait and
/// different for a different wait, or the hard timeout stops spanning re-runs.
#[test]
fn spec_is_stable_and_discriminating() {
    let mut a = args_for(WaitFor::File);
    a.path = Some("/tmp/gate.txt".into());
    let mut b = args_for(WaitFor::File);
    b.path = Some("/tmp/gate.txt".into());
    assert_eq!(condition::spec(&a), condition::spec(&b));

    b.contains = Some("EXIT=".into());
    assert_ne!(condition::spec(&a), condition::spec(&b));

    let mut c = args_for(WaitFor::Check);
    c.pr = Some(5843);
    assert_ne!(condition::spec(&a), condition::spec(&c));
}

// ── plan clamping ────────────────────────────────────────────────────────────

/// Why: a GitHub read is expensive; a local stat is not.
#[test]
fn plan_uses_the_verb_default_interval() {
    assert_eq!(
        resolve_plan(&args_for(WaitFor::File)).unwrap().interval_s,
        5
    );
    assert_eq!(
        resolve_plan(&args_for(WaitFor::Check)).unwrap().interval_s,
        20
    );
}

/// Why: `--interval 0` against GitHub is a blind-poll storm on someone else's
/// rate limit.
#[test]
fn plan_clamps_interval_to_the_verb_floor() {
    let mut a = args_for(WaitFor::Check);
    a.interval = Some(0);
    assert_eq!(resolve_plan(&a).unwrap().interval_s, 10);

    let mut b = args_for(WaitFor::File);
    b.interval = Some(0);
    assert_eq!(resolve_plan(&b).unwrap().interval_s, 1);
}

/// Why: the slice exists to stay under the harness ceiling — a 5000-second
/// slice would defeat the entire primitive.
#[test]
fn plan_clamps_slice() {
    let mut a = args_for(WaitFor::File);
    a.slice = 5_000;
    assert_eq!(resolve_plan(&a).unwrap().slice_s, 500);

    a.slice = 1;
    assert_eq!(resolve_plan(&a).unwrap().slice_s, 5);

    // A slice can never exceed the whole timeout.
    a.slice = 100;
    a.timeout = 30;
    assert_eq!(resolve_plan(&a).unwrap().slice_s, 30);
}

/// Why: a zero timeout would be met by nothing and expire before the first poll.
#[test]
fn plan_rejects_a_zero_timeout() {
    let mut a = args_for(WaitFor::File);
    a.timeout = 0;
    assert!(resolve_plan(&a).is_err());
}

// ── the poll loop ────────────────────────────────────────────────────────────

fn plan(interval_s: u64, slice_s: u64, timeout_s: u64) -> Plan {
    Plan {
        timeout_s,
        interval_s,
        slice_s,
    }
}

/// Why: an already-satisfied condition must cost zero sleeps — the common case
/// on a re-run.
#[test]
fn loop_returns_met_on_first_poll() {
    let clock = FakeClock::at(1_000);
    let cond = FakeCondition::new(vec![Ok(Poll::Met("done".into()))]);
    let mut b = budget_at(1_000, 1800);
    let out = poll_until(&cond, &plan(10, 100, 1800), &mut b, &clock).unwrap();
    assert_eq!(out.status, Status::Met);
    assert_eq!(clock.now_unix(), 1_000, "met must not sleep");
    assert_eq!(b.polls, 1);
}

/// Why: THE contract. A wait longer than one slice must come back as `pending`
/// — inside the harness ceiling, with budget left — rather than block on.
#[test]
fn loop_returns_pending_at_slice_end() {
    let clock = FakeClock::at(1_000);
    let cond = FakeCondition::pending_forever();
    let mut b = budget_at(1_000, 1800);
    let out = poll_until(&cond, &plan(10, 100, 1800), &mut b, &clock).unwrap();

    assert_eq!(out.status, Status::Pending);
    assert_eq!(out.status.exit_code(), 75, "pending is EX_TEMPFAIL");
    assert!(!out.status.is_terminal());
    assert_eq!(clock.now_unix(), 1_100, "one invocation blocks one slice");
    assert_eq!(
        b.remaining(clock.now_unix()),
        1_700,
        "budget must carry over"
    );
    assert_eq!(b.polls, 11);
}

/// Why: the other half of the contract. A budget already spent across earlier
/// invocations ends the wait for good, and never as a success.
#[test]
fn loop_returns_timeout_when_budget_exhausted() {
    // Started 1790s ago with a 1800s budget: 10s of budget left, 100s of slice.
    let clock = FakeClock::at(11_790);
    let cond = FakeCondition::pending_forever();
    let mut b = budget_at(10_000, 1_800);
    let out = poll_until(&cond, &plan(10, 100, 1_800), &mut b, &clock).unwrap();

    assert_eq!(out.status, Status::Timeout);
    assert_eq!(out.status.exit_code(), 1);
    assert!(out.status.is_terminal());
    assert_eq!(clock.now_unix(), 11_800, "the nap stops at the deadline");
    assert_eq!(b.remaining(clock.now_unix()), 0);
}

/// Why: a repeated probe failure is a real fault (missing `gh`, deleted PR) and
/// hiding it behind an endless retry would waste the whole budget.
#[test]
fn loop_gives_up_after_repeated_probe_errors() {
    let clock = FakeClock::at(0);
    let cond = CheckCondition::new(FailingProbe, 1, None, false);
    let mut b = budget_at(0, 1800);
    let err = poll_until(&cond, &plan(10, 100, 1800), &mut b, &clock).unwrap_err();
    assert!(
        format!("{err:#}").contains("failed 4 times in a row"),
        "{err:#}"
    );
}

/// Why: one dropped connection must not kill an hour-long wait.
#[test]
fn loop_recovers_from_a_transient_probe_error() {
    let clock = FakeClock::at(0);
    let cond = FakeCondition::new(vec![
        Err("connection reset".into()),
        Ok(Poll::Pending("still going".into())),
        Ok(Poll::Met("done".into())),
    ]);
    let mut b = budget_at(0, 1800);
    let out = poll_until(&cond, &plan(10, 100, 1800), &mut b, &clock).unwrap();
    assert_eq!(out.status, Status::Met);
    assert_eq!(b.polls, 3);
}

// ── budget persistence ───────────────────────────────────────────────────────

/// Why: two different waits must not share one deadline, and one wait must
/// always find its own record.
#[test]
fn path_for_is_stable_and_discriminating() {
    let dir = std::path::Path::new("/tmp/tm-wait");
    assert_eq!(
        budget::path_for(dir, "file:/tmp/gate.txt"),
        budget::path_for(dir, "file:/tmp/gate.txt")
    );
    assert_ne!(
        budget::path_for(dir, "file:/tmp/gate.txt"),
        budget::path_for(dir, "file:/tmp/other.txt")
    );
    let name = budget::path_for(dir, "check:owner/repo#1")
        .file_name()
        .unwrap()
        .to_string_lossy()
        .to_string();
    assert!(
        !name.contains('/'),
        "the spec's separators must be sanitised"
    );
}

/// Why: without this the hard timeout resets on every re-run, and a wait never
/// expires.
#[test]
fn budget_resumes_across_invocations() {
    let dir = tempfile::tempdir().unwrap();
    let p = budget::path_for(dir.path(), "file:/tmp/gate.txt");

    let mut first = budget::load_or_start(&p, "file:/tmp/gate.txt", 1800, 1_000, 600);
    first.polls = 11;
    first.updated_at = 1_100;
    budget::save(&p, &first).unwrap();

    let second = budget::load_or_start(&p, "file:/tmp/gate.txt", 1800, 1_150, 600);
    assert_eq!(second.started_at, 1_000, "the deadline must not restart");
    assert_eq!(second.polls, 11);
    assert_eq!(second.remaining(1_150), 1_650);
}

/// Why: a changed `--timeout` means the agent changed the terms of the wait.
#[test]
fn budget_restarts_on_timeout_change() {
    let dir = tempfile::tempdir().unwrap();
    let p = budget::path_for(dir.path(), "file:/tmp/gate.txt");
    budget::save(
        &p,
        &budget::load_or_start(&p, "file:/tmp/gate.txt", 1800, 1_000, 600),
    )
    .unwrap();

    let next = budget::load_or_start(&p, "file:/tmp/gate.txt", 60, 1_100, 600);
    assert_eq!(next.started_at, 1_100);
    assert_eq!(next.timeout_s, 60);
}

/// Why: a budget file left behind by a wait nobody came back to must not make
/// the next identical wait report an instant timeout.
#[test]
fn budget_abandons_a_stale_record() {
    let dir = tempfile::tempdir().unwrap();
    let p = budget::path_for(dir.path(), "file:/tmp/gate.txt");
    budget::save(
        &p,
        &budget::load_or_start(&p, "file:/tmp/gate.txt", 1800, 1_000, 600),
    )
    .unwrap();

    let much_later = budget::load_or_start(&p, "file:/tmp/gate.txt", 1800, 100_000, 600);
    assert_eq!(much_later.started_at, 100_000);
    assert_eq!(much_later.remaining(100_000), 1800);
}

/// Why: a clock that jumped backwards would wrap `elapsed` and fake a timeout.
#[test]
fn budget_restarts_when_the_clock_moved_backwards() {
    let dir = tempfile::tempdir().unwrap();
    let p = budget::path_for(dir.path(), "file:/tmp/gate.txt");
    budget::save(
        &p,
        &budget::load_or_start(&p, "file:/tmp/gate.txt", 1800, 5_000, 600),
    )
    .unwrap();
    let earlier = budget::load_or_start(&p, "file:/tmp/gate.txt", 1800, 4_000, 600);
    assert_eq!(earlier.started_at, 4_000);
}

/// Why: the budget is an optimisation for the deadline, not a source of truth
/// worth failing a wait over.
#[test]
fn budget_ignores_corrupt_file() {
    let dir = tempfile::tempdir().unwrap();
    let p = budget::path_for(dir.path(), "file:/tmp/gate.txt");
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(&p, "{not json").unwrap();
    let b = budget::load_or_start(&p, "file:/tmp/gate.txt", 1800, 7_000, 600);
    assert_eq!(b.started_at, 7_000);
}

/// Why: a terminal outcome must not leave an exhausted deadline behind for the
/// next wait on the same condition.
#[test]
fn budget_clear_removes_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let p = budget::path_for(dir.path(), "file:/tmp/gate.txt");
    budget::save(
        &p,
        &budget::load_or_start(&p, "file:/tmp/gate.txt", 1800, 1_000, 600),
    )
    .unwrap();
    assert!(p.exists());
    budget::clear(&p);
    assert!(!p.exists());
    // Clearing a file that is already gone is not an error.
    budget::clear(&p);
}

// ── the stdout contract ──────────────────────────────────────────────────────

/// Why: the pending line is the only thing the re-running agent reads. It has
/// to say how much budget is left and exactly what to re-run.
#[test]
fn status_line_carries_remaining_budget() {
    let b = budget_at(1_000, 1800);
    let out = Outcome {
        status: Status::Pending,
        detail: "3 of 12 check(s) unsettled".into(),
    };
    let line = status_line(
        &out,
        WaitFor::Check,
        &b,
        1_100,
        "tm wait --for check --pr 1",
    );
    assert!(
        line.starts_with("tm-wait status=pending for=check "),
        "{line}"
    );
    assert!(line.contains("elapsed=100"), "{line}");
    assert!(line.contains("remaining=1700"), "{line}");
    assert!(
        line.contains(r#"rerun="tm wait --for check --pr 1""#),
        "{line}"
    );
}

/// Why: a terminal line must not invite a re-run, and a detail containing a
/// quote or a newline must not break the line into two.
#[test]
fn status_line_quotes_detail() {
    let b = budget_at(1_000, 1800);
    let out = Outcome {
        status: Status::Met,
        detail: "/tmp/g.txt contains \"EXIT=\"\nand more".into(),
    };
    let line = status_line(&out, WaitFor::File, &b, 1_005, "tm wait --for file");
    assert!(!line.contains("rerun="), "{line}");
    assert_eq!(line.lines().count(), 1, "{line}");
    assert!(
        line.contains("detail=\"/tmp/g.txt contains 'EXIT=' and more\""),
        "{line}"
    );
}

/// Why: "re-run the identical command" only works if the printed command really
/// is identical — dropping `--timeout` would silently restart the deadline.
#[test]
fn rerun_command_reproduces_the_selector() {
    let mut a = args_for(WaitFor::File);
    a.path = Some("/tmp/gate.txt".into());
    a.contains = Some("EXIT=".into());
    a.timeout = 600;
    let cmd = rerun_command(&a);
    assert_eq!(
        cmd,
        r#"tm wait --for file --path /tmp/gate.txt --contains "EXIT=" --timeout 600 --slice 100"#
    );
}

/// Why: re-running with `--reset` would wipe the very budget the pending line
/// just reported.
#[test]
fn rerun_command_never_repeats_reset() {
    let mut a = args_for(WaitFor::Run);
    a.pid = Some(4242);
    a.reset = true;
    assert!(!rerun_command(&a).contains("--reset"));
    assert!(rerun_command(&a).contains("--pid 4242"));
}

/// Why: the four exit codes ARE the contract; a change to one is a breaking
/// change to every agent that reads them.
#[test]
fn exit_codes_are_the_documented_contract() {
    assert_eq!(Status::Met.exit_code(), 0);
    assert_eq!(Status::Timeout.exit_code(), 1);
    assert_eq!(Status::Error.exit_code(), 2);
    assert_eq!(Status::Pending.exit_code(), 75);
    assert!(Status::Met.is_terminal());
    assert!(Status::Timeout.is_terminal());
    assert!(Status::Error.is_terminal());
    assert!(!Status::Pending.is_terminal());
}

// ── clap surface ─────────────────────────────────────────────────────────────
//
// These live here rather than in `tests_behavior_a.rs` because the SLOC cap
// classifies that file as PRODUCTION (its basename is not `tests.rs`) and it is
// already at the 500-SLOC line.

/// Why (#5843): `tm wait --for run` is the verb an agent reaches for after
/// backgrounding a cold build; the PID selector and the hard timeout must
/// round-trip.
#[test]
fn cli_parses_wait_for_run() {
    use clap::Parser as _;
    let cli = crate::cli::Cli::try_parse_from([
        "trusty-mpm",
        "wait",
        "--for",
        "run",
        "--pid",
        "4242",
        "--timeout",
        "600",
    ])
    .unwrap();
    match cli.command.unwrap() {
        crate::cli::Command::Wait(args) => {
            assert_eq!(args.condition, WaitFor::Run);
            assert_eq!(args.pid, Some(4242));
            assert_eq!(args.timeout, 600);
            assert_eq!(args.slice, 100, "the slice default keeps one call short");
        }
        other => panic!("expected Wait, got {other:?}"),
    }
}

/// Why (#5843): the sentinel form — a path plus the literal the file must carry
/// once the backgrounded command has written its exit line.
#[test]
fn cli_parses_wait_for_file() {
    use clap::Parser as _;
    let cli = crate::cli::Cli::try_parse_from([
        "trusty-mpm",
        "wait",
        "--for",
        "file",
        "--path",
        "/tmp/gate.txt",
        "--contains",
        "EXIT=",
    ])
    .unwrap();
    match cli.command.unwrap() {
        crate::cli::Command::Wait(args) => {
            assert_eq!(args.condition, WaitFor::File);
            assert_eq!(
                args.path.as_deref(),
                Some(std::path::Path::new("/tmp/gate.txt"))
            );
            assert_eq!(args.contains.as_deref(), Some("EXIT="));
            assert_eq!(
                args.timeout, 1800,
                "the hard timeout defaults to 30 minutes"
            );
        }
        other => panic!("expected Wait, got {other:?}"),
    }
}

/// Why (#5843): the CI form. `--allow-empty-checks` must stay OFF unless it is
/// typed — an empty rollup reading as settled is the recorded false-DONE trap.
#[test]
fn cli_parses_wait_for_check() {
    use clap::Parser as _;
    let cli = crate::cli::Cli::try_parse_from([
        "trusty-mpm",
        "wait",
        "--for",
        "check",
        "--pr",
        "5843",
        "--repo",
        "bobmatnyc/trusty-tools",
    ])
    .unwrap();
    match cli.command.unwrap() {
        crate::cli::Command::Wait(args) => {
            assert_eq!(args.condition, WaitFor::Check);
            assert_eq!(args.pr, Some(5843));
            assert_eq!(args.repo.as_deref(), Some("bobmatnyc/trusty-tools"));
            assert!(!args.allow_empty_checks);
        }
        other => panic!("expected Wait, got {other:?}"),
    }
}
