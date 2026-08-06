//! `tm doctor` managed-session-store integrity probe (#5007).
//!
//! Why: on 2026-08-06 `~/.trusty-mpm/session-manager/sessions.json` was a valid
//! JSON document followed by 1111 stale bytes. Every write path failed, because
//! `SessionStore::upsert` reloads before it saves. Nothing reported it: `tm ls`
//! kept printing a fleet from the daemon's in-memory copy, and `tm doctor` had
//! no opinion about the store at all. The corruption surfaced only because the
//! owner happened to attempt a rename. A store that cannot be written is a
//! diagnosable condition, so it belongs in the diagnostic.
//! What: one read-only `session_store` check that reads the file and reports
//! whether it parses. It never writes, never truncates, and never touches the
//! daemon — the repair is the operator's explicit `tm repair session-store`.
//! Test: `session_store_check_is_ok_for_a_healthy_store`,
//! `session_store_check_fails_on_a_trailing_tail_and_names_the_offset`,
//! `session_store_check_is_ok_when_no_store_exists_yet`,
//! `session_store_check_is_unknown_when_the_file_cannot_be_read`.

use std::path::{Path, PathBuf};

use crate::core::doctor::{CheckStatus, DoctorCheck};
use crate::session_manager::store_integrity::analyze;

/// Where the managed-session store lives under a framework root.
///
/// Why: three call sites (the daemon's `SessionManager`, `tm supervisor`, this
/// probe) derive this path, and a probe that checks a different file than the
/// daemon writes is worse than no probe.
/// What: `<fw_root>/session-manager/sessions.json`.
/// Test: `store_path_is_under_the_session_manager_dir`.
pub(super) fn store_path(fw_root: &Path) -> PathBuf {
    fw_root.join("session-manager").join("sessions.json")
}

/// Probe the managed-session store for corruption.
///
/// Why: see the module doc.
/// What: reads `<fw_root>/session-manager/sessions.json` and classifies it —
/// absent is `Ok` (a machine that has never run a managed session legitimately
/// has no store), parseable is `Ok` with the session count, unparseable is
/// `Fail` naming the byte offset and the repair command, and an unreadable file
/// is `Unknown` rather than `Ok`, because a probe that learned nothing must not
/// report health.
/// Test: `session_store_check_is_ok_for_a_healthy_store`,
/// `session_store_check_fails_on_a_trailing_tail_and_names_the_offset`,
/// `session_store_check_is_ok_when_no_store_exists_yet`,
/// `session_store_check_is_unknown_when_the_file_cannot_be_read`.
pub(super) fn check_session_store(fw_root: &Path) -> DoctorCheck {
    let path = store_path(fw_root);
    let raw = match std::fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return DoctorCheck::new(
                "session_store",
                CheckStatus::Ok,
                format!("no managed-session store yet at {}", path.display()),
            );
        }
        Err(e) => {
            return DoctorCheck::new(
                "session_store",
                CheckStatus::Unknown,
                format!("cannot read {}: {e}", path.display()),
            );
        }
    };
    match serde_json::from_str::<serde_json::Value>(&raw) {
        Ok(v) => {
            let count = v
                .get("sessions")
                .and_then(|s| s.as_object())
                .map(|m| m.len())
                .unwrap_or(0);
            DoctorCheck::new(
                "session_store",
                CheckStatus::Ok,
                format!(
                    "{} parses cleanly — {count} session record(s), {} byte(s)",
                    path.display(),
                    raw.len()
                ),
            )
        }
        // Fail, not Warn: while this holds, EVERY mutation of the store fails,
        // because every write path reloads before it saves.
        Err(e) => DoctorCheck::new(
            "session_store",
            CheckStatus::Fail,
            analyze(&path, &raw, &e).diagnostic(),
        ),
    }
}

#[cfg(test)]
#[path = "doctor_session_store_tests.rs"]
mod doctor_session_store_tests;
