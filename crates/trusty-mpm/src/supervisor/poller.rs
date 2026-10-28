//! The fleet sweeps: auto-resume `stopped` sessions, classify idle `active` ones.
//!
//! Why: each sweep must be a single, testable unit so we can assert "N stopped
//! sessions get resumed" without spinning real timers. This module holds those
//! units — pure-ish async functions over the [`SessionManager`], returning a
//! [`TickReport`] of what they did.
//! What: [`resume_sweep`] auto-resumes each `stopped` session whose stop nobody
//! asked for — a session an operator stopped or killed is left down (#6194).
//! #10356 (owner ruling D74): the DAEMON runs it
//! (`crate::daemon::auto_resume_loop`), because only the daemon installs the
//! runtime relauncher. [`run_tick`] is the `tm supervisor` sweep: it classifies
//! idle `active` sessions through the activity monitor when a classifier is
//! supplied, mirroring a confident `blocked_on_permission` verdict into the
//! record's `blocked_prompt` (#8393). It no longer resumes anything. Neither
//! sweep answers or clears a `pending_decision`.
//! Test: `tick_auto_resumes_stopped`, `tick_skips_resume_when_disabled`,
//! `tick_fleet_of_n_resumed`, `tick_classifies_active`,
//! `tick_never_answers_pending_decision`,
//! `tick_never_resumes_a_deliberately_stopped_session`,
//! `tick_still_resumes_a_session_whose_runtime_exited`,
//! `a_blocked_verdict_raises_a_pending_decision` in `super::tests`.

use std::sync::Arc;

use tracing::{debug, error, info, warn};

use crate::activity::cache::{ActivityState, ActivityVerdict};
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

/// Run one auto-resume sweep against the session manager.
///
/// Why: #10356 (owner ruling D74) — the daemon owns auto-resume, because only
/// the daemon installs the runtime relauncher a resume needs. This is the unit
/// its loop runs, kept free of timers so tests drive it directly.
/// What: lists all sessions; for each `Stopped` one, resumes it when
/// `cfg.auto_resume` is true AND
/// [`SessionRecord::is_auto_resumable`](crate::session_manager::SessionRecord::is_auto_resumable)
/// agrees the stop was not asked for (#6194). See [`resume_one`] for how each
/// outcome is settled. Returns a [`TickReport`] with nothing classified.
/// Test: `tick_auto_resumes_stopped`, `tick_skips_resume_when_disabled`,
/// `tick_fleet_of_n_resumed`, `tick_never_resumes_a_deliberately_stopped_session`,
/// `tick_still_resumes_a_session_whose_runtime_exited` (#6194),
/// `one_failed_auto_resume_appends_exactly_one_error_note`.
pub async fn resume_sweep(mgr: &Arc<SessionManager>, cfg: &SupervisorConfig) -> TickReport {
    // #8233 review round 2 (finding 4): the launch specs this sweep's relaunches
    // write are reaped on the daemon's own heartbeat too.
    crate::runtime::launch_spec::reap_orphans();
    let records = mgr.list().await;
    let mut report = TickReport {
        observed: records.len(),
        ..Default::default()
    };
    if !cfg.auto_resume {
        return report;
    }
    for record in records {
        if record.state == ManagedSessionState::Stopped && record.is_auto_resumable() {
            resume_one(mgr, cfg, &record, &mut report).await;
        }
    }
    debug!(
        observed = report.observed,
        resumed = report.resumed.len(),
        resume_failures = report.resume_failures,
        "auto-resume: sweep complete"
    );
    report
}

/// Auto-resume one eligible `Stopped` session and settle the outcome.
///
/// What: calls `resume_auto` (#6568: the automatic path stamps the breaker),
/// bounded by `cfg.step_timeout` (#8335; an expired resume counts as a failure
/// and is retried next sweep). `Ok` counts a resume. `ResumeInFlight` (#8233)
/// is skipped without a failure. `AutoResumeRecorded` is counted but not
/// re-marked. Any other error goes to [`settle_failed_resume`].
/// Test: as [`resume_sweep`].
async fn resume_one(
    mgr: &Arc<SessionManager>,
    cfg: &SupervisorConfig,
    record: &SessionRecord,
    report: &mut TickReport,
) {
    let resume = mgr.resume_auto(&record.id);
    let Some(outcome) = step_bounded(cfg, "resume_auto", record, resume).await else {
        report.resume_failures += 1;
        return;
    };
    match outcome {
        Ok(_) => {
            info!(
                id = %record.id,
                name = %record.tmux_name,
                "auto-resume: resumed stopped session; launch verified"
            );
            report.resumed.push(record.id.to_string());
        }
        // #8233 item 4: another path is mid-resume on this session.
        // Nothing failed, so the sweep neither errors the record nor counts a
        // failure — it leaves the session to the writer that holds it.
        Err(ManagedError::ResumeInFlight(_)) => {
            info!(
                id = %record.id,
                name = %record.tmux_name,
                "auto-resume: another path is resuming this session; skipping this sweep"
            );
        }
        // #8233 round 3, finding 3: `resume_auto` already marked the record
        // errored with this very message. Counting the failure is right;
        // appending a SECOND `[error: …]` note to the task is not.
        Err(ManagedError::AutoResumeRecorded(msg)) => {
            error!(
                id = %record.id,
                name = %record.tmux_name,
                "auto-resume: failed (already recorded on the record): {msg}"
            );
            report.resume_failures += 1;
        }
        Err(e) => settle_failed_resume(mgr, record, e, report).await,
    }
}

/// Run a single `tm supervisor` sweep against the session manager.
///
/// Why: this is the supervisor's heartbeat. Factoring it out of the timer loop
/// makes classification unit-testable with a fake tmux driver and a mock
/// classifier, with zero real time elapsed.
/// What: lists all sessions via `mgr.list()`; for each `Active` session,
/// classifies its pane through `monitor` when `cfg.classify_idle` is true and a
/// `monitor` is supplied. #10356 (owner ruling D74): it no longer resumes a
/// `Stopped` session — [`resume_sweep`] in the daemon does. Never answers or
/// clears a `pending_decision`; it mirrors the classifier's verdict into
/// `blocked_prompt` (#8393, [`record_blocked_prompt`]), which the metrics
/// surface. Returns a [`TickReport`]. #8335: each classification is bounded by
/// `cfg.step_timeout` ([`step_bounded`]).
/// Test: `tick_classifies_active`, `tick_never_answers_pending_decision`,
/// `the_supervisor_tick_no_longer_resumes`.
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
        if record.state == ManagedSessionState::Active
            && cfg.classify_idle
            && let Some(monitor) = monitor
        {
            // #8335: bounded per session, like `resume_auto` in `resume_one`.
            let classify = classify_active(mgr, monitor, &record.id, &record.tmux_name);
            if step_bounded(cfg, "classify", &record, classify).await == Some(true) {
                report.classified += 1;
            }
        }
    }

    debug!(
        observed = report.observed,
        classified = report.classified,
        "supervisor: sweep complete"
    );
    report
}

/// Await one per-session sweep step for at most `cfg.step_timeout` (#8335).
///
/// Why: one session's hung step must not use up the whole sweep's bound and
/// starve the sessions after it.
/// What: `None` on expiry, after logging the session id, name and step at
/// `error`. The bound fires at an await point — a slow LLM reply, a held store
/// lock. A step blocked inside a tmux call is caught by the whole-sweep bound
/// instead ([`super::watchdog::DetachedSweep`]). A dropped `resume_auto`
/// releases its in-flight marker (an RAII guard), so the next sweep may retry.
/// Test: `a_hung_classify_step_is_abandoned_and_the_sweep_moves_on`.
async fn step_bounded<F: std::future::Future>(
    cfg: &SupervisorConfig,
    step: &str,
    record: &SessionRecord,
    fut: F,
) -> Option<F::Output> {
    let out = tokio::time::timeout(cfg.step_timeout, fut).await.ok();
    if out.is_none() {
        error!(
            id = %record.id,
            name = %record.tmux_name,
            step,
            limit_secs = cfg.step_timeout.as_secs(),
            "sweep step did not finish within its bound; abandoned it and \
             moved to the next session (#8335)"
        );
    }
    out
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
            "auto-resume: session is already active; nothing to resume"
        );
        return;
    }
    error!(
        id = %record.id,
        name = %record.tmux_name,
        "auto-resume: failed: {e}"
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
            "auto-resume: could not mark a failed resume as errored: {mark_err}"
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
/// at debug, hands it to [`record_blocked_prompt`], and returns `true` iff a
/// classification was produced (cache hit or fresh LLM verdict) and any
/// decision it owed was recorded. Capture / classify errors log a warning and
/// return `false`.
/// Test: `tick_classifies_active` (via `run_tick`),
/// `a_failed_decision_write_is_not_counted_and_is_retried`.
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
            // #8393: a confident blocked verdict surfaces as the record's
            // pending decision; an unpersisted one is not counted as handled.
            record_blocked_prompt(mgr, id, tmux_name, &result.verdict).await
        }
        Err(e) => {
            warn!(id = %id, name = %tmux_name, "supervisor: classification failed: {e}");
            false
        }
    }
}

/// Lowest classifier confidence at which a `BlockedOnPermission` verdict is
/// raised as a pending decision (#8393). A reply with no confidence parses as
/// 0.5 and stays below it.
const BLOCKED_CONFIDENCE_FLOOR: f32 = 0.7;

/// Decision text used when a blocked verdict carries an empty summary.
const BLOCKED_FALLBACK_QUESTION: &str = "session is blocked on a permission prompt (classifier)";

/// Mirror the classifier's verdict into the session's `blocked_prompt` (#8393).
///
/// Why: `pending_decision` was set only by the `[Y/n]` / `(yes/no)` regex
/// parser, so a free-text "which option?" prompt the LLM classifier caught
/// never reached `supervisor-metrics.json` or `supervisor_status`. The verdict
/// goes in its own `blocked_prompt` field, not `pending_decision`: that is the
/// front gate's hold, and a classifier-written one made `resume`, the answer
/// route and reconcile refuse the session.
/// What: for a `BlockedOnPermission` verdict at or above
/// [`BLOCKED_CONFIDENCE_FLOOR`], stores the verdict summary (or a fixed
/// fallback) through [`SessionManager::sync_blocked_prompt`], replacing a
/// different earlier text; a repeat of the same text writes nothing. Any other
/// verdict, or a blocked one below the floor, clears the field, because the
/// pane no longer shows the prompt that was raised. The setter takes one store
/// guard and writes only an `Active` record. Nothing answers the field: it
/// goes when the pane stops reading as blocked, and the metrics hide it once
/// the session is not `Active`. Returns `false` when the write fails, after
/// logging at `error`, so the sweep does not count the session as classified;
/// the next sweep retries from the verdict cache. A clear on a cache hit is as
/// safe as on a fresh verdict: an unchanged pane that last read as not blocked
/// cannot own a blocked prompt.
/// Test: `a_blocked_verdict_raises_a_pending_decision`,
/// `a_fresh_verdict_clears_or_replaces_the_raised_prompt`,
/// `a_low_confidence_or_unblocked_verdict_raises_nothing`,
/// `a_failed_decision_write_is_not_counted_and_is_retried`,
/// `a_raised_blocked_verdict_does_not_wedge_stop_and_resume`.
async fn record_blocked_prompt(
    mgr: &Arc<SessionManager>,
    id: &crate::session_manager::ManagedSessionId,
    tmux_name: &str,
    verdict: &ActivityVerdict,
) -> bool {
    let blocked = verdict.state == ActivityState::BlockedOnPermission
        && verdict.confidence >= BLOCKED_CONFIDENCE_FLOOR;
    let summary = verdict.summary.trim();
    let question = match (blocked, summary.is_empty()) {
        (false, _) => None,
        (true, true) => Some(BLOCKED_FALLBACK_QUESTION),
        (true, false) => Some(summary),
    };
    match mgr.sync_blocked_prompt(id, question).await {
        Ok(changed) => {
            if changed && question.is_some() {
                info!(id = %id, name = %tmux_name, "supervisor: surfaced blocked session prompt");
            }
            true
        }
        Err(e) => {
            error!(id = %id, name = %tmux_name, "supervisor: blocked prompt not recorded: {e}");
            false
        }
    }
}
