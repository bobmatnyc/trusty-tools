//! A daemon SIGTERM leaves every managed session running (#10597, owner
//! ruling D82).
//!
//! Why: the daemon's graceful shutdown used to stop every live managed
//! session it owned (#1455). D82 makes the opposite the contract: a daemon
//! stop or restart ends no session. Only a real signal to a real daemon
//! process proves that, because tm's SIGTERM handler and the serve drain race
//! each other to exit.
//! What: starts the real binary the way `scripts/sandbox_daemon.sh` does, on
//! a private tmux server under its data dir (#9557). A managed-named session
//! running `sleep` is created on that server first, so the daemon's boot
//! reconcile adopts it as an `Active` record carrying its pane and server.
//! The test then sends SIGTERM to the daemon PID and asserts the tmux session
//! still holds the same pane and the record is still `active`. A Ctrl-C to
//! the pane, which the deleted shutdown sent to a pane with no `claude`,
//! ends `sleep` and the session with it.
//! Test: this file IS the test module; it needs a live tmux binary, so run it
//! with `cargo test -p trusty-mpm --test integration daemon_sigterm_survival_10597:: -- --include-ignored`.

use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use super::common;

/// How long the daemon may take to boot and adopt the session.
const BOOT_BOUND: Duration = Duration::from_secs(60);

/// How long the daemon may take to exit after SIGTERM.
const EXIT_BOUND: Duration = Duration::from_secs(30);

/// A managed-prefix name outside the reserved `tm-xtest-` namespace, which
/// boot reconcile refuses to adopt (#6116).
const NAME: &str = "tm-sig10597";

/// A private tmux server under `dir`; killed on drop.
struct PrivateTmux {
    dir: PathBuf,
}

impl PrivateTmux {
    /// Run tmux on this server; `None` when it exits non-zero.
    fn run(&self, args: &[&str]) -> Option<String> {
        let out = Command::new("tmux")
            .env("TMUX_TMPDIR", &self.dir)
            .env_remove("TMUX")
            .args(args)
            .output()
            .expect("run tmux");
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).trim().to_owned())
    }

    /// The pane id the session `NAME` holds, or `None` when it is gone.
    fn pane(&self) -> Option<String> {
        let target = trusty_common::tmux::exact_window_target(NAME);
        self.run(&["list-panes", "-t", &target, "-F", "#{pane_id}"])
    }
}

impl Drop for PrivateTmux {
    fn drop(&mut self) {
        let _ = self.run(&["kill-server"]);
    }
}

/// The `state` of the store record named `NAME`, read from the first
/// `sessions.json` under `root` (the store sits under `$HOME/.trusty-mpm`).
fn record_state(root: &Path) -> Option<String> {
    let text = find_sessions_json(root).and_then(|p| std::fs::read_to_string(p).ok())?;
    let json: serde_json::Value = serde_json::from_str(&text).ok()?;
    let sessions = json.get("sessions")?.as_object()?;
    sessions
        .values()
        .find(|r| r["tmux_name"] == NAME)
        .and_then(|r| r["state"].as_str().map(str::to_owned))
}

/// The first `sessions.json` under `dir`, depth-first.
fn find_sessions_json(dir: &Path) -> Option<PathBuf> {
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if let Some(found) = find_sessions_json(&path) {
                return Some(found);
            }
        } else if path.file_name().is_some_and(|n| n == "sessions.json") {
            return Some(path);
        }
    }
    None
}

/// Why (#10597): D82's acceptance — SIGTERM to a live daemon leaves its
/// adopted managed session running and its record `active`.
/// Test: this test.
#[test]
#[ignore = "requires a live tmux binary; run with --include-ignored"]
fn a_daemon_sigterm_leaves_a_managed_session_running() {
    // A short root: the tmux socket path must fit sun_path (#9557).
    let root = tempfile::tempdir_in("/tmp").expect("scratch root");
    let home = root.path().join("home");
    let data = root.path().join("data");
    let work = root.path().join("work");
    let tmux_dir = data.join("tmux");
    std::fs::create_dir_all(&home).expect("home");
    std::fs::create_dir_all(&work).expect("work");
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&tmux_dir)
        .expect("tmux dir");
    let tmux = PrivateTmux {
        dir: tmux_dir.clone(),
    };
    let work_arg = work.to_str().expect("utf-8 work dir");
    tmux.run(&["new-session", "-d", "-s", NAME, "-c", work_arg, "sleep 600"])
        .expect("tmux must start the session; is tmux installed?");
    let pane = tmux.pane().expect("the session holds a pane");

    let stderr = std::fs::File::create(root.path().join("daemon.stderr")).expect("stderr");
    let mut child = common::tm_command_in(&home)
        .current_dir(&home)
        .env_clear()
        .env("HOME", &home)
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("TRUSTY_DATA_DIR_OVERRIDE", &data)
        .env("TRUSTY_SANDBOX", "1")
        .env("TMUX_TMPDIR", &tmux_dir)
        .args(["daemon", "--sandbox"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(stderr)
        .spawn()
        .expect("spawn tm daemon --sandbox");
    let log = || std::fs::read_to_string(root.path().join("daemon.stderr")).unwrap_or_default();

    let socket = data.join("trusty-mpm").join("trusty-mpm.sock");
    let started = Instant::now();
    while !(socket.exists() && record_state(root.path()).as_deref() == Some("active")) {
        if let Some(status) = child.try_wait().expect("try_wait") {
            panic!("the daemon exited during boot ({status}):\n{}", log());
        }
        if started.elapsed() > BOOT_BOUND {
            let _ = child.kill();
            let _ = child.wait();
            panic!(
                "the daemon never adopted {NAME} as active (state {:?}):\n{}",
                record_state(root.path()),
                log()
            );
        }
        std::thread::sleep(Duration::from_millis(100));
    }

    let pid = libc::pid_t::try_from(child.id()).expect("pid fits pid_t");
    // SAFETY: `pid` is this test's own live child, not yet reaped.
    assert_eq!(unsafe { libc::kill(pid, libc::SIGTERM) }, 0, "send SIGTERM");
    let stopped = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().expect("try_wait") {
            break status;
        }
        if stopped.elapsed() > EXIT_BOUND {
            let _ = child.kill();
            let _ = child.wait();
            panic!("the daemon did not exit on SIGTERM:\n{}", log());
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    // Give any signal the shutdown sent time to end `sleep` and its pane.
    std::thread::sleep(Duration::from_secs(3));

    assert_eq!(
        tmux.pane().as_deref(),
        Some(pane.as_str()),
        "the daemon's SIGTERM ({status}) ended {NAME}:\n{}",
        log()
    );
    assert_eq!(
        record_state(root.path()).as_deref(),
        Some("active"),
        "the daemon's SIGTERM moved {NAME}'s record:\n{}",
        log()
    );
}
