//! The daemon serves its Unix socket only (#9214 PR-A, ADR-0032), and the
//! `start` inputs it used to ignore are gone (#9536).
//!
//! Why: the last #9214 phase removes the daemon's `:7878` bind. A daemon that
//! still announced a TCP port in `http_addr` / `daemon.port` would keep every
//! pre-socket client working by accident and hide the cut-over. Ruling D2 kept
//! `--port`, `--no-http` and `TRUSTY_SEARCH_NO_HTTP` for one release; #9536
//! makes clap reject both flags and stops reading the variable.
//! What: `daemon_binds_no_tcp_listener` boots the real binary on a scratch
//! data dir and asserts it announces no TCP port once the socket serves.
//! `removed_start_inputs_are_refused_or_unread` runs `start` against a
//! lockfile naming PID 1, so a run that parses exits at the already-running
//! check before booting.
//! Test: `cargo test -p trusty-search --test integration daemon_no_tcp_9214::`.

use std::path::Path;
use std::time::{Duration, Instant};

use crate::test_daemon::{self, DaemonGuard};

/// How long a daemon gets to boot far enough to serve its socket.
const BOOT_TIMEOUT: Duration = Duration::from_secs(90);

/// True once something accepts a connection on `data_dir`'s daemon socket.
fn socket_serving(data_dir: &Path) -> bool {
    std::os::unix::net::UnixStream::connect(data_dir.join("trusty-search.sock")).is_ok()
}

/// Why (#9214): the daemon must bind and announce no TCP port.
/// What: boots the real daemon; once its socket serves, no `http_addr` or
/// `daemon.port` file may exist. Pre-#9214 the daemon wrote both. #9536: no
/// `--port` is passed — `start` rejects it now, so there is no port to dial.
/// Test: this function.
#[test]
fn daemon_binds_no_tcp_listener() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let data_dir = tmp.path();
    let guard = DaemonGuard::spawn(data_dir);

    let deadline = Instant::now() + BOOT_TIMEOUT;
    while !socket_serving(data_dir) {
        assert!(
            Instant::now() < deadline,
            "daemon {} never served its socket; stderr:\n{}",
            guard.pid(),
            std::fs::read_to_string(data_dir.join("daemon.stderr.log")).unwrap_or_default()
        );
        std::thread::sleep(Duration::from_millis(100));
    }

    let http_addr = data_dir.join("http_addr").exists();
    let port_file = data_dir.join("daemon.port").exists();
    drop(guard);

    assert!(!http_addr, "#9214: the daemon wrote an http_addr file");
    assert!(!port_file, "#9214: the daemon wrote a daemon.port file");
}

/// Run `start --foreground` on `data_dir` with `args` and `env`, and return
/// `(exit code, combined output)`.
///
/// Why: `daemon.lock` names PID 1, which is always alive, so `start` exits 1
/// at the already-running check before it binds or opens anything — the same
/// fast path `daemon_env_precedence.rs` uses.
fn run_start(data_dir: &Path, args: &[&str], env: &[(&str, &str)]) -> (Option<i32>, String) {
    let home = data_dir.join("home");
    std::fs::create_dir_all(&home).expect("create the fake HOME");
    std::fs::write(data_dir.join("daemon.lock"), b"1").expect("write daemon.lock");
    let mut cmd = test_daemon::command();
    cmd.args(["start", "--foreground"])
        .args(args)
        .current_dir(data_dir)
        .env("TRUSTY_DATA_DIR", data_dir)
        .env("TRUSTY_DATA_DIR_OVERRIDE", data_dir)
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", &home)
        .env("TRUSTY_SKIP_RAM_CHECK", "1");
    for (key, value) in env {
        cmd.env(key, value);
    }
    let out = test_daemon::run_bounded(&mut cmd, Duration::from_secs(120));
    (out.code, out.combined)
}

/// Why (#9536): `--port` and `--no-http` did nothing since #9214. A unit or
/// script that still passes one must fail at parse time, before the daemon
/// takes its lock, instead of starting with a setting that is silently
/// dropped. `TRUSTY_SEARCH_NO_HTTP` is no longer read at all.
/// What: a plain run exits 1 at the already-running check (precondition).
/// Each removed flag exits 2 with clap's unexpected-argument error naming it,
/// and never reaches that check. `TRUSTY_SEARCH_NO_HTTP=1` exits like the
/// plain run and is named nowhere in the output. Before #9536 all three
/// exited 1 with an "is ignored" warning.
/// Test: this function.
#[test]
fn removed_start_inputs_are_refused_or_unread() {
    let plain = tempfile::tempdir().expect("tempdir");
    let (plain_code, plain_out) = run_start(plain.path(), &[], &[]);
    assert_eq!(
        plain_code,
        Some(1),
        "precondition: the lockfile fast path refuses:\n{plain_out}"
    );

    for (args, flag) in [
        (&["--port", "17997"][..], "--port"),
        (&["--no-http"][..], "--no-http"),
    ] {
        let dir = tempfile::tempdir().expect("tempdir");
        let (code, out) = run_start(dir.path(), args, &[]);
        assert_eq!(
            code,
            Some(2),
            "#9536: `start {args:?}` must be a usage error:\n{out}"
        );
        assert!(
            out.contains(&format!("unexpected argument '{flag}")),
            "clap must name `{flag}`:\n{out}"
        );
        assert!(
            !out.contains("already running"),
            "a removed flag must stop start before the lock check:\n{out}"
        );
    }

    let env = tempfile::tempdir().expect("tempdir");
    let (code, out) = run_start(env.path(), &[], &[("TRUSTY_SEARCH_NO_HTTP", "1")]);
    assert_eq!(code, plain_code, "the variable must change nothing:\n{out}");
    assert!(
        !out.contains("TRUSTY_SEARCH_NO_HTTP"),
        "#9536: the variable is not read, so nothing names it:\n{out}"
    );
}
