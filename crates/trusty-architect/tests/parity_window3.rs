//! MB1595 parity window 3: one regression test per divergence cause, each red
//! on the pre-fix commit, plus the fail-open checks of the new read paths.
//! (a) cold start, (b) the idle epoch, (c) the reminder the live poller fired.
//! Every file is in a tempdir; tmux is scripted; nothing reads a live inbox.

mod cycle_support;
mod sched_support;

use std::fs;
use std::path::Path;

use cycle_support::{Env, QuietHost, TestResult, fleet};
use sched_support::{FakeRunner, T0, advance, engine, layout, reg_with, tick_and_wait};
use serde_json::Value;
use trusty_architect::classify::{PaneState, parse_pane};
use trusty_architect::poller::live_idle;
use trusty_architect::poller::steps::live_reminders::{SEEN_KEY, shadow_reminder_step};
use trusty_architect::poller::{CycleSummary, PollSession};
use trusty_architect::sched::shadow::DRY_RUN_STATE;
use trusty_architect::sched::slot::parse_iso;
use trusty_architect::{Clock, FakeClock};

const LIVE_KEY: &str = "||session_exited|S02@2026-11-01T13:49:32Z";

fn read_json(p: &Path) -> TestResult<Value> {
    Ok(serde_json::from_str(&fs::read_to_string(p)?)?)
}

// --- (a) cold start ----------------------------------------------------------

/// Window 3 opened with 15 replayed alerts: the restarted shadow kept a state
/// file that lacked the live poller's dedup keys. Red before the fix: the
/// stale state was kept. The daemon's later runs keep the shadow's own state.
#[test]
fn shadow_restart_reseeds_from_the_live_state() -> TestResult {
    let dir = tempfile::tempdir()?;
    let l = layout(dir.path());
    fs::create_dir_all(dir.path().join("inbox"))?;
    fs::create_dir_all(l.shadow_dir())?;
    let live = format!(r#"{{"alerts": {{"{LIVE_KEY}": {{"first": 1.0, "last": 2.0}}}}}}"#);
    fs::write(dir.path().join("inbox/poll-state.json"), &live)?;
    let state = l.shadow_dir().join(DRY_RUN_STATE);
    fs::write(
        &state,
        r#"{"alerts": {}, "shadow_live_offsets_seeded": true}"#,
    )?;
    let (clock, runner) = (FakeClock::new(T0 as f64), FakeRunner::new());
    let reg = reg_with(
        &sched_support::script("job1", "1h", ""),
        r#""poll":{"every_s":60}"#,
    );
    let mut e = engine(dir.path(), &clock, &runner, reg);
    assert_eq!(tick_and_wait(&mut e).0, ["poll"]);
    let seeded = read_json(&state)?;
    assert!(seeded["alerts"].get(LIVE_KEY).is_some(), "{seeded}");
    // The next run of the same daemon keeps what the dry run wrote.
    let own = r#"{"alerts":{"own":{}},"shadow_live_offsets_seeded":true}"#;
    fs::write(&state, own)?;
    advance(&clock, 60);
    assert_eq!(tick_and_wait(&mut e).0, ["poll"]);
    assert_eq!(fs::read_to_string(&state)?, own);
    Ok(())
}

/// Fail-open check: a reseed that cannot copy the live state is a failed
/// poll run, never a run on the stale state, and the next run retries it.
#[test]
fn shadow_restart_reseed_failure_is_an_error() -> TestResult {
    let dir = tempfile::tempdir()?;
    let l = layout(dir.path());
    let live = dir.path().join("inbox/poll-state.json");
    fs::create_dir_all(&live)?; // a directory: the copy fails
    fs::create_dir_all(l.shadow_dir())?;
    let state = l.shadow_dir().join(DRY_RUN_STATE);
    let stale = r#"{"alerts": {}, "shadow_live_offsets_seeded": true}"#;
    fs::write(&state, stale)?;
    let (clock, runner) = (FakeClock::new(T0 as f64), FakeRunner::new());
    let reg = reg_with(
        &sched_support::script("job1", "1h", ""),
        r#""poll":{"every_s":60}"#,
    );
    let mut e = engine(dir.path(), &clock, &runner, reg);
    let (started, records) = tick_and_wait(&mut e);
    assert!(started.is_empty(), "{started:?}");
    assert_eq!(records[0].outcome, "fail");
    assert_eq!(fs::read_to_string(&state)?, stale);
    fs::remove_dir(&live)?;
    fs::write(
        &live,
        format!(r#"{{"alerts": {{"{LIVE_KEY}": {{"first": 1.0, "last": 2.0}}}}}}"#),
    )?;
    advance(&clock, 60);
    assert_eq!(tick_and_wait(&mut e).0, ["poll"]);
    assert!(read_json(&state)?["alerts"].get(LIVE_KEY).is_some());
    Ok(())
}

// --- (b) the idle epoch --------------------------------------------------------

fn idle_capture() -> String {
    let r = "─".repeat(40);
    format!("⏺ Done.\n\n{r}\n❯ \n{r}\n  ⏵⏵ bypass\n")
}

/// One shadow dry-run cycle at `now` over pane `tm-a %2`, with `live` as the
/// live `poll-state.json` text and `own` as the dry run's state.
fn shadow_cycle(live: &str, own: &str, now: f64) -> TestResult<CycleSummary> {
    let live_dir = tempfile::tempdir()?;
    fs::write(live_dir.path().join("poll-state.json"), live)?;
    let scratch = tempfile::tempdir()?;
    let state = scratch.path().join("dry-run-state.json");
    fs::write(&state, own)?;
    let (live_s, state_s) = (
        live_dir.path().display().to_string(),
        state.display().to_string(),
    );
    let mut env = Env::new(&[
        ("ARCHITECT_READ_INBOX_DIR", &live_s),
        ("ARCHITECT_DRY_RUN_STATE", &state_s),
    ])?;
    env.config.shadow = true;
    let tmux = fleet(&[("tm-a", "%2", "claude")], &idle_capture());
    let clock = FakeClock::new(now);
    let mut runner = env.runner(&tmux, &clock, &QuietHost);
    runner.dry_run = true;
    let (mut session, _) = PollSession::load(&env.config, true, clock.now_unix());
    Ok(runner.run_once(&mut session)?)
}

fn idle_fps(s: &CycleSummary) -> Vec<String> {
    s.new
        .iter()
        .filter(|a| a.kind == "idle")
        .map(|a| a.fingerprint.clone())
        .collect()
}

/// Window 3: the shadow stamped `idle_since` at its own first idle cycle,
/// 81 s before the live poller, so every idle alert had another fingerprint.
/// Red before the fix: the fingerprint carried the shadow's own epoch.
#[test]
fn shadow_idle_epoch_follows_the_live_poller() -> TestResult {
    let (live_since, own_since) = (1_795_334_598.7, 1_795_334_517.2);
    let screen = parse_pane(&idle_capture()).screen;
    let live = format!(r#"{{"panes": {{"tm-a|%2": {{"idle_since": {live_since}}}}}}}"#);
    let own = format!(r#"{{"panes": {{"tm-a|%2": {{"idle_since": {own_since}}}}}}}"#);
    let got = shadow_cycle(&live, &own, live_since + 700.0)?;
    assert_eq!(idle_fps(&got), [format!("since-1795334598-{screen}")]);
    // The live poller saw the pane busy (null): the shadow starts a new
    // stretch too, so no alert with its stale epoch.
    let busy = r#"{"panes": {"tm-a|%2": {"idle_since": null}}}"#;
    assert!(idle_fps(&shadow_cycle(busy, &own, live_since + 700.0)?).is_empty());
    // A pane the live state lacks keeps the shadow's own epoch.
    let got = shadow_cycle(r#"{"panes": {}}"#, &own, live_since + 700.0)?;
    assert_eq!(idle_fps(&got), [format!("since-1795334517-{screen}")]);
    Ok(())
}

/// Fail-open check: an unreadable live state is no epochs and one log line;
/// the pane keeps its own epoch. A missing one is quiet.
#[test]
fn shadow_idle_epoch_unreadable_live_state_keeps_its_own_and_logs() -> TestResult {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("poll-state.json");
    let mut log = Vec::new();
    assert!(live_idle::read(&path, &mut log).is_none() && log.is_empty());
    fs::write(&path, "{broken")?;
    assert!(live_idle::read(&path, &mut log).is_none());
    assert!(
        log.len() == 1 && log[0].starts_with("live idle ERROR parse"),
        "{log:?}"
    );
    let mut ps = PaneState {
        idle_since: Some(5.0),
        ..PaneState::default()
    };
    fs::write(&path, r#"{"panes": {"s|%1": {"errors": []}}}"#)?;
    let live = live_idle::read(&path, &mut log).ok_or("no epochs")?;
    live_idle::adopt(&mut ps, &live, "s|%1");
    assert_eq!(ps.idle_since, Some(5.0)); // no idle_since: says nothing
    Ok(())
}

// --- (c) the reminder ------------------------------------------------------------

fn write_ledger(root: &Path, reminders: &str) -> TestResult {
    let dir = root.join("local/il/.trusty-mpm/datg");
    fs::create_dir_all(&dir)?;
    let st = format!(
        r#"{{"code": "ar", "v": 1, "next": {{}}, "goals": [], "items": [], "session": {{}},
        "tasks": [{{"id": "tk-T9u", "title": "parity windows", "reminders": [{reminders}]}}]}}"#
    );
    fs::write(dir.join("state.json"), st)?;
    Ok(())
}

/// Window 3: the live poller fired `tk-T9u #3` at 11:05:01Z; the shadow's dry
/// shim at 11:05:40Z saw it `fired`, not pending, and raised nothing. Red
/// before the fix. The alert matches `fleet-poll.py`'s (fingerprint
/// `e439233a4745`, the live row's), is new once, and a reminder fired before
/// the shadow's first look is never replayed.
#[test]
fn shadow_reminder_fired_by_the_live_poller_is_raised_once() -> TestResult {
    let scratch = tempfile::tempdir()?;
    let state = scratch.path().join("dry-run-state.json");
    fs::write(&state, "{}")?;
    let state_s = state.display().to_string();
    let mut env = Env::new(&[("ARCHITECT_DRY_RUN_STATE", &state_s)])?;
    env.config.shadow = true;
    write_ledger(
        env.root(),
        r#"{"n": 3, "at": "2026-11-22T11:05Z", "every": null, "note": "verdict due", "state": "fired", "fired_at": "2026-11-22T11:05Z", "fires": 1},
           {"n": 1, "at": "2026-11-22T10:00Z", "every": null, "note": "old", "state": "fired", "fired_at": "2026-11-22T10:00Z", "fires": 1}"#,
    )?;
    let tmux = fleet(&[], "");
    let t = parse_iso("2026-11-22T11:05:40Z").ok_or("iso")? as f64;
    let clock = FakeClock::new(t);
    let mut runner = env.runner(&tmux, &clock, &QuietHost);
    runner.dry_run = true;
    let mut reminders = Vec::new();
    for _ in 0..3 {
        let (mut session, _) = PollSession::load(&env.config, true, clock.now_unix());
        let s = runner.run_once(&mut session)?;
        reminders.push(
            s.new
                .iter()
                .filter(|a| a.kind == "reminder")
                .map(|a| (a.session.clone(), a.fingerprint.clone(), a.detail.clone()))
                .collect::<Vec<_>>(),
        );
        clock.sleep(std::time::Duration::from_secs(60));
    }
    let detail = "tk-T9u #3: verdict due (due 2026-11-22T11:05Z; task: parity windows)";
    assert_eq!(
        reminders,
        [
            vec![(String::new(), "e439233a4745".to_owned(), detail.to_owned())],
            vec![],
            vec![]
        ]
    );
    Ok(())
}

/// Fail-open check: a ledger that cannot be read is no alerts, one log line,
/// and the window mark is kept, so the next cycle reads the same window.
#[test]
fn shadow_reminder_unreadable_ledger_logs_and_keeps_the_window() -> TestResult {
    let env = Env::new(&[])?;
    let mut other = serde_json::Map::new();
    other.insert(SEEN_KEY.to_owned(), Value::from(100.0));
    let (found, log) = shadow_reminder_step(&env.config, &mut other, 200.0);
    assert!(found.is_empty());
    assert!(
        log.len() == 1 && log[0].starts_with("live reminders ERROR"),
        "{log:?}"
    );
    assert_eq!(other[SEEN_KEY], Value::from(100.0));
    write_ledger(env.root(), "")?;
    let (found, log) = shadow_reminder_step(&env.config, &mut other, 200.0);
    assert!(found.is_empty() && log.is_empty());
    assert_eq!(other[SEEN_KEY], Value::from(200.0));
    Ok(())
}
