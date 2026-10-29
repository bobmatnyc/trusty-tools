//! Tests for [`super`] (#10083).

use super::*;
use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};
use std::process::Command;

/// A bound unix socket whose `FD_CLOEXEC` is cleared: the descriptor a child
/// forked inside the macOS socket-creation window would inherit. Duplicated
/// to fd 200+ so it cannot collide with a descriptor the child opens itself.
fn leaked_socket() -> OwnedFd {
    // SAFETY: `socket` takes no pointers; the result is checked.
    let raw = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0) };
    assert!(raw >= 0, "socket: {}", io::Error::last_os_error());
    // SAFETY: `raw` is a fresh descriptor nothing else owns.
    let low = unsafe { OwnedFd::from_raw_fd(raw) };
    // SAFETY: F_DUPFD returns a new descriptor (without FD_CLOEXEC) or -1.
    let high = unsafe { libc::fcntl(low.as_raw_fd(), libc::F_DUPFD, 200) };
    assert!(high >= 200, "F_DUPFD: {}", io::Error::last_os_error());
    // SAFETY: `high` is a fresh descriptor nothing else owns.
    let high = unsafe { OwnedFd::from_raw_fd(high) };
    // SAFETY: plain fcntl on an fd `high` keeps open.
    let rc = unsafe { libc::fcntl(high.as_raw_fd(), libc::F_SETFD, 0) };
    assert_eq!(rc, 0, "clear FD_CLOEXEC");
    high
}

/// The descriptors a `/bin/ls /dev/fd` child sees, as `cmd` configures it.
fn child_sees(cmd: &mut Command, fd: RawFd) -> bool {
    let out = cmd.output().expect("spawn ls");
    assert!(out.status.success(), "{out:?}");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .any(|l| l.trim().parse::<RawFd>() == Ok(fd))
}

fn ls_fd() -> Command {
    let mut cmd = Command::new("/bin/ls");
    cmd.arg("/dev/fd");
    cmd
}

#[test]
fn a_plain_child_inherits_a_leaked_socket() {
    // The control: without the helper the descriptor reaches the child, so the
    // sealed assertions below can fail.
    let sock = leaked_socket();
    assert!(child_sees(&mut ls_fd(), sock.as_raw_fd()));
}

#[test]
fn a_sealed_child_does_not_inherit_a_leaked_socket() {
    let sock = leaked_socket();
    let mut cmd = ls_fd();
    install(&mut cmd, &[]);
    assert!(
        !child_sees(&mut cmd, sock.as_raw_fd()),
        "the child still holds fd {}",
        sock.as_raw_fd()
    );
}

#[test]
fn a_kept_descriptor_still_reaches_the_child() {
    let sock = leaked_socket();
    let mut cmd = ls_fd();
    install(&mut cmd, &[sock.as_raw_fd()]);
    assert!(child_sees(&mut cmd, sock.as_raw_fd()));
}

#[tokio::test]
async fn a_sealed_tokio_child_does_not_inherit_a_leaked_socket() {
    let sock = leaked_socket();
    let mut cmd = tokio::process::Command::new("/bin/ls");
    cmd.arg("/dev/fd");
    install_tokio(&mut cmd, &[]);
    let out = cmd.output().await.expect("spawn ls");
    let seen = String::from_utf8_lossy(&out.stdout)
        .lines()
        .any(|l| l.trim().parse::<RawFd>() == Ok(sock.as_raw_fd()));
    assert!(!seen, "the child still holds fd {}", sock.as_raw_fd());
}

#[test]
fn a_sealed_spawn_of_a_missing_binary_still_fails() {
    // std reports a failed exec through a close-on-exec pipe; sealing must not
    // break it (closing that pipe would read as a successful spawn).
    let mut cmd = Command::new("/nonexistent/trusty-child-fds-probe");
    install(&mut cmd, &[]);
    let err = cmd.spawn().expect_err("exec of a missing binary must fail");
    assert_eq!(err.kind(), io::ErrorKind::NotFound);
}

/// Four threads spawn children while 3,000 tokio connections open and drop;
/// every server side must see EOF. The standalone model leaked 511 of 20,000.
/// Probabilistic before the fix, so a regression guard rather than a proof.
#[cfg(target_os = "macos")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "stress: ~3,000 connections against 4 spawner threads; run with --include-ignored"]
async fn dropped_connections_reach_eof_while_sealed_children_spawn() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;
    use tokio::io::AsyncReadExt as _;

    const CYCLES: usize = 3000;
    // Spawns are unpaced, so a child lives for ~RING more spawns: tens of ms.
    const RING: usize = 60;
    const EOF_WAIT: Duration = Duration::from_millis(30);
    let dir = tempfile::tempdir().expect("tempdir");
    let listener = tokio::net::UnixListener::bind(dir.path().join("s.sock")).expect("bind");
    let path = dir.path().join("s.sock");

    let stop = Arc::new(AtomicBool::new(false));
    let spawners: Vec<_> = (0..4)
        .map(|_| {
            let stop = Arc::clone(&stop);
            std::thread::spawn(move || {
                let mut live = std::collections::VecDeque::new();
                while !stop.load(Ordering::Relaxed) {
                    let mut cmd = Command::new("/bin/sleep");
                    cmd.arg("5");
                    seal_inherited(&mut cmd);
                    if let Ok(child) = cmd.spawn() {
                        live.push_back(child);
                    }
                    while live.len() > RING {
                        let mut old = live.pop_front().expect("non-empty");
                        let _ = old.kill();
                        let _ = old.wait();
                    }
                }
                for mut c in live {
                    let _ = c.kill();
                    let _ = c.wait();
                }
            })
        })
        .collect();

    let mut leaked = 0;
    for _ in 0..CYCLES {
        let client = tokio::net::UnixStream::connect(&path)
            .await
            .expect("connect");
        let (mut server, _) = listener.accept().await.expect("accept");
        drop(client);
        let mut buf = [0u8; 1];
        match tokio::time::timeout(EOF_WAIT, server.read(&mut buf)).await {
            Ok(Ok(0)) => {}
            _ => leaked += 1,
        }
    }
    stop.store(true, Ordering::Relaxed);
    for t in spawners {
        t.join().expect("spawner thread");
    }
    assert_eq!(leaked, 0, "{leaked} of {CYCLES} connections stayed open");
}
