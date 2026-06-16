//! Goal-tracking data model for the Session Manager (DOC-14 §9.1).
//!
//! Why: the SM tracks operator intent as durable, queryable [`Goal`]s, each
//! fanned out to one-or-many delegated t-mpm sessions (§9.3). Progress and the
//! BLOCKING verification gate (§3.5) are *derived* from the per-session
//! verification state, so the data model must capture that state precisely and
//! serialisably — it is the source-of-truth payload written to the SM palace and
//! mirrored in the `goals.json` hot cache (§9.4). Pulling the pure types into
//! their own file keeps the store (`store.rs`) focused and under the SLOC cap and
//! keeps timestamps injectable so tests stay deterministic.
//! What: defines [`GoalStatus`], [`SessionTaskState`], [`SessionLink`], and
//! [`Goal`] — all `serde`-serialisable — plus the derived [`Goal::progress`]
//! recompute and the [`Goal::all_verified`] verification-gate predicate. No I/O,
//! no clock reads here: every timestamp is passed IN.
//! Test: `goals/model_tests.rs` covers id stability, progress recompute, the
//! all-verified predicate, status transitions, and serde round-trips.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Lifecycle status of a [`Goal`] (DOC-14 §9.1).
///
/// Why: the operator and the SM need a single, explicit state for each goal that
/// drives surfacing (§9.2) and the close decision. The spec enumerates exactly
/// these five variants — `Done` is reachable ONLY through the verification gate
/// (§3.5), enforced in [`super::store::SmGoalStore`].
/// What: a closed enum, serialised in lowerCamelCase to keep the palace/cache
/// JSON stable and human-readable. `Pending` is the `Default` (a freshly created
/// goal with no linked work yet).
/// Test: `status_default_is_pending`, `goal_status_serde_roundtrip`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum GoalStatus {
    /// Created from operator intent; no linked work has started.
    #[default]
    Pending,
    /// At least one linked session is in flight.
    InProgress,
    /// Progress is stalled awaiting an operator decision or an external blocker.
    Blocked,
    /// Verification gate passed — every linked task verified and acceptance met.
    Done,
    /// Closed without completion (cancelled / superseded), with a reason note.
    Abandoned,
}

impl GoalStatus {
    /// The stable, human-readable label for this status.
    ///
    /// Why: callers (the delegation wire response, logs, operator surfaces) need a
    /// single canonical status string so they can DISTINGUISH "in progress" from
    /// "blocked"/"done" without re-deriving it or misreading a boolean
    /// `goal_done == false` (which is ambiguous between "still running" and
    /// "failed"). Centralising the label keeps every surface consistent.
    /// What: returns the PascalCase variant name (`"Pending"`, `"InProgress"`,
    /// `"Blocked"`, `"Done"`, `"Abandoned"`).
    /// Test: `goal_status_label_matches_variants`.
    pub fn label(self) -> &'static str {
        match self {
            GoalStatus::Pending => "Pending",
            GoalStatus::InProgress => "InProgress",
            GoalStatus::Blocked => "Blocked",
            GoalStatus::Done => "Done",
            GoalStatus::Abandoned => "Abandoned",
        }
    }
}

/// Verification state of a single linked session task (DOC-14 §9.1 / §3.5).
///
/// Why: `Goal` progress and the close gate are derived purely from how many
/// linked tasks have reached `Verified` (observed evidence, §3.5). A precise
/// per-task state machine is therefore the foundation of the whole feature: a
/// goal cannot be `Done` unless EVERY link is `Verified`.
/// What: a closed enum mirroring the §3.4 delegation loop — `Launched` (spawned)
/// → `Running` (observed working) → `Verified` (evidence captured) or `Failed`.
/// `Launched` is the `Default` (the state a link is created in when a session is
/// first spawned). Serialised in lowerCamelCase for stable JSON.
/// Test: `session_state_default_is_launched`, `session_state_serde_roundtrip`,
/// and the progress/gate tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum SessionTaskState {
    /// The session was spawned for this task; no progress observed yet.
    #[default]
    Launched,
    /// The session is observed actively working the task.
    Running,
    /// The task is complete WITH observed evidence (gate-satisfying, §3.5).
    Verified,
    /// The session failed or was abandoned for this task.
    Failed,
}

impl SessionTaskState {
    /// Whether this state counts as gate-satisfying completion.
    ///
    /// Why: the verification gate (§3.5) and the [`Goal::progress`] numerator both
    /// ask the same question — "is this link done WITH evidence?" — so centralising
    /// it avoids the two drifting apart.
    /// What: returns `true` only for [`SessionTaskState::Verified`].
    /// Test: `verified_is_the_only_gate_satisfying_state`.
    pub fn is_verified(self) -> bool {
        matches!(self, SessionTaskState::Verified)
    }
}

/// A link from a [`Goal`] to one delegated t-mpm session task (DOC-14 §9.1).
///
/// Why: a goal fans out to one-or-many sessions (§9.3); each launched
/// session-sized task is recorded here with its current verification `state` and
/// any captured `evidence` (the PR URL / test-pass output the gate requires,
/// §3.5). The `session_id` is the join key returned by `POST /sessions`.
/// What: the `session_id`, the `task` description it was launched for, its
/// [`SessionTaskState`], and `evidence` (`None` until observed). Serialisable for
/// the palace/cache payload.
/// Test: `session_link_serde_roundtrip`, and the progress/gate tests.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionLink {
    /// The control-surface session id (`POST /sessions`) — the goal↔session join.
    pub session_id: String,
    /// The session-sized task this session was launched to complete.
    pub task: String,
    /// Current verification state of the linked task.
    pub state: SessionTaskState,
    /// Observed evidence (PR URL, captured test output, …); `None` until seen.
    #[serde(default)]
    pub evidence: Option<String>,
}

impl SessionLink {
    /// Construct a freshly launched link (state `Launched`, no evidence yet).
    ///
    /// Why: linking a session at spawn time (§9.2 decompose) is the common case;
    /// a terse constructor keeps the store and tests readable.
    /// What: builds a link with the given `session_id`/`task`,
    /// [`SessionTaskState::Launched`], and `evidence = None`.
    /// Test: `launched_link_has_no_evidence`.
    pub fn launched(session_id: impl Into<String>, task: impl Into<String>) -> Self {
        Self {
            session_id: session_id.into(),
            task: task.into(),
            state: SessionTaskState::Launched,
            evidence: None,
        }
    }
}

/// A tracked operator goal with its linked sessions and derived progress (§9.1).
///
/// Why: the central SM artifact — operator intent normalised into a stable,
/// durable record the SM reasons over, surfaces, and closes through the
/// verification gate. It is the exact payload persisted to the palace
/// (source-of-truth) and mirrored in `goals.json` (§9.4), so it must be fully
/// `serde`-serialisable and carry a STABLE `id` that survives restarts.
/// What: an `id` (`g-<short-uuid>`, assigned once at create), the normalised
/// `description`, the [`GoalStatus`], testable `acceptance` criteria, the
/// `sessions` fan-out, a derived `progress` percentage `0..=100`, `created` /
/// `updated` timestamps, and free-form `notes` (decisions / blockers). Timestamps
/// are passed in by the store so tests stay deterministic.
/// Test: `goals/model_tests.rs` (progress recompute, gate predicate, serde) and
/// `goals/store_tests.rs` (id stability, lifecycle).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Goal {
    /// Stable goal id, e.g. `"g-1a2b3c4d"`. Assigned once; never changes.
    pub id: String,
    /// Operator intent, normalised to a single-line description.
    pub description: String,
    /// Lifecycle status. `Done` is reachable only through the verification gate.
    pub status: GoalStatus,
    /// Testable acceptance criteria proposed at intake (§9.2).
    #[serde(default)]
    pub acceptance: Vec<String>,
    /// Linked delegated sessions (the one-or-many fan-out, §9.3).
    #[serde(default)]
    pub sessions: Vec<SessionLink>,
    /// Derived completion percentage `0..=100` (fraction of links `Verified`).
    pub progress: u8,
    /// Creation timestamp (UTC), passed in by the store.
    pub created: DateTime<Utc>,
    /// Last-mutation timestamp (UTC), bumped on every change.
    pub updated: DateTime<Utc>,
    /// Free-form decisions / blockers / closing-outcome notes (§9.2).
    #[serde(default)]
    pub notes: Vec<String>,
}

impl Goal {
    /// Construct a fresh goal with a caller-supplied stable id and timestamp.
    ///
    /// Why: the store owns id generation (deterministic `g-<uuid>`) and the clock,
    /// so the model takes both as arguments — keeping this constructor pure and
    /// the tests deterministic. A new goal starts `Pending` with `0%` progress and
    /// no links.
    /// What: builds a [`Goal`] with the given `id`, normalised `description`,
    /// `acceptance`, `status = Pending`, empty `sessions`/`notes`, `progress = 0`,
    /// and `created == updated == now`.
    /// Test: `new_goal_is_pending_zero_progress`, `goal_serde_roundtrip`.
    pub fn new(
        id: impl Into<String>,
        description: impl Into<String>,
        acceptance: Vec<String>,
        now: DateTime<Utc>,
    ) -> Self {
        Self {
            id: id.into(),
            description: description.into(),
            status: GoalStatus::Pending,
            acceptance,
            sessions: Vec::new(),
            progress: 0,
            created: now,
            updated: now,
            notes: Vec::new(),
        }
    }

    /// Recompute `progress` from the fraction of linked tasks that are `Verified`.
    ///
    /// Why: progress is DERIVED state (§9.2) — never set directly — so a single
    /// recompute keeps it consistent with the links after any mutation
    /// (link/update). With no links progress is `0` (nothing verified yet).
    /// What: sets `progress` to `round(100 * verified / total)` as a `u8`, or `0`
    /// when `sessions` is empty. Uses integer rounding via `+ total/2` to avoid
    /// floating-point drift in the persisted value.
    /// Test: `progress_one_of_three_verified_is_33`,
    /// `progress_all_verified_is_100`, `progress_no_links_is_zero`.
    pub fn recompute_progress(&mut self) {
        let total = self.sessions.len();
        if total == 0 {
            self.progress = 0;
            return;
        }
        let verified = self
            .sessions
            .iter()
            .filter(|s| s.state.is_verified())
            .count();
        // Integer rounding to nearest percent: (100*v + total/2) / total.
        self.progress = ((100 * verified + total / 2) / total) as u8;
    }

    /// Whether EVERY linked task is `Verified` (the §3.5 close precondition).
    ///
    /// Why: the verification gate forbids `Done` unless all linked work carries
    /// observed evidence. Expressing it as one predicate over the links keeps the
    /// gate logic in [`super::store::SmGoalStore::close`] a single readable check.
    /// A goal with NO links cannot be verified-done (there is no observed evidence
    /// of anything), so this returns `false` for an empty fan-out.
    /// What: returns `true` iff `sessions` is non-empty and every link
    /// `is_verified()`.
    /// Test: `all_verified_false_when_any_unverified`,
    /// `all_verified_false_when_no_links`, `all_verified_true_when_every_link_verified`.
    pub fn all_verified(&self) -> bool {
        !self.sessions.is_empty() && self.sessions.iter().all(|s| s.state.is_verified())
    }
}
