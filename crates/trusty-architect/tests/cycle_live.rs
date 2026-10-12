//! #9171 A3b-2 live check: one poll cycle against a scratch tmux server.
//!
//! Why: the scripted tests prove the argv; this proves real tmux accepts the
//! exact window target (`rename-window -t =S:IDX`) and that a cycle with no
//! Architect pane sends no keys. It never reaches the fleet: it runs only
//! under `--include-ignored`, on its own `/tmp` socket (`tmux -S`, started
//! with `-f /dev/null`), with `TMUX` removed, and kills that server on drop.
//! The `tm-` session lives only on that server; a `tm-` name is needed for
//! the poller to watch it.
//! What: session `tm-arch-scratch` runs `sleep` in a window tmux names
//! itself; `scratch-other` is not watched and its window is named `keepme`.
//! One `run_once` must rename only the `tm-` window, capture its pane (no
//! box, so no alert), and report `skipped: no tm-architect pane`; no keys
//! can go out (`SystemTmux::run` refuses `send-keys`).

mod cycle_support;

use std::fs;
use std::io;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::time::{Duration, Instant};

use cycle_support::{Env, QuietHost, TestResult};
use trusty_architect::poller::{PollSession, PollState, Runner, WakeAll};
use trusty_architect::{FakeClock, SystemTmux};

/// The scratch server; dropping it kills the server and removes its socket.
struct Scratch {
    socket: PathBuf,
}

impl Scratch {
    fn tmux(&self, args: &[&str]) -> io::Result<Output> {
        Command::new("tmux")
            .arg("-f")
            .arg("/dev/null")
            .arg("-S")
            .arg(&self.socket)
            .args(args)
            .env_remove("TMUX")
            .output()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = self.tmux(&["kill-server"]);
        let _ = fs::remove_file(&self.socket);
    }
}

fn windows(scratch: &Scratch) -> io::Result<String> {
    let out = scratch.tmux(&[
        "list-windows",
        "-a",
        "-F",
        "#{session_name}\t#{window_index}\t#{window_name}",
    ])?;
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

#[test]
#[ignore = "live: starts a scratch tmux server on a /tmp socket; run with --include-ignored"]
fn live_cycle_renames_exactly_and_sends_nothing() -> TestResult {
    let env = Env::new(&[])?;
    let scratch = Scratch {
        socket: PathBuf::from(format!("/tmp/a3b2-{}.sock", std::process::id())),
    };
    let starts: [&[&str]; 2] = [
        &["new-session", "-d", "-s", "tm-arch-scratch", "sleep 600"],
        &[
            "new-session",
            "-d",
            "-s",
            "scratch-other",
            "-n",
            "keepme",
            "sleep 600",
        ],
    ];
    for args in starts {
        let started = scratch.tmux(args)?;
        assert!(started.status.success(), "{started:?}");
    }
    let start = Instant::now();
    let mut before = windows(&scratch)?;
    while before.lines().count() < 2 && start.elapsed() < Duration::from_secs(5) {
        std::thread::sleep(Duration::from_millis(50));
        before = windows(&scratch)?;
    }
    println!("--- windows before ---\n{before}");
    assert_eq!(before.lines().count(), 2, "{before}");

    let tmux = SystemTmux::with_socket(&scratch.socket);
    let clock = FakeClock::new(1_000.0);
    let runner = Runner {
        tmux: &tmux,
        clock: &clock,
        probe: &QuietHost,
        triage: &WakeAll,
        config: &env.config,
        dry_run: false,
    };
    let mut session = PollSession::new(PollState::default());
    let summary = runner.run_once(&mut session)?;
    let after = windows(&scratch)?;
    println!(
        "--- windows after ---\n{after}\n--- poll.log ---\n{}",
        env.log()
    );

    assert_eq!(
        summary.wake.as_deref(),
        Some("skipped: no tm-architect pane")
    );
    assert!(
        after
            .lines()
            .any(|l| l.starts_with("tm-arch-scratch\t") && l.ends_with("\ttm-arch-scratch")),
        "{after}"
    );
    assert!(
        after.lines().any(|l| l == "scratch-other\t0\tkeepme"),
        "a non-tm window is left alone: {after}"
    );
    assert!(env.written("tm-arch-scratch")?.is_empty());
    assert!(
        env.log().contains("window-rename tm-arch-scratch:"),
        "{}",
        env.log()
    );

    let socket = scratch.socket.clone();
    drop(scratch);
    assert!(!socket.exists(), "the scratch server's socket is gone");
    Ok(())
}
