//! `tm doctor` row for channel taint files (#8433 P0).
//!
//! Why: a taint is retired only when its managed session ends, so a file that
//! outlives its session, a hook-side re-apply after a daemon failure (D09), and
//! a double re-apply failure each need an operator's eye. The row reports
//! them; it never deletes anything, because deleting a taint is an untaint.
//! What: [`check_channel_taint`] reads the taint directory under the home and
//! the manager's records, and [`channel_taint_row`] grades them: `Unknown`
//! when the directory, the delivery trace or the question log cannot be read;
//! `Fail` when the hook recorded a double failure, or (#9873) an inbound
//! answer was resolved but its delivery failed, never settled, or left no
//! line; `Warn` naming each orphaned file or marker (its managed session is
//! missing, terminal or deliberately stopped, so its retire did not complete),
//! each failed re-apply and each unreadable file; `Ok` otherwise.
//! Test: `doctor_lists_orphan_taint_files_and_deletes_none`,
//! `doctor_row_is_unknown_when_taint_cannot_be_listed`,
//! `failed_retire_at_deliberate_stop_keeps_taint_and_doctor_reports_it`,
//! `delivery_failed_line_is_a_doctor_fail_naming_question_and_asker`.

use std::collections::HashSet;
use std::path::Path;

use crate::core::channels_delivery;
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
    // #9873 (Q5, F18): an unreadable delivery trace or log is never `Ok`.
    let undelivered = match delivery_problems_in(dir) {
        Ok(problems) => problems,
        Err(e) => {
            return DoctorCheck::new(
                NAME,
                CheckStatus::Unknown,
                format!("cannot read the channel answer delivery trace or question log: {e}"),
            );
        }
    };
    let held = files.len();
    let tail = format!("Nothing was deleted; see {}.", dir.display());
    if failures > 0 || !undelivered.is_empty() {
        let mut parts = Vec::new();
        if !undelivered.is_empty() {
            // #9873 (AC2, Q4): a resolved answer is final; name each loss.
            parts.push(format!(
                "{} channel answer(s) resolved but not confirmed delivered (final, #8433 Q4; see \
                 {}): {}.",
                undelivered.len(),
                channels_delivery::DELIVERY_FILE,
                undelivered.join("; ")
            ));
        }
        if failures > 0 {
            parts.push(format!(
                "{failures} SessionStart(s) could not re-apply a channel taint at all (D09 double \
                 failure; those sessions may read untainted): see {}.",
                channels_taint::REAPPLY_FAILURES_FILE
            ));
        }
        if !lines.is_empty() {
            parts.push(lines.join("; "));
        }
        return DoctorCheck::new(NAME, CheckStatus::Fail, parts.join(" "));
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

/// The delivery problems in taint directory `dir` (#9873 Q5, Q4a).
///
/// What: the trace's lines against the answered questions of the question
/// log beside `dir` (`channels/questions.jsonl`; [`channels_taint::taint_dir`]
/// is `channels/taint`). Either read failing is an `Err`.
/// Test: `delivery_pending_without_settle_is_a_doctor_fail`,
/// `resolved_question_without_a_delivery_line_is_a_doctor_fail`,
/// `doctor_row_is_unknown_when_delivery_log_unreadable`.
fn delivery_problems_in(dir: &Path) -> Result<Vec<String>, String> {
    let lines = channels_delivery::read_deliveries(dir).map_err(|e| e.to_string())?;
    let resolved = answered_beside(dir)?;
    Ok(channels_delivery::delivery_problems(&lines, &resolved))
}

/// The answered questions (id, asker) in the question log beside `dir`.
#[cfg(feature = "channels")]
fn answered_beside(dir: &Path) -> Result<Vec<(String, String)>, String> {
    let log = dir.with_file_name("questions.jsonl");
    crate::channels::question_table::answered_in_log(&log)
        .map(|found| {
            found
                .into_iter()
                .map(|(id, asker)| (id.to_string(), asker))
                .collect()
        })
        .map_err(|e| e.to_string())
}

/// A build without the `channels` feature holds no question table.
#[cfg(not(feature = "channels"))]
fn answered_beside(_dir: &Path) -> Result<Vec<(String, String)>, String> {
    Ok(Vec::new())
}

#[cfg(test)]
#[path = "doctor_channel_taint_tests.rs"]
mod tests;
