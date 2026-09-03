//! The verdict a merged-PR reclaim candidate carries, and the gate that set it
//! (#2919, #6507).
//!
//! Why: split out of `worktree_reclaim` so that file stays under the 500-SLOC
//! production cap. The two halves also answer different questions — that module
//! RUNS the gates, this one is the vocabulary their answers are expressed in,
//! and the operator surfaces (`managed_merged_prs`, the prune route) read only
//! this vocabulary.
//! Test: `worktree_reclaim_tests` — every gate's refusal test asserts on the
//! kind and the gate recorded here.

use serde::Serialize;

/// Which of [`classify`]'s gates refused a candidate (#6507).
///
/// Why: the survey used to record a refusal as prose only, and the HTTP report
/// disclosed a per-candidate reason for gate 4 alone. Every other refusal —
/// including the gate-6 squash-merge misread this issue is about — was
/// invisible at every log level and in every field of the reply, so three
/// verification passes attributed one live worktree's refusal by elimination
/// and got it wrong. Naming the gate as DATA rather than as wording keeps the
/// operator surface from having to match on a message string.
/// What: one variant per gate, plus [`Deadline`](Self::Deadline) for the
/// candidate the survey ran out of time to inspect.
/// Test: `survey_names_the_gate_that_blocked_each_candidate`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ReclaimGate {
    /// Gate 1 — git's own admission verdict.
    Admission,
    /// Gate 2 — a session still claims the workspace.
    Liveness,
    /// Gate 3 — trusty-mpm cannot remove this worktree.
    Removability,
    /// Gate 4 — a dispatched agent owns it.
    AgentOwnership,
    /// Gate 5 — the branch's pull-request state is not a merge.
    PrState,
    /// Gate 6 — the working tree holds unsaved work.
    UnsavedWork,
    /// Not a gate: the survey's classify budget expired first.
    Deadline,
}

impl ReclaimGate {
    /// The operator-facing name of this gate.
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Admission => "gate 1 (admission)",
            Self::Liveness => "gate 2 (liveness)",
            Self::Removability => "gate 3 (removability)",
            Self::AgentOwnership => "gate 4 (agent ownership)",
            Self::PrState => "gate 5 (pull-request state)",
            Self::UnsavedWork => "gate 6 (unsaved work)",
            Self::Deadline => "the survey deadline",
        }
    }
}

/// Whether a worktree may be reclaimed, or the first reason it may not (#2919).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ReclaimVerdict {
    /// Every gate passed; the named pull request is the landing evidence.
    Reclaimable {
        /// The merged pull request that proves the branch's work landed.
        pr: u64,
    },
    /// Refused — `reason` names the FIRST gate that said no.
    Blocked {
        /// Which gate refused (#6507).
        gate: ReclaimGate,
        /// Operator-facing explanation of the refusal.
        reason: String,
    },
    /// Refused by gate 4 because a DISPATCHED AGENT owns the worktree (#5829).
    ///
    /// Why a separate variant rather than a `Blocked` carrying agent wording:
    /// the operator has to be TOLD about this one. Every other refusal leaves
    /// a directory the operator can see and re-run against; this one spares a
    /// tree an agent is working in right now, and a sweep that spared it
    /// silently is indistinguishable from a sweep that found nothing to do —
    /// which is what `--merged-prs --force` printed while the gate was working
    /// correctly. Matching on the reason STRING to recover the distinction
    /// would re-couple the operator surface to the wording of a refusal
    /// message, so the survey records the kind instead.
    /// Test: `survey_discloses_a_live_agents_spared_worktree`,
    /// `classify_blocks_a_live_agents_worktree`.
    BlockedByAgent {
        /// Which gate refused (#6507) — gate 1 or gate 4.
        gate: ReclaimGate,
        /// Operator-facing explanation naming the agent being protected.
        reason: String,
    },
}

impl ReclaimVerdict {
    /// Shorthand for a refusal.
    pub(crate) fn blocked(gate: ReclaimGate, reason: impl Into<String>) -> Self {
        Self::Blocked {
            gate,
            reason: reason.into(),
        }
    }

    /// Shorthand for gate 4's agent-ownership refusal (#5829).
    pub(crate) fn blocked_by_agent(gate: ReclaimGate, reason: impl Into<String>) -> Self {
        Self::BlockedByAgent {
            gate,
            reason: reason.into(),
        }
    }

    /// True when this verdict permits deletion.
    pub(crate) fn is_reclaimable(&self) -> bool {
        matches!(self, Self::Reclaimable { .. })
    }

    /// The gate that refused, and its message — `None` when nothing refused.
    ///
    /// Why: the survey folds one disclosure line per non-reclaimable candidate
    /// and must not have to match on either verdict kind to build it (#6507).
    pub(crate) fn refusal(&self) -> Option<(ReclaimGate, &str)> {
        match self {
            Self::Reclaimable { .. } => None,
            Self::Blocked { gate, reason } | Self::BlockedByAgent { gate, reason } => {
                Some((*gate, reason.as_str()))
            }
        }
    }
}
