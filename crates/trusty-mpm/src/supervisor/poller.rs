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
use crate::session_manager::{ManagedSessionState, SessionManager};

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
/// `tick_still_resumes_a_session_whose_runtime_exited` (#6194).
pub async fn run_tick<C: LlmClassifier>(
    mgr: &Arc<SessionManager>,
    cfg: &SupervisorConfig,
    monitor: Option<&ActivityMonitor<C>>,
) -> TickReport {
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
                match mgr.resume(&record.id).await {
                    Ok(_) => {
                        info!(
                            id = %record.id,
                            name = %record.tmux_name,
                            "supervisor: auto-resumed stopped session"
                        );
                        report.resumed.push(record.id.to_string());
                    }
                    Err(e) => {
                        error!(
                            id = %record.id,
                            name = %record.tmux_name,
                            "supervisor: auto-resume failed: {e}"
                        );
                        // #5208: a failed auto-resume must not degrade to a log line
                        // while the session stays dead and `Stopped`. Marking it
                        // `Errored` puts it in `FleetMetrics.errored` — which drives
                        // the console's Degraded health — and stops the sweep silently
                        // retrying the same doomed session every interval forever.
                        // `resume` still accepts `Errored`, so a manual retry works.
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
