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
//! #6624 adds the exclusion trusty-memory's `ensure_daemon_running` gained in
//! #6619: the PID-file check below coordinates this daemon's OWN bridges with
//! each other (whoever wrote `daemon.pid` is presumably already spawning) and
//! cannot see launchd. ADR-0032 retired `com.trusty.analyze` — a client starts
//! the daemon on demand and it exits on its own idle window — so today's
//! ordinary host has no plist and [`socket_owner_for`] answers
//! [`SocketOwner::OnDemand`], a pure no-op for this fix. A host that still
//! carries a pre-#6350 plist (an upgrade that has not yet evicted it) is the
//! case this guards: during that unit's `bootout`/`bootstrap` window the
//! socket is transiently unserved, and spawning onto it races the very unit
//! that is coming back for the same path. See [`ensure_daemon_running`].
//!
//! Test: `probe_health_returns_false_for_an_absent_socket`,
//! `ensure_daemon_running_returns_ok_when_something_is_already_serving`,
//! `ensure_daemon_running_defers_to_a_launchd_unit_instead_of_spawning`,
//! `ensure_daemon_running_spawns_when_no_launchd_unit_owns_the_socket`.
//!
//! Note: only call this from commands that *require* the daemon. Commands like
//! `start`, `stop`, `serve`, `service`, and `completions` deliberately do not
//! call this guard.

use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context as _, Result};
use colored::Colorize;
use trusty_common::launchd_claim::SocketOwner;

/// How long a liveness connect may take before it is called dead.
///
/// A local socket accepts or refuses in microseconds; this is headroom for a
/// loaded machine, not a latency budget.
const PROBE_TIMEOUT: Duration = Duration::from_millis(500);

/// How long to wait for a freshly-spawned daemon to answer.
const STARTUP_TIMEOUT: Duration = Duration::from_secs(30);

/// How often to re-probe while waiting.
const POLL_INTERVAL: Duration = Duration::from_millis(500);

/// How long to wait for a launchd-supervised daemon to come back (#6624).
///
/// Matches `trusty_common::shutdown::TERMINATION_GRACE_SECS`, the window a
/// launchd-managed daemon is allowed to spend exiting during a restart —
/// waiting less would give up while a legitimate bootout/bootstrap was still
/// in progress.
const LAUNCHD_RESTART_TIMEOUT: Duration =
    Duration::from_secs(trusty_common::shutdown::TERMINATION_GRACE_SECS);

/// Whether `socket` is trusty-analyze's canonical production path (#6624).
///
/// Why: mirrors trusty-memory's `commands::single_instance::is_production_socket`
/// — the launchd question only applies to the one path a fresh install ever
/// bootstraps. A test temp-dir socket, or a `TRUSTY_DATA_DIR_OVERRIDE` sandbox,
/// is never that path even on a host where the retired `com.trusty.analyze`
/// plist is still installed.
/// What: false whenever the override env var is set; otherwise compares
/// `socket` against `trusty_analyze::service::socket_path()`.
fn is_production_socket(socket: &Path) -> bool {
    if std::env::var_os(trusty_common::DATA_DIR_OVERRIDE_ENV).is_some() {
        return false;
    }
    trusty_analyze::service::socket_path().is_ok_and(|p| p == socket)
}

/// Whether launchd owns the socket this guard is about to spawn onto (#6624).
///
/// Why: the answer turns on the socket, not just the host — see
/// [`is_production_socket`].
/// What: asks [`trusty_common::launchd_claim`] about
/// [`trusty_common::launchd_labels::ANALYZE`] (retired but still the label an
/// upgrade must evict, per that constant's own doc).
/// Test: the decision is covered by trusty-common's `socket_owner_*`; both
/// branches of the caller by `ensure_daemon_running_defers_to_a_launchd_unit_\
/// instead_of_spawning` and `ensure_daemon_running_spawns_when_no_launchd_unit_\
/// owns_the_socket`.
fn socket_owner_for(socket: &Path) -> SocketOwner {
    trusty_common::launchd_claim::launchd_socket_owner(
        trusty_common::launchd_labels::ANALYZE,
        is_production_socket(socket),
    )
}

/// Wait for a launchd-supervised daemon to serve `socket` again (#6624).
///
/// Why: launchd is bringing this daemon back. Spawning our own in the gap
/// produces a second process racing for the same path, and this bridge has no
/// way to tell which one launchd's own instance will find already bound.
/// What: polls until something serves, bounded by `wait`. The error names the
/// unit, so an operator knows to look at launchd rather than at this bridge.
///
/// # Errors
///
/// When nothing serves the socket within `wait`.
async fn await_launchd_daemon(socket: &Path, label: &str, wait: Duration) -> Result<()> {
    eprintln!(
        "{} Waiting for launchd unit {label} to serve trusty-analyze…",
        "◉".cyan()
    );
    let deadline = Instant::now() + wait;
    while Instant::now() < deadline {
        tokio::time::sleep(POLL_INTERVAL).await;
        if probe_health(socket).await {
            return Ok(());
        }
    }
    Err(anyhow!(
        "launchd unit {label} owns {} but nothing served it within {}s. Not \
         spawning a second daemon onto that path while launchd's own instance \
         may still be starting (#6624). Check `launchctl print \
         gui/$(id -u)/{label}`, or evict the retired unit with `launchctl \
         bootout gui/$(id -u)/{label}` if it should not be there at all",
        socket.display(),
        wait.as_secs()
    ))
}

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
/// What: delegates to [`ensure_daemon_running_with`] with the real launchd
/// verdict, wait, and spawn.
///
/// # Errors
///
/// When the spawn fails, or when nothing answers inside the budget.
///
/// Test: `ensure_daemon_running_returns_ok_when_something_is_already_serving`.
pub async fn ensure_daemon_running(socket: &Path) -> Result<()> {
    ensure_daemon_running_with(
        socket,
        socket_owner_for(socket),
        LAUNCHD_RESTART_TIMEOUT,
        pid_file_shows_a_starting_daemon,
        spawn_daemon,
    )
    .await
}

/// Whether the PID file names a daemon that is presumably already booting.
///
/// Why: split out so a test can inject a fixed answer instead of reading the
/// real `~/.trusty-analyze/daemon.pid` — a file this process does not own and
/// whose presence on the host running the test is not the thing under test
/// (#6624 regression coverage hit exactly this: a stale PID file left by an
/// unrelated run made the guard wait instead of spawning, in a temp-dir socket
/// scenario that has no PID file of its own to check).
/// What: true iff the PID file exists and its contents parse as a `u32`. Does
/// NOT check the PID is actually alive — this only coordinates against a
/// bridge that is concurrently spawning, not against a stale leftover.
fn pid_file_shows_a_starting_daemon() -> bool {
    super::daemon::pid_file_path()
        .ok()
        .and_then(|p| {
            let raw = std::fs::read_to_string(&p).ok()?;
            raw.trim().parse::<u32>().ok()
        })
        .is_some()
}

/// [`ensure_daemon_running`] with the launchd verdict, the wait, the PID-file
/// check, and the spawn supplied by the caller (#6624).
///
/// Why: the branch that must never be taken while launchd owns the socket is
/// the SPAWN, so proving it is not taken means being able to hand in a spawn
/// that panics if called. Reading launchd and forking a real daemon are both
/// untestable in process; injecting them — and the PID-file read, which
/// otherwise touches the real `$HOME` — is what makes the exclusion an
/// assertion rather than a claim.
///
/// What: identical to [`ensure_daemon_running`] up to the point of spawning. A
/// [`SocketOwner::Launchd`] verdict diverts to [`await_launchd_daemon`] before
/// `already_running` is even consulted, and `spawn` is never called.
///
/// # Errors
///
/// As [`ensure_daemon_running`], plus the launchd-wait timeout.
///
/// Test: `ensure_daemon_running_defers_to_a_launchd_unit_instead_of_spawning`,
/// `ensure_daemon_running_spawns_when_no_launchd_unit_owns_the_socket`.
pub(crate) async fn ensure_daemon_running_with(
    socket: &Path,
    owner: SocketOwner,
    launchd_wait: Duration,
    already_running: impl FnOnce() -> bool,
    spawn: impl FnOnce() -> Result<u32>,
) -> Result<()> {
    // Fast path: daemon is already up.
    if probe_health(socket).await {
        return Ok(());
    }

    // #6624: the PID-file check below coordinates this daemon's bridges with
    // each other, never with launchd. An unserved socket during a retired
    // unit's bootout/bootstrap window is launchd's restart, not an absent
    // daemon — wait for it rather than racing a second process onto its path.
    if let SocketOwner::Launchd { label } = &owner {
        return await_launchd_daemon(socket, label, launchd_wait).await;
    }

    // Check for a stale-but-booting daemon via the PID file before spawning
    // a duplicate.
    if already_running() {
        eprintln!(
            "{} trusty-analyze daemon already starting, waiting for it to become ready…",
            "◉".cyan()
        );
    } else {
        eprintln!("{} Starting trusty-analyze daemon…", "◉".cyan());
        spawn().context("spawning the trusty-analyze daemon")?;
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

    /// Why (#6624): this is the defect. A retired `com.trusty.analyze` unit
    /// left loaded by a pre-#6350 install boots the socket out transiently, and
    /// the pre-fix guard read that as licence to spawn a second, unsupervised
    /// process racing the unit that is coming back. The spawn here panics if
    /// reached, so passing proves the exclusion rather than describing it.
    /// What: a `Launchd` verdict over a socket nothing serves waits, never
    /// spawns, and errors naming the unit.
    /// Test: itself.
    #[tokio::test(flavor = "multi_thread")]
    async fn ensure_daemon_running_defers_to_a_launchd_unit_instead_of_spawning() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let err = ensure_daemon_running_with(
            &tmp.path().join("absent.sock"),
            SocketOwner::Launchd {
                label: "com.trusty.analyze".to_owned(),
            },
            Duration::from_millis(600),
            || unreachable!("the launchd branch returns before checking the PID file"),
            || unreachable!("a launchd-owned socket must never be spawned onto"),
        )
        .await
        .expect_err("a unit that never comes back is an error, not a spawn");

        assert!(
            err.to_string().contains("com.trusty.analyze"),
            "the owning unit must be named: {err}"
        );
    }

    /// Why: the exclusion must not reach the ordinary host — ADR-0032 retired
    /// `com.trusty.analyze`, so a fresh install has no plist and the #5267
    /// on-demand contract (a client spawns the daemon, it exits on its own
    /// idle window) must keep working exactly as before.
    /// What: an `OnDemand` verdict reaches the spawn, which stands a listener
    /// up on the socket; the readiness wait then succeeds exactly as it does
    /// for a real daemon.
    /// Test: itself.
    #[tokio::test(flavor = "multi_thread")]
    async fn ensure_daemon_running_spawns_when_no_launchd_unit_owns_the_socket() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let socket = tmp.path().join("sockets").join("analyze.sock");
        let spawned = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = std::sync::Arc::clone(&spawned);
        let bind_at = socket.clone();

        ensure_daemon_running_with(
            &socket,
            SocketOwner::OnDemand,
            Duration::from_millis(600),
            // #6624 regression: this must be injected, not the real PID-file
            // read — a stale `~/.trusty-analyze/daemon.pid` left by an
            // unrelated process on the machine running this test would
            // otherwise report "already starting" and this assertion would
            // time out without `spawn` ever running.
            || false,
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
