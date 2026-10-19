//! A sandbox daemon reaches only its own tmux server (#9557).
//!
//! Why: `tm daemon --sandbox` refused `TMUX_TMPDIR` and its host-state gate
//! refused every tmux spawn under a scratch `$HOME`, so tmux-dependent fixes
//! could not be live-checked in isolation. Lifting the gate alone would have
//! put the sandbox on the operator's default socket.
//! What: starts the real binary the way `scripts/sandbox_daemon.sh` does, plus
//! `TMUX_TMPDIR=<data>/tmux`, with a fake `tmux` first on `PATH` that logs the
//! socket-selecting environment and argv of every call. The daemon's boot
//! reconcile must reach tmux, and every call must name the sandbox directory,
//! carry no `$TMUX` and no `-L`/`-S`, so none can reach the default socket.
//! Test: this file IS the test module.

use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, Instant};

use super::common;

/// How long the daemon may take to make its first tmux call.
const BOUND: Duration = Duration::from_secs(60);

/// Write the logging fake `tmux` into `bin_dir`.
fn write_fake_tmux(bin_dir: &Path, log: &Path) {
    use std::io::Write as _;
    let script = format!(
        "#!/bin/sh\necho \"TMUX_TMPDIR=${{TMUX_TMPDIR-unset}} TMUX=${{TMUX-unset}} ARGS=$*\" >> '{}'\n\
         case \"$1\" in\n  show-options) case \"$4\" in history-limit) echo 10000 ;; *) echo off ;; esac ;;\nesac\nexit 0\n",
        log.display()
    );
    let mut f = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o755)
        .open(bin_dir.join("tmux"))
        .expect("create fake tmux");
    f.write_all(script.as_bytes()).expect("write fake tmux");
}

/// Why (#9557): the acceptance — with `TRUSTY_SANDBOX=1` every tmux call
/// targets the sandbox socket and none reaches the default one.
/// Test: this test.
#[test]
fn a_sandbox_daemon_reaches_only_its_own_tmux_server() {
    let root = tempfile::tempdir().expect("scratch root");
    let home = root.path().join("home");
    let data = root.path().join("data");
    let tmux_dir = data.join("tmux");
    let bin_dir = root.path().join("bin");
    std::fs::create_dir_all(&home).expect("home");
    std::fs::create_dir_all(&bin_dir).expect("bin");
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&tmux_dir)
        .expect("tmux dir");
    let log = root.path().join("tmux-calls.log");
    write_fake_tmux(&bin_dir, &log);

    let mut child = common::tm_command_in(&home)
        .current_dir(&home)
        .env_clear()
        .env("HOME", &home)
        .env(
            "PATH",
            format!("{}:/usr/bin:/bin:/usr/sbin:/sbin", bin_dir.display()),
        )
        .env("TRUSTY_DATA_DIR_OVERRIDE", &data)
        .env("TRUSTY_SANDBOX", "1")
        .env("TMUX_TMPDIR", &tmux_dir)
        .args(["daemon", "--sandbox"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn tm daemon --sandbox");

    let started = Instant::now();
    let exited = loop {
        if std::fs::metadata(&log).is_ok_and(|m| m.len() > 0) {
            // Let the boot reconcile finish its burst of calls.
            std::thread::sleep(Duration::from_secs(2));
            break None;
        }
        if let Some(status) = child.try_wait().expect("try_wait") {
            break Some(status);
        }
        if started.elapsed() > BOUND {
            break None;
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    let _ = child.kill();
    let output = child.wait_with_output().expect("reap the daemon");
    let stderr = String::from_utf8_lossy(&output.stderr);

    let calls = std::fs::read_to_string(&log).unwrap_or_default();
    assert!(
        !calls.is_empty(),
        "the sandbox daemon never reached tmux (exited: {exited:?})\nstderr: {stderr}"
    );
    let want = format!("TMUX_TMPDIR={} TMUX=unset ARGS=", tmux_dir.display());
    for line in calls.lines() {
        assert!(
            line.starts_with(&want),
            "a tmux call outside the sandbox socket dir: {line}"
        );
        let args = line.split_once("ARGS=").map_or("", |(_, a)| a);
        assert!(
            !args.split_whitespace().any(|w| w == "-L" || w == "-S"),
            "a tmux call naming another socket: {line}"
        );
    }
}

/// Why (#9557): with `TRUSTY_SANDBOX=1` but no sandbox-owned `TMUX_TMPDIR`,
/// tmux must be refused outright, never fall back to the default socket.
/// Test: this test.
#[test]
fn a_sandbox_daemon_without_its_socket_dir_spawns_no_tmux() {
    let root = tempfile::tempdir().expect("scratch root");
    let home = root.path().join("home");
    let bin_dir = root.path().join("bin");
    std::fs::create_dir_all(&home).expect("home");
    std::fs::create_dir_all(&bin_dir).expect("bin");
    let log = root.path().join("tmux-calls.log");
    write_fake_tmux(&bin_dir, &log);

    let mut child = common::tm_command_in(&home)
        .current_dir(&home)
        .env_clear()
        .env("HOME", &home)
        .env(
            "PATH",
            format!("{}:/usr/bin:/bin:/usr/sbin:/sbin", bin_dir.display()),
        )
        .env("TRUSTY_DATA_DIR_OVERRIDE", root.path().join("data"))
        .env("TRUSTY_SANDBOX", "1")
        .args(["daemon", "--sandbox"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn tm daemon --sandbox");
    // Long enough for the boot reconcile that the first test observes.
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(10) {
        if child.try_wait().expect("try_wait").is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let _ = child.kill();
    let _ = child.wait();

    let calls = std::fs::read_to_string(&log).unwrap_or_default();
    assert!(
        calls.is_empty(),
        "tmux reached without a sandbox socket dir: {calls}"
    );
}
