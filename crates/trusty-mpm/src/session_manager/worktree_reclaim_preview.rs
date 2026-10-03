//! The per-path rows a merged-PR reclaim preview prints (#8782).
//!
//! Why: the preview reported a reclaimable COUNT and no paths, so an operator
//! could not see what `--merged-prs --force` would delete. A worktree whose
//! pull-request lookup failed — 45 of them on 2026-09-27, "no origin remote" —
//! appeared only as a count beside that total.
//! What: [`preview_rows`] splits a survey into the rows it would reclaim and the
//! rows it keeps because their pull-request state is UNKNOWN, each with its
//! project and the verdict's own reason. Every other refusal is already listed
//! in `blocked_reasons`.
//! Test: `worktree_scope_tests`.

use serde::Serialize;

use super::worktree_reclaim::{BranchPrState, ReclaimCandidate, ReclaimSurvey};
use super::worktree_reclaim_verdict::{ReclaimGate, ReclaimVerdict};

/// One previewed worktree: where it is, which project owns it, and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct PreviewRow {
    /// The worktree directory.
    pub path: String,
    /// The checkout whose registry lists it.
    pub project: String,
    /// The verdict's decision line.
    pub reason: String,
}

/// A survey's reclaim set and its unknown set (#8782).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub(crate) struct Preview {
    /// Worktrees the pass would remove.
    pub reclaim: Vec<PreviewRow>,
    /// Worktrees kept because their pull-request state could not be read.
    pub unknown: Vec<PreviewRow>,
}

/// Whether a candidate is kept because nothing could establish its PR state.
///
/// Why: "kept, PR still open" and "kept, the lookup failed" need opposite
/// operator actions, so the preview names the second as UNKNOWN (#8782).
/// What: true for a non-reclaimable verdict refused at gate 5 or at the survey
/// deadline whose pull-request state is `LookupFailed` or `Unknown`.
/// Test: `a_repository_without_origin_is_kept_and_reported_unknown`.
pub(crate) fn is_unknown(candidate: &ReclaimCandidate) -> bool {
    let at_pr_gate = matches!(
        candidate.verdict,
        ReclaimVerdict::Blocked {
            gate: ReclaimGate::PrState | ReclaimGate::Deadline,
            ..
        }
    );
    at_pr_gate
        && matches!(
            candidate.pr,
            BranchPrState::LookupFailed { .. } | BranchPrState::Unknown
        )
}

/// Split `survey` into its reclaim rows and its unknown rows (#8782).
///
/// Test: `a_repository_without_origin_is_kept_and_reported_unknown`,
/// `the_preview_lists_exactly_what_force_removes`.
pub(crate) fn preview_rows(survey: &ReclaimSurvey) -> Preview {
    let row = |c: &ReclaimCandidate| PreviewRow {
        path: c.path.to_string_lossy().into_owned(),
        project: c.registry_root.to_string_lossy().into_owned(),
        reason: c.verdict.decision(),
    };
    let mut preview = Preview::default();
    for candidate in &survey.candidates {
        if candidate.verdict.is_reclaimable() {
            preview.reclaim.push(row(candidate));
        } else if is_unknown(candidate) {
            preview.unknown.push(row(candidate));
        }
    }
    preview
}
