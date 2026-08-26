//! Start the daemon when a command needs it, at most once across processes.
//!
//! Why (#6286, ADR-0032): this used to resolve a base URL out of the `http_addr`
//! discovery file and probe `GET /health`, and `start.rs` and the stdio bridge
//! both drove `trusty_mcp::ensure_daemon_up_single_flight` with a
//! `DaemonBridgeConfig` carrying a `health_path` and a `base_url_fn`. That
//! module's own docs record that it is built around a health URL, and this
//! daemon no longer has one. trusty-analyze hit the same wall in #6287 and
//! answered it with a local loop rather than extending the shared bridge
//! mid-migration; this is that answer, plus the exclusion trusty-analyze did not
//! need.
//!
//! What: [`probe`] is a bare connect. [`ensure_daemon_running`] adds the
//! spawn-and-wait, serialised through [`StartLock`] so N concurrent stdio
//! bridges converge on ONE daemon — the #5267 contract, kept intact. The lock
//! is what makes this "ensure it exists" rather than the auto-spawn #1152
//! removed, where every bridge started its own and they raced for redb's write
//! lock.
//!
//! **The lock is held across the readiness wait deliberately.** Releasing it at
//! spawn time reopens the race: a second caller would acquire, re-probe a daemon
//! that has not bound yet, and start another.
//!
//! Test: `probe_returns_false_for_an_absent_socket`,
//! `ensure_daemon_running_returns_early_when_something_is_serving`.

use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context as _, Result};
use colored::Colorize;
use trusty_mcp::StartLock;

/// How long a liveness connect may take before the path is called dead.
///
/// A local socket accepts or refuses in microseconds; this is headroom for a
/// loaded machine, not a latency budget.
const PROBE_TIMEOUT: Duration = Duration::from_millis(500);

/// How long to wait for a freshly-spawned daemon to answer.
///
/// Generous because a cold start hydrates every palace before it binds.
const STARTUP_TIMEOUT: Duration = Duration::from_secs(30);

/// How often to re-probe while waiting.
const POLL_INTERVAL: Duration = Duration::from_millis(500);

/// Is anything serving `socket`?
///
/// Why a bare connect rather than a `memory.health` call: the question is
/// whether the endpoint is live. A daemon that is up but degraded — its
/// embedder still warming, say — must not be reported absent and then spawned
/// on top of itself.
///
/// Test: `probe_returns_false_for_an_absent_socket`.
pub async fn probe(socket: &Path) -> bool {
    trusty_common::uds::socket_is_serving(socket, PROBE_TIMEOUT).await
}

/// Spawn the daemon detached, returning the child PID.
///
/// `--foreground` is what stops the child self-spawning again; no `--http` is
/// passed, and there is nothing for it to mean since #6286. Stdio is null-ed so
/// the daemon survives a terminal close.
fn spawn_daemon() -> Result<u32> {
    trusty_common::daemon_guard::spawn_current_exe(&["serve", "--foreground"])
        .map_err(|e| anyhow!("trusty-memory daemon spawn failed: {e}"))
}

/// Ensure something is serving `socket`, starting the daemon at most once
/// across every process that asks.
///
/// What, in order: (1) probe with no lock held, so the overwhelmingly common
/// "already up" case pays nothing and touches no filesystem; (2) on a miss,
/// block on `lock_path`; (3) RE-probe under the lock, because whoever held it
/// before us most likely just started the daemon and we must not start another;
/// (4) only then spawn; (5) poll to readiness with the lock still held.
///
/// Fails closed: a daemon that never answers is an `Err`, never an `Ok` the
/// caller discovers downstream as an empty result.
///
/// # Errors
///
/// When the lock cannot be taken, the spawn fails, or nothing answers inside
/// [`STARTUP_TIMEOUT`].
///
/// Test: `ensure_daemon_running_returns_early_when_something_is_serving`.
pub async fn ensure_daemon_running(socket: &Path, lock_path: &Path) -> Result<()> {
    if probe(socket).await {
        return Ok(());
    }

    // Blocking `flock` goes on a blocking-safe thread; the guard is `Send` and
    // is held across the awaits below on purpose.
    let lock_path_owned = lock_path.to_path_buf();
    let _lock = tokio::task::spawn_blocking(move || StartLock::acquire_blocking(&lock_path_owned))
        .await
        .context("start-lock acquisition task panicked")??;

    if probe(socket).await {
        return Ok(());
    }

    eprintln!("{} Starting trusty-memory daemon…", "◉".cyan());
    spawn_daemon()?;

    let deadline = Instant::now() + STARTUP_TIMEOUT;
    while Instant::now() < deadline {
        tokio::time::sleep(POLL_INTERVAL).await;
        if probe(socket).await {
            return Ok(());
        }
    }
    Err(anyhow!(
        "trusty-memory did not start serving {} within {}s — run \
         `trusty-memory serve --foreground` in the foreground to see the error",
        socket.display(),
        STARTUP_TIMEOUT.as_secs()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: an absent socket must not read as live — a caller that skipped the
    /// spawn would then fail on its first real call, having been told the
    /// daemon was there.
    /// Test: itself.
    #[tokio::test]
    async fn probe_returns_false_for_an_absent_socket() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let started = Instant::now();
        assert!(!probe(&tmp.path().join("absent.sock")).await);
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "a refused dial must not wait out the budget: {:?}",
            started.elapsed()
        );
    }

    /// Why: the fast path must return without taking the lock or spawning —
    /// a second daemon would fight the first for redb's write lock, which is
    /// the #1152 outage.
    /// Test: itself.
    #[tokio::test(flavor = "multi_thread")]
    async fn ensure_daemon_running_returns_early_when_something_is_serving() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let socket = tmp.path().join("sockets").join("trusty-memory.sock");
        let listener = trusty_common::uds::bind_hardened(&socket).expect("bind");
        tokio::spawn(async move { while listener.accept().await.is_ok() {} });

        let started = Instant::now();
        ensure_daemon_running(&socket, &tmp.path().join("start.lock"))
            .await
            .expect("a live socket must satisfy the guard");
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "the fast path must not wait: {:?}",
            started.elapsed()
        );
    }
}
