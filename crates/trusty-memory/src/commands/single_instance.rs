//! Single-instance guard for the trusty-memory daemon.
//!
//! Why: macOS launchd `KeepAlive { SuccessfulExit: false }` respawns the daemon
//! whenever it exits with a non-zero code. A second instance that fails to bind
//! exits non-zero, launchd reads that as a crash and spawns another copy, and
//! the resulting zombie herd (69 observed in the wild) exhausts file
//! descriptors on top of the existing fd-limit bug.
//!
//! The fix: before binding, probe the socket. If something is already serving
//! it, exit **0**. Launchd treats exit-0 as a clean shutdown and does not
//! respawn, which collapses the herd on the next invocation without touching
//! the launchd config.
//!
//! #6286 changed what is probed, not the decision. It used to read the
//! `http_addr` discovery file and GET `/health` at whatever address that named
//! — a file that goes stale after a SIGKILL, which is why the probe had to
//! tolerate one pointing at a dead port. The socket path is derived rather than
//! published, so there is nothing to be stale and the probe is a bare connect
//! through `trusty_common::uds::socket_is_serving`.
//!
//! What: [`single_instance_check`] (async, for real daemon startups) and
//! [`StartupAction`] (a pure enum, so the decision is unit-testable without
//! I/O).
//!
//! Test: `startup_action_*` for the decision, `single_instance_check_*` for the
//! probe.

use std::path::Path;
use std::time::Duration;

/// How long a liveness connect may take before the path is called dead.
///
/// A local socket accepts or refuses in microseconds; this is headroom for a
/// loaded machine, not a latency budget. It matches trusty-analyze's
/// `daemon_guard::PROBE_TIMEOUT` so the two daemons wait the same.
const PROBE_TIMEOUT: Duration = Duration::from_millis(500);

/// What the daemon startup should do after the single-instance check.
///
/// Why: separating the decision from the I/O lets us unit-test the logic
/// with injected probe results rather than spinning up real TCP listeners.
/// What: three variants covering the full decision tree.
/// Test: `startup_action_from_probe_result_*` tests in this module.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StartupAction {
    /// Proceed to bind the socket and start serving.
    Proceed,
    /// Another healthy instance is already running — exit 0 cleanly so
    /// launchd does not respawn.
    ExitAlreadyRunning,
    /// A probe attempt failed with an unexpected error — propagate as a startup
    /// failure so the operator sees a real error in the launchd log. Launchd
    /// respawns, correctly, because this is a genuine failure.
    ///
    /// Nothing constructs this today: `socket_is_serving` answers a bool, so
    /// every failure it can have is "nothing is serving". The variant stays
    /// because the caller in `main.rs` branches on it and a future probe that
    /// can distinguish a permission error from an absence has somewhere to
    /// report it.
    Fail(String),
}

/// Decide what to do based on the result of a liveness probe.
///
/// Why: the single-instance check reduces to "did the health probe succeed?".
/// Encoding the decision as a pure function (rather than embedding it in the
/// async probe body) makes the logic unit-testable without actual network I/O.
/// What: `probe_ok = true` → [`StartupAction::ExitAlreadyRunning`];
/// `probe_ok = false` → [`StartupAction::Proceed`].
/// Test: `startup_action_from_probe_result_when_alive`,
///       `startup_action_from_probe_result_when_dead`.
pub fn startup_action_from_probe_result(probe_ok: bool) -> StartupAction {
    if probe_ok {
        StartupAction::ExitAlreadyRunning
    } else {
        StartupAction::Proceed
    }
}

/// Whether `socket` is the path a launchd-managed trusty-memory would serve
/// (#6619).
///
/// Why: every #6619 guard turns on this one question, and getting it wrong in
/// either direction is a bug — too broad refuses a sandboxed daemon that launchd
/// never managed, too narrow lets the orphan back onto the production path. One
/// definition, used by both the caller-side spawn guard and the callee-side bind
/// refusal.
/// What: `socket` must be what [`crate::socket_path`] resolves AND the process
/// must not be running under a `TRUSTY_DATA_DIR_OVERRIDE` sandbox — the plist
/// sets no such override, so a process that has one is by construction not the
/// unit launchd runs.
/// Test: `production_socket_is_the_resolved_path`,
/// `production_socket_is_false_under_a_data_dir_override`.
#[must_use]
pub fn is_production_socket(socket: &Path) -> bool {
    if std::env::var_os(trusty_common::DATA_DIR_OVERRIDE_ENV).is_some() {
        return false;
    }
    crate::socket_path().is_ok_and(|p| p == socket)
}

/// Refuse the production socket when this process is not the launchd unit that
/// owns it (#6619).
///
/// Why: the spawn guard stops a bridge from racing launchd, but only a bridge
/// that runs the fixed code. This is the callee-side half, and it is the one
/// that holds for a daemon started any other way — by hand, by an older bridge,
/// by a script. Binding the production socket unsupervised is what left an
/// orphan without the plist's `FASTEMBED_CACHE_DIR` owning the path while
/// launchd's own instance exited 0 reporting success.
///
/// What: three inputs, and the refusal needs all three.
///
/// - `is_production_socket` — a test or sandbox socket is never launchd's.
/// - `unit_registered` — [`trusty_common::launchd_claim`]'s verdict.
/// - `launchd_runs_us` — a POSITIVE answer from launchd that it does NOT run
///   this PID (`LaunchdSupervision::NotSupervised`), never
///   [`trusty_common::supervision::LaunchdSupervision::Unknown`].
///
/// 🔴 The `Unknown` exclusion is deliberate and is the difference between a
/// guard and an outage. Both signals here read launchd, so a host where
/// `launchctl` cannot be queried would report "unit registered" from the plist
/// on disk and "not supervised" from a failed query — and refuse to start the
/// launchd-supervised daemon on every boot. Refusing only on a positive
/// `NotSupervised` fails closed against the observed defect (an unsupervised
/// spawn on a healthy host) and open against an unreadable launchd.
///
/// Test: `bind_refused_for_an_unsupervised_process_on_a_registered_socket`,
/// `bind_permitted_for_the_launchd_unit_itself`,
/// `bind_permitted_when_no_unit_is_registered`,
/// `bind_permitted_on_a_socket_launchd_does_not_own`,
/// `bind_permitted_when_launchd_cannot_be_asked`.
#[must_use]
pub fn production_bind_refusal(
    label: &str,
    is_production_socket: bool,
    unit_registered: bool,
    supervision: &trusty_common::supervision::LaunchdSupervision,
) -> Option<String> {
    use trusty_common::supervision::LaunchdSupervision;

    let positively_unsupervised = matches!(supervision, LaunchdSupervision::NotSupervised);
    if !(is_production_socket && unit_registered && positively_unsupervised) {
        return None;
    }
    Some(format!(
        "refusing to bind the trusty-memory production socket: launchd unit \
         {label} is registered for it and launchd does not run this process. An \
         unsupervised daemon here starts without the plist's EnvironmentVariables \
         and launchd's own instance then exits 0 reporting success (#6619). Start \
         it with `launchctl kickstart -k gui/$(id -u)/{label}`, or point this \
         process at a different socket"
    ))
}

/// Perform the single-instance check at daemon startup.
///
/// Why: launchd respawns any non-zero exit, so a second instance that fails to
/// bind causes an endless respawn storm. Exiting 0 when another healthy
/// instance is detected short-circuits it.
///
/// What: a bare connect to `socket`. It deliberately does NOT call
/// `memory.health`: the question is whether the endpoint is live, and a daemon
/// that is up but degraded must not be reported absent and spawned on top of
/// itself. An absent or dead socket returns [`StartupAction::Proceed`], so a
/// cold start is never blocked.
///
/// Test: `single_instance_check_proceeds_when_nothing_is_serving`,
/// `single_instance_check_exits_when_something_is_serving`.
pub async fn single_instance_check(socket: &Path) -> StartupAction {
    let probe_ok = trusty_common::uds::socket_is_serving(socket, PROBE_TIMEOUT).await;
    startup_action_from_probe_result(probe_ok)
}

/// Single-instance check with up to `max_retries` additional probes.
///
/// Why (issue #1152, Tier 3): a single probe can miss a daemon that is
/// mid-boot — it has not bound the socket yet. Retrying with a short sleep lets
/// a slow-boot daemon be detected and this caller exit 0, rather than
/// proceeding to open redb and triggering `DatabaseAlreadyOpen`.
/// What: calls `single_instance_check` repeatedly up to `1 + max_retries`
/// times, sleeping `delay_ms` between each call, stopping on the first
/// non-`Proceed` result. Returns the final `StartupAction`.
/// Test: covered by the unit tests for `startup_action_from_probe_result`;
/// the retry path is exercised by the integration guard in `main.rs`.
pub async fn single_instance_check_retried(
    socket: &Path,
    max_retries: u8,
    delay_ms: u64,
) -> StartupAction {
    let mut action = single_instance_check(socket).await;
    let mut retries = max_retries;
    while action == StartupAction::Proceed && retries > 0 {
        retries -= 1;
        tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
        action = single_instance_check(socket).await;
    }
    action
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: when the health probe returns `Some(url)` (daemon is alive),
    /// the startup action must be `ExitAlreadyRunning` so the caller can
    /// exit 0 and stop the launchd respawn storm.
    /// What: asserts the mapping for `probe_ok = true`.
    /// Test: itself (pure function, no I/O).
    #[test]
    fn startup_action_from_probe_result_when_alive() {
        assert_eq!(
            startup_action_from_probe_result(true),
            StartupAction::ExitAlreadyRunning,
            "alive probe → ExitAlreadyRunning"
        );
    }

    /// Why: when the health probe returns `None` (addr file missing, stale,
    /// or daemon not responding), the startup action must be `Proceed` so the
    /// daemon continues with its normal bind sequence.
    /// What: asserts the mapping for `probe_ok = false`.
    /// Test: itself (pure function, no I/O).
    #[test]
    fn startup_action_from_probe_result_when_dead() {
        assert_eq!(
            startup_action_from_probe_result(false),
            StartupAction::Proceed,
            "dead/absent probe → Proceed"
        );
    }

    /// Why: an absent socket means no daemon is running, and the guard must
    /// let the cold start proceed. A guard that reported "already running" for
    /// a path with nothing on it would stop the daemon ever starting.
    /// Test: itself.
    #[tokio::test]
    async fn single_instance_check_proceeds_when_nothing_is_serving() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let action = single_instance_check(&tmp.path().join("absent.sock")).await;
        assert_eq!(
            action,
            StartupAction::Proceed,
            "an absent socket must never block a cold start"
        );
    }

    /// Why: this is the branch that collapses the launchd respawn herd. A live
    /// socket must produce `ExitAlreadyRunning` so the second instance exits 0
    /// rather than failing its bind and being respawned.
    /// Test: itself.
    #[tokio::test(flavor = "multi_thread")]
    async fn single_instance_check_exits_when_something_is_serving() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let socket = tmp.path().join("sockets").join("trusty-memory.sock");
        let listener = trusty_common::uds::bind_hardened(&socket).expect("bind");
        tokio::spawn(async move { while listener.accept().await.is_ok() {} });

        assert_eq!(
            single_instance_check(&socket).await,
            StartupAction::ExitAlreadyRunning,
            "a live socket must stop a second instance from binding"
        );
    }

    use trusty_common::supervision::LaunchdSupervision;

    /// The unit that owns the production socket.
    const LABEL: &str = "com.trusty.memory";

    /// Why (#6619): the observed orphan. A bridge-spawned daemon bound the
    /// production socket while `com.trusty.memory` was mid-restart, without the
    /// plist's `FASTEMBED_CACHE_DIR`, and launchd's own instance then exited 0
    /// reporting success. The callee-side refusal is what holds even for a
    /// daemon started by something this fix did not change.
    /// What: production socket + registered unit + a positive "launchd does not
    /// run this PID" refuses, naming the unit.
    /// Test: itself.
    #[test]
    fn bind_refused_for_an_unsupervised_process_on_a_registered_socket() {
        let refusal =
            production_bind_refusal(LABEL, true, true, &LaunchdSupervision::NotSupervised)
                .expect("an unsupervised bind of a registered socket must be refused");
        assert!(refusal.contains(LABEL), "the unit must be named: {refusal}");
    }

    /// Why: the unit launchd runs IS the legitimate owner. Refusing it would
    /// stop trusty-memory starting on every supervised host — an outage caused
    /// by the guard.
    /// What: a `Supervised` answer permits the bind.
    /// Test: itself.
    #[test]
    fn bind_permitted_for_the_launchd_unit_itself() {
        assert_eq!(
            production_bind_refusal(
                LABEL,
                true,
                true,
                &LaunchdSupervision::Supervised(LABEL.to_owned())
            ),
            None
        );
    }

    /// Why: a dev machine that never installed the service must keep starting
    /// the daemon by hand.
    /// What: no registered unit permits the bind.
    /// Test: itself.
    #[test]
    fn bind_permitted_when_no_unit_is_registered() {
        assert_eq!(
            production_bind_refusal(LABEL, true, false, &LaunchdSupervision::NotSupervised),
            None
        );
    }

    /// Why: a sandboxed daemon under `TRUSTY_DATA_DIR_OVERRIDE` serves a path
    /// launchd never manages, so the guard must not reach it — this is what
    /// keeps the crate's own test daemons startable on an installed host.
    /// What: a non-production socket permits the bind even with a unit
    /// registered.
    /// Test: itself.
    #[test]
    fn bind_permitted_on_a_socket_launchd_does_not_own() {
        assert_eq!(
            production_bind_refusal(LABEL, false, true, &LaunchdSupervision::NotSupervised),
            None
        );
    }

    /// Why: `Unknown` means launchd could not be ASKED, and both of this
    /// guard's other signals read launchd too. Refusing on it would take the
    /// daemon down on every host whose `launchctl list` is unreadable — trading
    /// an orphan for an outage.
    /// What: `Unknown` permits, unlike `NotSupervised`.
    /// Test: itself.
    #[test]
    fn bind_permitted_when_launchd_cannot_be_asked() {
        assert_eq!(
            production_bind_refusal(
                LABEL,
                true,
                true,
                &LaunchdSupervision::Unknown("launchctl timed out".to_owned())
            ),
            None,
            "an unanswerable launchd is not evidence of an orphan"
        );
    }

    /// Why: every #6619 guard turns on this predicate, so the path it accepts
    /// must be the one the daemon actually resolves — not a re-derived guess.
    /// What: the resolved socket is production; a sibling path is not.
    /// Test: itself.
    #[test]
    fn production_socket_is_the_resolved_path() {
        let Ok(resolved) = crate::socket_path() else {
            return; // no home directory in this environment; nothing to assert
        };
        if std::env::var_os(trusty_common::DATA_DIR_OVERRIDE_ENV).is_some() {
            return; // a sibling test set the override; covered below instead
        }
        assert!(is_production_socket(&resolved));
        assert!(!is_production_socket(Path::new("/tmp/not-the-daemon.sock")));
    }

    /// Why: a `TRUSTY_DATA_DIR_OVERRIDE` sandbox is by construction not the unit
    /// launchd runs — the plist sets no override — so the guard must not reach
    /// it whatever launchd has registered.
    /// What: with the override set, even the resolved socket is not production.
    /// Test: itself.
    #[test]
    #[serial_test::serial]
    fn production_socket_is_false_under_a_data_dir_override() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let previous = std::env::var_os(trusty_common::DATA_DIR_OVERRIDE_ENV);
        // SAFETY: serialised by `#[serial]`; no concurrent env access here.
        unsafe { std::env::set_var(trusty_common::DATA_DIR_OVERRIDE_ENV, tmp.path()) };

        let resolved = crate::socket_path();
        let verdict = resolved.as_ref().map(|p| is_production_socket(p));

        // SAFETY: same as above.
        unsafe {
            match previous {
                Some(v) => std::env::set_var(trusty_common::DATA_DIR_OVERRIDE_ENV, v),
                None => std::env::remove_var(trusty_common::DATA_DIR_OVERRIDE_ENV),
            }
        }

        assert_eq!(
            verdict.ok(),
            Some(false),
            "a sandboxed socket is never launchd's production path"
        );
    }
}
