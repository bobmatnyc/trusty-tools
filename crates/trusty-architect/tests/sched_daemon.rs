//! The daemon: lock contention, reload, liveness, stop (MB1595).

mod sched_support;

use std::process::{Command, Stdio};
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use sched_support::*;
use trusty_architect::FakeClock;
use trusty_architect::sched::SchedError;
use trusty_architect::sched::daemon::{Instance, Reload, load_initial, read_alive, run_loop};

fn write_registry(dir: &std::path::Path, jobs: &str) -> std::path::PathBuf {
    let path = dir.join("local/meta/registry.json");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(
        &path,
        format!(r#"{{"version":1,"poll":{{"enabled":false}},"jobs":[{jobs}]}}"#),
    )
    .unwrap();
    path
}

#[test]
fn second_instance_is_refused_while_the_lock_is_held() {
    let dir = tempfile::tempdir().unwrap();
    let l = layout(dir.path());
    let first = Instance::acquire(&l).unwrap();
    let e = Instance::acquire(&l).unwrap_err();
    assert!(
        matches!(e, SchedError::Locked(ref p) if p == &l.lock()),
        "{e}"
    );
    assert_eq!(e.exit_code(), 1);
    drop(first);
    // The kernel released it: a restart after a crash is not blocked. A sibling
    // test forking a child can hold a copy of the fd until its exec, so retry
    // briefly (a fork child keeps the open file description, hence the lock).
    let until = Instant::now() + Duration::from_secs(5);
    loop {
        match Instance::acquire(&l) {
            Ok(_) => break,
            Err(e) => assert!(Instant::now() < until, "lock never freed: {e}"),
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn reload_keeps_the_previous_config_on_a_bad_file() {
    let dir = tempfile::tempdir().unwrap();
    let l = layout(dir.path());
    let path = write_registry(dir.path(), &script("job1", "10m", ""));
    let (reg, sha) = load_initial(&path).unwrap();
    let (clock, runner) = (FakeClock::new(T0 as f64), FakeRunner::new());
    let mut e = engine(dir.path(), &clock, &runner, reg);
    let mut reload = Reload::new(&path, sha);
    assert!(
        !reload.poll(&mut e, &clock),
        "unchanged file: nothing to do"
    );
    // A schema-failing edit: the previous config stays and one alert goes out.
    std::fs::write(&path, r#"{"version":1,"jobs":[{"id":"x"}]}"#).unwrap();
    assert!(!reload.poll(&mut e, &clock));
    assert_eq!(e.registry().jobs.len(), 1);
    assert_eq!(e.registry().jobs[0].id, "job1");
    assert!(
        !clock.sleeps().is_empty(),
        "a bad read is retried once after a pause"
    );
    let alerts = lines(&l.alerts());
    assert_eq!(alerts.len(), 1);
    assert!(alerts[0].contains("meta_miss") && alerts[0].contains("previous config kept"));
    // The same bad content does not alert again; half-written JSON is bad too.
    assert!(!reload.poll(&mut e, &clock));
    std::fs::write(&path, r#"{"version":1,"jobs":[{"id":"job"#).unwrap();
    assert!(!reload.poll(&mut e, &clock));
    assert_eq!(lines(&l.alerts()).len(), 2, "one per distinct bad content");
    // A good edit is picked up.
    let two = format!("{},{}", script("job1", "10m", ""), script("job2", "1h", ""));
    write_registry(dir.path(), &two);
    assert!(reload.poll(&mut e, &clock));
    assert_eq!(e.registry().jobs.len(), 2);
    // An empty file is bad as well, never an empty job list.
    std::fs::write(&path, "").unwrap();
    assert!(!reload.poll(&mut e, &clock));
    assert_eq!(e.registry().jobs.len(), 2);
}

#[test]
fn liveness_file_is_fresh_after_a_tick() {
    let dir = tempfile::tempdir().unwrap();
    let l = layout(dir.path());
    let path = write_registry(dir.path(), &script("job1", "10m", ""));
    let (reg, sha) = load_initial(&path).unwrap();
    let (clock, runner) = (FakeClock::new(T0 as f64), FakeRunner::new());
    let mut e = engine(dir.path(), &clock, &runner, reg);
    assert!(read_alive(&l).is_none());
    run_loop(
        &mut e,
        &mut Reload::new(&path, sha.clone()),
        &clock,
        &AtomicBool::new(false),
        Some(1),
    )
    .unwrap();
    let alive = read_alive(&l).expect("liveness file");
    assert_eq!(
        (alive.unix, alive.jobs, alive.mode.as_str()),
        (T0, 1, "shadow")
    );
    assert_eq!((alive.pid, alive.config_sha), (std::process::id(), sha));
    // Each tick rewrites it with the new time.
    run_loop(
        &mut e,
        &mut Reload::new(&path, "x".into()),
        &clock,
        &AtomicBool::new(false),
        Some(3),
    )
    .unwrap();
    assert!(
        read_alive(&l).unwrap().unix >= T0 + 10,
        "the loop slept between ticks"
    );
}

#[test]
fn run_loop_stops_on_the_flag_and_waits_for_children() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_registry(dir.path(), &script("job1", "1h", ""));
    let (_, sha) = load_initial(&path).unwrap();
    // The poll job is on in this copy, so the first tick starts a (held) run.
    let text = std::fs::read_to_string(&path)
        .unwrap()
        .replace(r#""poll":{"enabled":false}"#, r#""poll":{"every_s":60}"#);
    let reg = trusty_architect::sched::Registry::parse(&text, &path).unwrap();
    let (clock, runner) = (FakeClock::new(T0 as f64), FakeRunner::new());
    *runner.stdout_text.lock().unwrap() = Some("x".into());
    runner.hold.store(true, std::sync::atomic::Ordering::SeqCst);
    let mut e = engine(dir.path(), &clock, &runner, reg);
    let release = {
        let runner = runner.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            runner
                .hold
                .store(false, std::sync::atomic::Ordering::SeqCst);
        })
    };
    let started = Instant::now();
    run_loop(
        &mut e,
        &mut Reload::new(&path, sha),
        &clock,
        &AtomicBool::new(false),
        Some(1),
    )
    .unwrap();
    release.join().unwrap();
    assert!(
        started.elapsed() >= Duration::from_millis(250),
        "the loop waited for the held run"
    );
    assert_eq!(e.running_count(), 0);
    assert_eq!(
        lines(&layout(dir.path()).runs()).len(),
        1,
        "the finished run was recorded"
    );
    // A stop flag already set: no tick, nothing started.
    let runner2 = FakeRunner::new();
    let mut e2 = engine(dir.path(), &clock, &runner2, e.registry().clone());
    run_loop(
        &mut e2,
        &mut Reload::new(&path, "x".into()),
        &clock,
        &AtomicBool::new(true),
        None,
    )
    .unwrap();
    assert_eq!(runner2.count(), 0);
}

fn bin() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_trusty-architect-sched"));
    c.env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", "/nonexistent")
        .stdin(Stdio::null());
    c
}

#[test]
fn binary_fails_loudly_on_an_empty_or_missing_config_before_the_lock() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_str().unwrap();
    // Missing file.
    let out = bin().args(["daemon", "--root", root]).output().unwrap();
    assert_eq!(
        out.status.code(),
        Some(2),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    // Empty file.
    let path = dir.path().join("local/meta/registry.json");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, "").unwrap();
    for verb in ["daemon", "validate", "status"] {
        let out = bin().args([verb, "--root", root]).output().unwrap();
        assert_eq!(out.status.code(), Some(2), "{verb}");
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("empty"),
            "{verb}"
        );
    }
    assert!(
        !layout(dir.path()).lock().exists(),
        "no lock for a scheduler that never started"
    );
    // A schema-failing file: exit 1.
    std::fs::write(&path, r#"{"version":1,"jobs":[{"id":"x"}]}"#).unwrap();
    let out = bin().args(["validate", "--root", root]).output().unwrap();
    assert_eq!(out.status.code(), Some(1));
}

#[test]
fn daemon_binary_holds_the_lock_and_stops_on_sigterm() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_str().unwrap();
    write_registry(dir.path(), &script("job1", "1h", ""));
    let l = layout(dir.path());
    let mut first = bin()
        .args(["daemon", "--root", root])
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let until = Instant::now() + Duration::from_secs(20);
    while read_alive(&l).is_none() && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(50));
    }
    let alive = read_alive(&l).expect("the daemon wrote its liveness file");
    assert_eq!(alive.pid, first.id());
    // A second daemon on the same root exits with the lock error.
    let second = bin().args(["daemon", "--root", root]).output().unwrap();
    assert_eq!(second.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&second.stderr).contains("another scheduler is running"));
    // `status` sees the first one.
    let status = bin().args(["status", "--root", root]).output().unwrap();
    assert_eq!(
        status.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&status.stdout)
    );
    // SIGTERM stops it cleanly.
    Command::new("kill")
        .args(["-TERM", &first.id().to_string()])
        .status()
        .unwrap();
    let stop_by = Instant::now() + Duration::from_secs(20);
    let code = loop {
        if let Some(s) = first.try_wait().unwrap() {
            break s.code();
        }
        assert!(Instant::now() < stop_by, "the daemon ignored SIGTERM");
        std::thread::sleep(Duration::from_millis(50));
    };
    assert_eq!(code, Some(0));
    Instance::acquire(&l).expect("the lock is free after the stop");
}
