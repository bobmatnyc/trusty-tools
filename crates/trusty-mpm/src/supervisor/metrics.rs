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
//! Test: `metrics_counts_by_state`, `metrics_surfaces_pending_decisions`,
//! `metrics_last_activity_is_max` in `super::tests`.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::session_manager::{ManagedSessionState, SessionRecord};

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
/// Test: `metrics_counts_by_state`, `metrics_surfaces_pending_decisions`,
/// `metrics_last_activity_is_max`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
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
}

impl FleetMetrics {
    /// Derive the per-state snapshot from a slice of session records.
    ///
    /// Why: the supervisor recomputes fleet state from the authoritative session
    /// store on every sweep and on every `/metrics` request; this pure function
    /// keeps that derivation in one tested place, independent of I/O.
    /// What: tallies records by [`ManagedSessionState`], collects every record
    /// carrying a `pending_decision` into a [`PendingDecision`], and tracks the
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
            if let Some(ref question) = r.pending_decision {
                m.pending_decisions.push(PendingDecision {
                    session_id: r.id.to_string(),
                    tmux_name: r.tmux_name.clone(),
                    question: question.clone(),
                    proposed_default: r.proposed_default.clone(),
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
