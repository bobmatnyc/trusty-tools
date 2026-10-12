//! `tm doctor` row for channel taint files (#8433 P0).
//!
//! Why: a taint is retired only when its managed session ends, so a file that
//! outlives its session, a hook-side re-apply after a daemon failure (D09), and
//! a double re-apply failure each need an operator's eye. The row reports
//! them; it never deletes anything, because deleting a taint is an untaint.
//! What: [`check_channel_taint`] reads the taint directory under the home and
//! the manager's records, and [`channel_taint_row`] grades them: `Unknown`
//! when the directory cannot be listed; `Fail` when the hook recorded a double
//! failure; `Warn` naming each orphaned file or marker (its managed session is
//! missing, terminal or deliberately stopped, so its retire did not complete),
//! each failed re-apply and each unreadable file; `Ok` otherwise.
//! Test: `doctor_lists_orphan_taint_files_and_deletes_none`,
//! `doctor_row_is_unknown_when_taint_cannot_be_listed`,
//! `failed_retire_at_deliberate_stop_keeps_taint_and_doctor_reports_it`.

use std::collections::HashSet;
use std::path::Path;

use crate::core::channels_taint;
use crate::core::doctor::{CheckStatus, DoctorCheck};
use crate::session_manager::{ManagedSessionState, SessionManager, StopCause};

/// The row's name.
pub(crate) const NAME: &str = "channel_taint";

/// Grade the taint directory under the process home against `mgr`'s records.
pub(crate) async fn check_channel_taint(mgr: &SessionManager) -> DoctorCheck {
    let Some(home) = dirs::home_dir() else {
        return DoctorCheck::new(NAME, CheckStatus::Unknown, "the home directory is unknown");
    };
    check_channel_taint_in(mgr, &channels_taint::taint_dir(&home)).await
}

/// [`check_channel_taint`] for the taint directory `dir`.
///
/// What: a managed session is live unless it is terminal or was stopped
/// deliberately. A deliberate stop ends the session and retires its taint
/// (Dfc), so a taint that outlives one is a retire that failed (critic MEDIUM).
/// Test: `failed_retire_at_deliberate_stop_keeps_taint_and_doctor_reports_it`.
pub(crate) async fn check_channel_taint_in(mgr: &SessionManager, dir: &Path) -> DoctorCheck {
    let live: HashSet<String> = mgr
        .list()
        .await
        .into_iter()
        .filter(|r| {
            let ended = r.state == ManagedSessionState::Stopped
                && r.stop_cause == Some(StopCause::Deliberate);
            !r.state.is_terminal() && !ended
        })
        .map(|r| r.id.to_string())
        .collect();
    channel_taint_row(dir, &live)
}

/// The row for the taint directory `dir`, given the live managed ids.
///
/// Test: `doctor_lists_orphan_taint_files_and_deletes_none`,
/// `doctor_row_is_unknown_when_taint_cannot_be_listed`.
pub(crate) fn channel_taint_row(dir: &Path, live: &HashSet<String>) -> DoctorCheck {
    let (files, marks) = match (
        channels_taint::list_taints(dir),
        channels_taint::marked_managed(dir),
    ) {
        (Ok(files), Ok(marks)) => (files, marks),
        (Err(e), _) | (_, Err(e)) => {
            return DoctorCheck::new(
                NAME,
                CheckStatus::Unknown,
                format!("cannot list taint: {e}"),
            );
        }
    };
    let mut lines = Vec::new();
    for (path, parsed) in &files {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy())
            .unwrap_or_default();
        match parsed {
            Err(why) => lines.push(format!("{name}: unreadable ({why}); reads tainted")),
            // #8433 fix round: an owner-less one (the marks could not be
            // listed) is never retired; the operator removes it.
            Ok(r) if r.channel == channels_taint::REAPPLY_FAILED_CHANNEL => {
                let owner = match r.managed_session.as_str() {
                    "" => "no owner, so no retire removes it".to_owned(),
                    m => format!("retired with managed session {m}"),
                };
                lines.push(format!(
                    "{name}: failed re-apply, tainted by the SessionStart hook ({owner})"
                ));
            }
            Ok(r) if !live.contains(&r.managed_session) => lines.push(format!(
                "{name}: orphaned, managed session {} is gone or ended (a retire that did not \
                 complete)",
                r.managed_session
            )),
            Ok(_) => {}
        }
    }
    for managed in marks.iter().filter(|m| !live.contains(*m)) {
        lines.push(format!(
            "managed/{managed}.json: orphaned mark, the session is gone or ended"
        ));
    }
    // #8433 fix round: the record sits in the anchored taint dir.
    let failures = std::fs::read_to_string(dir.join(channels_taint::REAPPLY_FAILURES_FILE))
        .ok()
        .map_or(0, |text| {
            text.lines().filter(|l| !l.trim().is_empty()).count()
        });
    let held = files.len();
    let tail = format!("Nothing was deleted; see {}.", dir.display());
    if failures > 0 {
        return DoctorCheck::new(
            NAME,
            CheckStatus::Fail,
            format!(
                "{failures} SessionStart(s) could not re-apply a channel taint at all (D09 double \
                 failure; those sessions may read untainted): see {}. {}",
                channels_taint::REAPPLY_FAILURES_FILE,
                lines.join("; ")
            ),
        );
    }
    if lines.is_empty() {
        let msg = format!("{held} taint file(s), each owned by a live managed session");
        return DoctorCheck::new(NAME, CheckStatus::Ok, msg);
    }
    DoctorCheck::new(
        NAME,
        CheckStatus::Warn,
        format!("{} {tail}", lines.join("; ")),
    )
}

#[cfg(test)]
#[path = "doctor_channel_taint_tests.rs"]
mod tests;
