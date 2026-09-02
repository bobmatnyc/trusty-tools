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
//! #6619 adds the exclusion trusty-analyze still does not need: the spawn is
//! also serialised against LAUNCHD, because the `flock` above coordinates
//! bridges with each other and cannot see a supervised unit. See
//! [`ensure_daemon_running`].
//!
//! Test: `probe_returns_false_for_an_absent_socket`,
//! `ensure_daemon_running_returns_early_when_something_is_serving`,
//! `ensure_daemon_running_defers_to_a_launchd_unit_instead_of_spawning`,
//! `ensure_daemon_running_spawns_when_no_launchd_unit_owns_the_socket`.

use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context as _, Result};
use colored::Colorize;
use trusty_common::launchd_claim::SocketOwner;
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

/// How long to wait for a launchd-supervised daemon to come back (#6619).
///
/// Why: a `bootout`/`bootstrap` cycle leaves the socket unserved for a moment,
/// and a bridge that waits it out gets the correctly-configured daemon launchd
/// is bringing up. The bound matches
/// `trusty_common::shutdown::TERMINATION_GRACE_SECS`, the window the outgoing
/// daemon is allowed to spend exiting — waiting less would give up while the
/// restart was still legitimately in progress.
const LAUNCHD_RESTART_TIMEOUT: Duration =
    Duration::from_secs(trusty_common::shutdown::TERMINATION_GRACE_SECS);

/// Whether launchd owns the socket this guard is about to spawn onto (#6619).
///
/// Why: the answer turns on the socket, not just the host. A test or a
/// `TRUSTY_DATA_DIR_OVERRIDE` sandbox resolves a different path, and a daemon
/// launchd does not serve must keep the on-demand spawn.
/// What: asks [`trusty_common::launchd_claim`] about `com.trusty.memory`, having
/// first established whether `socket` IS this daemon's canonical production
/// path.
/// Test: the decision is covered by trusty-common's `socket_owner_*`; both
/// branches of the caller by `ensure_daemon_running_defers_to_a_launchd_unit_\
/// instead_of_spawning` and `ensure_daemon_running_spawns_when_no_launchd_unit_\
/// owns_the_socket`.
fn socket_owner_for(socket: &Path) -> SocketOwner {
    trusty_common::launchd_claim::launchd_socket_owner(
        trusty_common::launchd_labels::MEMORY,
        super::single_instance::is_production_socket(socket),
    )
}

/// Wait for a launchd-supervised daemon to serve `socket` again (#6619).
///
/// Why: launchd is bringing this daemon back with the plist's
/// `EnvironmentVariables`. Spawning our own in the gap produces a daemon without
/// them holding the path launchd's instance wants — which is the defect, and it
/// reports as success on both sides.
/// What: polls until something serves, bounded by `wait`. The error names the
/// unit, so an operator knows to look at launchd rather than at this bridge.
///
/// # Errors
///
/// When nothing serves the socket within `wait`.
async fn await_launchd_daemon(socket: &Path, label: &str, wait: Duration) -> Result<()> {
    eprintln!(
        "{} Waiting for launchd unit {label} to serve trusty-memory…",
        "◉".cyan()
    );
    let deadline = Instant::now() + wait;
    while Instant::now() < deadline {
        tokio::time::sleep(POLL_INTERVAL).await;
        if probe(socket).await {
            return Ok(());
        }
    }
    Err(anyhow!(
        "launchd unit {label} owns {} but nothing served it within {}s. Not \
         spawning an unsupervised daemon onto that path — it would start without \
         the plist's environment and launchd's own instance would then exit 0 \
         reporting success (#6619). Check `launchctl print gui/$(id -u)/{label}` \
         and the daemon's launchd stderr log",
        socket.display(),
        wait.as_secs()
    ))
}

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
    ensure_daemon_running_with(
        socket,
        lock_path,
        socket_owner_for(socket),
        LAUNCHD_RESTART_TIMEOUT,
        spawn_daemon,
    )
    .await
}

/// [`ensure_daemon_running`] with the launchd verdict, the wait, and the spawn
/// supplied by the caller (#6619).
///
/// Why: the branch that must never be taken on a supervised host is the SPAWN,
/// so proving it is not taken means being able to hand in a spawn that panics if
/// called. Reading launchd and forking a real daemon are both untestable in
/// process; injecting them is what makes the exclusion an assertion rather than
/// a claim.
///
/// What: identical to [`ensure_daemon_running`] up to the point of spawning. A
/// [`SocketOwner::Launchd`] verdict diverts to [`await_launchd_daemon`] and
/// `spawn` is never called.
///
/// # Errors
///
/// As [`ensure_daemon_running`], plus the launchd-wait timeout.
///
/// Test: `ensure_daemon_running_defers_to_a_launchd_unit_instead_of_spawning`,
/// `ensure_daemon_running_spawns_when_no_launchd_unit_owns_the_socket`.
pub(crate) async fn ensure_daemon_running_with(
    socket: &Path,
    lock_path: &Path,
    owner: SocketOwner,
    launchd_wait: Duration,
    spawn: impl FnOnce() -> Result<u32>,
) -> Result<()> {
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

    // #6619: the lock above serialises this bridge against other bridges, never
    // against launchd. An unserved socket during a bootout/bootstrap window is
    // launchd's restart, not an absent daemon — wait for it rather than spawning
    // an unsupervised one onto its path.
    if let SocketOwner::Launchd { label } = &owner {
        return await_launchd_daemon(socket, label, launchd_wait).await;
    }

    eprintln!("{} Starting trusty-memory daemon…", "◉".cyan());
    spawn()?;

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

    /// Why (#6619): this is the defect. With `com.trusty.memory` between
    /// `bootout` and `bootstrap` the socket is transiently unserved, and the
    /// pre-fix guard read that as licence to spawn — producing an unsupervised
    /// daemon on the production socket without the plist's
    /// `FASTEMBED_CACHE_DIR`, after which launchd's own instance exited 0
    /// reporting success. The spawn here panics if reached, so passing proves
    /// the exclusion rather than describing it.
    /// What: a `Launchd` verdict over a socket nothing serves waits, never
    /// spawns, and errors naming the unit.
    /// Test: itself.
    #[tokio::test(flavor = "multi_thread")]
    async fn ensure_daemon_running_defers_to_a_launchd_unit_instead_of_spawning() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let err = ensure_daemon_running_with(
            &tmp.path().join("absent.sock"),
            &tmp.path().join("start.lock"),
            SocketOwner::Launchd {
                label: "com.trusty.memory".to_owned(),
            },
            Duration::from_millis(600),
            || unreachable!("a launchd-owned socket must never be spawned onto"),
        )
        .await
        .expect_err("a unit that never comes back is an error, not a spawn");

        assert!(
            err.to_string().contains("com.trusty.memory"),
            "the owning unit must be named: {err}"
        );
    }

    /// Why: the exclusion must not reach a dev machine that never installed the
    /// service — the #5267 on-demand contract is the whole reason the bridge
    /// works there.
    /// What: an `OnDemand` verdict reaches the spawn, which stands a listener up
    /// on the socket; the readiness wait then succeeds exactly as it does for a
    /// real daemon.
    /// Test: itself.
    #[tokio::test(flavor = "multi_thread")]
    async fn ensure_daemon_running_spawns_when_no_launchd_unit_owns_the_socket() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let socket = tmp.path().join("sockets").join("trusty-memory.sock");
        let spawned = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = std::sync::Arc::clone(&spawned);
        let bind_at = socket.clone();

        ensure_daemon_running_with(
            &socket,
            &tmp.path().join("start.lock"),
            SocketOwner::OnDemand,
            Duration::from_millis(600),
            move || {
                flag.store(true, std::sync::atomic::Ordering::SeqCst);
                let listener = trusty_common::uds::bind_hardened(&bind_at)?;
                tokio::spawn(async move { while listener.accept().await.is_ok() {} });
                Ok(4242)
            },
        )
        .await
        .expect("an unmanaged host spawns and the daemon comes up");

        assert!(
            spawned.load(std::sync::atomic::Ordering::SeqCst),
            "an unmanaged host must still get its on-demand spawn"
        );
    }
}
