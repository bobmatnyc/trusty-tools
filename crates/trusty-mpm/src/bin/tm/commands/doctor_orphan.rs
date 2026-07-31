//! Client-side orphan-daemon check for `tm doctor` (issue #4230).
//!
//! Why: every existing daemon probe treats a 200 from `/health` as proof the
//! stack is fine. #4230 is the case where that is exactly wrong: an orphaned
//! 1.0.2 daemon (PID 98606, PPID 1, `cwd=$HOME`) held :7880 for two days while
//! launchd's `com.trusty.mpm` reported `state = not running`, answering every
//! request successfully. A fresh signed install plus behavioural verification
//! all passed against a binary the install had just replaced, and `launchctl
//! bootout` was a silent no-op because launchd did not own the listener. The
//! `supervised` flag that distinguishes this state has been on the wire since
//! #2486 — nothing ever read it, so the substitution stayed invisible. A check
//! that cannot fail when the thing it checks is broken has no value.
//!
//! What: [`orphan_daemon_check`] fetches the running daemon's `/health` and
//! combines its `supervised` flag with a LOCAL probe for a registered trusty-mpm
//! launchd unit ([`crate::commands::launchd_probe::mpm_launchd_plist_exists`]).
//! Both inputs come from production code, and the failing combination is the
//! same one [`crate::commands::launchd_probe::compute_supervised`] already
//! defines as hazardous — so this is not a restatement of a constant.
//!
//! This lives in the `tm` CLI binary rather than the daemon's server-side
//! `run_doctor` for the same reason as the #2332 stale-daemon check
//! (`doctor_stale`): the answer depends on state outside the responding process.
//! Only a client can compare "who answered" against "who launchd was told to
//! run", and in the #4230 state the daemon that answers is precisely the one
//! whose self-report cannot be trusted to be the supervised one.
//!
//! Deliberately STATIC — no spawn, no `launchctl` shell-out, no port scan: one
//! `/health` GET the caller already makes plus a `Path::exists`. That is enough
//! to catch the whole failure CLASS at zero cost on the one code path operators
//! reach for when things are already broken.
//!
//! Test: `crates/trusty-mpm/src/bin/tm/commands/doctor_orphan_tests.rs`.

use trusty_mpm::client::DaemonClient;
use trusty_mpm::core::doctor::{CheckStatus, DoctorCheck};

/// Name of this check as it appears in `tm doctor` output.
pub(crate) const CHECK_NAME: &str = "daemon_orphan";

/// Fetch `/health` and fold its `supervised` flag into the #4230 orphan check.
///
/// Why: separates the network fetch (untestable without a live daemon) from the
/// pure verdict in [`verdict`], and keeps `commands::misc::doctor` free of the
/// "daemon unreachable" branch.
/// What: probes for a registered trusty-mpm launchd unit locally, then calls
/// `daemon.health_snapshot()`. On success, delegates to [`verdict`]. On a
/// transport failure returns `Unknown` — the report's own checks already explain
/// an unreachable daemon in detail, and an undetermined check must not read as a
/// pass (issue #4005 precedent).
/// Test: the verdict logic is covered by `doctor_orphan_tests.rs`; the network
/// fetch is exercised indirectly by the executor's live-daemon doctor test.
pub(crate) async fn orphan_daemon_check(daemon: &DaemonClient) -> DoctorCheck {
    let plist_exists = crate::commands::launchd_probe::mpm_launchd_plist_exists();
    match daemon.health_snapshot().await {
        Ok(snapshot) => verdict(plist_exists, snapshot.supervised, &snapshot.version),
        Err(e) => DoctorCheck::new(
            CHECK_NAME,
            CheckStatus::Unknown,
            format!("could not fetch /health to check daemon supervision: {e}"),
        ),
    }
}

/// Pure verdict: is the daemon answering `/health` an unsupervised orphan?
///
/// Why: separated from [`orphan_daemon_check`] so the FAIL branch is directly
/// testable. A test that could only call the wrapper would depend on the host's
/// real `~/Library/LaunchAgents` and its real daemon, so it would assert the
/// happy path and silently lose the regression guard — the same shape of blind
/// spot #4230 is about.
/// What: `Fail` exactly when a launchd unit is registered AND the responding
/// daemon reports `supervised: false` — the #2486 orphan signature, restated on
/// the client side because the pre-#4230 client discarded the flag. The message
/// names the listener-identification command, the kill, and the launchctl
/// restart, in the order an operator must run them. `Ok` otherwise: no unit
/// registered means a bare dev daemon (not hazardous), and a supervised daemon
/// is the intended state.
/// Test: `fails_when_a_launchd_unit_exists_and_the_daemon_is_unsupervised`,
/// `ok_when_supervised`, `ok_when_no_launchd_unit_is_registered`,
/// `failure_names_the_pid_lookup_the_kill_and_the_launchctl_restart`,
/// `failure_is_a_hard_fail_not_a_warn`, `failure_names_the_serving_version`.
pub(crate) fn verdict(plist_exists: bool, supervised: bool, version: &str) -> DoctorCheck {
    if !plist_exists {
        return DoctorCheck::new(
            CHECK_NAME,
            CheckStatus::Ok,
            "no trusty-mpm launchd unit is registered — an unsupervised daemon is the \
             expected arrangement on this host",
        );
    }
    if supervised {
        return DoctorCheck::new(
            CHECK_NAME,
            CheckStatus::Ok,
            format!(
                "the daemon answering /health (version {}) is launchd-supervised",
                display_version(version)
            ),
        );
    }
    DoctorCheck::new(
        CHECK_NAME,
        CheckStatus::Fail,
        format!(
            "a trusty-mpm launchd unit is registered, but the daemon answering /health \
             (version {}) reports `supervised: false` — an ORPHAN process owns the port \
             while launchd's job is down. It answers 200, so a fresh install and its \
             verification both pass against the STALE binary the orphan is still serving, \
             and `launchctl bootout` is a no-op against a process launchd does not own \
             (issue #4230). Fix: `lsof -nP -iTCP -sTCP:LISTEN | grep tm` to find the PID, \
             `kill -TERM <pid>`, then \
             `launchctl kickstart -k gui/$(id -u)/com.trusty.mpm`.",
            display_version(version)
        ),
    )
}

/// Render a `/health` version for an operator-facing message.
///
/// Why: a daemon predating #2332 omits `version`, which arrives here as an empty
/// string; printing `version ` with nothing after it reads as a formatting bug
/// and hides that the build is unidentified — the exact ambiguity #2332 fixed.
/// What: returns the version unchanged, or `"unknown"` when it is empty.
/// Test: `unknown_version_is_labelled_not_blank`.
fn display_version(version: &str) -> &str {
    if version.is_empty() {
        "unknown"
    } else {
        version
    }
}

#[cfg(test)]
#[path = "doctor_orphan_tests.rs"]
mod tests;
