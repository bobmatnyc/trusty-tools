//! Tests for G0b `tm architect restart`: the requested checkpoint,
//! the role-based resolver, the failure arms, and that only the Architect is
//! touched. Temporary HOME, `ScriptedTmux` and a fake relaunch only.

use std::cell::Cell;
use std::path::Path;

use trusty_architect::ScriptedTmux;

use super::super::Step;
use super::super::relaunch::RelaunchReport;
use super::super::session_name::SessionNames;
use super::*;

/// An empty, idle Claude Code input box.
const IDLE: &str = "done.\n────────────\n❯ \n────────────\n";

const NOW: f64 = 1_792_600_000.0;

thread_local! {
    /// Seconds the fake `sleep` has advanced the fake clock.
    static SLEPT: Cell<f64> = const { Cell::new(0.0) };
    /// Checkpoint-poll sleeps seen so far (the 2 s [`CHECKPOINT_POLL`] ones).
    static POLLS: Cell<usize> = const { Cell::new(0) };
    /// A ledger to write, as an Architect would, on the Nth poll sleep.
    static WRITE_ON_POLL: std::cell::RefCell<Option<(usize, std::path::PathBuf)>> =
        const { std::cell::RefCell::new(None) };
}

fn now() -> f64 {
    NOW + SLEPT.with(Cell::get)
}
fn sleep(d: Duration) {
    SLEPT.with(|s| s.set(s.get() + d.as_secs_f64()));
    if d == CHECKPOINT_POLL {
        let n = POLLS.with(|p| {
            p.set(p.get() + 1);
            p.get()
        });
        let target = WRITE_ON_POLL.with(|w| w.borrow().clone());
        if let Some((at, dir)) = target
            && n == at
        {
            ledger(&dir, now());
        }
    }
}

/// A ready ledger with this `session` object.
fn put_ledger(dir: &Path, session: serde_json::Value) {
    let d = dir.join("local/il/.trusty-mpm/datg");
    std::fs::create_dir_all(&d).expect("datg dir");
    let mut ses = serde_json::json!({
        "task": "ship the restart verb", "next": "run the gates", "owed": [], "crons": []
    });
    ses.as_object_mut()
        .expect("object")
        .extend(session.as_object().expect("object").clone());
    let state = serde_json::json!({
        "code": "ar", "v": 3, "next": {}, "items": [], "tasks": [],
        "goals": [{"id": "ar-g01", "status": "active"}], "session": ses
    });
    std::fs::write(d.join("state.json"), state.to_string()).expect("state");
}

/// A ready ledger whose session was written at `written` epoch seconds.
fn ledger(dir: &Path, written: f64) {
    put_ledger(
        dir,
        serde_json::json!({"written_at": "2026-10-21T10:00Z", "written_epoch": written as i64}),
    );
}

/// The box after `line` was typed (the guarded send checks this echo).
fn echo(line: &str) -> String {
    let head: String = line.chars().take(70).collect();
    format!("done.\n────────────\n❯ {head}\n────────────\n")
}

/// A tmux whose `capture-pane` answers `caps` in order, then `IDLE`.
fn tmux_with(caps: &[String]) -> ScriptedTmux {
    let t = ScriptedTmux::new()
        .fallback("display-message", "%3\n")
        .fallback("send-keys", "");
    caps.iter()
        .fold(t, |t, c| t.script("capture-pane", c))
        .fallback("capture-pane", IDLE)
}

/// The capture sequence of a full checkpointed restart.
fn checkpointed_caps() -> Vec<String> {
    let i = || IDLE.to_owned();
    // 3 idle, reading + echo, 3 idle after the turn, 1 pre-relaunch,
    // box wait + reading + resume echo.
    vec![
        i(),
        i(),
        i(),
        i(),
        echo(CHECKPOINT),
        i(),
        i(),
        i(),
        i(),
        i(),
        i(),
        echo(RESUME),
    ]
}

fn ok_relaunch() -> anyhow::Result<RelaunchReport> {
    Ok(RelaunchReport {
        steps: vec![Step::Changed("started a new `claude`, bound".into())],
        session: "tm-architect".into(),
        registration: None,
    })
}

/// Runs `restart_with`; returns the report and the relaunch count.
fn run_gate(
    dir: &Path,
    tmux: &ScriptedTmux,
    gate: Gate,
    relaunch: &dyn Fn() -> anyhow::Result<RelaunchReport>,
) -> (RestartReport, usize) {
    SLEPT.with(|s| s.set(0.0));
    POLLS.with(|p| p.set(0));
    let calls = Cell::new(0);
    let counted = || {
        calls.set(calls.get() + 1);
        relaunch()
    };
    let ops = Ops {
        tmux,
        relaunch: &counted,
        now,
        sleep,
    };
    let names = SessionNames::default_names();
    let r = restart_with(dir, &names, false, None, gate, &ops);
    (r, calls.get())
}

const REQUEST: Gate = Gate { request: true };

fn last(r: &RestartReport) -> &str {
    r.lines.last().map_or("", String::as_str)
}

#[test]
fn a_requested_checkpoint_restarts_and_touches_only_the_architect() {
    let home = tempfile::tempdir().expect("home");
    let dir = home.path().join("architect");
    // A write after the request: the Architect answered the checkpoint.
    ledger(&dir, NOW + 30.0);
    // Another project's session and pane exist on the same tmux server.
    let tmux = tmux_with(&checkpointed_caps())
        .fallback("list-sessions", "tm-architect\ntm-proj-a\n")
        .fallback("list-panes", "%3 tm-architect\n%7 tm-proj-a\n");
    let (r, relaunches) = run_gate(&dir, &tmux, REQUEST, &ok_relaunch);
    assert_eq!((r.exit, relaunches), (0, 1), "{:?}", r.lines);
    assert_eq!(last(&r), "restarted");
    let sends = tmux.sends();
    assert_eq!(
        sends.len(),
        4,
        "checkpoint + Enter, resume + Enter: {sends:?}"
    );
    assert!(sends[0].iter().any(|a| a == CHECKPOINT), "{sends:?}");
    assert!(sends[2].iter().any(|a| a == RESUME), "{sends:?}");
    // Every tmux call targets the Architect's pane or session, never the other.
    let calls = tmux.calls();
    assert!(!calls.is_empty(), "the restart made tmux calls");
    for call in &calls {
        assert!(
            !call
                .iter()
                .any(|a| a.contains("tm-proj-a") || a.contains("%7")),
            "a call touched the other session: {call:?}"
        );
        let target = call.iter().skip_while(|a| *a != "-t").nth(1);
        let t = target.map_or("", String::as_str);
        assert!(t == "%3" || t.contains("tm-architect"), "{call:?}");
    }
}

#[test]
fn a_checkpoint_timeout_refuses_with_exit_4() {
    let tmp = tempfile::tempdir().expect("tmp");
    // The last write predates the request: no checkpoint lands.
    ledger(tmp.path(), NOW - 600.0);
    let caps = checkpointed_caps();
    let tmux = tmux_with(&caps[..5]);
    let (r, relaunches) = run_gate(tmp.path(), &tmux, REQUEST, &ok_relaunch);
    assert_eq!((r.exit, relaunches), (4, 0), "{:?}", r.lines);
    assert!(
        r.lines.iter().any(|l| l.contains("within 90s")),
        "{:?}",
        r.lines
    );
    assert!(last(&r).starts_with("NOT restarted"), "{:?}", r.lines);
    assert_eq!(
        tmux.sends().len(),
        2,
        "only the checkpoint request was typed"
    );
    assert!(SLEPT.with(Cell::get) >= 90.0, "the wait lasted 90 s");
}

#[test]
fn a_ledger_without_an_exact_epoch_is_not_a_checkpoint() {
    // `written_at` has minute precision: a write earlier in the request's own
    // minute must not count as written after the request.
    let tmp = tempfile::tempdir().expect("tmp");
    let minute = iso(NOW);
    put_ledger(
        tmp.path(),
        serde_json::json!({"written_at": format!("{}Z", &minute[..16])}),
    );
    let tmux = tmux_with(&checkpointed_caps()[..5]);
    let (r, relaunches) = run_gate(tmp.path(), &tmux, REQUEST, &ok_relaunch);
    assert_eq!((r.exit, relaunches), (4, 0), "{:?}", r.lines);
    assert!(last(&r).starts_with("NOT restarted"), "{:?}", r.lines);
    assert_eq!(tmux.sends().len(), 2, "only the request was typed");
}

#[test]
fn a_write_landing_after_the_second_poll_restarts() {
    let tmp = tempfile::tempdir().expect("tmp");
    ledger(tmp.path(), NOW - 600.0);
    WRITE_ON_POLL.with(|w| *w.borrow_mut() = Some((2, tmp.path().to_path_buf())));
    let tmux = tmux_with(&checkpointed_caps());
    let (r, relaunches) = run_gate(tmp.path(), &tmux, REQUEST, &ok_relaunch);
    WRITE_ON_POLL.with(|w| *w.borrow_mut() = None);
    assert_eq!((r.exit, relaunches), (0, 1), "{:?}", r.lines);
    assert_eq!(last(&r), "restarted");
    assert_eq!(POLLS.with(Cell::get), 2, "the write landed on the 2nd poll");
}

#[test]
fn no_checkpoint_with_a_not_ready_ledger_still_refuses() {
    // No ledger at all: `--no-checkpoint` skips the request but keeps the
    // #11365 gate, so nothing is typed and nothing is relaunched.
    let tmp = tempfile::tempdir().expect("tmp");
    let tmux = tmux_with(&[]);
    let (r, relaunches) = run_gate(tmp.path(), &tmux, Gate::default(), &ok_relaunch);
    assert_eq!((r.exit, relaunches), (4, 0), "{:?}", r.lines);
    assert!(tmux.sends().is_empty(), "{:?}", tmux.sends());
    assert!(last(&r).starts_with("NOT restarted"), "{:?}", r.lines);
}

#[test]
fn no_checkpoint_sends_no_request() {
    let tmp = tempfile::tempdir().expect("tmp");
    ledger(tmp.path(), NOW - 600.0);
    let caps = checkpointed_caps();
    let mut no_ck: Vec<String> = caps[..3].to_vec();
    no_ck.extend_from_slice(&caps[8..]);
    let tmux = tmux_with(&no_ck);
    let (r, _) = run_gate(tmp.path(), &tmux, Gate::default(), &ok_relaunch);
    assert_eq!(r.exit, 0, "{:?}", r.lines);
    assert!(
        tmux.sends()
            .iter()
            .all(|s| !s.iter().any(|a| a == CHECKPOINT)),
        "{:?}",
        tmux.sends()
    );
}

#[test]
fn a_failed_relaunch_reports_not_restarted() {
    let tmp = tempfile::tempdir().expect("tmp");
    ledger(tmp.path(), NOW + 30.0);
    let caps = checkpointed_caps();
    let tmux = tmux_with(&caps[..9]);
    let failed = || Err(anyhow::anyhow!("tmux new-session failed"));
    let (r, relaunches) = run_gate(tmp.path(), &tmux, REQUEST, &failed);
    assert_eq!((r.exit, relaunches), (1, 1), "{:?}", r.lines);
    assert!(
        r.lines.iter().any(|l| l.contains("new-session")),
        "{:?}",
        r.lines
    );
    assert!(!r.lines.iter().any(|l| l == "restarted"), "{:?}", r.lines);
    assert!(
        r.lines.iter().any(|l| l.contains("claude may be stopped")),
        "{:?}",
        r.lines
    );
    assert_eq!(tmux.sends().len(), 2, "no resume line after the failure");
}

#[test]
fn no_recorded_architect_is_a_clear_error() {
    let home = tempfile::tempdir().expect("home");
    let err = resolve_architect(None, home.path()).expect_err("no Architect");
    assert!(err.to_string().contains("tm fleet init"), "{err:#}");
}

#[test]
fn a_dir_that_is_not_the_recorded_architect_is_refused() {
    let home = tempfile::tempdir().expect("home");
    let arch = home.path().join("arch");
    super::super::init(&arch, home.path(), false, super::super::tests::NO_TMUX).expect("init");
    let other = home.path().join("other");
    std::fs::create_dir_all(&other).expect("other");
    let err = resolve_architect(other.to_str(), home.path()).expect_err("not the Architect");
    assert!(
        err.to_string().contains("not the recorded Architect"),
        "{err:#}"
    );
}

#[test]
fn the_recorded_architect_resolves() {
    let home = tempfile::tempdir().expect("home");
    let arch = home.path().join("arch");
    super::super::init(&arch, home.path(), false, super::super::tests::NO_TMUX).expect("init");
    let (dir, names) = resolve_architect(None, home.path()).expect("resolves");
    assert_eq!(dir, std::fs::canonicalize(&arch).expect("canon"));
    assert_eq!(names.architect(), "tm-architect");
}
