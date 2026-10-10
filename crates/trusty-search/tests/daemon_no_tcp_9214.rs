//! The daemon serves its Unix socket only, and the retired `start` inputs are
//! warned about and ignored (#9214 PR-A, ADR-0032).
//!
//! Why: the last #9214 phase removes the daemon's `:7878` bind. A daemon that
//! still bound a TCP port, or still announced one in `http_addr` /
//! `daemon.port`, would keep every pre-socket client working by accident and
//! hide the cut-over. Ruling D2 keeps `--port`, `--no-http` and
//! `TRUSTY_SEARCH_NO_HTTP` accepted for one release; each must say it is
//! ignored and change nothing else.
//! What: `daemon_binds_no_tcp_listener` boots the real binary with
//! `--port <free port>` on a scratch data dir and asserts nothing listens on
//! that port once the socket serves. `retired_flags_warn_and_change_nothing`
//! runs `start` against a lockfile naming PID 1, so it exits at the
//! already-running check before booting, with and without the retired
//! inputs.
//! Test: `cargo test -p trusty-search --test integration daemon_no_tcp_9214::`.

use std::net::{SocketAddr, TcpStream};
use std::path::Path;
use std::time::{Duration, Instant};

use crate::test_daemon::{self, DaemonGuard};

/// How long a daemon gets to boot far enough to serve its socket.
const BOOT_TIMEOUT: Duration = Duration::from_secs(90);

/// A loopback port nothing is bound to right now.
fn free_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind an ephemeral port");
    listener.local_addr().expect("local addr").port()
}

/// True once something accepts a connection on `data_dir`'s daemon socket.
fn socket_serving(data_dir: &Path) -> bool {
    std::os::unix::net::UnixStream::connect(data_dir.join("trusty-search.sock")).is_ok()
}

/// Why (#9214): the daemon must bind no TCP port, even when told one.
/// What: boots the real daemon with `--port <p>` for a free `p`; once its
/// socket serves, `127.0.0.1:<p>` must refuse a connection, no `http_addr`
/// or `daemon.port` file may exist, and the daemon's stderr must carry the
/// `--port` warning. Pre-#9214 the daemon bound `p` and wrote both files.
/// Test: this function.
#[test]
fn daemon_binds_no_tcp_listener() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let data_dir = tmp.path();
    let port = free_port();
    let guard = DaemonGuard::spawn_with(data_dir, &["--port", &port.to_string()]);

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

    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let dialled = TcpStream::connect_timeout(&addr, Duration::from_secs(2)).is_ok();
    let http_addr = data_dir.join("http_addr").exists();
    let port_file = data_dir.join("daemon.port").exists();
    let stderr = std::fs::read_to_string(data_dir.join("daemon.stderr.log")).unwrap_or_default();
    drop(guard);

    assert!(
        !dialled,
        "#9214: the daemon bound 127.0.0.1:{port}; it must serve its socket only"
    );
    assert!(!http_addr, "#9214: the daemon wrote an http_addr file");
    assert!(!port_file, "#9214: the daemon wrote a daemon.port file");
    assert!(
        stderr.contains(&format!("--port {port} is ignored")),
        "the ignored --port must be named on stderr:\n{stderr}"
    );
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
        .env("TRUSTY_SKIP_RAM_CHECK", "1")
        .env_remove("TRUSTY_SEARCH_NO_HTTP");
    for (key, value) in env {
        cmd.env(key, value);
    }
    let out = test_daemon::run_bounded(&mut cmd, Duration::from_secs(120));
    (out.code, out.combined)
}

/// Why (#9214, ruling D2): a unit or script that still passes a retired
/// input must start, and must be told the input does nothing now.
/// What: one run with no retired input and one with `--port`, `--no-http`
/// and `TRUSTY_SEARCH_NO_HTTP=1`. Both exit the same way at the
/// already-running check; only the second prints, and prints all three
/// warnings. Pre-#9214 all three were accepted silently.
/// Test: this function.
#[test]
fn retired_flags_warn_and_change_nothing() {
    let plain = tempfile::tempdir().expect("tempdir");
    let (plain_code, plain_out) = run_start(plain.path(), &[], &[]);
    let retired = tempfile::tempdir().expect("tempdir");
    let (code, out) = run_start(
        retired.path(),
        &["--port", "17997", "--no-http"],
        &[("TRUSTY_SEARCH_NO_HTTP", "1")],
    );

    assert_eq!(
        plain_code,
        Some(1),
        "precondition: the lockfile fast path refuses:\n{plain_out}"
    );
    assert_eq!(
        code, plain_code,
        "a retired input must not change how start exits:\n{out}"
    );
    assert!(
        !plain_out.contains("is ignored"),
        "no retired input, no warning:\n{plain_out}"
    );
    for needle in [
        "--port 17997 is ignored",
        "--no-http is ignored",
        "TRUSTY_SEARCH_NO_HTTP is ignored",
    ] {
        assert!(out.contains(needle), "missing `{needle}` in:\n{out}");
    }
}
