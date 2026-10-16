//! Is a daemon alive behind a socket that refused the guard's dial? (#6288)
//!
//! Why: `ECONNREFUSED` is ambiguous on a unix socket. A stale socket file left
//! by a dead daemon refuses, and on macOS so does a live daemon whose accept
//! backlog is full. Reading every refusal as "nothing listening" let a
//! saturated daemon open the shared-tree guard. Only the daemon's own record
//! can tell the two apart.
//! What: [`daemon_live_for`] reads the lock file (`~/.trusty-mpm/daemon.lock`,
//! written by the daemon with its pid and socket path) through the same
//! [`lock_verdict`](super::guided_autostart_plan::lock_verdict) the autostart
//! uses. Pid reuse is ruled out by that verdict, which asks the process table
//! what the lock's pid is running (argv: a `tm`/`trusty-mpm` binary on the
//! `daemon` subcommand): an unrelated program on a reused pid is not a daemon.
//! Fail direction: an argv that cannot be read counts as live, so the guard
//! denies rather than allows (the owner's "no reliable signal means deny").
//! Test: `pm_guard_daemon_live_tests.rs`.

use std::path::Path;

use super::daemon_pid_identity::PidIdentity;
use super::guided_autostart_plan::{IdentifyPid, LockVerdict, lock_verdict};
use super::guided_liveness::lock_names_target;

/// Does the lock at `lock_path` name a live daemon serving `url`?
///
/// Why: see the module doc; pure over its two host probes so the pid-reuse and
/// unreadable-argv arms are testable without a real process.
/// What: false when the lock is absent, not ours, names another socket, or its
/// pid is dead. A live pid whose argv is a daemon is live. A live pid whose
/// argv is positively another program is a reused pid and is NOT a daemon. A
/// live pid whose argv cannot be read is live: it cannot be ruled out.
/// Test: `a_refused_dial_with_a_live_daemon_denies` (live and unreadable argv),
/// `a_refused_dial_over_a_reused_pid_allows`, `a_stale_socket_with_no_live_daemon_allows`,
/// `a_lock_for_another_socket_or_a_foreign_file_is_not_live`.
pub(crate) fn daemon_live_at(url: &str, lock_path: &Path, identify: IdentifyPid<'_>) -> bool {
    let Some(lock) = std::fs::read_to_string(lock_path)
        .ok()
        .as_deref()
        .and_then(trusty_mpm::core::daemon_identity::parse_lock)
    else {
        return false;
    };
    // #6288: a lock for another socket says nothing about this one.
    if !lock_names_target(url, &lock) {
        return false;
    }
    match lock_verdict(lock_path, identify) {
        LockVerdict::Absent | LockVerdict::Stale(_) => false,
        LockVerdict::LiveDaemon(_) => true,
        LockVerdict::Unverified(_, PidIdentity::NotDaemon) => false,
        LockVerdict::Unverified(..) => true,
    }
}

/// [`daemon_live_at`] against the real lock file and process table.
///
/// Test: covered through `daemon_live_at`; the real probes are the ones
/// `guided_liveness::picker_or_autostart` already uses.
pub(crate) fn daemon_live_for(url: &str) -> bool {
    daemon_live_at(
        url,
        &trusty_mpm::core::lock_file_path(),
        &super::daemon_pid_identity::pid_identity,
    )
}

#[cfg(test)]
#[path = "pm_guard_daemon_live_tests.rs"]
mod tests;
