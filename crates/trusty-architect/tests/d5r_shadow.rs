//! #9779: D5r (routines ran, push due) and the shadow run. Every path lives
//! in a tempdir; nothing touches a live inbox.

mod cycle_support;

use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use cycle_support::{Env, QuietHost, TestResult, config_at, fleet, screen};
use serde_json::{Map, json};
use trusty_architect::FakeClock;
use trusty_architect::classify::alert;
use trusty_architect::poller::prepare_shadow;
use trusty_architect::poller::push::{PushRoute, push_step};
use trusty_architect::poller::routines::{RoutinesError, routine_alerts};

fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64())
}

fn routines_file(dir: &Path, body: &str) -> TestResult<std::path::PathBuf> {
    let p = dir.join("routines.json");
    fs::write(&p, body)?;
    Ok(p)
}

#[test]
fn routines_flag_a_stale_and_a_missing_stamp() -> TestResult {
    let dir = tempfile::tempdir()?;
    let fresh = dir.path().join("fresh.stamp");
    fs::write(&fresh, "")?;
    let missing = dir.path().join("never.stamp");
    let list = json!([
        {"name": "heartbeat", "stamp": fresh, "interval_s": 3600},
        {"name": "morning-brief", "stamp": missing, "interval_s": 86400},
    ]);
    let path = routines_file(dir.path(), &list.to_string())?;
    let t = now();
    assert!(
        routine_alerts(&path, "tm-architect", t)?
            .iter()
            .all(|a| a.fingerprint != "heartbeat")
    );
    let alerts = routine_alerts(&path, "tm-architect", t + 3.0 * 3600.0)?;
    let got: Vec<_> = alerts
        .iter()
        .map(|a| (a.kind.as_str(), a.fingerprint.as_str(), a.session.as_str()))
        .collect();
    assert_eq!(
        got,
        [
            ("routine_stale", "heartbeat", "tm-architect"),
            ("routine_stale", "morning-brief", "tm-architect")
        ]
    );
    assert!(
        alerts[0]
            .detail
            .contains("last ran 180 min ago (expected every 60 min)")
    );
    assert!(alerts[1].detail.contains("no run recorded"));
    Ok(())
}

#[test]
fn routines_file_absent_turns_the_check_off() -> TestResult {
    let dir = tempfile::tempdir()?;
    assert!(routine_alerts(&dir.path().join("none.json"), "s", now())?.is_empty());
    Ok(())
}

#[test]
fn routines_file_malformed_is_an_error() -> TestResult {
    let dir = tempfile::tempdir()?;
    let path = routines_file(dir.path(), "{not json")?;
    assert!(matches!(
        routine_alerts(&path, "s", now()),
        Err(RoutinesError::Parse { .. })
    ));
    // An unreadable list (a directory) is a read error.
    let err = routine_alerts(dir.path(), "s", now());
    assert!(matches!(err, Err(RoutinesError::Read { .. })), "{err:?}");
    Ok(())
}

#[test]
fn routines_relative_stamp_is_an_error() -> TestResult {
    let dir = tempfile::tempdir()?;
    let rel = routines_file(
        dir.path(),
        r#"[{"name":"x","stamp":"rel/x","interval_s":60}]"#,
    )?;
    let err = routine_alerts(&rel, "s", now());
    assert!(matches!(err, Err(RoutinesError::Invalid { .. })), "{err:?}");
    let zero = routines_file(dir.path(), r#"[{"name":"x","stamp":"/x","interval_s":0}]"#)?;
    let err = routine_alerts(&zero, "s", now());
    assert!(matches!(err, Err(RoutinesError::Invalid { .. })), "{err:?}");
    Ok(())
}

/// `push_step` with the channel unset: the bookkeeping alone.
fn push_due(other: &mut Map<String, serde_json::Value>, on: bool, now: f64) -> Vec<String> {
    let failing = if on {
        vec![alert("architect_dead", "dead", "dead", "s", "", now)]
    } else {
        Vec::new()
    };
    let route = PushRoute {
        channel: None,
        recipient: None,
        sender: None,
        hands_off: false,
        supervisor: "s",
    };
    let (alerts, lines) = push_step(other, &failing, now, &route);
    assert!(alerts.is_empty(), "{alerts:?}");
    lines
}

#[test]
fn push_due_fires_once_after_thirty_minutes_and_clears() {
    let mut other = Map::new();
    assert!(push_due(&mut other, false, 0.0).is_empty());
    assert!(
        !other.contains_key("push"),
        "no failure leaves the state untouched"
    );
    assert!(push_due(&mut other, true, 1000.0).is_empty());
    assert!(push_due(&mut other, true, 2799.0).is_empty());
    let due = push_due(&mut other, true, 2800.0);
    assert_eq!(
        due,
        [
            "push due: architect_dead:dead uncleared 30 min (ARCHITECT_PUSH_CHANNEL unset; nothing sent)"
        ]
    );
    assert!(
        push_due(&mut other, true, 9000.0).is_empty(),
        "reported once"
    );
    // Cleared, then failing again: a fresh 30 minutes.
    assert!(push_due(&mut other, false, 9100.0).is_empty());
    assert_eq!(other["push"], json!({}));
    assert!(push_due(&mut other, true, 9200.0).is_empty());
    assert_eq!(push_due(&mut other, true, 11_000.0).len(), 1);
    // A corrupt record is replaced, not trusted.
    other.insert("push".to_owned(), json!([1]));
    assert_eq!(
        push_due(&mut other, true, 0.0),
        ["push: state record was not an object; reset"]
    );
    assert!(other["push"].is_object());
}

#[test]
fn config_reads_the_d5r_and_shadow_keys() -> TestResult {
    let dir = tempfile::tempdir()?;
    let c = config_at(dir.path(), &[])?;
    assert_eq!((c.silent_minutes, c.frozen_minutes), (45.0, 15.0));
    assert_eq!(c.push_channel, None, "the push channel ships unset");
    assert_eq!(c.routines, dir.path().join("local/routines.json"));
    assert_eq!(
        c.relaunch_script,
        dir.path().join("scripts/session-relaunch.sh")
    );
    assert!(!c.shadow);
    let c = config_at(
        dir.path(),
        &[
            ("SILENT_MINUTES", "junk"),
            ("FROZEN_MIN", "5"),
            ("ARCHITECT_PUSH_CHANNEL", ""),
        ],
    )?;
    assert_eq!(
        (c.silent_minutes, c.frozen_minutes, c.push_channel.clone()),
        (45.0, 5.0, None)
    );
    let mut c = c;
    c.shadow = true;
    let shadow = c.inbox.join("shadow");
    assert_eq!(c.alerts(), shadow.join("alerts.jsonl"));
    assert_eq!(c.state(), shadow.join("poll-state.json"));
    assert_eq!(c.log(), shadow.join("shadow.log"));
    assert_eq!(
        c.events(),
        c.inbox.join("events.jsonl"),
        "events are read live"
    );
    Ok(())
}

fn listing(dir: &Path) -> TestResult<Vec<String>> {
    let mut names: Vec<String> = fs::read_dir(dir)?
        .map(|e| e.map(|e| e.file_name().to_string_lossy().into_owned()))
        .collect::<Result<_, _>>()?;
    names.sort();
    Ok(names)
}

#[test]
fn shadow_seeds_state_once_and_never_writes_the_inbox() -> TestResult {
    let mut env = Env::new(&[])?;
    let live_state = env.config.inbox.join("poll-state.json");
    fs::write(&live_state, r#"{"alerts": {}, "wake_sent_at": null}"#)?;
    let before = listing(&env.config.inbox)?;
    env.config.shadow = true;
    let tmux = fleet(&[("tm-a", "%2", "claude")], &screen(50));
    let clock = FakeClock::new(now());
    let runner = env.runner(&tmux, &clock, &QuietHost);
    runner.run_single("60")?;
    let mut after = listing(&env.config.inbox)?;
    after.retain(|n| n != "shadow");
    assert_eq!(
        before, after,
        "no live file created (no poll.lock, no poll.log)"
    );
    assert_eq!(
        fs::read_to_string(&live_state)?,
        r#"{"alerts": {}, "wake_sent_at": null}"#,
        "the live state is only read"
    );
    let shadow = listing(&env.config.shadow_dir())?;
    // MB1595 fix round: the token watcher keeps its own state in the shadow dir.
    // #11629: a shadow scan saves that scratch state, so it locks it here; the inbox stays untouched.
    assert_eq!(
        shadow,
        [
            "alerts.jsonl",
            "poll-state.json",
            "shadow.log",
            "token-watch-state.json",
            "token-watch-state.json.lock"
        ]
    );
    assert!(env.log().contains("shadow: seeded"), "{}", env.log());
    assert!(tmux.sends().is_empty(), "a shadow run sends no keys");
    // A second start keeps the shadow's own state.
    let saved = fs::read_to_string(env.config.state())?;
    assert!(prepare_shadow(&env.config)?.is_empty());
    assert_eq!(fs::read_to_string(env.config.state())?, saved);
    Ok(())
}

#[test]
fn shadow_seed_copy_failure_is_an_error() -> TestResult {
    let mut env = Env::new(&[])?;
    // A directory where the live state should be: the copy fails.
    fs::create_dir_all(env.config.inbox.join("poll-state.json"))?;
    env.config.shadow = true;
    assert!(prepare_shadow(&env.config).is_err());
    let tmux = fleet(&[], "");
    let clock = FakeClock::new(now());
    let err = env.runner(&tmux, &clock, &QuietHost).run_single("60");
    assert!(err.is_err_and(|e| e.to_string().contains("shadow")));
    assert!(
        tmux.calls().is_empty(),
        "nothing ran before the failed setup"
    );
    Ok(())
}

#[test]
fn routines_config_error_raises_an_alert_in_the_cycle() -> TestResult {
    // A malformed routines list must not switch D5r off silently (#9779).
    let env = Env::new(&[])?;
    fs::create_dir_all(env.root().join("local"))?;
    fs::write(&env.config.routines, "{not json")?;
    let tmux = fleet(&[], "");
    let clock = FakeClock::new(now());
    env.runner(&tmux, &clock, &QuietHost).run_single("60")?;
    let alerts = fs::read_to_string(env.config.alerts()).unwrap_or_default();
    let rows: Vec<serde_json::Value> = alerts
        .lines()
        .map(serde_json::from_str)
        .collect::<Result<_, _>>()?;
    let hit = rows.iter().find(|a| a["kind"] == "routine_stale");
    let hit = hit.ok_or_else(|| format!("no routine_stale alert in {alerts:?}"))?;
    assert_eq!(hit["fingerprint"], "config", "{hit}");
    assert!(
        hit["detail"].as_str().is_some_and(|d| d.contains("parse")),
        "{hit}"
    );
    Ok(())
}

const DAY: f64 = 86_400.0;
const HOUR: f64 = 3_600.0;

/// A goal_reconcile routine over `goals`, 24 h interval, stale after 26 h.
fn goal_routine(dir: &Path) -> TestResult<(std::path::PathBuf, std::path::PathBuf)> {
    let goals = dir.join("goals");
    fs::create_dir_all(&goals)?;
    let list = json!([{
        "name": "goal_reconcile", "goals_dir": goals,
        "interval_s": DAY, "stale_after_s": 26.0 * HOUR,
    }]);
    Ok((routines_file(dir, &list.to_string())?, goals))
}

fn goal(goals: &Path, id: &str, doc: &serde_json::Value) -> TestResult {
    fs::write(goals.join(format!("{id}.json")), doc.to_string())?;
    Ok(())
}

/// 2026-11-20T00:00:00Z as epoch seconds.
const T0: f64 = 1_795_132_800.0;

fn fingerprints(path: &Path, now: f64) -> TestResult<Vec<(String, String)>> {
    Ok(routine_alerts(path, "tm-architect", now)?
        .into_iter()
        .map(|a| (a.fingerprint, a.detail))
        .collect())
}

#[test]
fn goal_reconcile_fresh_goals_raise_nothing() -> TestResult {
    let dir = tempfile::tempdir()?;
    let (path, goals) = goal_routine(dir.path())?;
    let fresh = json!({"lifecycle": "active", "reconciled_at": "2026-11-20T00:00:00Z"});
    goal(&goals, "G01", &fresh)?;
    goal(&goals, "G02", &fresh)?;
    assert!(fingerprints(&path, T0 + 25.0 * HOUR)?.is_empty());
    Ok(())
}

#[test]
fn goal_reconcile_flags_a_stale_active_goal() -> TestResult {
    let dir = tempfile::tempdir()?;
    let (path, goals) = goal_routine(dir.path())?;
    goal(
        &goals,
        "G01",
        &json!({"lifecycle": "active", "reconciled_at": "2026-11-20T00:00:00Z"}),
    )?;
    goal(
        &goals,
        "G02",
        &json!({"lifecycle": "active", "reconciled_at": "2026-11-19T22:00:00Z"}),
    )?;
    let got = fingerprints(&path, T0 + 25.0 * HOUR)?;
    assert_eq!(got.len(), 1, "{got:?}");
    assert_eq!(got[0].0, "goal_reconcile:G02");
    assert!(
        got[0]
            .1
            .contains("goal G02 last reconciled 27 h ago (stale after 26 h)"),
        "{got:?}"
    );
    Ok(())
}

#[test]
fn goal_reconcile_skips_an_inactive_stale_goal() -> TestResult {
    let dir = tempfile::tempdir()?;
    let (path, goals) = goal_routine(dir.path())?;
    let old = "2026-10-01T00:00:00Z";
    goal(
        &goals,
        "G01",
        &json!({"lifecycle": "parked", "reconciled_at": old}),
    )?;
    goal(&goals, "G02", &json!({"lifecycle": "done"}))?;
    goal(
        &goals,
        "G03",
        &json!({"lifecycle": "active", "reconciled_at": old}),
    )?;
    let got = fingerprints(&path, T0)?;
    let fps: Vec<_> = got.iter().map(|(f, _)| f.as_str()).collect();
    assert_eq!(fps, ["goal_reconcile:G03"], "only the active one: {got:?}");
    Ok(())
}

#[test]
fn goal_reconcile_flags_a_missing_reconciled_at() -> TestResult {
    let dir = tempfile::tempdir()?;
    let (path, goals) = goal_routine(dir.path())?;
    goal(&goals, "G01", &json!({"lifecycle": "active"}))?;
    goal(
        &goals,
        "G02",
        &json!({"lifecycle": "active", "reconciled_at": "yesterday"}),
    )?;
    let got = fingerprints(&path, T0)?;
    assert_eq!(got.len(), 2, "{got:?}");
    assert!(got[0].1.contains("goal G01 never reconciled"), "{got:?}");
    assert!(
        got[1].1.contains("goal G02 reconciled_at \"yesterday\""),
        "{got:?}"
    );
    Ok(())
}

#[test]
fn goal_reconcile_unreadable_doc_fails_closed() -> TestResult {
    let dir = tempfile::tempdir()?;
    let (path, goals) = goal_routine(dir.path())?;
    fs::write(goals.join("G01.json"), "{not json")?;
    fs::create_dir(goals.join("G02.json"))?; // a read error, not a parse error
    goal(&goals, "G03", &json!(["not", "an", "object"]))?;
    let got = fingerprints(&path, T0)?;
    let fps: Vec<_> = got.iter().map(|(f, _)| f.as_str()).collect();
    assert_eq!(
        fps,
        [
            "goal_reconcile:G01",
            "goal_reconcile:G02",
            "goal_reconcile:G03"
        ],
        "{got:?}"
    );
    assert!(got.iter().all(|(_, d)| d.contains("unreadable")), "{got:?}");
    // A missing goals folder is one alert on the routine itself.
    fs::remove_dir_all(&goals)?;
    let got = fingerprints(&path, T0)?;
    assert_eq!(got.len(), 1, "{got:?}");
    assert_eq!(got[0].0, "goal_reconcile");
    assert!(got[0].1.contains("goals dir"), "{got:?}");
    Ok(())
}

#[test]
fn routines_stale_after_override() -> TestResult {
    let dir = tempfile::tempdir()?;
    let stamp = dir.path().join("brief.stamp");
    fs::write(&stamp, "")?;
    let list = json!([
        {"name": "default", "stamp": stamp, "interval_s": HOUR},
        {"name": "tight", "stamp": stamp, "interval_s": HOUR, "stale_after_s": 1.5 * HOUR},
    ]);
    let path = routines_file(dir.path(), &list.to_string())?;
    let got = fingerprints(&path, now() + 1.75 * HOUR)?;
    let fps: Vec<_> = got.iter().map(|(f, _)| f.as_str()).collect();
    assert_eq!(fps, ["tight"], "default stays 2x interval: {got:?}");
    for bad in [
        json!([{"name": "x", "stamp": "/x", "interval_s": 60, "stale_after_s": 0}]),
        json!([{"name": "x", "stamp": "/x", "goals_dir": "/g", "interval_s": 60}]),
        json!([{"name": "x", "goals_dir": "rel/g", "interval_s": 60}]),
        json!([{"name": "x", "interval_s": 60}]),
    ] {
        let p = routines_file(dir.path(), &bad.to_string())?;
        let err = routine_alerts(&p, "s", now());
        assert!(
            matches!(err, Err(RoutinesError::Invalid { .. })),
            "{bad}: {err:?}"
        );
    }
    let typo = routines_file(
        dir.path(),
        r#"[{"name":"x","stamp":"/x","interval_s":60,"stale_after":1}]"#,
    )?;
    let err = routine_alerts(&typo, "s", now());
    assert!(matches!(err, Err(RoutinesError::Parse { .. })), "{err:?}");
    Ok(())
}
