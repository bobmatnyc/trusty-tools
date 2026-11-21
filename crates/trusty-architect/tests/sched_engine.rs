//! The engine's rules: due slots, jitter, catch-up, overlap, cap, backoff,
//! freshness, env allowlist (MB1595). The runner is a fake throughout.

mod sched_support;

use std::time::{Duration, UNIX_EPOCH};

use sched_support::*;
use trusty_architect::FakeClock;
use trusty_architect::sched::engine_run::allowed_env;
use trusty_architect::sched::slot::jitter;

fn stamp_file(path: &std::path::Path, mtime: i64) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, "x").unwrap();
    let f = std::fs::OpenOptions::new().write(true).open(path).unwrap();
    f.set_modified(UNIX_EPOCH + Duration::from_secs(mtime as u64))
        .unwrap();
}

#[test]
fn first_sight_anchors_the_next_slot_and_runs_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let (clock, runner) = (FakeClock::new(T0 as f64), FakeRunner::new());
    let mut e = engine(dir.path(), &clock, &runner, reg(&script("job1", "10m", "")));
    let (started, records) = tick_and_wait(&mut e);
    assert!(started.is_empty() && records.is_empty());
    assert_eq!(e.state().jobs["job1"].next_due, T0 + 600);
    assert_eq!(runner.count(), 0);
}

#[test]
fn due_slot_runs_once_and_logs_one_line() {
    let dir = tempfile::tempdir().unwrap();
    let (clock, runner) = (FakeClock::new(T0 as f64), FakeRunner::new());
    let mut e = engine(dir.path(), &clock, &runner, reg(&script("job1", "10m", "")));
    tick_and_wait(&mut e);
    advance(&clock, 600);
    let (started, records) = tick_and_wait(&mut e);
    assert_eq!(started, ["job1"]);
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].outcome, "ok");
    assert_eq!(records[0].slot, T0 + 600);
    // The same instant again does not rerun the slot.
    let (again, _) = tick_and_wait(&mut e);
    assert!(again.is_empty());
    assert_eq!(runner.count(), 1);
    let st = &e.state().jobs["job1"];
    assert_eq!(
        (st.next_due, st.last_result.as_deref()),
        (T0 + 1200, Some("ok"))
    );
    // runs.jsonl holds the contract's fields, one line.
    let log = lines(&layout(dir.path()).runs());
    assert_eq!(log.len(), 1);
    let v: serde_json::Value = serde_json::from_str(&log[0]).unwrap();
    for k in ["ts", "job", "slot", "start", "end", "exit", "outcome"] {
        assert!(v.get(k).is_some(), "runs.jsonl line lacks {k}");
    }
    assert_eq!(
        (v["job"].as_str(), v["exit"].as_i64()),
        (Some("job1"), Some(0))
    );
}

#[test]
fn script_job_runs_its_argv_in_the_job_cwd_with_a_job_log() {
    let dir = tempfile::tempdir().unwrap();
    let (clock, runner) = (FakeClock::new(T0 as f64), FakeRunner::new());
    let fields = r#""cwd":"sub","timeout_s":7,"env":["EXTRA"]"#;
    let mut e = engine(
        dir.path(),
        &clock,
        &runner,
        reg(&script("job1", "1m", fields)),
    );
    tick_and_wait(&mut e);
    advance(&clock, 60);
    tick_and_wait(&mut e);
    let spec = runner.last();
    assert_eq!(spec.argv, ["job-job1", "x"]);
    assert_eq!(spec.cwd, dir.path().join("sub"));
    assert_eq!(spec.timeout, Duration::from_secs(7));
    assert_eq!(spec.stdin, None);
    assert!(spec.env.iter().all(|(k, _)| k != "SECRET_TOKEN"));
    assert_eq!(
        spec.stdout,
        dir.path().join("local/logs/jobs/job1/2023-11-15.log")
    );
    assert_eq!(spec.stdout, spec.stderr);
}

#[test]
fn env_allowlist_drops_unlisted_variables() {
    let all: Vec<(String, String)> = [
        ("PATH", "/bin"),
        ("HOME", "/h"),
        ("TMUX_SOCKET", "/s"),
        ("ARCHITECT_SESSION", "a"),
        ("SLACK_BOT_TOKEN", "xoxb"),
        ("EXTRA", "1"),
        ("GH_TOKEN", "t"),
    ]
    .iter()
    .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
    .collect();
    let got: Vec<String> = allowed_env(&all, &["EXTRA".to_owned()])
        .into_iter()
        .map(|(k, _)| k)
        .collect();
    assert_eq!(
        got,
        ["PATH", "HOME", "TMUX_SOCKET", "ARCHITECT_SESSION", "EXTRA"]
    );
    let bare: Vec<String> = allowed_env(&all, &[]).into_iter().map(|(k, _)| k).collect();
    assert!(!bare.contains(&"EXTRA".to_owned()) && !bare.contains(&"GH_TOKEN".to_owned()));
}

#[test]
fn jitter_delays_the_fire_time_past_the_slot() {
    let dir = tempfile::tempdir().unwrap();
    // Pick an id whose jitter for the first slot is above zero.
    let slot = T0 + 600;
    let id = (0..50)
        .map(|i| format!("jit{i}"))
        .find(|id| jitter(id, slot, 30) > 2)
        .unwrap();
    let j = jitter(&id, slot, 30) as u64;
    let (clock, runner) = (FakeClock::new(T0 as f64), FakeRunner::new());
    let mut e = engine(
        dir.path(),
        &clock,
        &runner,
        reg(&script(&id, "10m", r#""schedule_note":1"#)
            .replace(r#""every":"10m""#, r#""every":"10m","jitter_s":30"#)),
    );
    tick_and_wait(&mut e);
    advance(&clock, 600);
    assert!(
        tick_and_wait(&mut e).0.is_empty(),
        "the slot is due but the jitter has not passed"
    );
    advance(&clock, j - 1);
    assert!(tick_and_wait(&mut e).0.is_empty());
    advance(&clock, 1);
    assert_eq!(tick_and_wait(&mut e).0.len(), 1);
}

#[test]
fn catch_up_run_once_runs_one_late_slot_and_counts_misses() {
    let dir = tempfile::tempdir().unwrap();
    let (clock, runner) = (FakeClock::new(T0 as f64), FakeRunner::new());
    let mut e = engine(dir.path(), &clock, &runner, reg(&script("job1", "10m", "")));
    tick_and_wait(&mut e);
    // The daemon was away for an hour: six slots passed.
    advance(&clock, 3600);
    let (started, records) = tick_and_wait(&mut e);
    assert_eq!(started.len(), 1, "one catch-up run, not six");
    assert_eq!(records[0].outcome, "ok");
    let st = &e.state().jobs["job1"];
    assert_eq!(st.misses, 5);
    assert_eq!(st.next_due, T0 + 4200);
}

#[test]
fn catch_up_skip_drops_a_late_slot_but_runs_an_on_time_one() {
    let dir = tempfile::tempdir().unwrap();
    let (clock, runner) = (FakeClock::new(T0 as f64), FakeRunner::new());
    let job =
        script("job1", "10m", "").replace(r#""every":"10m""#, r#""every":"10m","catch_up":"skip""#);
    let mut e = engine(dir.path(), &clock, &runner, reg(&job));
    tick_and_wait(&mut e);
    advance(&clock, 3600);
    let (started, records) = tick_and_wait(&mut e);
    assert!(started.is_empty());
    assert_eq!(records[0].outcome, "skipped");
    assert_eq!(e.state().jobs["job1"].misses, 6);
    assert_eq!(e.state().jobs["job1"].next_due, T0 + 4200);
    // The next slot, observed on time, runs.
    advance(&clock, 600);
    assert_eq!(tick_and_wait(&mut e).0.len(), 1);
}

#[test]
fn overlap_skips_a_slot_while_the_previous_run_is_alive() {
    let dir = tempfile::tempdir().unwrap();
    let (clock, runner) = (FakeClock::new(T0 as f64), FakeRunner::new());
    let mut e = engine(dir.path(), &clock, &runner, reg(&script("job1", "10m", "")));
    runner.hold.store(true, std::sync::atomic::Ordering::SeqCst);
    tick_and_wait_nowait(&mut e);
    advance(&clock, 600);
    assert_eq!(e.tick().unwrap().started, ["job1"]);
    advance(&clock, 600);
    let rep = e.tick().unwrap();
    assert!(rep.started.is_empty());
    assert_eq!(rep.records.len(), 1);
    assert_eq!(rep.records[0].outcome, "overrun");
    assert_eq!(e.state().jobs["job1"].overruns, 1);
    runner
        .hold
        .store(false, std::sync::atomic::Ordering::SeqCst);
    let done = e.wait_idle(Duration::from_secs(10));
    assert_eq!(done.records[0].outcome, "ok");
    assert_eq!(runner.count(), 1, "the overrun slot started nothing");
}

/// A tick that does not wait for runs (the runner is held).
fn tick_and_wait_nowait(e: &mut trusty_architect::sched::Engine<'_>) {
    e.tick().unwrap();
}

#[test]
fn a_timed_out_run_is_recorded_as_timeout() {
    let dir = tempfile::tempdir().unwrap();
    let (clock, runner) = (FakeClock::new(T0 as f64), FakeRunner::new());
    runner.push(timed_out());
    let mut e = engine(dir.path(), &clock, &runner, reg(&script("job1", "10m", "")));
    tick_and_wait(&mut e);
    advance(&clock, 600);
    let (_, records) = tick_and_wait(&mut e);
    assert_eq!((records[0].outcome, records[0].exit), ("timeout", None));
    assert_eq!(
        e.state().jobs["job1"].last_result.as_deref(),
        Some("timeout")
    );
}

#[test]
fn daily_cap_skips_the_extra_slots_and_resets_at_utc_midnight() {
    let dir = tempfile::tempdir().unwrap();
    let (clock, runner) = (FakeClock::new(T0 as f64), FakeRunner::new());
    let mut e = engine(
        dir.path(),
        &clock,
        &runner,
        reg(&script("job1", "1m", r#""daily_cap":2"#)),
    );
    tick_and_wait(&mut e);
    let mut outcomes = Vec::new();
    for _ in 0..4 {
        advance(&clock, 60);
        outcomes.extend(tick_and_wait(&mut e).1.into_iter().map(|r| r.outcome));
    }
    assert_eq!(outcomes, ["ok", "ok", "skipped", "skipped"]);
    assert_eq!(runner.count(), 2);
    // Next UTC day: the count starts over.
    advance(&clock, 86_400);
    assert_eq!(tick_and_wait(&mut e).0.len(), 1);
    assert_eq!(e.state().jobs["job1"].runs_today, 1);
}

#[test]
fn failures_back_off_and_raise_one_alert_per_streak() {
    let dir = tempfile::tempdir().unwrap();
    let (clock, runner) = (FakeClock::new(T0 as f64), FakeRunner::new());
    for _ in 0..6 {
        runner.push(failed());
    }
    let mut e = engine(dir.path(), &clock, &runner, reg(&script("job1", "10s", "")));
    tick_and_wait(&mut e);
    let mut outcomes = Vec::new();
    for _ in 0..20 {
        advance(&clock, 10);
        outcomes.extend(
            tick_and_wait(&mut e)
                .1
                .into_iter()
                .map(|r| (now(&clock) - T0, r.outcome)),
        );
    }
    // Run at +10 (fails, backoff 30 s), skips at +20 and +30, run at +40
    // (fails, backoff 60 s), skips to +90, run at +100 (third failure).
    let runs: Vec<i64> = outcomes
        .iter()
        .filter(|(_, o)| *o == "fail")
        .map(|(t, _)| *t)
        .collect();
    assert_eq!(&runs[..3], [10, 40, 100]);
    assert!(outcomes.iter().any(|(t, o)| *t == 20 && *o == "skipped"));
    let alerts = lines(&layout(dir.path()).alerts());
    assert_eq!(alerts.len(), 1, "one alert for the streak: {alerts:?}");
    assert!(alerts[0].contains("meta_miss") && alerts[0].contains("job1"));
    // A success ends the streak.
    assert_eq!(e.state().jobs["job1"].fail_streak, 3);
}

#[test]
fn managed_disabled_and_nonscheduled_jobs_never_run() {
    let dir = tempfile::tempdir().unwrap();
    let (clock, runner) = (FakeClock::new(T0 as f64), FakeRunner::new());
    let managed = script("mng", "1m", r#""managed_by":"meta_due""#);
    let external = script("ext", "1m", r#""managed_by":"external:poller""#);
    let off = script("off", "1m", r#""reason":"paused""#)
        .replace(r#""enabled":true"#, r#""enabled":false"#);
    let mut e = engine(
        dir.path(),
        &clock,
        &runner,
        reg(&format!("{managed},{external},{off}")),
    );
    for _ in 0..6 {
        tick_and_wait(&mut e);
        advance(&clock, 60);
    }
    assert_eq!(runner.count(), 0);
    assert!(
        e.state().jobs.is_empty(),
        "not even state is kept for jobs the scheduler does not run"
    );
}

#[test]
fn stale_artifact_escalates_once_per_streak_and_fresh_resets() {
    let dir = tempfile::tempdir().unwrap();
    let (clock, runner) = (FakeClock::new(T0 as f64), FakeRunner::new());
    let check = r#""check":{"artifact":{"type":"glob","path":"out/*.md","max_age_h":1},"escalate_after":2,"grace_s":100}"#;
    let mut e = engine(
        dir.path(),
        &clock,
        &runner,
        reg(&script("job1", "1h", check)),
    );
    tick_and_wait(&mut e);
    let alerts = || lines(&layout(dir.path()).alerts());
    // Slot 1 at +1h runs; the check waits for slot + grace.
    advance(&clock, 3600);
    tick_and_wait(&mut e);
    advance(&clock, 99);
    tick_and_wait(&mut e);
    assert_eq!(e.state().jobs["job1"].misses, 0);
    advance(&clock, 1);
    tick_and_wait(&mut e);
    assert_eq!(
        e.state().jobs["job1"].misses,
        1,
        "no artifact: one miss, no alert yet"
    );
    assert!(alerts().is_empty());
    // Slot 2: a second miss raises the one alert.
    advance(&clock, 3600);
    tick_and_wait(&mut e);
    advance(&clock, 100);
    tick_and_wait(&mut e);
    assert_eq!(e.state().jobs["job1"].misses, 2);
    let got = alerts();
    assert_eq!(got.len(), 1);
    assert!(
        got[0].contains("meta_miss") && got[0].contains("out/*.md") && got[0].contains("job1"),
        "{got:?}"
    );
    // Slot 3: a third miss is the same streak: no second alert. A stale file
    // (two hours old) is stale, not missing.
    stamp_file(&dir.path().join("out/a.md"), now(&clock) - 7200);
    advance(&clock, 3600);
    tick_and_wait(&mut e);
    advance(&clock, 100);
    tick_and_wait(&mut e);
    assert_eq!(e.state().jobs["job1"].misses, 3);
    assert_eq!(alerts().len(), 1);
    // A fresh artifact resets the streak.
    stamp_file(&dir.path().join("out/b.md"), now(&clock) + 3500 - 10);
    advance(&clock, 3600);
    tick_and_wait(&mut e);
    advance(&clock, 100);
    tick_and_wait(&mut e);
    let st = &e.state().jobs["job1"];
    assert_eq!((st.misses, st.alerted), (0, false));
}
