//! Client-side stale-daemon version check for `tm doctor` (issue #2332).
//!
//! Why: split out of `commands::misc` to keep that file under the 500-SLOC
//! production cap (`misc.rs` was already near the ceiling before this
//! addition). This check must live in the `tm` CLI binary — not the daemon's
//! server-side `run_doctor` — because it needs THIS process's own
//! `CARGO_PKG_VERSION`: `tm doctor` always executes as the just-installed
//! binary, so its compiled-in version is the "installed" side of the
//! comparison; the daemon can only ever report on itself.
//! What: [`stale_daemon_check`] folds the caller's `/health` snapshot into a
//! [`trusty_mpm::core::doctor::DoctorCheck`] via
//! [`trusty_mpm::core::version_staleness::check_daemon_version_staleness`].
//! Test: the pure comparison logic is covered by
//! `core::version_staleness`'s own unit tests; the network fetch itself is
//! exercised indirectly by the executor's live-daemon doctor test.

use trusty_mpm::client::HealthSnapshot;
use trusty_mpm::core::doctor::{CheckStatus, DoctorCheck};
use trusty_mpm::core::version_staleness::{CHECK_NAME, check_daemon_version_staleness};

/// Fold the daemon's `/health` version into the #2332 stale-daemon
/// [`DoctorCheck`].
///
/// Why: separates the network fetch (untestable without a live daemon) from
/// the pure comparison in [`trusty_mpm::core::version_staleness`], and keeps
/// `commands::misc::doctor` itself free of the "daemon unreachable" branch. The
/// snapshot is passed IN rather than fetched here (#4230 review) so `tm doctor`
/// samples `/health` exactly once and both client-side checks reason about the
/// same daemon rather than two probes that could straddle a restart.
/// What: on `Some`, compares the snapshot's `version` against this process's own
/// `env!("CARGO_PKG_VERSION")` and resolves the remediation verb via
/// `restart_hint` — which is `launchctl kickstart …` on a host where launchd owns
/// the daemon, because #4230 makes `tm restart` refuse there. On `None` (the fetch
/// failed) returns `Warn`: the daemon is presumably unreachable, which the
/// caller's own `report.checks` output already explains in detail; this just keeps
/// the stale-daemon line from silently vanishing.
/// Test: the comparison logic is covered by `core::version_staleness`'s unit
/// tests; the network fetch itself is exercised indirectly by the executor's
/// live-daemon doctor test.
pub(crate) fn stale_daemon_check(
    snapshot: Option<&HealthSnapshot>,
    restart_hint: &str,
) -> DoctorCheck {
    match snapshot {
        Some(snapshot) => check_daemon_version_staleness(
            env!("CARGO_PKG_VERSION"),
            &snapshot.version,
            restart_hint,
        ),
        None => DoctorCheck::new(
            CHECK_NAME,
            CheckStatus::Warn,
            "could not fetch daemon version from /health".to_string(),
        ),
    }
}
