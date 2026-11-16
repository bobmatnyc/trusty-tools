//! Tests for `tm architect restart` (#11365): each gate's error arm touches
//! nothing, and the happy path relaunches once and types one resume line.

use std::cell::Cell;
use std::path::Path;

use trusty_architect::ScriptedTmux;

use super::super::Step;
use super::super::relaunch::RelaunchReport;
use super::super::session_name::SessionNames;
use super::*;

/// An empty, idle Claude Code input box.
const IDLE: &str = "done.\n────────────\n❯ \n────────────\n";
/// A box holding a typed draft.
const DRAFT: &str = "done.\n────────────\n❯ half-written reply\n────────────\n";
/// The box after the resume line was typed.
const ECHO: &str = "done.\n────────────\n❯ Resume from the Architect IL: run `python3 scripts/datg.py --project local/il digest`\n────────────\n";

const NOW: f64 = 1_792_600_000.0;

fn now() -> f64 {
    NOW
}
fn no_sleep(_: Duration) {}

/// Writes the Architect's ledger under `dir/local/il`; `fresh` controls the stamp.
fn ledger(dir: &Path, state: &str, stamp: Option<f64>) {
    let d = dir.join("local/il/.trusty-mpm/datg");
    std::fs::create_dir_all(&d).expect("datg dir");
    std::fs::write(d.join("state.json"), state).expect("state");
    if let Some(c) = stamp {
        std::fs::write(d.join("stamp.json"), format!("{{\"changed_epoch\": {c}}}")).expect("stamp");
    }
}

const READY: &str = r#"{"code":"ar","v":3,"next":{},"items":[],"tasks":[],
 "goals":[{"id":"ar-G01","status":"active"}],
 "session":{"written_at":"2026-10-21T10:00Z","written_epoch":1792576800,
  "task":"ship the restart verb","next":"run the gates","owed":[],"crons":[]}}"#;

fn names() -> SessionNames {
    SessionNames::default_names()
}

fn relaunched() -> anyhow::Result<RelaunchReport> {
    Ok(RelaunchReport {
        steps: vec![Step::Changed("started a new `claude`, bound".into())],
        session: "tm-architect".into(),
        registration: Some(trusty_mpm::session_manager::ArchitectRegistration {
            dir: "/arch".into(),
            session: "tm-architect".into(),
            poll_session: None,
            collector_session: None,
        }),
    })
}

/// Runs `restart` with `tmux`; returns the report and the relaunch count.
fn run(dir: &Path, tmux: &ScriptedTmux, inside: bool) -> (RestartReport, usize) {
    let calls = Cell::new(0);
    let relaunch = || {
        calls.set(calls.get() + 1);
        relaunched()
    };
    let ops = Ops {
        tmux,
        relaunch: &relaunch,
        now,
        sleep: no_sleep,
    };
    let r = restart(dir, &names(), inside, None, &ops);
    (r, calls.get())
}

fn idle_tmux() -> ScriptedTmux {
    ScriptedTmux::new()
        .fallback("display-message", "%3\n")
        .fallback("send-keys", "")
        .fallback("capture-pane", IDLE)
}

#[test]
fn a_stale_checkpoint_exits_4_and_touches_nothing() {
    let tmp = tempfile::tempdir().expect("tmp");
    // The state changed after the last session write.
    ledger(tmp.path(), READY, Some(1_792_576_800.0 + 60.0));
    let tmux = idle_tmux();
    let (r, relaunches) = run(tmp.path(), &tmux, false);
    assert_eq!(r.exit, 4, "{:?}", r.lines);
    assert_eq!((relaunches, tmux.calls().len()), (0, 0));
    assert!(!tmp.path().join("inbox/restart.log").exists());
}

#[test]
fn a_checkpoint_read_error_refuses_the_restart() {
    let tmp = tempfile::tempdir().expect("tmp");
    let tmux = idle_tmux();
    for state in [None, Some("{not json")] {
        if let Some(s) = state {
            ledger(tmp.path(), s, None);
        }
        let (r, relaunches) = run(tmp.path(), &tmux, false);
        assert_eq!((r.exit, relaunches), (4, 0), "{state:?}: {:?}", r.lines);
    }
    assert!(tmux.calls().is_empty(), "fail closed before any tmux call");
}

#[test]
fn a_busy_box_exits_2_and_touches_nothing() {
    let tmp = tempfile::tempdir().expect("tmp");
    ledger(tmp.path(), READY, None);
    let tmux = ScriptedTmux::new()
        .fallback("display-message", "%3\n")
        .fallback("send-keys", "")
        .script("capture-pane", IDLE)
        .fallback("capture-pane", DRAFT);
    let (r, relaunches) = run(tmp.path(), &tmux, false);
    assert_eq!((r.exit, relaunches), (2, 0), "{:?}", r.lines);
    assert!(tmux.sends().is_empty());
}

#[test]
fn inside_claude_code_restart_is_refused() {
    let tmp = tempfile::tempdir().expect("tmp");
    ledger(tmp.path(), READY, None);
    let tmux = idle_tmux();
    let (r, relaunches) = run(tmp.path(), &tmux, true);
    assert_eq!((r.exit, relaunches, tmux.calls().len()), (2, 0, 0));
}

#[test]
fn an_idle_box_and_a_fresh_checkpoint_restart_and_send_one_resume_line() {
    let tmp = tempfile::tempdir().expect("tmp");
    ledger(tmp.path(), READY, None);
    // 3 idle reads, 1 pre-relaunch read, 1 box read, 1 pre-type reading, then the echo.
    let tmux = ScriptedTmux::new()
        .script("capture-pane", IDLE)
        .fallback("display-message", "%3\n")
        .fallback("send-keys", "")
        .script("capture-pane", IDLE)
        .script("capture-pane", IDLE)
        .script("capture-pane", IDLE)
        .script("capture-pane", IDLE)
        .script("capture-pane", IDLE)
        .fallback("capture-pane", ECHO);
    let (r, relaunches) = run(tmp.path(), &tmux, false);
    assert_eq!((r.exit, relaunches), (0, 1), "{:?}", r.lines);
    assert!(
        r.registration.is_some(),
        "the relaunch's registration is passed on"
    );
    let sends = tmux.sends();
    assert_eq!(sends.len(), 2, "{sends:?}");
    assert!(sends[0].iter().any(|a| a == RESUME), "{sends:?}");
    assert_eq!(sends[1].last().map(String::as_str), Some("Enter"));
    let log = std::fs::read_to_string(tmp.path().join("inbox/restart.log")).expect("log");
    assert_eq!(log.lines().count(), 1, "{log}");
    assert!(log.contains("exit=0 resume=sent"), "{log}");
}

#[test]
fn a_held_up_lock_refuses_the_restart_before_any_tmux_call() {
    let tmp = tempfile::tempdir().expect("tmp");
    ledger(tmp.path(), READY, None);
    let _held = super::super::up::lock_within(tmp.path(), Duration::ZERO).expect("hold");
    let tmux = idle_tmux();
    let (r, relaunches) = run(tmp.path(), &tmux, false);
    assert_eq!((r.exit, relaunches), (1, 0), "{:?}", r.lines);
    assert!(tmux.calls().is_empty(), "{:?}", tmux.calls());
    assert!(r.lines.iter().any(|l| l.contains("holds")), "{:?}", r.lines);
}

#[test]
fn a_box_busy_just_before_relaunch_refuses() {
    let tmp = tempfile::tempdir().expect("tmp");
    ledger(tmp.path(), READY, None);
    let tmux = ScriptedTmux::new()
        .fallback("display-message", "%3\n")
        .fallback("send-keys", "")
        .script("capture-pane", IDLE)
        .script("capture-pane", IDLE)
        .script("capture-pane", IDLE)
        .fallback("capture-pane", DRAFT);
    let (r, relaunches) = run(tmp.path(), &tmux, false);
    assert_eq!((r.exit, relaunches), (2, 0), "{:?}", r.lines);
    assert!(tmux.sends().is_empty());
}
