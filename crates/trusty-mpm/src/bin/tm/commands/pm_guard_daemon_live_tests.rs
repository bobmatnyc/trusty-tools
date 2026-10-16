//! Tests for the stale-versus-saturated rule (#6288, df-D49).

use std::path::{Path, PathBuf};

use trusty_mpm::core::daemon_identity::{DaemonLock, LOCK_PRODUCT, render_lock};

use super::super::daemon_pid_identity::{PidIdentity, pid_identity};
use super::super::pm_guard_dispatch::{SharedTreeReply, classify_transport_failure_with};
use super::daemon_live_at;

/// A stale socket file: bound by a daemon that is gone, so a dial is refused
/// with `ECONNREFUSED` — the errno a saturated live listener also gives.
struct Stale {
    _dir: tempfile::TempDir,
    socket: PathBuf,
    lock: PathBuf,
}

fn stale() -> Stale {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("s").join("trusty-mpm.sock");
    drop(trusty_common::uds::bind_hardened(&socket).expect("bind"));
    let lock = dir.path().join("daemon.lock");
    Stale {
        _dir: dir,
        socket,
        lock,
    }
}

impl Stale {
    fn url(&self) -> String {
        trusty_mpm::core::socket_locator(&self.socket)
    }

    /// Write a lock naming `pid` and this socket.
    fn write_lock(&self, pid: u32) {
        let lock = DaemonLock {
            product: LOCK_PRODUCT.to_string(),
            pid,
            addr: "http://127.0.0.1:7880".to_string(),
            started_at: String::new(),
            socket_path: self.socket.display().to_string(),
        };
        std::fs::write(&self.lock, render_lock(&lock)).expect("write lock");
    }

    /// Classify the refused dial with the daemon-live probe fed by the lock.
    async fn classify(&self, identify: &dyn Fn(u32) -> PidIdentity) -> SharedTreeReply {
        let err = trusty_mpm::client::DaemonClient::over_socket(&self.socket)
            .get("/health")
            .send()
            .await
            .expect_err("a stale socket refuses");
        assert!(err.dial_refused() && !err.socket_absent(), "{err}");
        let url = self.url();
        classify_transport_failure_with(&url, "/x", &err, &|| {
            daemon_live_at(&url, &self.lock, identify)
        })
    }
}

fn allows(reply: &SharedTreeReply) -> bool {
    matches!(reply, SharedTreeReply::Unavailable(_))
}

/// A short-lived child that has exited: its pid is dead.
fn dead_pid() -> u32 {
    let mut child = std::process::Command::new("true").spawn().expect("spawn");
    let pid = child.id();
    child.wait().expect("wait");
    pid
}

/// Rule 1: socket file present, nothing listening, no live daemon -> allow.
#[tokio::test]
async fn a_stale_socket_with_no_live_daemon_allows() {
    let s = stale();
    // No lock at all.
    assert!(allows(&s.classify(&|_| PidIdentity::Daemon).await));
    // A lock whose pid is dead.
    s.write_lock(dead_pid());
    assert!(allows(&s.classify(&|_| PidIdentity::Daemon).await));
}

/// Rule 2: a refused dial while a live daemon pid holds the lock -> deny.
#[tokio::test]
async fn a_refused_dial_with_a_live_daemon_denies() {
    let s = stale();
    s.write_lock(std::process::id());
    let reply = s.classify(&|_| PidIdentity::Daemon).await;
    assert!(matches!(reply, SharedTreeReply::Unanswered(_)), "denies");
    // Fail closed: a live pid whose argv cannot be read cannot be ruled out.
    let reply = s.classify(&|_| PidIdentity::Unknown).await;
    assert!(matches!(reply, SharedTreeReply::Unanswered(_)), "denies");
}

/// Rule 3: the lock names a pid held by an unrelated live process -> no daemon.
#[tokio::test]
async fn a_refused_dial_over_a_reused_pid_allows() {
    let s = stale();
    let mut unrelated = std::process::Command::new("sleep")
        .arg("30")
        .spawn()
        .expect("spawn sleep");
    s.write_lock(unrelated.id());
    // The real process-table identity, not a stub: `sleep` is not a daemon.
    let reply = s.classify(&pid_identity).await;
    let _ = unrelated.kill();
    let _ = unrelated.wait();
    assert!(allows(&reply), "a reused pid is not a live daemon");
}

/// The lock decides only for the socket it names.
#[tokio::test]
async fn a_lock_for_another_socket_or_a_foreign_file_is_not_live() {
    let s = stale();
    s.write_lock(std::process::id());
    let other = trusty_mpm::core::socket_locator(Path::new("/tmp/elsewhere.sock"));
    assert!(!daemon_live_at(&other, &s.lock, &|_| PidIdentity::Daemon));
    assert!(daemon_live_at(&s.url(), &s.lock, &|_| PidIdentity::Daemon));
    std::fs::write(&s.lock, "product = \"someone-else\"\npid = 1\n").expect("foreign");
    assert!(!daemon_live_at(&s.url(), &s.lock, &|_| PidIdentity::Daemon));
}
