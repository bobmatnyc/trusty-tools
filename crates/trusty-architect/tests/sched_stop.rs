//! Stop behaviour and the fail-closed lock and state paths (MB1595 fix round).

mod sched_support;

use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use sched_support::*;
use trusty_architect::FakeClock;
use trusty_architect::sched::cli::{Deps, execute};
use trusty_architect::sched::daemon::{Instance, Reload, load_initial, run_loop_with};
use trusty_architect::sched::state::SchedState;
use trusty_architect::sched::{Engine, SchedError, SystemRunner};

#[test]
fn stopping_kills_children_still_running_after_the_wait() {
    let dir = tempfile::tempdir().unwrap();
    let pidfile = dir.path().join("child.pid");
    let argv = format!(
        r#"["sh","-c","echo $$ > {}; exec sleep 300"]"#,
        pidfile.display()
    );
    let text = format!(
        r#"{{"version":1,"poll":{{"every_s":60,"shadow_argv":{argv}}},"jobs":[{}]}}"#,
        script("job1", "1h", "")
    );
    let path = dir.path().join("local/meta/registry.json");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, &text).unwrap();
    let (reg, sha) = load_initial(&path).unwrap();
    let clock = FakeClock::new(T0 as f64);
    let env = vec![("PATH".to_owned(), "/bin:/usr/bin".to_owned())];
    let mut e = Engine::new(
        layout(dir.path()),
        &clock,
        Arc::new(SystemRunner),
        env,
        reg,
        SchedState::default(),
        dir.path().join("scratch"),
    );
    let started = Instant::now();
    // One tick starts the poll job (a 300 s sleep); the stop wait is 1 s.
    run_loop_with(
        &mut e,
        &mut Reload::new(&path, sha),
        &clock,
        &AtomicBool::new(false),
        Some(1),
        Duration::from_secs(1),
    )
    .unwrap();
    assert!(
        started.elapsed() < Duration::from_secs(30),
        "stop is bounded"
    );
    let pid = std::fs::read_to_string(&pidfile).unwrap().trim().to_owned();
    let alive = || {
        Command::new("kill")
            .args(["-0", &pid])
            .output()
            .unwrap()
            .status
            .success()
    };
    let until = Instant::now() + Duration::from_secs(5);
    while alive() && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(50));
    }
    if alive() {
        // Do not leak the child of a failing test.
        let _ = Command::new("kill").args(["-KILL", &pid]).status();
        panic!("the child outlived the scheduler's stop");
    }
    assert_eq!(e.running_count(), 0);
}

#[test]
fn run_once_refuses_while_another_scheduler_holds_the_lock() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("local/meta/registry.json");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(
        &path,
        format!(r#"{{"version":1,"jobs":[{}]}}"#, script("job1", "1m", "")),
    )
    .unwrap();
    let (clock, runner) = (FakeClock::new(T0 as f64), FakeRunner::new());
    let held = Instance::acquire(&layout(dir.path())).unwrap();
    let deps = Deps {
        clock: &clock,
        runner: runner.clone(),
        ops: None,
        env: vec![],
        home: dir.path().to_owned(),
        cwd: dir.path().to_owned(),
        scratch: dir.path().join("scratch"),
    };
    let argv: Vec<String> = ["run", "--once", "--root", dir.path().to_str().unwrap()]
        .map(str::to_owned)
        .to_vec();
    let e = execute(&argv, &deps).unwrap_err();
    assert!(matches!(e, SchedError::Locked(_)), "{e}");
    assert_eq!(runner.count(), 0, "nothing ran beside the daemon");
    assert!(!dir.path().join("local/meta/sched-state.json").exists());
    drop(held);
}

#[test]
fn empty_state_file_is_an_error_not_a_reset() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sched-state.json");
    for blank in ["", "  \n"] {
        std::fs::write(&path, blank).unwrap();
        let e = SchedState::load(&path).unwrap_err();
        assert!(matches!(e, SchedError::Parse { .. }), "{e}");
    }
}
