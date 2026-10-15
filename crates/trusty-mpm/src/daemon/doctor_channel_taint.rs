//! `tm doctor` row for channel taint files (#8433 P0).
//!
//! Why: a taint is retired only when its managed session ends, so a file that
//! outlives its session, a hook-side re-apply after a daemon failure (D09), and
//! a double re-apply failure each need an operator's eye. The row reports
//! them; it never deletes anything, because deleting a taint is an untaint.
//! What: [`check_channel_taint`] reads the taint directory under the home and
//! the manager's records, and [`channel_taint_row`] grades them: `Unknown`
//! when the directory cannot be listed; `Fail` when the hook recorded a
//! double failure or (#9877) its record exists but cannot be read, or
//! (#9873) an inbound answer was resolved but its delivery
//! failed, never settled, or left no line, or a trace line is malformed;
//! `Unknown` when the delivery trace or the question log cannot be read and
//! nothing grades `Fail` (the unreadable part is named either way, #9873 fix
//! round); `Warn` naming each orphaned file or marker (its managed session is
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
    let (failures, d09_unreadable) = reapply_failures_in(dir);
    // #9873 (Q5, F18): an unreadable delivery trace or log is never `Ok`;
    // #9873 fix round: it adds a part and never lowers a `Fail`.
    let (undelivered, mut unreadable) = delivery_problems_in(dir);
    let held = files.len();
    let tail = format!("Nothing was deleted; see {}.", dir.display());
    // #9877: a D09 record that exists but cannot be read may hold failures,
    // so it grades `Fail` like one that names them.
    let d09_fail = d09_unreadable.is_some();
    unreadable.extend(d09_unreadable);
    if failures > 0 || d09_fail || !undelivered.is_empty() {
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
        parts.extend(unreadable);
        if !lines.is_empty() {
            parts.push(lines.join("; "));
        }
        return DoctorCheck::new(NAME, CheckStatus::Fail, parts.join(" "));
    }
    if !unreadable.is_empty() {
        let mut parts = unreadable;
        if !lines.is_empty() {
            parts.push(lines.join("; "));
        }
        return DoctorCheck::new(NAME, CheckStatus::Unknown, parts.join(" "));
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

/// The D09 double failures recorded in taint directory `dir`, and the reason
/// the record cannot be read, if it cannot.
///
/// Why (#9877): the hook writes the record only on a double failure, so one
/// that exists but cannot be read or decoded must never count as none.
/// What: an absent record is `(0, None)`; otherwise the non-blank lines.
/// Any other read error, a non-UTF-8 record included, is `(0, Some(why))`.
/// Test: `unreadable_reapply_failures_record_is_never_ok`.
fn reapply_failures_in(dir: &Path) -> (usize, Option<String>) {
    let path = dir.join(channels_taint::REAPPLY_FAILURES_FILE);
    match std::fs::read_to_string(&path) {
        Ok(text) => (text.lines().filter(|l| !l.trim().is_empty()).count(), None),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (0, None),
        Err(e) => (
            0,
            Some(format!(
                "The D09 re-apply failures record {} is unreadable ({e}); a SessionStart may \
                 have left a session reading untainted.",
                channels_taint::REAPPLY_FAILURES_FILE
            )),
        ),
    }
}

/// The delivery problems in taint directory `dir` (#9873 Q5, Q4a).
///
/// What: the trace's lines against the answered questions of the question
/// log beside `dir` (`channels/questions.jsonl`; [`channels_taint::taint_dir`]
/// is `channels/taint`), plus each malformed trace line; then one sentence
/// per read that failed (#9873 fix round). An unreadable trace yields no
/// problems, since every resolved question would read unrecorded; an
/// unreadable log still yields the trace's own problems.
/// Test: `delivery_pending_without_settle_is_a_doctor_fail`,
/// `resolved_question_without_a_delivery_line_is_a_doctor_fail`,
/// `doctor_row_is_unknown_when_delivery_log_unreadable`,
/// `unreadable_trace_never_hides_a_doctor_fail`.
fn delivery_problems_in(dir: &Path) -> (Vec<String>, Vec<String>) {
    let trace = match channels_delivery::read_deliveries(dir) {
        Ok(trace) => trace,
        Err(e) => {
            let why = format!(
                "The channel answer delivery trace is unreadable ({e}); delivery state is unknown."
            );
            return (Vec::new(), vec![why]);
        }
    };
    let mut unreadable = Vec::new();
    let resolved = answered_beside(dir).unwrap_or_else(|e| {
        unreadable.push(format!(
            "The question log is unreadable ({e}); a resolved answer with no delivery record \
             cannot be listed."
        ));
        Vec::new()
    });
    let mut problems = channels_delivery::delivery_problems(&trace.lines, &resolved);
    problems.extend(trace.malformed);
    (problems, unreadable)
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
