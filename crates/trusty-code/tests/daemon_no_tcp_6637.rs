//! The real `tcode` daemon serves its Unix socket only and holds no TCP
//! listener, on every command line that starts one (#6637 slice 2c).
//!
//! Why: until 2c the daemon bound a loopback HTTP/SSE listener beside its
//! socket whenever `--http` was passed, and `tcode tui` passed it on every
//! autospawn — so every daemon an operator never started by hand held a TCP
//! port. `scripts/check_no_tcp_listeners.sh` proves no bind call is left in
//! source; these tests prove the BUILT binary listens on nothing, which is what
//! an operator and `tm doctor`'s `tcp_listeners` row observe.
//! What: each case starts the real binary with a cleared environment and a
//! scratch data directory (`support::tcode_command`), waits until `health`
//! answers on the socket, then inspects the live process: its TCP LISTEN
//! sockets, the socket and directory modes, its exit status.
//! Test: this file IS the test module; run with
//! `cargo test -p trusty-code --test integration daemon_no_tcp_6637::`.

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::support;

/// How long a daemon may take to answer `health` on its socket, or to exit.
const BOUND: Duration = Duration::from_secs(60);

/// One daemon's scratch tree: a data directory and a stderr log.
struct Sandbox {
    root: tempfile::TempDir,
}

impl Sandbox {
    fn new() -> Self {
        // Under `/tmp`, not `$TMPDIR`: the per-project socket path must fit
        // `sun_path` (108 bytes on Linux), and a CI or macOS `$TMPDIR` is long.
        let root = tempfile::Builder::new()
            .prefix("tc6637-")
            .tempdir_in("/tmp")
            .expect("scratch root");
        std::fs::create_dir(root.path().join("data")).expect("data dir");
        Self { root }
    }

    /// The `TRUSTY_DATA_DIR_OVERRIDE` every process in this sandbox shares.
    fn data(&self) -> PathBuf {
        self.root.path().join("data")
    }

    /// The shared socket `tcode serve` binds with no `--project-socket`.
    fn socket(&self) -> PathBuf {
        self.data().join("trusty-code").join("trusty-code.sock")
    }

    fn log(&self, name: &str) -> PathBuf {
        self.root.path().join(format!("{name}.stderr"))
    }

    /// `tcode <args…>` in this sandbox, stderr captured to `log`.
    fn spawn<S: AsRef<std::ffi::OsStr>>(&self, args: &[S], log: &Path) -> Daemon {
        let stderr = std::fs::File::create(log).expect("stderr log");
        let mut cmd = support::tcode_command();
        let child = cmd
            .args(args)
            .env("TRUSTY_DATA_DIR_OVERRIDE", self.data())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::from(stderr))
            .spawn()
            .expect("spawn tcode");
        Daemon { child, _cmd: cmd }
    }
}

/// A child killed when this value drops, pass or panic.
struct Daemon {
    child: Child,
    /// #9139: the isolation tree must outlive the child.
    _cmd: support::TcodeCommand,
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

/// `health` over `socket`, or `None` when nothing answers there yet.
async fn health(socket: &Path) -> Option<Value> {
    trusty_code::tui_client::uds_rpc::UdsRpcClient::new(socket)
        .call("health", json!({}))
        .await
        .ok()
}

/// Wait until `health` answers on `socket`; panic with the daemon's stderr
/// if it exits first or the bound passes.
async fn wait_healthy(daemon: &mut Daemon, socket: &Path, log: &Path) -> Value {
    let started = Instant::now();
    loop {
        if let Some(payload) = health(socket).await {
            return payload;
        }
        if let Some(status) = daemon.child.try_wait().expect("try_wait") {
            panic!(
                "the daemon exited ({status}) before answering on {}\nstderr:\n{}",
                socket.display(),
                read(log)
            );
        }
        assert!(
            started.elapsed() < BOUND,
            "no health answer on {} within {BOUND:?}\nstderr:\n{}",
            socket.display(),
            read(log)
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Wait for `daemon` to exit on its own, panicking past the bound.
fn wait_exit(daemon: &mut Daemon, log: &Path) -> std::process::ExitStatus {
    let started = Instant::now();
    loop {
        if let Some(status) = daemon.child.try_wait().expect("try_wait") {
            return status;
        }
        assert!(
            started.elapsed() < BOUND,
            "the process kept running past {BOUND:?}\nstderr:\n{}",
            read(log)
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Every TCP socket `pid` holds in LISTEN state, one line each.
///
/// macOS: `lsof`. A missing `lsof` panics, and a non-empty stderr fails the
/// test, because a failing `lsof` prints nothing on stdout either and an empty
/// listing is only evidence when the tool ran cleanly. Linux: the process's
/// socket inodes matched against the LISTEN (`0A`) rows of its
/// `/proc/<pid>/net/tcp{,6}` tables, so a CI runner without `lsof` still
/// checks — the same split `trusty-mpm`'s `daemon_no_tcp_6288_s3` uses.
#[cfg(target_os = "macos")]
fn tcp_listeners(pid: u32) -> Vec<String> {
    let out = std::process::Command::new("/usr/sbin/lsof")
        .args(["-nP", "-a", "-p", &pid.to_string(), "-iTCP", "-sTCP:LISTEN"])
        .output()
        .expect("lsof must run on macOS");
    assert!(
        out.stderr.is_empty(),
        "lsof failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .skip(1) // the header
        .map(str::to_owned)
        .collect()
}

#[cfg(target_os = "linux")]
fn tcp_listeners(pid: u32) -> Vec<String> {
    let mut inodes = std::collections::HashSet::new();
    let fd_dir = format!("/proc/{pid}/fd");
    for entry in std::fs::read_dir(&fd_dir).expect("read the daemon's fd table") {
        let Ok(target) = std::fs::read_link(entry.expect("fd entry").path()) else {
            continue;
        };
        let target = target.to_string_lossy().into_owned();
        if let Some(inode) = target
            .strip_prefix("socket:[")
            .and_then(|s| s.strip_suffix(']'))
        {
            inodes.insert(inode.to_owned());
        }
    }
    let mut listening = Vec::new();
    for table in ["tcp", "tcp6"] {
        let text = std::fs::read_to_string(format!("/proc/{pid}/net/{table}")).unwrap_or_default();
        for row in text.lines().skip(1) {
            let cols: Vec<&str> = row.split_whitespace().collect();
            // local_address is col 1, st col 3, inode col 9.
            if cols.len() > 9 && cols[3] == "0A" && inodes.contains(cols[9]) {
                listening.push(format!("{table} {}", cols[1]));
            }
        }
    }
    listening
}

fn mode_of(path: &Path) -> u32 {
    std::fs::metadata(path)
        .unwrap_or_else(|e| panic!("stat {}: {e}", path.display()))
        .permissions()
        .mode()
        & 0o777
}

/// Assert `pid` holds no TCP LISTEN socket, naming what it found otherwise.
fn assert_no_tcp_listener(pid: u32, what: &str, log: &Path) {
    let listeners = tcp_listeners(pid);
    assert!(
        listeners.is_empty(),
        "{what}: the daemon (pid {pid}) must hold no TCP LISTEN socket, found:\n{}\nstderr:\n{}",
        listeners.join("\n"),
        read(log)
    );
}

/// Criteria 1 and 2: every `tcode serve` command line binds its socket at
/// `0600` inside a `0700` directory and listens on no TCP port.
///
/// Why each line: bare `serve` is the operator's spelling, `--project` the
/// bound one, and `--project-socket` the one `tcode tui` uses beside another
/// project's daemon (#4600). A SIGTERM must stop each cleanly.
/// Test: this function IS the test.
#[tokio::test]
async fn every_serve_command_line_binds_the_socket_and_no_tcp_listener() {
    let project = tempfile::tempdir().expect("project");
    let root = project.path().canonicalize().expect("canonicalize");
    let root_arg = root.as_os_str().to_owned();
    let cases: [(&str, Vec<std::ffi::OsString>); 3] = [
        ("serve", vec!["serve".into()]),
        (
            "serve --project",
            vec!["serve".into(), "--project".into(), root_arg.clone()],
        ),
        (
            "serve --project --project-socket",
            vec![
                "serve".into(),
                "--project".into(),
                root_arg,
                "--project-socket".into(),
            ],
        ),
    ];
    for (what, args) in cases {
        let sandbox = Sandbox::new();
        let socket = if what.ends_with("--project-socket") {
            trusty_code::serve::uds::project_socket_path(&sandbox.socket(), Some(&root))
        } else {
            sandbox.socket()
        };
        let log = sandbox.log("serve");
        let mut daemon = sandbox.spawn(&args, &log);
        let payload = wait_healthy(&mut daemon, &socket, &log).await;
        assert_eq!(payload["status"], "ok", "{what}: {payload}");

        assert_no_tcp_listener(daemon.child.id(), what, &log);
        assert_eq!(mode_of(&socket), 0o600, "{what}: the socket is owner-only");
        assert_eq!(
            mode_of(socket.parent().expect("socket dir")),
            0o700,
            "{what}: the socket directory is owner-only"
        );

        // SAFETY: the pid is our own live child; `kill` touches no memory.
        unsafe {
            libc::kill(daemon.child.id() as libc::pid_t, libc::SIGTERM);
        }
        let status = wait_exit(&mut daemon, &log);
        assert!(
            status.success(),
            "{what}: SIGTERM must stop the daemon cleanly ({status})\nstderr:\n{}",
            read(&log)
        );
    }
}

/// Criteria 2 and 4: the daemon `tcode tui` autospawns listens on no TCP port.
///
/// Why: before #6637 slice 2c the autospawn command line was
/// `serve --http --port 0`, so this is the path that held a TCP port on every
/// machine whose operator only ever ran `tcode tui`. The TUI itself fails
/// afterwards for want of a TTY; the daemon it started outlives it by design
/// (owner directive 2026-08-01), is found on the shared socket, and is stopped
/// here.
/// Test: this function IS the test.
#[tokio::test]
async fn the_tui_autospawned_daemon_binds_no_tcp_listener() {
    let sandbox = Sandbox::new();
    let log = sandbox.log("tui");
    let mut tui = sandbox.spawn(&["tui", "--projectless"], &log);
    let status = wait_exit(&mut tui, &log);
    drop(tui);

    let socket = sandbox.socket();
    let started = Instant::now();
    let payload = loop {
        if let Some(payload) = health(&socket).await {
            break payload;
        }
        assert!(
            started.elapsed() < BOUND,
            "the TUI ({status}) left no daemon answering on {}\nstderr:\n{}\nlog:\n{}",
            socket.display(),
            read(&log),
            read(&sandbox.data().join("trusty-code/tui-spawned-daemon.log"))
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    let pid = payload["pid"].as_u64().expect("health reports a pid") as u32;
    let listeners = tcp_listeners(pid);
    // Stop the daemon before any assertion can strand it.
    // SAFETY: `pid` was just reported by the daemon this test caused to start.
    unsafe {
        libc::kill(pid as libc::pid_t, libc::SIGTERM);
    }
    assert!(
        listeners.is_empty(),
        "the autospawned daemon (pid {pid}) must hold no TCP LISTEN socket, found:\n{}",
        listeners.join("\n")
    );
    assert_eq!(
        payload["binding"]["state"], "projectless",
        "a --projectless TUI must spawn a projectless daemon: {payload}"
    );
}

/// Criterion 3: `--http` and `--port` are hard errors that name the socket,
/// never a silent no-op (supervisor ruling Q4).
///
/// Why: a script or plist still passing `--http` must learn that the daemon it
/// asked for no longer exists, and where the one that does answers.
/// Test: this function IS the test.
#[test]
fn serve_http_and_port_exit_non_zero_naming_the_socket() {
    let cases: [&[&str]; 3] = [
        &["serve", "--http"],
        &["serve", "--port", "0"],
        &["serve", "--http", "--port", "7882"],
    ];
    for args in cases {
        let sandbox = Sandbox::new();
        let log = sandbox.log("serve");
        let mut daemon = sandbox.spawn(args, &log);
        let status = wait_exit(&mut daemon, &log);
        let stderr = read(&log);
        assert!(
            !status.success(),
            "{args:?} must exit non-zero ({status})\nstderr:\n{stderr}"
        );
        assert!(
            stderr.contains("removed") && stderr.contains("Unix socket"),
            "{args:?} must say the HTTP transport is gone and name the socket:\n{stderr}"
        );
        assert!(
            stderr.contains(&sandbox.socket().display().to_string()),
            "{args:?} must print the socket path {}:\n{stderr}",
            sandbox.socket().display()
        );
        assert!(
            !sandbox.socket().exists(),
            "{args:?} must not bind anything before refusing"
        );
    }
}

/// Criterion 6: a second daemon on the same socket exits non-zero, and the
/// first keeps answering.
///
/// Why: with no TCP port the socket bind is the only duplicate guard. A build
/// that downgraded its failure to a warning would keep a second daemon running
/// with no socket, or take the socket from the live one.
/// Test: this function IS the test.
#[tokio::test]
async fn a_second_daemon_on_the_same_socket_exits_non_zero() {
    let sandbox = Sandbox::new();
    let first_log = sandbox.log("first");
    let mut first = sandbox.spawn(&["serve"], &first_log);
    let socket = sandbox.socket();
    let before = wait_healthy(&mut first, &socket, &first_log).await;

    let second_log = sandbox.log("second");
    let mut second = sandbox.spawn(&["serve"], &second_log);
    let status = wait_exit(&mut second, &second_log);
    assert!(
        !status.success(),
        "a daemon that cannot take the socket must exit non-zero ({status})\nstderr:\n{}",
        read(&second_log)
    );
    let after = health(&socket)
        .await
        .expect("the first daemon still answers on its socket");
    assert_eq!(
        after["pid"], before["pid"],
        "the socket still belongs to the first daemon"
    );
}
