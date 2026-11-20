//! #9779: D5r (routines ran, push due) and the shadow run. Every path lives
//! in a tempdir; nothing touches a live inbox.

mod cycle_support;

use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use cycle_support::{Env, QuietHost, TestResult, config_at, fleet, screen};
use serde_json::{Map, json};
use trusty_architect::FakeClock;
use trusty_architect::poller::prepare_shadow;
use trusty_architect::poller::routines::{RoutinesError, push_due, routine_alerts};

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

#[test]
fn push_due_fires_once_after_thirty_minutes_and_clears() {
    let mut other = Map::new();
    assert!(push_due(&mut other, &[], 0.0, None).is_empty());
    assert!(
        !other.contains_key("push"),
        "no failure leaves the state untouched"
    );
    let key = vec!["architect_dead:dead".to_owned()];
    assert!(push_due(&mut other, &key, 1000.0, None).is_empty());
    assert!(push_due(&mut other, &key, 2799.0, None).is_empty());
    let due = push_due(&mut other, &key, 2800.0, None);
    assert_eq!(
        due,
        [
            "push due: architect_dead:dead uncleared 30 min (ARCHITECT_PUSH_CHANNEL unset; nothing sent)"
        ]
    );
    assert!(
        push_due(&mut other, &key, 9000.0, None).is_empty(),
        "reported once"
    );
    // Cleared, then failing again: a fresh 30 minutes.
    assert!(push_due(&mut other, &[], 9100.0, None).is_empty());
    assert_eq!(other["push"], json!({}));
    assert!(push_due(&mut other, &key, 9200.0, Some("slack")).is_empty());
    let due = push_due(&mut other, &key, 11_000.0, Some("slack"));
    assert!(due[0].contains("channel slack has no sender in this build; nothing sent"));
    // A corrupt record is replaced, not trusted.
    other.insert("push".to_owned(), json!([1]));
    assert!(push_due(&mut other, &key, 0.0, None).is_empty());
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
    assert_eq!(shadow, ["alerts.jsonl", "poll-state.json", "shadow.log"]);
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
    assert!(hit["detail"].as_str().is_some_and(|d| d.contains("parse")), "{hit}");
    Ok(())
}
