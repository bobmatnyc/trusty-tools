//! Auto-start the trusty-analyze daemon when a CLI command needs it.
//!
//! Why: a command that requires the daemon used to fail with a static "is not
//! running" error. This guard probes the socket, spawns the daemon in the
//! background when nothing answers, then polls until it does. Users get a
//! single informational line and the command they typed Just Works.
//!
//! What: [`probe_health`] is a bare connect, [`ensure_daemon_running`] adds the
//! spawn-and-wait loop.
//!
//! #6287 stopped delegating to `trusty_common::daemon_guard::spin_until_ready`
//! and `trusty_mcp::ensure_daemon_up`. Both are built around a health URL —
//! `DaemonGuardConfig` carries `health_url: String` and `DaemonBridgeConfig`
//! carries `health_path` plus a `base_url_fn` — and this daemon no longer has
//! one. Extending either to speak UDS is worth doing when a second service
//! needs it (`trusty-review` does not: its MCP surface runs in-process and
//! never bridges to a daemon); until then, a fifteen-line loop here is smaller
//! than the abstraction, and `trusty_common::uds::socket_is_serving` is the
//! shared entry point that actually matters.
//!
//! Test: `probe_health_returns_false_for_an_absent_socket`,
//! `ensure_daemon_running_returns_ok_when_something_is_already_serving`.
//!
//! Note: only call this from commands that *require* the daemon. Commands like
//! `start`, `stop`, `serve`, `service`, and `completions` deliberately do not
//! call this guard.

use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use colored::Colorize;

/// How long a liveness connect may take before it is called dead.
///
/// A local socket accepts or refuses in microseconds; this is headroom for a
/// loaded machine, not a latency budget.
const PROBE_TIMEOUT: Duration = Duration::from_millis(500);

/// How long to wait for a freshly-spawned daemon to answer.
const STARTUP_TIMEOUT: Duration = Duration::from_secs(30);

/// How often to re-probe while waiting.
const POLL_INTERVAL: Duration = Duration::from_millis(500);

/// Is anything serving `socket`?
///
/// Why: a bare connect rather than an `analyze.health` call, because the
/// question here is "is the endpoint live". A daemon that is up but degraded —
/// trusty-search down, say — must not be reported as absent and re-spawned on
/// top of itself.
/// What: delegates to [`trusty_common::uds::socket_is_serving`], the shared
/// probe every trusty-* consumer uses.
/// Test: `probe_health_returns_false_for_an_absent_socket`.
pub async fn probe_health(socket: &Path) -> bool {
    trusty_common::uds::socket_is_serving(socket, PROBE_TIMEOUT).await
}

/// Spawn the daemon in the background, returning the child PID.
///
/// Why: invokes `<current_exe> serve` with all stdio null-ed so the daemon
/// outlives the parent process and does not pollute the terminal.
/// What: delegates to `trusty_common::daemon_guard::spawn_current_exe`. No
/// `--socket` is passed: the default path is what every consumer dials, so a
/// spawn that overrode it would start a daemon nothing could find.
/// Test: `handle_start` in `daemon.rs` exercises the same spawn pattern.
fn spawn_daemon() -> Result<u32> {
    trusty_common::daemon_guard::spawn_current_exe(&["serve"])
        .map_err(|e| anyhow!("trusty-analyze daemon spawn failed: {e}"))
}

/// Ensure something is serving `socket`, spawning the daemon if not.
///
/// Why: gives any daemon-requiring command a single shared "boot if absent"
/// path so the user never has to run `trusty-analyze start` first.
/// What: fast-path probes the socket; on miss, checks the PID file to avoid
/// double-spawning a booting daemon, then spawns (or just waits), and polls for
/// up to [`STARTUP_TIMEOUT`].
///
/// # Errors
///
/// When the spawn fails, or when nothing answers inside the budget.
///
/// Test: `ensure_daemon_running_returns_ok_when_something_is_already_serving`.
pub async fn ensure_daemon_running(socket: &Path) -> Result<()> {
    // Fast path: daemon is already up.
    if probe_health(socket).await {
        return Ok(());
    }

    // Check for a stale-but-booting daemon via the PID file before spawning
    // a duplicate.
    let already_running = super::daemon::pid_file_path()
        .ok()
        .and_then(|p| {
            let raw = std::fs::read_to_string(&p).ok()?;
            raw.trim().parse::<u32>().ok()
        })
        .is_some();

    if already_running {
        eprintln!(
            "{} trusty-analyze daemon already starting, waiting for it to become ready…",
            "◉".cyan()
        );
    } else {
        eprintln!("{} Starting trusty-analyze daemon…", "◉".cyan());
        spawn_daemon()?;
    }

    let deadline = Instant::now() + STARTUP_TIMEOUT;
    while Instant::now() < deadline {
        tokio::time::sleep(POLL_INTERVAL).await;
        if probe_health(socket).await {
            return Ok(());
        }
    }
    Err(anyhow!(
        "trusty-analyze did not start serving {} within {}s — run \
         `trusty-analyze serve` in the foreground to see the error",
        socket.display(),
        STARTUP_TIMEOUT.as_secs()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: an absent socket must not return true — if it did, callers would
    /// skip the spawn and then fail on the first real call.
    /// What: probes a path inside an empty temp dir.
    /// Test: this function.
    #[tokio::test]
    async fn probe_health_returns_false_for_an_absent_socket() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let started = Instant::now();
        assert!(!probe_health(&tmp.path().join("absent.sock")).await);
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "probe took too long: {:?}",
            started.elapsed()
        );
    }

    /// Why: the already-healthy path must return early without spawning
    /// anything — a second daemon would fight the first for the socket.
    /// What: binds a real hardened socket and accepts connections, then calls
    /// `ensure_daemon_running` and asserts it returns promptly.
    /// Test: this function.
    #[tokio::test(flavor = "multi_thread")]
    async fn ensure_daemon_running_returns_ok_when_something_is_already_serving() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let socket = tmp.path().join("sockets").join("analyze.sock");
        let listener = trusty_common::uds::bind_hardened(&socket).expect("bind");
        tokio::spawn(async move { while listener.accept().await.is_ok() {} });

        let started = Instant::now();
        ensure_daemon_running(&socket)
            .await
            .expect("a live socket must satisfy the guard");
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "the fast path must not wait: {:?}",
            started.elapsed()
        );
    }
}
