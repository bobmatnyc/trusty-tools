//! `tm daemon` serves its Unix socket only and holds no TCP listener (#6288 S3a).
//!
//! Why: ADR-0032 leaves trusty-console as the one TCP surface. Until S3a the
//! daemon bound `127.0.0.1:7880`, fell back to an ephemeral port when that was
//! busy, and advertised the address in its lock file and an `http_addr` file.
//! The source lint (`scripts/check_no_tcp_listeners.sh`) proves no bind call is
//! left in source; these tests prove the BUILT daemon listens on nothing, which
//! is what an operator and the doctor's `tcp_listeners` row observe.
//! What: each case starts the real binary the way `scripts/sandbox_daemon.sh`
//! does — cleared environment, scratch `$HOME` and data dir, `--sandbox` —
//! waits until `mpm.health` answers on the sandbox socket, and then inspects
//! the live process: its TCP LISTEN sockets, its lock file, its discovery
//! files, its stderr.
//! Test: this file IS the test module; run with
//! `cargo test -p trusty-mpm --test integration daemon_no_tcp_6288_s3::`.

use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::time::{Duration, Instant};

use super::common;

/// How long a daemon may take to answer `mpm.health` on its socket.
const BOUND: Duration = Duration::from_secs(60);

/// The deprecation line `--addr` / `TRUSTY_MPM_ADDR` logs (#6288 S3a, Q1).
const ADDR_DEPRECATION: &str = "--addr / TRUSTY_MPM_ADDR is deprecated and ignored";

/// One sandbox's scratch tree.
struct Sandbox {
    _root: tempfile::TempDir,
    home: PathBuf,
    data: PathBuf,
}

impl Sandbox {
    fn new() -> Self {
        let root = tempfile::tempdir().expect("scratch root");
        let home = root.path().join("home");
        let data = root.path().join("data");
        std::fs::create_dir_all(&home).expect("home");
        std::fs::create_dir_all(&data).expect("data");
        Self {
            _root: root,
            home,
            data,
        }
    }

    /// `<data>/trusty-mpm`, the directory the socket and `http_addr` live in.
    fn app_dir(&self) -> PathBuf {
        self.data.join("trusty-mpm")
    }

    fn socket(&self) -> PathBuf {
        self.app_dir().join("trusty-mpm.sock")
    }

    fn lock(&self) -> PathBuf {
        self.home.join(".trusty-mpm").join("daemon.lock")
    }

    /// `tm daemon --sandbox <extra…>`, stderr captured to `log`.
    fn spawn(&self, extra: &[&str], log: &Path) -> Child {
        let stderr = std::fs::File::create(log).expect("stderr log");
        common::tm_command_in(&self.home)
            .current_dir(&self.home)
            .env_clear()
            .env("HOME", &self.home)
            .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
            .env("TRUSTY_DATA_DIR_OVERRIDE", &self.data)
            .env("TRUSTY_SANDBOX", "1")
            .args(["daemon", "--sandbox"])
            .args(extra)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::from(stderr))
            .spawn()
            .expect("spawn tm daemon --sandbox")
    }
}

/// A daemon killed when this value drops, pass or panic.
struct Daemon(Child);

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Whether `mpm.health` on `socket` answers with a result.
fn socket_healthy(socket: &Path) -> bool {
    let Ok(mut stream) = UnixStream::connect(socket) else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let frame = "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"mpm.health\",\"params\":{}}\n";
    if stream.write_all(frame.as_bytes()).is_err() {
        return false;
    }
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line).is_ok() && line.contains("\"result\"")
}

/// Wait until the daemon answers on `socket`; panic with its stderr otherwise.
fn wait_healthy(child: &mut Child, socket: &Path, log: &Path) {
    let started = Instant::now();
    loop {
        if socket_healthy(socket) {
            return;
        }
        if let Some(status) = child.try_wait().expect("try_wait") {
            panic!(
                "the daemon exited ({status}) before answering on {}\nstderr:\n{}",
                socket.display(),
                read(log)
            );
        }
        assert!(
            started.elapsed() < BOUND,
            "no mpm.health answer on {} within {BOUND:?}\nstderr:\n{}",
            socket.display(),
            read(log)
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn read(path: &Path) -> String {
    let mut text = String::new();
    if let Ok(mut f) = std::fs::File::open(path) {
        let _ = f.read_to_string(&mut text);
    }
    text
}

/// Every TCP socket `pid` holds in LISTEN state, one line each.
///
/// macOS: `lsof`. Linux: the process's socket inodes matched against the
/// LISTEN (`0A`) rows of its `/proc/<pid>/net/tcp{,6}` tables, so the check
/// does not depend on `lsof` being installed on a CI runner.
#[cfg(target_os = "macos")]
fn tcp_listeners(pid: u32) -> Vec<String> {
    let out = std::process::Command::new("/usr/sbin/lsof")
        .args(["-nP", "-a", "-p", &pid.to_string(), "-iTCP", "-sTCP:LISTEN"])
        .output()
        .expect("lsof must run on macOS");
    // A failing lsof prints nothing on stdout too; only an empty stderr makes
    // an empty listing evidence of no listener.
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

/// A1 + A4: a sandbox daemon reaches healthy on its socket, listens on no TCP
/// port, publishes no address, and removes a stale `http_addr` it inherits.
///
/// Why `--addr 127.0.0.1:0`: before S3a that asked for an ephemeral TCP port,
/// so the pre-S3a binary fails on the listener assertion rather than on the
/// host's own daemon answering 7880. After S3a the flag is accepted and
/// ignored (#6288).
/// Test: this function IS the test.
#[test]
fn a_sandbox_daemon_holds_no_tcp_listener_and_publishes_no_address() {
    let sandbox = Sandbox::new();
    // A stale discovery file from an older daemon, which S3a removes at start.
    std::fs::create_dir_all(sandbox.app_dir()).expect("app dir");
    let http_addr = sandbox.app_dir().join("http_addr");
    std::fs::write(&http_addr, "127.0.0.1:7880").expect("stale http_addr");

    let log = sandbox.home.join("daemon.stderr");
    let mut daemon = Daemon(sandbox.spawn(&["--addr", "127.0.0.1:0"], &log));
    wait_healthy(&mut daemon.0, &sandbox.socket(), &log);

    let listeners = tcp_listeners(daemon.0.id());
    assert!(
        listeners.is_empty(),
        "the daemon must hold no TCP LISTEN socket, found:\n{}\nstderr:\n{}",
        listeners.join("\n"),
        read(&log)
    );

    let lock = trusty_mpm::core::daemon_identity::read_lock_at(&sandbox.lock())
        .expect("the daemon writes a readable lock");
    assert_eq!(lock.pid, daemon.0.id(), "the lock names this daemon");
    assert_eq!(
        lock.socket_path,
        sandbox.socket().display().to_string(),
        "the lock names the socket"
    );
    assert!(
        lock.addr.is_empty(),
        "the lock advertises no TCP address: {:?}",
        lock.addr
    );
    assert!(
        !http_addr.exists(),
        "no http_addr discovery file may remain: {}",
        read(&http_addr)
    );
}

/// A2 + F4 + Q1: a busy `--addr` neither stops the daemon nor feeds a bind, and
/// the deprecated flag is logged exactly once.
///
/// Why: a daemon that kept the bind without the `AddrInUse` fallback would fail
/// to start here; one that kept the fallback would listen on an ephemeral port.
/// The launchd plist on the host still passes `--addr 127.0.0.1:7880`, so the
/// flag must parse and be ignored, never be an error (#6288).
/// Test: this function IS the test.
#[test]
fn a_busy_addr_flag_is_ignored_and_logged_once() {
    let sandbox = Sandbox::new();
    // Held for the whole test: the address the flag names is taken.
    let busy = std::net::TcpListener::bind("127.0.0.1:0").expect("bind a busy port");
    let busy_addr = busy.local_addr().expect("busy addr").to_string();

    let log = sandbox.home.join("daemon.stderr");
    let mut daemon = Daemon(sandbox.spawn(&["--addr", &busy_addr], &log));
    wait_healthy(&mut daemon.0, &sandbox.socket(), &log);

    let listeners = tcp_listeners(daemon.0.id());
    assert!(
        listeners.is_empty(),
        "--addr must not feed a bind, found:\n{}\nstderr:\n{}",
        listeners.join("\n"),
        read(&log)
    );
    let stderr = read(&log);
    assert_eq!(
        stderr.matches(ADDR_DEPRECATION).count(),
        1,
        "the deprecated --addr is logged exactly once:\n{stderr}"
    );
    drop(busy);
}

/// F1: a second daemon on the same sandbox refuses to start and exits non-zero,
/// and the first daemon's lock is left as it was.
///
/// Why: with the TCP probe gone, the socket bind is the only duplicate guard. A
/// build that downgraded its failure to a warning would keep a second daemon
/// running with no socket, or overwrite the incumbent's lock.
/// Test: this function IS the test.
#[test]
fn a_second_daemon_on_the_same_socket_exits_non_zero_and_leaves_the_lock() {
    let sandbox = Sandbox::new();
    let first_log = sandbox.home.join("first.stderr");
    let mut first = Daemon(sandbox.spawn(&["--addr", "127.0.0.1:0"], &first_log));
    wait_healthy(&mut first.0, &sandbox.socket(), &first_log);

    let second_log = sandbox.home.join("second.stderr");
    let mut second = Daemon(sandbox.spawn(&["--addr", "127.0.0.1:0"], &second_log));
    let started = Instant::now();
    let status = loop {
        if let Some(status) = second.0.try_wait().expect("try_wait") {
            break status;
        }
        assert!(
            started.elapsed() < BOUND,
            "the second daemon kept running beside the first\nstderr:\n{}",
            read(&second_log)
        );
        std::thread::sleep(Duration::from_millis(100));
    };
    assert!(
        !status.success(),
        "a daemon that cannot take the socket must exit non-zero ({status})\nstderr:\n{}",
        read(&second_log)
    );
    let lock = trusty_mpm::core::daemon_identity::read_lock_at(&sandbox.lock())
        .expect("the first daemon's lock survives");
    assert_eq!(
        lock.pid,
        first.0.id(),
        "the lock still names the first daemon"
    );
    assert!(
        socket_healthy(&sandbox.socket()),
        "the first daemon still answers on its socket"
    );
}
