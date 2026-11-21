//! #9779: `tm architect serve` over the real poll [`Runner`] cycle.
//!
//! Every path is a tempdir Architect root; tmux is scripted.

mod cycle_support;

use std::fs;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use cycle_support::{Env, QuietHost, TestResult};
use trusty_architect::poller::{PollLock, RunError};
use trusty_architect::serve::{FileWatch, ServeLoop, ServeOptions, WatchRoot, serve_runner};
use trusty_architect::{FakeClock, ScriptedTmux};

fn empty_fleet() -> ScriptedTmux {
    ScriptedTmux::new()
        .fallback("list-panes", "")
        .fallback("list-windows", "")
}

/// The inbox and transcript watch of `env`, the transcripts made a file so
/// that root cannot be read.
fn broken_watch(env: &Env) -> TestResult<FileWatch> {
    fs::write(&env.config.transcript_base, "not a dir")?;
    Ok(FileWatch::new(vec![
        WatchRoot::inbox(&env.config.inbox, vec![env.config.shadow_dir()]),
        WatchRoot::transcripts(&env.config.transcript_base),
    ]))
}

fn once() -> ServeOptions {
    ServeOptions {
        once: true,
        ..ServeOptions::default()
    }
}

/// A watcher that fails to start is a `serve` alert in `alerts.jsonl`
/// (fingerprint `watch-transcripts`), raised by the start cycle.
#[test]
fn watcher_start_failure_is_a_serve_alert() -> TestResult {
    let env = Env::new(&[])?;
    let tmux = empty_fleet();
    let clock = FakeClock::new(1_000.0);
    let runner = env.runner(&tmux, &clock, &QuietHost);
    let stop = AtomicBool::new(false);
    let log = |_: &str| {};
    let mut lp = ServeLoop {
        opts: once(),
        stop: &stop,
        files: Some(broken_watch(&env)?),
        tmux: None,
        events: None,
        log: &log,
    };
    serve_runner(&runner, &mut lp)?;
    let keys = env.keys()?;
    assert!(
        keys.iter()
            .any(|k| k.contains("\"serve\"") && k.contains("\"watch-transcripts\"")),
        "{keys:?}"
    );
    Ok(())
}

/// A held `poll.lock` stops a second instance before any cycle.
#[test]
fn lock_contention_second_instance_does_not_cycle() -> TestResult {
    let env = Env::new(&[])?;
    let _held = PollLock::acquire(&env.config.lock())?;
    let tmux = empty_fleet();
    let clock = FakeClock::new(1_000.0);
    let runner = env.runner(&tmux, &clock, &QuietHost);
    let stop = AtomicBool::new(false);
    let log = |_: &str| {};
    let mut lp = ServeLoop {
        opts: once(),
        stop: &stop,
        files: None,
        tmux: None,
        events: None,
        log: &log,
    };
    let r = serve_runner(&runner, &mut lp);
    assert!(matches!(r, Err(RunError::AlreadyRunning { .. })), "{r:?}");
    assert!(tmux.calls().is_empty(), "{:?}", tmux.calls());
    assert!(!env.config.state().exists());
    Ok(())
}

/// A shadow run writes only `<inbox>/shadow/`, takes no lock and sends no
/// keys, even with an alert to write.
#[test]
fn shadow_serve_writes_only_the_shadow_dir() -> TestResult {
    let mut env = Env::new(&[])?;
    env.config.shadow = true;
    let watch = broken_watch(&env)?;
    let tmux = empty_fleet();
    let clock = FakeClock::new(1_000.0);
    let runner = env.runner(&tmux, &clock, &QuietHost);
    let stop = AtomicBool::new(false);
    let log = |_: &str| {};
    let mut lp = ServeLoop {
        opts: once(),
        stop: &stop,
        files: Some(watch),
        tmux: None,
        events: None,
        log: &log,
    };
    serve_runner(&runner, &mut lp)?;
    let mut inbox: Vec<String> = fs::read_dir(&env.config.inbox)?
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    inbox.sort();
    assert_eq!(inbox, ["events.jsonl", "shadow"]);
    let shadow = env.config.shadow_dir();
    assert!(
        fs::read_to_string(shadow.join("alerts.jsonl"))?.contains("watch-transcripts"),
        "the alert lands in the shadow dir"
    );
    assert!(tmux.sends().is_empty());
    Ok(())
}

/// The tick runs the poll cycle (its `panes=` summary) with no event, and
/// the stop flag ends the service.
#[test]
fn tick_runs_the_poll_cycle() -> TestResult {
    let env = Env::new(&[])?;
    let tmux = empty_fleet();
    let clock = FakeClock::new(1_000.0);
    let runner = env.runner(&tmux, &clock, &QuietHost);
    let stop = AtomicBool::new(false);
    let log = |_: &str| {};
    let mut lp = ServeLoop {
        opts: ServeOptions {
            tick: Duration::from_millis(100),
            ..ServeOptions::default()
        },
        stop: &stop,
        files: None,
        tmux: None,
        events: None,
        log: &log,
    };
    let stats = std::thread::scope(|s| {
        s.spawn(|| {
            std::thread::sleep(Duration::from_millis(450));
            stop.store(true, Ordering::SeqCst);
        });
        serve_runner(&runner, &mut lp)
    })?;
    assert!(stats.ticks >= 2, "{stats:?}");
    let cycles = env.log().matches("panes=0").count() as u64;
    assert_eq!(cycles, stats.start + stats.ticks, "{}", env.log());
    Ok(())
}
