//! A stale socket file whose dial is known to be refused (#10026).
//!
//! Why: a test that binds a socket, drops the listener and then expects a dial
//! to be REFUSED races every concurrent test that spawns a process. A child
//! forked between the bind and the drop inherits the listener fd and holds it
//! until its `exec`, so the "dropped" socket briefly still accepts and the dial
//! is accepted, then reset. `O_CLOEXEC` (#10443) closes the fd at `exec`, not at
//! `fork`, so it does not close that window. #7215 fixed the same race for the
//! doctor `events` row with a local `await_listener_gone`; this file is that
//! wait, shared by every test with the shape.
//!
//! What: [`bind_stale`] binds a hardened socket at a path, drops the listener,
//! and returns only once a dial is refused. Once refused, no fd keeps the
//! listener alive, and nothing can bring it back.
//!
//! This file compiles into TWO targets, the `trusty-mpm` lib and the `tm`
//! binary, through a `#[path]` include from each one's `test_support` module,
//! as `test_tmux_session.rs` does. It names only external crates, so one
//! spelling compiles in both.
//! Test: `tests` below.

use std::io::ErrorKind;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::{Duration, Instant};

use tokio::net::UnixListener;

/// How long a dropped listener may keep accepting before the wait fails.
const STALE_BUDGET: Duration = Duration::from_secs(5);

/// The interval between dials while the listener is still alive.
const PROBE_INTERVAL: Duration = Duration::from_millis(20);

/// Bind a hardened socket at `path`, drop the listener, and wait until a dial
/// is refused.
///
/// Why (#10026): a concurrent test's `fork` can inherit the listener fd; see
/// the module docs.
/// What: creates the 0700 parent as `bind_hardened` does, leaves the socket
/// file behind, and panics if a dial still connects after 5 s. Call it inside
/// a Tokio runtime, because the listener is Tokio's.
/// Test: `a_held_listener_fd_is_waited_out_before_the_socket_reads_stale`.
pub(crate) fn bind_stale(path: &Path) {
    bind_stale_with(path, STALE_BUDGET, |_| {});
}

/// [`bind_stale`] with the budget and a hook run on the listener before the
/// drop, so a test can hold a duplicate of its fd as a forked child would.
fn bind_stale_with(path: &Path, budget: Duration, before_drop: impl FnOnce(&UnixListener)) {
    let listener = trusty_common::uds::bind_hardened(path).expect("bind a hardened socket");
    before_drop(&listener);
    drop(listener);
    // #10026: an inherited fd keeps the listener alive past the drop.
    await_dial_refused(path, budget);
}

/// Dial `path` until the dial is refused, failing loudly after `budget`.
///
/// A dial that connects means some fd still holds the listener; the probe
/// stream is dropped and the dial retried. Any error other than a refusal is a
/// broken fixture, not a leak, so it fails at once. The listener's backlog
/// (1024 for Tokio) exceeds the 250 probes a 5 s budget can queue, so a probe
/// never blocks on a full queue.
fn await_dial_refused(path: &Path, budget: Duration) {
    let deadline = Instant::now() + budget;
    loop {
        match UnixStream::connect(path) {
            Err(err) if err.kind() == ErrorKind::ConnectionRefused => return,
            Err(err) => panic!("dial {} failed without a refusal: {err}", path.display()),
            Ok(_) if Instant::now() >= deadline => panic!(
                "{} still accepts {budget:?} after its listener dropped",
                path.display()
            ),
            Ok(_) => std::thread::sleep(PROBE_INTERVAL),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::os::fd::{AsFd, OwnedFd};

    use super::*;

    /// A socket path under a fresh tempdir, in a subdirectory the hardened
    /// bind creates at 0700.
    fn socket_in(dir: &tempfile::TempDir) -> std::path::PathBuf {
        dir.path().join("s").join("trusty-mpm.sock")
    }

    /// A duplicate of the listener fd, standing in for a forked child's copy.
    fn dup(listener: &UnixListener) -> OwnedFd {
        listener
            .as_fd()
            .try_clone_to_owned()
            .expect("dup the listener fd")
    }

    /// #10026: a listener fd held past the drop, as a child forked by a
    /// concurrent test holds it until its `exec`, must not reach the caller.
    #[tokio::test]
    async fn a_held_listener_fd_is_waited_out_before_the_socket_reads_stale() {
        let dir = tempfile::tempdir().expect("tempdir");
        let socket = socket_in(&dir);
        let mut releaser = None;
        bind_stale_with(&socket, STALE_BUDGET, |listener| {
            let held = dup(listener);
            releaser = Some(std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(200));
                drop(held);
            }));
        });
        let err = UnixStream::connect(&socket).expect_err("a stale socket refuses the dial");
        assert_eq!(err.kind(), ErrorKind::ConnectionRefused, "{err}");
        assert!(socket.exists(), "the stale socket file stays behind");
        releaser.expect("hook ran").join().expect("releaser");
    }

    /// #10026: a listener that never goes away fails the wait loudly rather
    /// than letting the test run on a socket that still accepts.
    #[tokio::test]
    #[should_panic(expected = "still accepts")]
    async fn a_listener_that_outlives_the_budget_fails_loudly() {
        let dir = tempfile::tempdir().expect("tempdir");
        let socket = socket_in(&dir);
        let mut held = None;
        bind_stale_with(&socket, Duration::from_millis(100), |listener| {
            held = Some(dup(listener));
        });
        drop(held);
    }
}
