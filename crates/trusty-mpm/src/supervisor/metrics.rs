//! Fleet metrics: lifecycle-state counts, pending decisions, last-activity, run stats.
//!
//! Why: the whole point of an unattended supervisor is that a human (or a
//! higher-level fleet manager) can glance at fleet state without attaching to any
//! session. The supervisor surfaces that state over `/metrics`, so it needs a
//! serializable snapshot type computed purely from the session records plus the
//! supervisor's own run counters.
//! What: defines [`FleetMetrics`] (counts by [`ManagedSessionState`], surfaced
//! `pending_decisions`, last-activity timestamp, and cumulative supervisor run
//! stats) and [`PendingDecision`]; [`FleetMetrics::from_records`] derives the
//! per-state snapshot from a slice of [`SessionRecord`]s.
//! #8394 adds each live session's latest context figures and one account-wide
//! plan value, read from the statusline's per-session store by
//! [`attach_context_usage`].
//! Test: `metrics_counts_by_state`, `metrics_surfaces_pending_decisions`,
//! `metrics_last_activity_is_max` in `super::tests`; the #8394 fields in
//! `metrics_context_tests.rs`.

use std::path::Path;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::core::context_usage::{ContextUsage, PlanUsage, read_context_usage};
use crate::session_manager::{ManagedSessionState, SessionRecord};

/// One live session's latest context figures, as [`FleetMetrics`] reports them
/// (#8394).
///
/// Why: the poller keys its view by the tmux name it already shows.
/// What: the managed id, the tmux name, and the attached [`ContextUsage`].
/// Test: `fleet_metrics_report_each_live_sessions_context`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionContextUsage {
    /// Stringified managed session id.
    pub session_id: String,
    /// Friendly tmux name for the session.
    pub tmux_name: String,
    /// The session's latest reading.
    pub context: ContextUsage,
}

/// Attach each record's latest context reading from `root`'s usage store
/// (#8394).
///
/// Why: the figures live in the statusline's per-session file, keyed by the
/// Claude session id, not in the session store; a reader that wants them on
/// [`SessionRecord::context_usage`] calls this on its own copy of the records.
/// What: sets `context_usage` to [`read_context_usage`] for the record's
/// `claude_session_id` (stale mark computed at `now`), or `None` when the
/// record has no Claude session id or no reading exists. Never writes.
/// Test: `attach_reads_the_reading_keyed_by_claude_session_id`.
pub fn attach_context_usage(records: &mut [SessionRecord], root: &Path, now: DateTime<Utc>) {
    let _ = (records, root, now, read_context_usage);
}

/// A pending decision surfaced (not answered) by the supervisor.
///
/// Why: the supervisor is a passive OBSERVER — it never auto-answers a
/// `pending_decision`. Instead it surfaces each one so a human or fleet manager
/// can act. This struct is the wire shape for one such surfaced decision.
/// What: the managed session id, the human-readable tmux name, the decision text,
/// and the harness's proposed default (if any).
/// Test: `metrics_surfaces_pending_decisions`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingDecision {
    /// Stringified managed session id the decision belongs to.
    pub session_id: String,
    /// Friendly tmux name for the session (operator-facing).
    pub tmux_name: String,
    /// The decision question the harness is blocked on.
    pub question: String,
    /// The harness's proposed default answer, if it supplied one.
    pub proposed_default: Option<String>,
}

/// Cumulative counters for the supervisor's own activity over a run.
///
/// Why: operators need to confirm the supervisor is actually doing work (and how
/// much) — how many sweeps it has run, how many sessions it has auto-resumed, and
/// how many idle sessions it has classified — to trust an unattended process.
/// What: monotonically-increasing counters updated by the loop after each sweep.
/// Test: `metrics_run_stats_accumulate` (driven through the loop in `super::tests`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SupervisorRunStats {
    /// Number of fleet sweeps completed since the supervisor started.
    pub sweeps: u64,
    /// Number of `stopped` sessions auto-resumed across all sweeps.
    pub auto_resumed: u64,
    /// Number of resume attempts that failed across all sweeps.
    pub resume_failures: u64,
    /// Number of idle `active` sessions classified across all sweeps.
    pub classified: u64,
    /// #8335: fleet sweeps that did not complete — abandoned at `tick_timeout`,
    /// or skipped because an abandoned one was still running. Not in `sweeps`.
    #[serde(default)]
    pub sweeps_abandoned: u64,
    /// #8335: fleet sweeps abandoned in a row since the last completed one; a
    /// completed sweep resets it. Non-zero makes the reader report `stale`.
    #[serde(default)]
    pub consecutive_sweeps_abandoned: u64,
    /// #8335: post-merge cleanup sweeps abandoned at `tick_timeout`, or
    /// skipped because an abandoned one was still running.
    #[serde(default)]
    pub cleanup_sweeps_abandoned: u64,
}

/// A serializable snapshot of fleet state for the `/metrics` endpoint.
///
/// Why: a single JSON object that a dashboard, `curl`, or fleet manager can poll
/// gives full visibility into the unattended fleet — counts by lifecycle state,
/// the decisions waiting on a human, the freshest activity, and what the
/// supervisor itself has done.
/// What: per-[`ManagedSessionState`] counts, the total, the surfaced
/// `pending_decisions`, the maximum `last_activity_at` across the fleet, and the
/// supervisor's [`SupervisorRunStats`].
/// #8394: plus each live session's latest context figures and the newest
/// account-wide plan figures. Not `Eq`, since those figures are `f64`.
/// Test: `metrics_counts_by_state`, `metrics_surfaces_pending_decisions`,
/// `metrics_last_activity_is_max`, `fleet_metrics_report_each_live_sessions_context`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct FleetMetrics {
    /// Sessions in `provisioning`.
    pub provisioning: u64,
    /// Sessions in `active`.
    pub active: u64,
    /// Sessions in `stopped` (resumable).
    pub stopped: u64,
    /// Sessions in `errored`.
    pub errored: u64,
    /// Sessions in `decommissioned` (tombstones).
    pub decommissioned: u64,
    /// Total session records (including tombstones).
    pub total: u64,
    /// Decisions waiting on a human, surfaced (never auto-answered).
    pub pending_decisions: Vec<PendingDecision>,
    /// The most recent `last_activity_at` across the whole fleet, if any.
    pub last_activity_at: Option<DateTime<Utc>>,
    /// The supervisor's own cumulative run statistics.
    pub run_stats: SupervisorRunStats,
    /// #8394: every non-terminal session with an attached context reading.
    /// `#[serde(default)]` loads an older snapshot as empty.
    #[serde(default)]
    pub context_usage: Vec<SessionContextUsage>,
    /// #8394: the plan figures from the newest reading that carried any. Plan
    /// limits are account-wide, so one value stands for the fleet.
    #[serde(default)]
    pub plan_usage: Option<PlanUsage>,
}

impl FleetMetrics {
    /// Derive the per-state snapshot from a slice of session records.
    ///
    /// Why: the supervisor recomputes fleet state from the authoritative session
    /// store on every sweep and on every `/metrics` request; this pure function
    /// keeps that derivation in one tested place, independent of I/O.
    /// What: tallies records by [`ManagedSessionState`], collects every record
    /// carrying a `pending_decision` into a [`PendingDecision`] (else an `Active`
    /// record's classifier `blocked_prompt`, #8393), and tracks the
    /// maximum `last_activity_at`. Leaves `run_stats` at its default — the caller
    /// overlays the supervisor's live counters.
    /// Test: `metrics_counts_by_state`, `metrics_surfaces_pending_decisions`,
    /// `metrics_last_activity_is_max`.
    pub fn from_records(records: &[SessionRecord]) -> Self {
        let mut m = FleetMetrics {
            total: records.len() as u64,
            ..Default::default()
        };
        for r in records {
            match r.state {
                ManagedSessionState::Provisioning => m.provisioning += 1,
                ManagedSessionState::Active => m.active += 1,
                ManagedSessionState::Stopped => m.stopped += 1,
                ManagedSessionState::Errored => m.errored += 1,
                // `Deleted` tombstones are counted alongside `Decommissioned`
                // tombstones (both are terminal, non-live records).
                ManagedSessionState::Decommissioned | ManagedSessionState::Deleted => {
                    m.decommissioned += 1
                }
            }
            // #4400: filter to non-terminal states regardless of whether
            // `pending_decision` is actually cleared on the decommission path
            // (defense in depth) — a `Decommissioned`/`Deleted` tombstone can
            // never be acted on by a human, and existing rows persisted
            // before the decommission-path fix landed still carry the stale
            // field. Surfacing a dead entry here trains operators to ignore
            // the one queue whose entire purpose is to stop and wait.
            // #8393: a front-gate decision wins; else the classifier's blocked
            // prompt, surfaced only while the session is `Active`.
            let surfaced = match (&r.pending_decision, &r.blocked_prompt) {
                (Some(q), _) if !r.state.is_terminal() => Some((q, r.proposed_default.clone())),
                (None, Some(p)) if r.state == ManagedSessionState::Active => {
                    Some((&p.question, None))
                }
                _ => None,
            };
            if let Some((question, proposed_default)) = surfaced {
                m.pending_decisions.push(PendingDecision {
                    session_id: r.id.to_string(),
                    tmux_name: r.tmux_name.clone(),
                    question: question.clone(),
                    proposed_default,
                });
            }
            if let Some(ts) = r.last_activity_at {
                m.last_activity_at = Some(match m.last_activity_at {
                    Some(cur) if cur >= ts => cur,
                    _ => ts,
                });
            }
        }
        m
    }
}

#[cfg(test)]
#[path = "metrics_context_tests.rs"]
mod context_tests;
