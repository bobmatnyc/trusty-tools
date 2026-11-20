//! #9779: the prototype's watchdog tests (`test_fleet_poll_watchdog.py`), by
//! name, against the port, plus the relaunch path and the cycle wiring.
//!
//! Every path lives in a tempdir; no test starts a process except
//! `system_runner_reports_exit_codes_and_timeouts`, which runs a tempdir
//! script.

mod cycle_support;

use std::cell::RefCell;
use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::Duration;

use cycle_support::{Env, QuietHost, TestResult, listing};
use serde_json::{Map, Value, json};
use trusty_architect::poller::watchdog::relaunch::{
    ScriptExit, ScriptRelauncher, ScriptRunner, SystemScriptRunner,
};
use trusty_architect::poller::watchdog::wiring::watchdog_alerts;
use trusty_architect::poller::watchdog::{
    Class, Observed, Relaunch, Relauncher, StepResult, classify, step,
};
use trusty_architect::poller::{Pane, PollSession, PollState, architect_busy_in, parse_list_panes};
use trusty_architect::{FakeClock, ScriptedTmux};

const S: &str = "tm-architect";
const NOW: f64 = 1_000_000.0;
const SPIN: &str = "✻ Running PreToolUse hook… (2m 20s)";

fn panes(rows: &[(&str, &str, &str)]) -> Vec<Pane> {
    parse_list_panes(&listing(rows)).panes
}
fn live() -> Vec<Pane> {
    panes(&[(S, "%1", "claude")])
}
fn shell() -> Vec<Pane> {
    panes(&[(S, "%1", "zsh")])
}
fn other() -> Vec<Pane> {
    panes(&[("tm-api", "%2", "claude")])
}

fn obs<'a>(p: &'a [Pane], wake_at: Option<f64>, mtime: Option<f64>, now: f64) -> Observed<'a> {
    Observed {
        panes: p,
        session: S,
        wake_at,
        transcript_mtime: mtime,
        now,
        silent_min: 45.0,
        frozen_min: 15.0,
        busy: false,
        spinner: "",
    }
}

fn cls(p: &[Pane], wake_at: Option<f64>, mtime: Option<f64>) -> Class {
    classify(&obs(p, wake_at, mtime, NOW), None).0
}

/// Counts calls and answers each with the next scripted result.
struct Fake {
    answers: RefCell<Vec<Relaunch>>,
    calls: RefCell<usize>,
}
impl Fake {
    fn new(answers: Vec<Relaunch>) -> Self {
        Self {
            answers: RefCell::new(answers),
            calls: RefCell::new(0),
        }
    }
    fn ok() -> Self {
        Self::new(Vec::new())
    }
    fn calls(&self) -> usize {
        *self.calls.borrow()
    }
}
impl Relauncher for Fake {
    fn relaunch(&self) -> Relaunch {
        *self.calls.borrow_mut() += 1;
        let mut a = self.answers.borrow_mut();
        if a.is_empty() {
            Relaunch::Ok("x".to_owned())
        } else {
            a.remove(0)
        }
    }
}

fn run(book: &mut Map<String, Value>, p: &[Pane], now: f64, r: &Fake, dry: bool) -> StepResult {
    step(book, &obs(p, None, None, now), r, dry)
}
fn kinds(r: &StepResult) -> Vec<&str> {
    r.found.iter().map(|f| f.kind).collect()
}

#[test]
fn test_classify_dead_when_session_missing() {
    assert_eq!(cls(&other(), None, None), Class::Dead);
    assert_eq!(cls(&[], None, None), Class::Dead);
}

#[test]
fn test_classify_dead_when_pane_at_shell() {
    assert_eq!(cls(&shell(), None, None), Class::Dead);
    assert_eq!(cls(&panes(&[(S, "%1", "-zsh")]), None, None), Class::Dead);
}

#[test]
fn test_classify_silent_after_deadline() {
    let wake = NOW - 46.0 * 60.0;
    assert_eq!(cls(&live(), Some(wake), Some(wake - 5.0)), Class::Silent);
    assert_eq!(cls(&live(), Some(wake), None), Class::Silent);
}

#[test]
fn test_classify_ok_cases() {
    let wake = NOW - 46.0 * 60.0;
    assert_eq!(cls(&live(), None, None), Class::Ok);
    assert_eq!(cls(&live(), Some(NOW - 44.0 * 60.0), Some(0.0)), Class::Ok);
    assert_eq!(cls(&live(), Some(wake), Some(wake + 1.0)), Class::Ok);
}

fn frozen_cls(busy: bool, spinner: &str, prev: Option<&str>, mtime: Option<f64>) -> Class {
    let p = live();
    let o = Observed {
        busy,
        spinner,
        ..obs(&p, None, mtime, NOW)
    };
    classify(&o, prev).0
}

#[test]
fn test_classify_frozen_by_stale_transcript() {
    assert_eq!(
        frozen_cls(true, "", None, Some(NOW - 16.0 * 60.0)),
        Class::Frozen
    );
    assert_eq!(
        frozen_cls(true, "", None, Some(NOW - 14.0 * 60.0)),
        Class::Ok
    );
}

#[test]
fn test_classify_frozen_by_identical_spinner() {
    assert_eq!(
        frozen_cls(true, SPIN, Some(SPIN), Some(NOW - 10.0)),
        Class::Frozen
    );
}

#[test]
fn test_classify_not_frozen_when_idle() {
    assert_eq!(
        frozen_cls(false, SPIN, Some(SPIN), Some(NOW - 3600.0)),
        Class::Ok
    );
}

#[test]
fn test_classify_not_frozen_when_spinner_changes() {
    let moved = SPIN.replace("2m 20s", "2m 30s");
    assert_eq!(
        frozen_cls(true, &moved, Some(SPIN), Some(NOW - 10.0)),
        Class::Ok
    );
    assert_eq!(frozen_cls(true, SPIN, None, Some(NOW - 10.0)), Class::Ok);
}

#[test]
fn test_step_dead_relaunches_once_then_debounces() {
    let mut book = Map::new();
    let r = Fake::ok();
    let first = run(&mut book, &shell(), NOW, &r, false);
    assert_eq!(kinds(&first), ["architect_dead"]);
    assert!(first.relaunched && r.calls() == 1);
    let r = Fake::ok();
    let second = run(&mut book, &other(), NOW + 600.0, &r, false);
    assert!(!second.relaunched && r.calls() == 0);
    assert!(second.phrase.contains("debounced"), "{}", second.phrase);
    let third = run(&mut book, &other(), NOW + 1801.0, &r, false);
    assert!(third.relaunched && r.calls() == 1);
}

#[test]
fn test_step_gives_up_after_two_failures() {
    let mut book = Map::new();
    let fail = || Fake::new(vec![Relaunch::Failed("boom".to_owned())]);
    let k1 = run(&mut book, &shell(), NOW, &fail(), false);
    let k2 = run(&mut book, &shell(), NOW + 1801.0, &fail(), false);
    assert_eq!(kinds(&k1), ["architect_dead"]);
    assert_eq!(kinds(&k2), ["architect_dead", "architect_relaunch_failed"]);
    let r = Fake::ok();
    let k3 = run(&mut book, &shell(), NOW + 9000.0, &r, false);
    assert!(kinds(&k3).contains(&"architect_relaunch_failed"));
    assert!(!k3.relaunched && r.calls() == 0);
    assert!(k3.phrase.contains("suspended"), "{}", k3.phrase);
}

#[test]
fn test_step_success_then_dead_counts_failure() {
    let mut book = Map::new();
    run(&mut book, &shell(), NOW, &Fake::ok(), false);
    run(&mut book, &shell(), NOW + 1801.0, &Fake::ok(), false);
    assert_eq!(book["failures"], json!(1));
    let k = run(&mut book, &shell(), NOW + 3700.0, &Fake::ok(), false);
    assert_eq!(kinds(&k), ["architect_dead", "architect_relaunch_failed"]);
}

#[test]
fn test_step_ok_resets_failures() {
    let mut book = Map::new();
    book.insert("failures".to_owned(), json!(1));
    book.insert("pending".to_owned(), json!(true));
    assert!(
        run(&mut book, &live(), NOW, &Fake::ok(), false)
            .found
            .is_empty()
    );
    assert_eq!(
        (&book["failures"], &book["pending"]),
        (&json!(0), &json!(false))
    );
}

#[test]
fn test_step_silent_never_relaunches() {
    let wake = NOW - 50.0 * 60.0;
    let p = live();
    let r = Fake::ok();
    let out = step(
        &mut Map::new(),
        &obs(&p, Some(wake), Some(wake - 1.0), NOW),
        &r,
        false,
    );
    assert_eq!(kinds(&out), ["architect_silent"]);
    assert_eq!(out.found[0].fingerprint, format!("wake-{}", wake as i64));
    assert!(!out.relaunched && r.calls() == 0);
}

#[test]
fn test_step_dry_run_does_not_relaunch() {
    let mut book = Map::new();
    let r = Fake::ok();
    let out = run(&mut book, &shell(), NOW, &r, true);
    assert_eq!(kinds(&out), ["architect_dead"]);
    assert!(out.phrase.contains("would relaunch"), "{}", out.phrase);
    assert!(r.calls() == 0 && !out.relaunched && !book.contains_key("last_relaunch"));
}

fn run_frozen(book: &mut Map<String, Value>, now: f64, mtime: f64, r: &Fake) -> StepResult {
    let p = live();
    let o = Observed {
        busy: true,
        spinner: SPIN,
        ..obs(&p, None, Some(mtime), now)
    };
    step(book, &o, r, false)
}

#[test]
fn test_step_frozen_never_relaunches() {
    let r = Fake::ok();
    let out = run_frozen(&mut Map::new(), NOW, NOW - 20.0 * 60.0, &r);
    assert_eq!(kinds(&out), ["architect_frozen"]);
    assert!(!out.relaunched && r.calls() == 0);
    assert!(out.phrase.contains("no relaunch"), "{}", out.phrase);
    assert!(
        out.found[0].detail.contains("20 min (limit 15)"),
        "{:?}",
        out.found
    );
}

#[test]
fn test_step_frozen_by_spinner_needs_two_polls() {
    let mut book = Map::new();
    let r = Fake::ok();
    assert!(run_frozen(&mut book, NOW, NOW - 5.0, &r).found.is_empty());
    let second = run_frozen(&mut book, NOW + 600.0, NOW + 595.0, &r);
    assert_eq!(kinds(&second), ["architect_frozen"]);
    assert_eq!(r.calls(), 0);
}

#[test]
fn test_architect_busy_reads_spinner() -> TestResult {
    let top = "─".repeat(40);
    let pane = format!("{SPIN}\n\n{top}\n❯ \n{top}\n  footer\n");
    let tmux = ScriptedTmux::new().fallback("capture-pane", &pane);
    assert_eq!(
        architect_busy_in(&tmux, &live(), S)?,
        (true, SPIN.to_owned())
    );
    assert_eq!(
        architect_busy_in(&tmux, &shell(), S)?,
        (false, String::new())
    );
    Ok(())
}

// --- the relaunch path: tmux and the script runner injected ---------------

/// Answers with a fixed exit or error and records each run.
struct Runner(Result<ScriptExit, io::ErrorKind>, RefCell<Vec<Vec<String>>>);
impl ScriptRunner for Runner {
    fn run(&self, script: &Path, args: &[&str], _: Duration) -> io::Result<ScriptExit> {
        let mut argv = vec![script.to_string_lossy().into_owned()];
        argv.extend(args.iter().map(|a| (*a).to_owned()));
        self.1.borrow_mut().push(argv);
        self.0.map_err(io::Error::from)
    }
}

fn relauncher<'a>(
    tmux: &'a ScriptedTmux,
    runner: &'a Runner,
    script: &Path,
) -> ScriptRelauncher<'a> {
    ScriptRelauncher {
        tmux,
        runner,
        script: script.to_owned(),
        session: S.to_owned(),
        root: "/arch".into(),
        timeout: Duration::from_secs(300),
    }
}

/// A tempdir holding an (empty) installed `session-relaunch.sh`.
fn installed() -> TestResult<(tempfile::TempDir, std::path::PathBuf)> {
    let dir = tempfile::tempdir()?;
    let script = dir.path().join("session-relaunch.sh");
    fs::write(&script, "")?;
    Ok((dir, script))
}

fn tmux_listing(seq: &[&[(&str, &str, &str)]]) -> ScriptedTmux {
    let mut t = ScriptedTmux::new().fallback("new-session", "");
    for rows in seq {
        t = t.script("list-panes", &listing(rows));
    }
    // The prototype test lister repeats its last listing.
    match seq.last() {
        Some(rows) => t.fallback("list-panes", &listing(rows)),
        None => t,
    }
}

#[test]
fn test_relaunch_aborts_when_live() -> TestResult {
    let (_d, script) = installed()?;
    let tmux = tmux_listing(&[&[(S, "%1", "claude")]]);
    let runner = Runner(Ok(ScriptExit::Code(0)), RefCell::default());
    let out = relauncher(&tmux, &runner, &script).relaunch();
    assert!(matches!(out, Relaunch::Aborted(_)), "{out:?}");
    assert!(runner.1.borrow().is_empty());
    assert!(tmux.calls().iter().all(|c| c[0] != "new-session"));
    Ok(())
}

#[test]
fn test_relaunch_creates_missing_session() -> TestResult {
    let (_d, script) = installed()?;
    let tmux = tmux_listing(&[&[("tm-api", "%2", "claude")], &[(S, "%1", "claude")]]);
    let runner = Runner(Ok(ScriptExit::Code(0)), RefCell::default());
    let out = relauncher(&tmux, &runner, &script).relaunch();
    assert_eq!(out, Relaunch::Ok("session-relaunch.sh exit 0".to_owned()));
    let made: Vec<_> = tmux
        .calls()
        .into_iter()
        .filter(|c| c[0] == "new-session")
        .collect();
    assert_eq!(made, [["new-session", "-d", "-s", S, "-c", "/arch"]]);
    let script = script.to_string_lossy().into_owned();
    assert_eq!(*runner.1.borrow(), [[script.as_str(), "--rc", S]]);
    Ok(())
}

#[test]
fn test_relaunch_fails_on_script_error() -> TestResult {
    let (_d, script) = installed()?;
    let sh: &[(&str, &str, &str)] = &[(S, "%1", "zsh")];
    for (exit, want) in [
        (Ok(ScriptExit::Code(1)), "session-relaunch.sh exit 1"),
        (
            Ok(ScriptExit::Code(0)),
            "pane still at a shell after relaunch",
        ),
        (Ok(ScriptExit::TimedOut), "timeout after 300s"),
        (Err(io::ErrorKind::PermissionDenied), "OSError: "),
    ] {
        let tmux = tmux_listing(&[sh]);
        let runner = Runner(exit, RefCell::default());
        match relauncher(&tmux, &runner, &script).relaunch() {
            Relaunch::Failed(why) => assert!(why.starts_with(want), "{why}"),
            other => panic!("{want}: {other:?}"),
        }
    }
    // A tmux that cannot list panes fails the attempt; it never runs the script.
    let tmux = ScriptedTmux::new().fail("list-panes", "no server");
    let runner = Runner(Ok(ScriptExit::Code(0)), RefCell::default());
    let out = relauncher(&tmux, &runner, &script).relaunch();
    assert!(
        matches!(out, Relaunch::Failed(ref w) if w.starts_with("TmuxError")),
        "{out:?}"
    );
    assert!(runner.1.borrow().is_empty());
    Ok(())
}

#[test]
fn test_relaunch_without_the_script_never_touches_tmux() {
    let tmux = ScriptedTmux::new();
    let runner = Runner(Ok(ScriptExit::Code(0)), RefCell::default());
    let out = relauncher(
        &tmux,
        &runner,
        Path::new("/nonexistent/session-relaunch.sh"),
    )
    .relaunch();
    assert_eq!(
        out,
        Relaunch::Failed("session-relaunch.sh is not installed".to_owned())
    );
    assert!(tmux.calls().is_empty() && runner.1.borrow().is_empty());
}

#[test]
fn system_runner_reports_exit_codes_and_timeouts() -> TestResult {
    let dir = tempfile::tempdir()?;
    let write = |name: &str, body: &str| -> io::Result<std::path::PathBuf> {
        let p = dir.path().join(name);
        fs::write(&p, body)?;
        fs::set_permissions(&p, fs::Permissions::from_mode(0o755))?;
        Ok(p)
    };
    let quick = write("quick.sh", "#!/bin/sh\nexit 3\n")?;
    let slow = write("slow.sh", "#!/bin/sh\nsleep 30\n")?;
    let r = SystemScriptRunner;
    assert_eq!(
        r.run(&quick, &["--rc", S], Duration::from_secs(10))?,
        ScriptExit::Code(3)
    );
    assert_eq!(
        r.run(&slow, &[], Duration::from_millis(300))?,
        ScriptExit::TimedOut
    );
    // Error arm: a script that cannot be started is an io error.
    assert!(
        r.run(&dir.path().join("absent.sh"), &[], Duration::from_secs(1))
            .is_err()
    );
    Ok(())
}

// --- the cycle wiring ------------------------------------------------------

#[test]
fn watchdog_alerts_dead_architect_alerts_and_relaunches() -> TestResult {
    let env = Env::new(&[])?;
    let tmux = ScriptedTmux::new();
    let mut state = PollState {
        wake_sent_at: Some(NOW - 10.0),
        ..PollState::default()
    };
    let r = Fake::ok();
    let (alerts, phrase, log) =
        watchdog_alerts(&tmux, &shell(), &mut state, &env.config, NOW, &r, false);
    assert_eq!(
        alerts.iter().map(|a| a.kind.as_str()).collect::<Vec<_>>(),
        ["architect_dead"]
    );
    assert_eq!(alerts[0].session, S);
    assert!(
        phrase.starts_with("architect=dead pane runs zsh"),
        "{phrase}"
    );
    assert!(log.is_empty() && r.calls() == 1);
    assert_eq!(
        state.wake_sent_at, None,
        "a relaunch restarts the silent clock"
    );
    assert_eq!(state.other["watchdog"]["pending"], json!(true));
    Ok(())
}

#[test]
fn watchdog_alerts_capture_failure_reads_idle_and_logs() -> TestResult {
    let env = Env::new(&[])?;
    let tmux = ScriptedTmux::new().fail("capture-pane", "gone");
    let mut state = PollState::default();
    state.other.insert("watchdog".to_owned(), json!("junk"));
    let (alerts, phrase, log) = watchdog_alerts(
        &tmux,
        &live(),
        &mut state,
        &env.config,
        NOW,
        &Fake::ok(),
        false,
    );
    assert!(alerts.is_empty());
    assert_eq!(phrase, "architect=ok");
    assert!(
        log.iter().any(|l| l.starts_with("architect capture ERROR")),
        "{log:?}"
    );
    assert!(log.iter().any(|l| l.contains("reset")), "{log:?}");
    assert!(state.other["watchdog"].is_object());
    Ok(())
}

#[test]
fn watchdog_alerts_shadow_never_relaunches() -> TestResult {
    let mut env = Env::new(&[])?;
    env.config.shadow = true;
    trusty_architect::poller::prepare_shadow(&env.config)?;
    let tmux = cycle_support::fleet(&[(S, "%1", "zsh")], "");
    let clock = FakeClock::new(NOW);
    let r = Fake::ok();
    let mut runner = env.runner(&tmux, &clock, &QuietHost);
    runner.relauncher = Some(&r);
    let summary = runner.run_once(&mut PollSession::default())?;
    assert_eq!(r.calls(), 0, "a shadow run relaunches nothing");
    assert!(summary.new.iter().any(|a| a.kind == "architect_dead"));
    assert!(
        env.log().contains("dry-run: would relaunch"),
        "{}",
        env.log()
    );
    assert!(
        env.notifier.sent().is_empty(),
        "a shadow run notifies nobody"
    );
    assert!(
        env.log()
            .contains("would notify: [tm-architect]: architect_dead")
    );
    assert!(tmux.sends().is_empty(), "a shadow run sends no keys");
    Ok(())
}
