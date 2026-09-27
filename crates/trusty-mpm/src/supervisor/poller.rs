//! One fleet sweep: detect `stopped` → auto-resume, classify idle `active` sessions.
//!
//! Why: the supervisor's behavior per tick must be a single, testable unit so we
//! can assert "N stopped sessions get resumed" without spinning real timers. This
//! module is that unit — a pure-ish async function over the [`SessionManager`] and
//! an optional classifier, returning a [`TickReport`] of what it did.
//! What: defines [`TickReport`] and [`run_tick`], which (1) lists all sessions,
//! (2) auto-resumes each `stopped` session whose stop nobody asked for when
//! `auto_resume` is set — a session an operator stopped or killed is left down
//! (#6194) — and (3) classifies idle `active` sessions through the activity
//! monitor when a classifier is supplied. It is a passive observer: it never
//! answers a `pending_decision`.
//! Test: `tick_auto_resumes_stopped`, `tick_skips_resume_when_disabled`,
//! `tick_fleet_of_n_resumed`, `tick_classifies_active`,
//! `tick_never_answers_pending_decision`,
//! `tick_never_resumes_a_deliberately_stopped_session`,
//! `tick_still_resumes_a_session_whose_runtime_exited` in `super::tests`.

use std::sync::Arc;

use tracing::{debug, error, info, warn};

use crate::activity::monitor::{ActivityMonitor, LlmClassifier};
use crate::session_manager::manager::ManagedError;
use crate::session_manager::{ManagedSessionState, SessionManager, SessionRecord};

use super::config::SupervisorConfig;

/// What a single fleet sweep observed and changed.
///
/// Why: the loop accumulates these into the supervisor's run stats, and tests
/// assert on them directly; returning a struct (rather than mutating shared
/// state) keeps [`run_tick`] easy to reason about and verify.
/// What: counts of sessions seen, resumed, resume-failures, and classified, plus
/// the ids that were resumed (for logging / assertions).
/// Test: returned and asserted by every `tick_*` test.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct TickReport {
    /// Total session records observed this sweep.
    pub observed: usize,
    /// Stringified ids of sessions auto-resumed this sweep.
    pub resumed: Vec<String>,
    /// Number of resume attempts that failed this sweep.
    pub resume_failures: usize,
    /// Number of idle `active` sessions classified this sweep.
    pub classified: usize,
}

/// Run a single fleet sweep against the session manager.
///
/// Why: this is the supervisor's heartbeat. Factoring it out of the timer loop
/// makes the consequential logic (auto-resume gating, classification) unit-
/// testable with a `crate::session_manager::tests`-style fake tmux driver and a
/// mock classifier, with zero real time elapsed.
/// What: lists all sessions via `mgr.list()`; for each `Stopped` session, resumes
/// it when `cfg.auto_resume` is true AND
/// [`SessionRecord::is_auto_resumable`](crate::session_manager::SessionRecord::is_auto_resumable)
/// agrees the stop was not asked for (#6194 — a deliberately stopped or killed
/// session is left down), counting successes and — on failure —
/// marking the session `Errored` so the fleet snapshot shows it (#5208); for each
/// `Active` session, classifies its pane through `monitor` when `cfg.classify_idle`
/// is true and a `monitor` is supplied. Never touches `pending_decision` — the
/// supervisor surfaces decisions via metrics but never answers them. Returns a
/// [`TickReport`].
/// Test: `tick_auto_resumes_stopped`, `tick_skips_resume_when_disabled`,
/// `tick_fleet_of_n_resumed`, `tick_classifies_active`,
/// `tick_never_answers_pending_decision`;
/// `tick_never_resumes_a_deliberately_stopped_session`,
/// `tick_still_resumes_a_session_whose_runtime_exited` (#6194);
/// `an_already_active_session_is_not_marked_errored_by_a_stale_resume` (#8396,
/// via [`settle_failed_resume`]).
pub async fn run_tick<C: LlmClassifier>(
    mgr: &Arc<SessionManager>,
    cfg: &SupervisorConfig,
    monitor: Option<&ActivityMonitor<C>>,
) -> TickReport {
    // #8233 review round 2 (finding 4): a spec abandoned by a launch nobody was
    // around to finish holds `GH_TOKEN` and `CLAUDE_CODE_OAUTH_TOKEN` in
    // cleartext, and `LaunchSpec::write_in`'s own sweep only runs when a NEXT
    // launch arrives. Tying it to the heartbeat makes reaping depend on the
    // daemon being alive rather than on a launch happening.
    crate::runtime::launch_spec::reap_orphans();
    let records = mgr.list().await;
    let mut report = TickReport {
        observed: records.len(),
        ..Default::default()
    };

    for record in records {
        match record.state {
            // #6194: state alone made every `Stopped` record a relaunch
            // candidate, which respawned sessions the operator had just killed.
            // `is_auto_resumable` reads the recorded stop cause.
            ManagedSessionState::Stopped if cfg.auto_resume && record.is_auto_resumable() => {
                // #6568: `resume_auto`, not `resume` — the automatic path stamps
                // the attempt so the next runtime exit can be attributed to it.
                // `resume` is the operator's entry point and forgives the streak.
                match mgr.resume_auto(&record.id).await {
                    Ok(_) => {
                        info!(
                            id = %record.id,
                            name = %record.tmux_name,
                            "supervisor: auto-resumed stopped session"
                        );
                        report.resumed.push(record.id.to_string());
                    }
                    // #8233 item 4: another path is mid-resume on this session.
                    // Nothing failed, so the tick neither errors the record nor
                    // counts a failure — it leaves the session to the writer
                    // that holds it and looks again next tick.
                    Err(ManagedError::ResumeInFlight(_)) => {
                        info!(
                            id = %record.id,
                            name = %record.tmux_name,
                            "supervisor: another path is resuming this session; skipping this tick"
                        );
                    }
                    // #8233 round 3, finding 3: `resume_auto` already marked the
                    // record errored with this very message. Counting the
                    // failure is right; appending a SECOND `[error: …]` note to
                    // the task is not — `auto_relaunch` hands that task to the
                    // next relaunch.
                    Err(ManagedError::AutoResumeRecorded(msg)) => {
                        error!(
                            id = %record.id,
                            name = %record.tmux_name,
                            "supervisor: auto-resume failed (already recorded on the record): {msg}"
                        );
                        report.resume_failures += 1;
                    }
                    Err(e) => settle_failed_resume(mgr, &record, e, &mut report).await,
                }
            }
            ManagedSessionState::Active if cfg.classify_idle => {
                if let Some(monitor) = monitor
                    && classify_active(mgr, monitor, &record.id, &record.tmux_name).await
                {
                    report.classified += 1;
                }
            }
            _ => {}
        }
    }

    debug!(
        observed = report.observed,
        resumed = report.resumed.len(),
        resume_failures = report.resume_failures,
        classified = report.classified,
        "supervisor: sweep complete"
    );
    report
}

/// Settle an auto-resume that returned an error not already recorded.
///
/// Why (#8396): the sweep resumes what `mgr.list()` read as `Stopped`, but by
/// the time `resume_auto` reads the record another path may have made it
/// `Active`. The manager then refuses with "cannot resume a session in state
/// 'active'", and marking that refusal errored demoted a RUNNING session and
/// appended the message to its task every sweep — two to eight copies were
/// observed. A session that is already active is resume's goal reached.
/// What: when the error is [`ManagedError::InvalidState`] AND a fresh read
/// shows the record `Active`, logs and returns — no errored mark, no failure
/// counted. Every other error, including an `InvalidState` refusal for any
/// other state, keeps the #5208 handling: mark the record `Errored` so the
/// sweep stops retrying it, and count the failure.
/// Test: `an_already_active_session_is_not_marked_errored_by_a_stale_resume`,
/// `a_resume_refusal_for_a_non_active_state_is_still_recorded`,
/// `a_non_state_resume_error_is_still_recorded`.
pub(super) async fn settle_failed_resume(
    mgr: &Arc<SessionManager>,
    record: &SessionRecord,
    e: ManagedError,
    report: &mut TickReport,
) {
    // #8396: a stale `Stopped` read racing a resume that already succeeded.
    if matches!(e, ManagedError::InvalidState(..))
        && mgr
            .get(&record.id)
            .await
            .is_ok_and(|now| now.state == ManagedSessionState::Active)
    {
        info!(
            id = %record.id,
            name = %record.tmux_name,
            "supervisor: session is already active; nothing to resume"
        );
        return;
    }
    error!(
        id = %record.id,
        name = %record.tmux_name,
        "supervisor: auto-resume failed: {e}"
    );
    // #5208: a failed auto-resume must not degrade to a log line while the
    // session stays dead and `Stopped`. Marking it `Errored` puts it in
    // `FleetMetrics.errored` — which drives the console's Degraded health — and
    // stops the sweep silently retrying the same doomed session every interval
    // forever. `resume` still accepts `Errored`, so a manual retry works.
    if let Err(mark_err) = mgr
        .mark_errored(&record.id, &format!("auto-resume failed: {e}"))
        .await
    {
        error!(
            id = %record.id,
            name = %record.tmux_name,
            "supervisor: could not mark failed auto-resume as errored: {mark_err}"
        );
    }
    report.resume_failures += 1;
}

/// Classify one active session's pane through the activity monitor.
///
/// Why: keeping the capture+classify+log sequence in a helper keeps [`run_tick`]
/// readable and lets the error paths (no pane, classifier failure) degrade
/// quietly without aborting the whole sweep — an unattended supervisor must never
/// die because one session's tmux pane was momentarily unreadable.
/// What: captures the last 60 pane lines, runs `monitor.check`, logs the verdict
/// at debug, and returns `true` iff a classification was produced (cache hit or
/// fresh LLM verdict). Capture / classify errors log a warning and return `false`.
/// Test: `tick_classifies_active` (via `run_tick`).
async fn classify_active<C: LlmClassifier>(
    mgr: &Arc<SessionManager>,
    monitor: &ActivityMonitor<C>,
    id: &crate::session_manager::ManagedSessionId,
    tmux_name: &str,
) -> bool {
    let pane = match mgr.capture_pane(id, 60).await {
        Ok(text) => text,
        Err(e) => {
            warn!(id = %id, name = %tmux_name, "supervisor: pane capture failed: {e}");
            return false;
        }
    };
    match monitor.check(&id.to_string(), &pane).await {
        Ok(result) => {
            debug!(
                id = %id,
                name = %tmux_name,
                state = ?result.verdict.state,
                cache_hit = result.cache_hit,
                "supervisor: classified idle session"
            );
            true
        }
        Err(e) => {
            warn!(id = %id, name = %tmux_name, "supervisor: classification failed: {e}");
            false
        }
    }
}
