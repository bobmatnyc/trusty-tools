//! `tm doctor` session-claude registry probe (#8980).
//!
//! Why: a `session-claudes.json` the daemon cannot trust SEALS the registry
//! (#8531): no owning session may repair its own delegation records,
//! `set_pid` refuses every session, and the reaper skips every session. The
//! daemon logs this once, at load, and nothing else reports it. The daemon
//! reads the file only at start, so it stays sealed until it restarts, even
//! after the file is fixed.
//! What: one read-only `session_claudes` check that reads the file through
//! the daemon's own trust rules (`session_claudes::read_registry`). Absent is
//! `Ok`; trusted is `Ok` with the entry counts; anything the daemon would
//! refuse — unreadable, corrupt, foreign-owned, open to other users — is
//! `Warn`, never `Ok`. It never writes the file. The daemon's own doctor route
//! then overrides the row with the running daemon's seal
//! ([`apply_daemon_seal`]), since a fixed file does not unseal a daemon.
//! Test: `doctor_session_claudes_tests.rs`.

use std::path::Path;

use crate::core::doctor::{CheckStatus, DoctorCheck, DoctorReport};
use crate::daemon::state::session_claudes::{
    Announcement, SESSION_CLAUDES_FILE, current_uid, read_registry,
};

/// The row's name.
const NAME: &str = "session_claudes";

/// Probe the session-claude registry under `fw_root` as the daemon reads it.
///
/// Why: see the module doc.
/// What: [`check_session_claudes_as`] for this process's uid.
/// Test: `doctor_session_claudes_tests.rs`.
pub(super) fn check_session_claudes(fw_root: &Path) -> DoctorCheck {
    check_session_claudes_as(fw_root, current_uid())
}

/// [`check_session_claudes`] trusting a file only when `uid` owns it.
///
/// What: `Ok` when no file exists (no session has announced itself over the
/// socket yet), `Ok` naming the bound and unproven counts when the file is
/// trusted, and `Warn` naming the refusal when it is not. The `Warn` text
/// says the daemon stays sealed until restart even once the file is fixed.
/// Test: `a_missing_registry_is_ok_8980`, `a_trusted_registry_is_ok_8980`,
/// `a_corrupt_registry_warns_and_names_the_restart_8980`,
/// `a_foreign_owned_registry_warns_8980`, `an_open_registry_warns_8980`.
pub(super) fn check_session_claudes_as(fw_root: &Path, uid: u32) -> DoctorCheck {
    let path = fw_root.join(SESSION_CLAUDES_FILE);
    // #8980: `read_registry` reads NotFound as empty; only a path with nothing
    // there at all, not even a dangling symlink, is "absent".
    if let Err(e) = std::fs::symlink_metadata(&path)
        && e.kind() == std::io::ErrorKind::NotFound
    {
        return DoctorCheck::new(
            NAME,
            CheckStatus::Ok,
            format!(
                "no session-claude registry yet at {} — it is written at the first \
                 SessionStart over the daemon socket (this describes the file, not the \
                 running daemon)",
                path.display()
            ),
        );
    }
    match read_registry(&path, uid) {
        Ok(map) => {
            let bound = map
                .values()
                .filter(|a| matches!(a, Announcement::Claude(_)))
                .count();
            DoctorCheck::new(
                NAME,
                CheckStatus::Ok,
                format!(
                    "{} is trusted — {bound} session(s) bound to their claude, {} unproven \
                     (this describes the file, not the running daemon)",
                    path.display(),
                    map.len() - bound
                ),
            )
        }
        Err(why) => DoctorCheck::new(
            NAME,
            CheckStatus::Warn,
            format!(
                "the session-claude registry is not trusted, so a daemon that loads it is \
                 SEALED: no session can repair its own delegation records, `set_pid` is \
                 refused for every session, and the reaper skips every session. {why}. Fix \
                 or remove the file, then restart the daemon (`tm restart`): the daemon reads \
                 it only at start and stays sealed until restart, even after the file is fixed \
                 (#8980)"
            ),
        ),
    }
}

/// Override the file-read `session_claudes` row with the running daemon's seal.
///
/// Why: the daemon reads the registry only at start, so a file fixed or
/// removed after a sealed load reads `Ok` while the daemon stays sealed
/// (#8980). Only the daemon's own route holds that state; the CLI fallback
/// keeps the file-only row.
/// What: when `sealed` is `Some(why)`, replaces the row with a `Warn` naming
/// `why` and re-folds `overall`. `None` leaves the report untouched.
/// Test: `a_sealed_daemon_warns_on_the_doctor_route_after_the_file_is_fixed_8980`.
pub fn apply_daemon_seal(report: &mut DoctorReport, sealed: Option<String>) {
    let Some(why) = sealed else { return };
    let row = DoctorCheck::new(
        NAME,
        CheckStatus::Warn,
        format!(
            "this daemon loaded an untrusted file and stays sealed until restart: {why}. \
             Restart the daemon (`tm restart`) to reload the file (#8980)"
        ),
    );
    match report.checks.iter_mut().find(|c| c.name == NAME) {
        Some(check) => *check = row,
        None => report.checks.push(row),
    }
    report.overall = report
        .checks
        .iter()
        .fold(CheckStatus::Ok, |acc, c| acc.worst(c.status));
}

#[cfg(test)]
#[path = "doctor_session_claudes_tests.rs"]
mod doctor_session_claudes_tests;
