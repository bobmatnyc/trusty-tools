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
        required_only: false,
        watch: false,
        owner: None,
        timeout: 1800,
        interval: None,
        slice: 100,
        state_dir: None,
        reset: false,
    }
}

/// A clock that never really sleeps — it just advances, and records each nap
/// (#10550) so a test can pin the backoff the loop chose.
struct FakeClock {
    now: Cell<u64>,
    sleeps: std::cell::RefCell<Vec<u64>>,
}

impl FakeClock {
    fn at(now: u64) -> Self {
        Self {
            now: Cell::new(now),
            sleeps: std::cell::RefCell::new(Vec::new()),
        }
    }
    /// Every nap, in seconds, in call order.
    fn sleeps(&self) -> Vec<u64> {
        self.sleeps.borrow().clone()
    }
}

impl Clock for FakeClock {
    fn now_unix(&self) -> u64 {
        self.now.get()
    }
    fn sleep(&self, secs: u64) {
        self.sleeps.borrow_mut().push(secs);
        self.now.set(self.now.get() + secs);
    }
}

/// A budget generous enough that no direct-`poll` test is about it (#7956).
const ANY_BUDGET: std::time::Duration = std::time::Duration::from_secs(30);

/// A condition that yields a scripted sequence, then repeats its last value.
///
/// #7956: it also RECORDS the budget each poll was handed, which is what pins
/// that the driver never lets a poll outlive the slice it was started in.
struct FakeCondition {
    script: Vec<Result<Poll, String>>,
    calls: Cell<usize>,
    budgets: std::cell::RefCell<Vec<u64>>,
}

impl FakeCondition {
    fn new(script: Vec<Result<Poll, String>>) -> Self {
        Self {
            script,
            calls: Cell::new(0),
            budgets: std::cell::RefCell::new(Vec::new()),
        }
    }
    fn pending_forever() -> Self {
        Self::new(vec![Ok(Poll::Pending("not yet".into()))])
    }
    /// Seconds each poll was told it had, in call order (#7956).
    fn budgets(&self) -> Vec<u64> {
        self.budgets.borrow().clone()
    }
}

impl Condition for FakeCondition {
    fn poll(&self, budget: std::time::Duration) -> anyhow::Result<Poll> {
        self.budgets.borrow_mut().push(budget.as_secs());
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
    fn pr_view_json(
        &self,
        _pr: u64,
        _repo: Option<&str>,
        _budget: std::time::Duration,
    ) -> anyhow::Result<String> {
        Ok(self.json.clone())
    }
}

/// A `gh` seam that always fails, for the retry-budget test.
struct FailingProbe;

impl ChecksProbe for FailingProbe {
    fn pr_view_json(
        &self,
        _pr: u64,
        _repo: Option<&str>,
        _budget: std::time::Duration,
    ) -> anyhow::Result<String> {
        anyhow::bail!("gh: command not found")
    }
}

/// A fresh budget: `load_or_start` on a path that cannot exist.
///
/// #10668: built through the loader rather than a struct literal, so a new
/// `Budget` field never has to be threaded through every test.
fn budget_at(now: u64, timeout_s: u64) -> Budget {
    let missing = std::path::Path::new("/nonexistent/tm-wait-test/none.json");
    budget::load_or_start(missing, "file:/tmp/gate", timeout_s, now, 600)
}

// ── run verb ─────────────────────────────────────────────────────────────────

/// Why: the whole point of `--for run` — a PID that is gone means the
/// background job finished.
#[test]
fn run_condition_met_for_dead_pid() {
    // `u32::MAX` can never be a live pid (see `core::process::is_process_alive`).
    let c = RunCondition::new(Some(u32::MAX), None);
    assert!(matches!(c.poll(ANY_BUDGET).unwrap(), Poll::Met(_)));
}

/// Why: the inverse — a process we know is alive must never read as finished.
#[test]
fn run_condition_pending_for_self() {
    let c = RunCondition::new(Some(std::process::id()), None);
    assert!(matches!(c.poll(ANY_BUDGET).unwrap(), Poll::Pending(_)));
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
    assert!(matches!(c.poll(ANY_BUDGET).unwrap(), Poll::Pending(_)));
}

/// Why: a handle file with no PID in it is a real error — silently treating it
/// as "met" would report a job finished that never started.
#[test]
fn run_condition_rejects_unparsable_handle() {
    let dir = tempfile::tempdir().unwrap();
    let handle = dir.path().join("job.pid");
    std::fs::write(&handle, "starting up\n").unwrap();
    let c = RunCondition::new(None, Some(handle));
    let err = c.poll(ANY_BUDGET).unwrap_err();
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
    assert!(matches!(c.poll(ANY_BUDGET).unwrap(), Poll::Pending(_)));
}

/// Why: with no `--contains`, existence alone is the condition.
#[test]
fn file_condition_met_on_existence() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("gate.txt");
    std::fs::write(&p, "").unwrap();
    let c = FileCondition::new(p, None);
    assert!(matches!(c.poll(ANY_BUDGET).unwrap(), Poll::Met(_)));
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
    assert!(matches!(c.poll(ANY_BUDGET).unwrap(), Poll::Pending(_)));

    std::fs::write(&p, "compiling trusty-mpm...\nEXIT=0\n").unwrap();
    assert!(matches!(c.poll(ANY_BUDGET).unwrap(), Poll::Met(_)));
}

/// Why: build logs carry non-UTF-8 bytes; a wait must not die on one.
#[test]
fn file_condition_tolerates_non_utf8() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("gate.bin");
    std::fs::write(&p, [0xff, 0xfe, b'D', b'O', b'N', b'E']).unwrap();
    let c = FileCondition::new(p, Some("DONE".to_string()));
    assert!(matches!(c.poll(ANY_BUDGET).unwrap(), Poll::Met(_)));
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
    .poll(ANY_BUDGET)
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

/// REGRESSION (#8638): a concurrency-cancelled run superseded by a fresh
/// SUCCESS of the same check is not a live failure; a newer run still in
/// flight keeps the wait pending.
#[test]
fn check_condition_duplicate_run_uses_latest() {
    let settled = r#"{"state":"OPEN","statusCheckRollup":[
        {"__typename":"CheckRun","name":"Tests","status":"COMPLETED","conclusion":"CANCELLED",
         "startedAt":"2026-09-25T22:50:00Z","completedAt":"2026-09-25T22:51:15Z"},
        {"__typename":"CheckRun","name":"Tests","status":"COMPLETED","conclusion":"SUCCESS",
         "startedAt":"2026-09-25T22:51:10Z","completedAt":"2026-09-25T22:53:26Z"}
    ]}"#;
    let Poll::Met(detail) = check_with(settled, false) else {
        panic!("the latest run settled");
    };
    assert!(detail.contains("1 check(s) settled, 0 failing"), "{detail}");

    let running = r#"{"state":"OPEN","statusCheckRollup":[
        {"__typename":"CheckRun","name":"Tests","status":"COMPLETED","conclusion":"SUCCESS",
         "startedAt":"2026-09-25T22:50:00Z","completedAt":"2026-09-25T22:51:15Z"},
        {"__typename":"CheckRun","name":"Tests","status":"IN_PROGRESS","conclusion":"",
         "startedAt":"2026-09-25T22:52:00Z","completedAt":"0001-01-01T00:00:00Z"}
    ]}"#;
    assert!(matches!(check_with(running, false), Poll::Pending(_)));
}

/// REGRESSION (#8638): a queued rerun with no timestamps keeps the wait
/// pending; an older SUCCESS of the same check must not read as settled.
#[test]
fn check_condition_duplicate_queued_rerun_is_pending() {
    let json = r#"{"state":"OPEN","statusCheckRollup":[
        {"__typename":"CheckRun","name":"Tests","status":"COMPLETED","conclusion":"SUCCESS",
         "startedAt":"2026-09-25T22:50:00Z","completedAt":"2026-09-25T22:51:15Z"},
        {"__typename":"CheckRun","name":"Tests","status":"QUEUED","conclusion":""}
    ]}"#;
    assert!(matches!(check_with(json, false), Poll::Pending(_)));
}

/// REGRESSION (#8638): a CheckRun and a StatusContext that share a name are
/// two checks; neither may hide the other.
#[test]
fn check_condition_check_and_status_same_name_both_counted() {
    let json = r#"{"state":"OPEN","statusCheckRollup":[
        {"__typename":"StatusContext","context":"Tests","state":"FAILURE",
         "startedAt":"2026-09-25T22:50:00Z"},
        {"__typename":"CheckRun","name":"Tests","status":"COMPLETED","conclusion":"SUCCESS",
         "startedAt":"2026-09-25T22:51:00Z","completedAt":"2026-09-25T22:53:26Z"}
    ]}"#;
    let Poll::Met(detail) = check_with(json, false) else {
        panic!("both checks settled");
    };
    assert!(detail.contains("2 check(s) settled, 1 failing"), "{detail}");
}

/// REGRESSION (#8638): only SUCCESS, NEUTRAL and SKIPPED pass; every other
/// settled result counts as failing.
#[test]
fn check_condition_counts_every_non_passing_result() {
    let json = r#"{"state":"OPEN","statusCheckRollup":[
        {"__typename":"CheckRun","name":"a","status":"COMPLETED","conclusion":"STARTUP_FAILURE"},
        {"__typename":"CheckRun","name":"b","status":"COMPLETED","conclusion":"ACTION_REQUIRED"},
        {"__typename":"CheckRun","name":"c","status":"COMPLETED","conclusion":"STALE"},
        {"__typename":"CheckRun","name":"d","status":"COMPLETED","conclusion":"NEUTRAL"},
        {"__typename":"CheckRun","name":"e","status":"COMPLETED","conclusion":"SKIPPED"},
        {"__typename":"CheckRun","name":"f","status":"COMPLETED","conclusion":"SUCCESS"}
    ]}"#;
    let Poll::Met(detail) = check_with(json, false) else {
        panic!("every check settled");
    };
    assert!(detail.contains("6 check(s) settled, 3 failing"), "{detail}");
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

/// The production probe runner returns a child's output (#7956).
///
/// Why: the bound must not cost the normal answer. This is the same function
/// `GhChecksProbe` runs `gh` through, with an argv a test can rely on.
#[test]
fn probe_command_answers_within_its_budget() {
    let mut cmd = std::process::Command::new("echo");
    cmd.arg("hello");
    let out = condition::run_within(cmd, "echo", std::time::Duration::from_secs(10))
        .expect("a command that exits must be reported");
    assert_eq!(out, "hello");
}

/// A probe that outlives its budget is killed and reported, never awaited.
///
/// Why (#7956): `gh pr view` used to run through `nonempty_stdout_blocking`,
/// which waits forever. One stalled read therefore held `tm wait` past its
/// `--slice` ceiling and past whatever ran it, and the process was SIGKILLed —
/// exit 137, which is none of the four documented statuses. This is a real
/// process-level test: it spawns a child that would run for 30 seconds, gives
/// it one, and requires an `Err` back promptly. Against the pre-#7956 probe
/// there is no deadline at all and the call blocks for the full 30 seconds.
#[test]
fn probe_command_that_outlives_its_budget_is_killed_not_awaited() {
    let mut cmd = std::process::Command::new("sleep");
    cmd.arg("30");
    let started = std::time::Instant::now();
    let err = condition::run_within(cmd, "sleep 30", std::time::Duration::from_secs(1))
        .expect_err("a child past its budget must not be waited on");
    let elapsed = started.elapsed();
    assert!(
        elapsed < std::time::Duration::from_secs(10),
        "the probe blocked for {elapsed:?} instead of returning at its budget"
    );
    assert!(
        format!("{err:#}").contains("did not answer within its budget"),
        "{err:#}"
    );
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
    // #7956: ten polls, at t+0 through t+90. The eleventh — at t+100, ON the
    // ceiling — is the one this used to make and then return after; see
    // `loop_never_starts_a_poll_it_has_no_slice_for`.
    assert_eq!(b.polls, 10);
}

/// A poll is never started at or past the slice ceiling (#7956).
///
/// Why: the nap is capped AT `slice_end`, so the pre-#7956 loop woke exactly on
/// the ceiling and polled once more. That last poll's whole duration lands
/// OUTSIDE the slice the caller sized its own timeout against — with a real
/// `gh` read that is seconds, and with a stalled one it is unbounded, which is
/// how the invocation got SIGKILLed (137) instead of returning 75. Against the
/// pre-#7956 loop this reads 11 polls and fails.
#[test]
fn loop_never_starts_a_poll_it_has_no_slice_for() {
    let clock = FakeClock::at(1_000);
    let cond = FakeCondition::pending_forever();
    let mut b = budget_at(1_000, 1800);
    let out = poll_until(&cond, &plan(10, 100, 1800), &mut b, &clock).unwrap();

    assert_eq!(out.status, Status::Pending);
    let started_at: Vec<u64> = (0..b.polls).map(|i| 1_000 + i * 10).collect();
    assert_eq!(*started_at.last().expect("at least one poll"), 1_090);
    assert!(
        started_at.iter().all(|t| *t < 1_100),
        "a poll was started on or past the ceiling: {started_at:?}"
    );
}

/// Every poll is told how much of the slice is left (#7956).
///
/// Why: the `--slice` ceiling used to bound only the naps, so a probe that
/// blocked carried the invocation past it. The budget handed down is what lets
/// `GhChecksProbe` kill a stalled `gh` in time; it must shrink toward the
/// ceiling and never exceed the time remaining before it.
#[test]
fn loop_bounds_each_poll_to_the_time_left_in_the_slice() {
    let clock = FakeClock::at(1_000);
    let cond = FakeCondition::pending_forever();
    let mut b = budget_at(1_000, 1800);
    poll_until(&cond, &plan(10, 100, 1800), &mut b, &clock).unwrap();

    assert_eq!(
        cond.budgets(),
        vec![100, 90, 80, 70, 60, 50, 40, 30, 20, 10],
        "each poll must be bounded by the slice it was started in"
    );
}

/// The hard deadline bounds a poll too, not only the slice (#7956).
///
/// Why: with 10 seconds of budget left, a probe allowed the full 100-second
/// slice would spend ninety seconds the wait does not have before anyone could
/// report `timeout`.
#[test]
fn loop_bounds_a_poll_to_the_hard_deadline_when_it_is_nearer() {
    let clock = FakeClock::at(11_790);
    let cond = FakeCondition::pending_forever();
    let mut b = budget_at(10_000, 1_800);
    poll_until(&cond, &plan(10, 100, 1_800), &mut b, &clock).unwrap();

    assert_eq!(cond.budgets(), vec![10]);
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

// ── the rate-limit backoff (#10550) ──────────────────────────────────────────

/// The probe error `GhChecksProbe` returns when the pinned gh shim refuses a
/// call on the primary GraphQL rate limit. A LITERAL copy of the shim's line,
/// so a change to the shim's wording fails these tests.
fn shim_refusal(reset_epoch: Option<u64>) -> String {
    let when = match reset_epoch {
        Some(epoch) => format!("it resets at 2026-11-09 02:00:00 UTC (epoch {epoch})"),
        None => "the reset time is unknown (gh api rate_limit did not answer)".to_string(),
    };
    format!(
        "`gh pr view 7` failed (exit status: 1): tm: GitHub refused this gh call on its \
         primary GraphQL rate limit (gh account 'bot' on github.com); {when}. tm did not retry it."
    )
}

/// Why: only the shim's own sentence earns a backoff, and its epoch is read.
#[test]
fn classify_reads_the_shim_refusal_and_its_reset_epoch() {
    let hit = |text: String| rate_limit::classify(&anyhow::anyhow!(text));
    assert_eq!(
        hit(shim_refusal(Some(1_767_225_600))),
        Some(rate_limit::RateLimited {
            reset_epoch: Some(1_767_225_600)
        })
    );
    assert_eq!(
        hit(shim_refusal(None)),
        Some(rate_limit::RateLimited { reset_epoch: None })
    );
    // A REST quota and gh's own unmarked text are generic probe errors.
    for other in [
        "gh: API rate limit exceeded for user ID 1. (HTTP 403)",
        "GraphQL: API rate limit already exceeded for user ID 1.",
        "connection reset",
    ] {
        assert_eq!(hit(other.to_string()), None, "{other}");
    }
}

/// Why: a refused poll must wait for the reset the shim named, not re-poll on
/// the 20-second interval and burn the probe-error budget.
#[test]
fn loop_backs_off_to_the_reset_on_a_rate_limited_probe() {
    let clock = FakeClock::at(1_000);
    let cond = FakeCondition::new(vec![
        Err(shim_refusal(Some(1_137))),
        Ok(Poll::Met("done".into())),
    ]);
    let mut b = budget_at(1_000, 1800);
    let out = poll_until(&cond, &plan(20, 500, 1800), &mut b, &clock).unwrap();
    assert_eq!(out.status, Status::Met);
    assert_eq!(clock.sleeps(), vec![137], "the nap must be reset minus now");
    assert_eq!(b.polls, 2);
}

/// Why: with no reset to wait for, back off 20, 40, 80 s, never past 5 min,
/// and never count those polls against the 3-error budget.
#[test]
fn loop_backs_off_exponentially_when_the_reset_is_unknown() {
    let clock = FakeClock::at(0);
    let cond = FakeCondition::new(vec![
        Err(shim_refusal(None)),
        Err(shim_refusal(None)),
        Err(shim_refusal(None)),
        Err(shim_refusal(None)),
        Ok(Poll::Met("done".into())),
    ]);
    let mut b = budget_at(0, 1800);
    let out = poll_until(&cond, &plan(20, 500, 1800), &mut b, &clock).unwrap();
    assert_eq!(out.status, Status::Met);
    assert_eq!(clock.sleeps(), vec![20, 40, 80, 160]);
    let unknown = rate_limit::RateLimited { reset_epoch: None };
    assert_eq!(
        rate_limit::backoff_secs(unknown, 0, 20, 5),
        300,
        "5-min cap"
    );
}

/// Why: fail closed. A refusal that keeps coming ends the wait as an error
/// naming the rate limit — never as met, never as a generic probe failure.
#[test]
fn rate_limit_exhaustion_errors_not_met() {
    let clock = FakeClock::at(0);
    let cond = FakeCondition::new(vec![Err(shim_refusal(None))]);
    let mut b = budget_at(0, 1800);
    let err = poll_until(&cond, &plan(20, 500, 1800), &mut b, &clock).unwrap_err();
    let text = format!("{err:#}");
    assert!(text.contains("rate limit"), "{text}");
    assert!(text.contains("5 polls in a row"), "{text}");
    assert_eq!(b.polls, 5);
    assert_eq!(Status::Error.exit_code(), EXIT_ERROR);
}

/// A probe error carrying the shim's quota-left refusal (#11233). A LITERAL
/// copy of the shim's line, like [`shim_refusal`].
fn shim_short_window_refusal() -> String {
    "`gh pr view 7` failed (exit status: 1): tm: GitHub refused this gh call on a \
     short-window or secondary GraphQL rate limit (gh account 'bot' on github.com); the \
     hourly GraphQL quota still has 4186 points left, so this is not the primary limit. \
     tm did not retry it. gh said: GraphQL: API rate limit already exceeded for user ID 1."
        .to_string()
}

/// Why: #11233 — a refusal with quota left must not be reported as the
/// primary limit, neither in the pending detail nor in the fail-closed error.
#[test]
fn a_short_window_refusal_is_not_reported_as_the_primary_limit() {
    const SHORT: &str = "short-window or secondary GraphQL rate limit";
    let cond = FakeCondition::new(vec![Err(shim_short_window_refusal())]);
    let clock = FakeClock::at(1_000);
    let mut b = budget_at(1_000, 1800);
    let out = poll_until(&cond, &plan(20, 10, 1800), &mut b, &clock).unwrap();
    assert_eq!(out.status, Status::Pending);
    assert!(
        out.detail
            .starts_with(&format!("GitHub {SHORT} (refusal 1)")),
        "{}",
        out.detail
    );
    assert!(
        !out.detail.contains("primary GraphQL rate limit"),
        "{}",
        out.detail
    );

    let cond = FakeCondition::new(vec![Err(shim_short_window_refusal())]);
    let clock = FakeClock::at(0);
    let mut b = budget_at(0, 1800);
    let err = poll_until(&cond, &plan(20, 500, 1800), &mut b, &clock).unwrap_err();
    let text = format!("{err:#}");
    assert!(
        text.starts_with(&format!("GitHub's {SHORT} refused")),
        "{text}"
    );
    assert!(!text.contains("primary GraphQL rate limit"), "{text}");
}

/// Why: a reset past the hard deadline must not stretch the wait past it.
#[test]
fn loop_rate_limit_sleep_never_extends_the_hard_deadline() {
    // 60s of budget left, a reset 300s away, a 500s slice.
    let clock = FakeClock::at(11_740);
    let cond = FakeCondition::new(vec![Err(shim_refusal(Some(12_040)))]);
    let mut b = budget_at(10_000, 1_800);
    let out = poll_until(&cond, &plan(20, 500, 1_800), &mut b, &clock).unwrap();
    assert_eq!(out.status, Status::Timeout);
    assert_eq!(clock.sleeps(), vec![60]);
    assert_eq!(clock.now_unix(), 11_800, "stopped at the deadline");
}

/// Why: a slice that ends mid-backoff is pending (75), and says why.
#[test]
fn loop_reports_pending_with_the_rate_limit_when_a_slice_ends_in_a_backoff() {
    let clock = FakeClock::at(1_000);
    let cond = FakeCondition::new(vec![Err(shim_refusal(Some(1_300)))]);
    let mut b = budget_at(1_000, 1800);
    let out = poll_until(&cond, &plan(20, 100, 1800), &mut b, &clock).unwrap();
    assert_eq!(out.status, Status::Pending);
    assert_eq!(out.status.exit_code(), EXIT_PENDING);
    assert_eq!(clock.sleeps(), vec![100], "clamped to the slice");
    assert!(out.detail.contains("rate limit"), "{}", out.detail);
}

/// Why: with a reset beyond the slice, each invocation sees one refusal and
/// returns pending. The streak must survive the state file, or a refusal that
/// never clears would re-poll until the hard deadline instead of exiting 2.
#[test]
fn rate_limit_streak_spans_slices_and_errors_on_the_fifth_refusal() {
    let dir = tempfile::tempdir().unwrap();
    let spec = "check:pr=7";
    let path = budget::path_for(dir.path(), spec);
    let clock = FakeClock::at(1_000);
    let cond = FakeCondition::new(vec![Err(shim_refusal(Some(100_000)))]);
    let p = plan(20, 100, 3_600);
    let resume = || budget::load_or_start(&path, spec, 3_600, clock.now_unix(), 600);
    for slice in 1..=4 {
        let mut b = resume();
        let out = poll_until(&cond, &p, &mut b, &clock).unwrap();
        assert_eq!(out.status, Status::Pending, "slice {slice}");
        budget::save(&path, &b).unwrap();
    }
    let mut b = resume();
    let err = poll_until(&cond, &p, &mut b, &clock).unwrap_err();
    let text = format!("{err:#}");
    assert!(text.contains("rate limit"), "{text}");
    assert!(text.contains("5 polls in a row"), "{text}");
    assert_eq!(b.polls, 5);
}

/// Why: the two error streaks interact one way only, and a reset that has
/// already passed falls back to the normal interval (#10550).
#[test]
fn rate_limit_streak_and_generic_errors_interact_as_documented() {
    let r = || Err(shim_refusal(None));
    let e = || Err::<Poll, String>("connection reset".into());
    let met = || Ok(Poll::Met("done".into()));
    // (name, script, expected sleeps, expected error text; None = met)
    type Case = (
        &'static str,
        Vec<Result<Poll, String>>,
        Vec<u64>,
        Option<&'static str>,
    );
    let cases: Vec<Case> = vec![
        (
            // A refusal never resets the generic count; an error resets the
            // refusal streak, so eight refusals never reach five in a row.
            "R,R,E alternation exits on the 4th generic error",
            [r(), r(), e()].into_iter().cycle().take(12).collect(),
            vec![20, 40, 20, 20, 40, 20, 20, 40, 20, 20, 40],
            Some("failed 4 times in a row"),
        ),
        (
            "a reset already passed naps the interval",
            vec![Err(shim_refusal(Some(990))), met()],
            vec![20],
            None,
        ),
        (
            "a reset equal to now naps the interval",
            vec![Err(shim_refusal(Some(1_000))), met()],
            vec![20],
            None,
        ),
    ];
    for (name, script, sleeps, error) in cases {
        let clock = FakeClock::at(1_000);
        let cond = FakeCondition::new(script);
        let mut b = budget_at(1_000, 1800);
        let out = poll_until(&cond, &plan(20, 500, 1800), &mut b, &clock);
        match error {
            Some(want) => {
                let text = format!("{:#}", out.expect_err(name));
                assert!(text.contains(want), "{name}: {text}");
            }
            None => assert_eq!(out.expect(name).status, Status::Met, "{name}"),
        }
        assert_eq!(clock.sleeps(), sleeps, "{name}");
    }
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

/// Why: a state file written before #10550 has no rate-limit streak; a wait
/// already in flight must resume it, not restart its deadline.
#[test]
fn budget_resumes_a_state_file_written_before_the_rate_limit_streak() {
    let dir = tempfile::tempdir().unwrap();
    let p = budget::path_for(dir.path(), "file:/tmp/gate.txt");
    let legacy = r#"{"spec":"file:/tmp/gate.txt","started_at":1000,"updated_at":1100,"polls":11,"timeout_s":1800}"#;
    std::fs::write(&p, legacy).unwrap();
    let resumed = budget::load_or_start(&p, "file:/tmp/gate.txt", 1800, 1_150, 600);
    assert_eq!((resumed.started_at, resumed.polls), (1_000, 11));
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
        100,
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

/// Why (#10339): a slice-ended `pending` must not read as a timeout or failure.
#[test]
fn status_line_pending_note_says_the_slice_ended() {
    let b = budget_at(1_000, 1800);
    let out = Outcome {
        status: Status::Pending,
        detail: "waiting".into(),
    };
    let line = status_line(&out, WaitFor::File, &b, 1_100, "tm wait --for file", 100);
    assert!(
        line.contains(
            r#"note="the 100s slice ended before the 1800s --timeout; this is not a timeout or a failure, 1700s of the budget remain. Re-run the command in rerun= (or pass --slice <n>, up to 500)""#
        ),
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
    let line = status_line(&out, WaitFor::File, &b, 1_005, "tm wait --for file", 100);
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
    let plan = resolve_plan(&a).expect("plan");
    let cmd = rerun_command(&a, &plan);
    assert_eq!(
        cmd,
        r#"tm wait --for file --path /tmp/gate.txt --contains "EXIT=" --timeout 600 --slice 100"#
    );
}

/// Why (#8120): the rerun line is the ONLY place a caller reads its own slice
/// back, and an agent that re-issues it verbatim inherits whatever it says. A
/// line that always said `--slice 100` would cost a nine-minute wait nine round
/// trips instead of two. `540` is the value the reporting agent passed.
#[test]
fn rerun_command_echoes_a_non_default_slice() {
    let mut a = args_for(WaitFor::File);
    a.path = Some("/tmp/gate.txt".into());
    a.slice = 400;
    let plan = resolve_plan(&a).expect("plan");
    let cmd = rerun_command(&a, &plan);
    assert!(cmd.contains("--slice 400"), "{cmd}");
    assert!(!cmd.contains("--slice 100"), "{cmd}");
}

/// Why (#8120): `resolve_plan` clamps the slice to `SLICE_MAX` and the timeout
/// to `TIMEOUT_MAX`, so echoing the RAW args printed a command that does not
/// reproduce the invocation that printed it — `--slice 540` ran as 500 and
/// `--timeout 200000` ran as 86400, with nothing saying so.
#[test]
fn rerun_command_echoes_the_clamped_slice_and_timeout() {
    let mut a = args_for(WaitFor::File);
    a.path = Some("/tmp/gate.txt".into());
    a.slice = 540;
    a.timeout = 200_000;
    let plan = resolve_plan(&a).expect("plan");
    let cmd = rerun_command(&a, &plan);
    assert!(cmd.contains("--slice 500"), "{cmd}");
    assert!(cmd.contains("--timeout 86400"), "{cmd}");
    assert!(!cmd.contains("540"), "{cmd}");
    assert!(!cmd.contains("200000"), "{cmd}");
}

/// Why (#8120): `--interval` is floored per verb, so the same clamp applies —
/// and the flag stays absent when the caller passed none, keeping the default
/// line unchanged.
#[test]
fn rerun_command_echoes_the_floored_interval_only_when_given() {
    let mut a = args_for(WaitFor::Check);
    a.pr = Some(1);
    assert!(!rerun_command(&a, &resolve_plan(&a).expect("plan")).contains("--interval"));
    a.interval = Some(2);
    let cmd = rerun_command(&a, &resolve_plan(&a).expect("plan"));
    assert!(cmd.contains("--interval 10"), "{cmd}");
}

/// Why: re-running with `--reset` would wipe the very budget the pending line
/// just reported.
#[test]
fn rerun_command_never_repeats_reset() {
    let mut a = args_for(WaitFor::Run);
    a.pid = Some(4242);
    a.reset = true;
    let plan = resolve_plan(&a).expect("plan");
    assert!(!rerun_command(&a, &plan).contains("--reset"));
    assert!(rerun_command(&a, &plan).contains("--pid 4242"));
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
// These live here rather than in `tests_behavior_a_tests.rs` because the SLOC cap
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

/// #11673: `--required-only` judges only the required contexts — a failing
/// or running non-required check neither holds nor settles the wait.
#[test]
fn check_condition_required_only_ignores_other_checks() {
    let poll = |json: &str| {
        CheckCondition::new(
            FakeProbe {
                json: json.to_string(),
            },
            5843,
            None,
            false,
        )
        .with_required(Some(vec!["ci".to_string()]))
        .poll(ANY_BUDGET)
        .unwrap()
    };
    let ci_green = r#"{"state":"OPEN","statusCheckRollup":[
        {"name":"ci","status":"COMPLETED","conclusion":"SUCCESS"},
        {"name":"slow","status":"IN_PROGRESS","conclusion":""}]}"#;
    assert!(matches!(poll(ci_green), Poll::Met(d) if d.contains("green")));
    let ci_running = r#"{"state":"OPEN","statusCheckRollup":[
        {"name":"ci","status":"IN_PROGRESS","conclusion":""},
        {"name":"lint","status":"COMPLETED","conclusion":"FAILURE"}]}"#;
    assert!(matches!(poll(ci_running), Poll::Pending(_)));
    let ci_absent = r#"{"state":"OPEN","statusCheckRollup":[]}"#;
    assert!(matches!(poll(ci_absent), Poll::Pending(_)));
}

/// #11673: the new flags belong to `--for check`, and `--owner` to `--watch`.
#[test]
fn required_flags_need_for_check() {
    let mut a = args_for(WaitFor::File);
    a.watch = true;
    assert!(super::required::validate(&a).is_err());
    let mut c = args_for(WaitFor::Check);
    c.owner = Some("pm".into());
    assert!(super::required::validate(&c).is_err());
    c.watch = true;
    c.required_only = true;
    assert!(super::required::validate(&c).is_ok());
    c.pr = Some(7);
    let plan = resolve_plan(&c).unwrap();
    let rerun = rerun_command(&c, &plan);
    assert!(
        rerun.contains("--required-only --watch --owner pm"),
        "{rerun}"
    );
    assert!(condition::spec(&c).ends_with(":required"));
}

// ── #10668: the budget is keyed to the spec plus the PR head SHA ────────────

/// Canned `gh pr view` JSON: one check still running on `head`.
fn pending_view_on(head: &str) -> String {
    format!(
        r#"{{"state":"OPEN","headRefOid":"{head}","statusCheckRollup":[
        {{"name":"e2e","status":"IN_PROGRESS","conclusion":""}}]}}"#
    )
}

/// A `gh` seam that answers every read with one canned reply or error.
struct ReplyProbe(Result<String, String>);

impl ChecksProbe for ReplyProbe {
    fn pr_view_json(
        &self,
        _pr: u64,
        _repo: Option<&str>,
        _budget: std::time::Duration,
    ) -> anyhow::Result<String> {
        self.0.clone().map_err(|e| anyhow::anyhow!("{e}"))
    }
}

/// A stored budget for the no-head cases, started at t=1000 on head `aaaa`.
const RECORD_ON_HEAD: &str = r#"{"spec":"check:372","started_at":1000,"updated_at":1100,"polls":11,"timeout_s":1800,"rate_limited_streak":0,"head":"aaaa"}"#;

/// The same budget as written before #10668: no `head` field at all.
const RECORD_WITHOUT_HEAD: &str = r#"{"spec":"check:372","started_at":1000,"updated_at":1100,"polls":11,"timeout_s":1800,"rate_limited_streak":0}"#;

/// Why: a poll that yields no head must never restart the budget. If one did,
/// a wait whose reads keep failing or keep omitting the SHA would reset its
/// deadline every slice and never time out (fail open).
/// What: each case loads a stored budget started at t=1000, polls one slice
/// with a canned reply, and requires `started_at` (and so `elapsed`) kept.
/// Failures are collected so one run names every broken case.
// #10668: no-head paths keep `started_at`; a reset here means `tm wait` never times out.
#[test]
fn no_head_poll_paths_keep_the_budget_start() {
    const NO_HEAD_FIELD: &str = r#"{"state":"OPEN","statusCheckRollup":[
        {"name":"e2e","status":"IN_PROGRESS","conclusion":""}]}"#;
    /// (name, stored record, poll reply, head expected after the slice).
    type NoHeadCase = (
        &'static str,
        &'static str,
        Result<String, String>,
        Option<&'static str>,
    );
    let cases: [NoHeadCase; 4] = [
        (
            "reply_without_head_ref_oid",
            RECORD_ON_HEAD,
            Ok(NO_HEAD_FIELD.into()),
            Some("aaaa"),
        ),
        (
            "reply_with_blank_head_ref_oid",
            RECORD_ON_HEAD,
            Ok(pending_view_on("")),
            Some("aaaa"),
        ),
        (
            "poll_returns_err",
            RECORD_ON_HEAD,
            Err("connection reset".into()),
            Some("aaaa"),
        ),
        (
            "old_record_without_head_field",
            RECORD_WITHOUT_HEAD,
            Ok(pending_view_on("aaaa")),
            Some("aaaa"),
        ),
    ];

    let mut broken = Vec::new();
    for (name, record, reply, head_after) in cases {
        let dir = tempfile::tempdir().unwrap();
        let path = budget::path_for(dir.path(), "check:372");
        std::fs::write(&path, record).unwrap();
        let clock = FakeClock::at(1_150);
        let mut b = budget::load_or_start(&path, "check:372", 1_800, clock.now_unix(), 600);
        let loaded_start = b.started_at;

        let cond = CheckCondition::new(ReplyProbe(reply), 372, None, false);
        let out = poll_until(&cond, &plan(10, 20, 1_800), &mut b, &clock).unwrap();
        let now = clock.now_unix();

        let got = (
            loaded_start,
            b.started_at,
            b.elapsed(now),
            out.status,
            b.head.as_deref(),
        );
        let want = (1_000, 1_000, now - 1_000, Status::Pending, head_after);
        if got != want {
            broken.push(format!("{name}: got {got:?}, want {want:?}"));
        }
    }
    assert!(
        broken.is_empty(),
        "budget start reset:\n{}",
        broken.join("\n")
    );
}

/// One `tm wait` invocation against a canned head: the same steps
/// `run_inner` takes after it builds the condition — load or start the budget,
/// poll one slice, persist or clear, render the status line.
fn invoke_check(args: &WaitArgs, head: &str, clock: &FakeClock) -> String {
    let plan = resolve_plan(args).unwrap();
    let cond = CheckCondition::new(
        FakeProbe {
            json: pending_view_on(head),
        },
        args.pr.unwrap(),
        args.repo.clone(),
        args.allow_empty_checks,
    );
    let spec = condition::spec(args);
    let path = budget::path_for(args.state_dir.as_deref().unwrap(), &spec);
    let now = clock.now_unix();
    let abandon_after = ABANDON_AFTER_SECS.max(plan.interval_s.saturating_mul(10));
    let mut b = budget::load_or_start(&path, &spec, plan.timeout_s, now, abandon_after);
    let outcome = poll_until(&cond, &plan, &mut b, clock).unwrap();
    if outcome.status.is_terminal() {
        budget::clear(&path);
    } else {
        budget::save(&path, &b).unwrap();
    }
    status_line(
        &outcome,
        args.condition,
        &b,
        clock.now_unix(),
        &rerun_command(args, &plan),
        plan.slice_s,
    )
}

/// The `key=` value of a bare field on a status line.
fn line_field<'a>(line: &'a str, key: &str) -> &'a str {
    let prefix = format!("{key}=");
    line.split_whitespace()
        .find_map(|tok| tok.strip_prefix(prefix.as_str()))
        .unwrap_or_else(|| panic!("no {key}= in {line}"))
}

/// The numeric `key=` value of a status line.
fn line_num(line: &str, key: &str) -> u64 {
    line_field(line, key).parse().unwrap()
}

/// Parse the printed `rerun="…"` command back into `WaitArgs`, as a caller
/// re-issuing it would.
fn parse_rerun(line: &str) -> WaitArgs {
    use clap::Parser as _;
    let start = line
        .find("rerun=\"")
        .expect("a pending line carries rerun=")
        + 7;
    let cmd = &line[start..line.len() - 1];
    let mut argv: Vec<&str> = cmd.split_whitespace().collect();
    assert_eq!(&argv[..2], ["tm", "wait"], "{cmd}");
    argv[0] = "trusty-mpm";
    match crate::cli::Cli::try_parse_from(argv)
        .unwrap()
        .command
        .unwrap()
    {
        crate::cli::Command::Wait(args) => args,
        other => panic!("rerun= did not parse as wait: {other:?}"),
    }
}

/// The first invocation of the PR wait the issue recorded.
fn check_wait_args(dir: &std::path::Path) -> WaitArgs {
    let mut a = args_for(WaitFor::Check);
    a.pr = Some(372);
    a.repo = Some("owner/repo".into());
    a.timeout = 1500;
    a.state_dir = Some(dir.to_path_buf());
    a
}

/// #10668, the recorded incident: a wait on one head is re-issued through
/// `rerun=`, the PR gets a new push, and the caller keeps re-issuing. Every
/// line's `elapsed` must equal the wall time spent on the NEW head, and
/// `status=timeout` must not appear before the 1500s `--timeout` has passed on
/// the wall clock. Before the fix the old head's `started_at` carried over and
/// the wait timed out about 790s early.
#[test]
fn rerun_elapsed_tracks_wall_time_across_a_push() {
    let dir = tempfile::tempdir().unwrap();
    let clock = FakeClock::at(1_000_000);
    let agent_gap = 30;

    // Head A: four slices through the printed rerun= command.
    let mut line = invoke_check(&check_wait_args(dir.path()), "aaaa", &clock);
    for _ in 0..3 {
        clock.sleep(agent_gap);
        line = invoke_check(&parse_rerun(&line), "aaaa", &clock);
    }
    assert_eq!(line_field(&line, "status"), "pending", "{line}");

    // The caller pushes a fix (inside the 600s abandon window) and re-issues.
    clock.sleep(300);
    let pushed_at = clock.now_unix();
    let mut prior_pending_elapsed = 0;
    for _ in 0..40 {
        line = invoke_check(&parse_rerun(&line), "bbbb", &clock);
        let wall = clock.now_unix() - pushed_at;
        assert_eq!(
            line_num(&line, "elapsed"),
            wall,
            "elapsed is wall time: {line}"
        );
        if line_field(&line, "status") == "timeout" {
            assert!(wall >= 1500, "timed out {wall}s into a 1500s budget");
            assert!(prior_pending_elapsed < 1500);
            assert_eq!(line_num(&line, "remaining"), 0);
            return;
        }
        assert_eq!(line_field(&line, "status"), "pending", "{line}");
        prior_pending_elapsed = line_num(&line, "elapsed");
        clock.sleep(agent_gap);
    }
    panic!("the wait never reached its timeout: {line}");
}

/// #10668: the first poll that sees a new head starts a fresh budget — elapsed
/// and polls count from that poll, not from the earlier head's wait.
#[test]
fn rerun_on_a_new_head_starts_a_fresh_budget() {
    let dir = tempfile::tempdir().unwrap();
    let clock = FakeClock::at(2_000_000);
    let first = invoke_check(&check_wait_args(dir.path()), "aaaa", &clock);
    assert_eq!(
        (line_num(&first, "elapsed"), line_num(&first, "polls")),
        (100, 5)
    );

    clock.sleep(60);
    let next = invoke_check(&parse_rerun(&first), "bbbb", &clock);
    assert_eq!(line_num(&next, "elapsed"), 100, "{next}");
    assert_eq!(line_num(&next, "polls"), 5, "{next}");
    assert_eq!(line_num(&next, "remaining"), 1400, "{next}");
}

/// #10668: a re-run on the SAME head keeps its budget, gap included — or a
/// wait re-issued forever would never expire.
#[test]
fn rerun_on_the_same_head_keeps_its_budget() {
    let dir = tempfile::tempdir().unwrap();
    let clock = FakeClock::at(3_000_000);
    let first = invoke_check(&check_wait_args(dir.path()), "aaaa", &clock);

    clock.sleep(60);
    let next = invoke_check(&parse_rerun(&first), "aaaa", &clock);
    assert_eq!(line_num(&next, "elapsed"), 260, "{next}");
    assert_eq!(line_num(&next, "polls"), 10, "{next}");
    assert_eq!(line_num(&next, "remaining"), 1240, "{next}");
}
