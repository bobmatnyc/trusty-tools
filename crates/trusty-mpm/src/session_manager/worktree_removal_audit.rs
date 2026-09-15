//! The audit lines every worktree removal writes around its deletion (#7885).
//!
//! Why: on 2026-09-14 two merged worktrees — `agent-a9826013bc7683c2b` and
//! `agent-a1afc1489a1adbf97`, both PR #7858 — lost their entire tracked
//! `crates/` subtree (6652 `D` entries, no `M`, no `??`), and no daemon log
//! named either path or either session. Several code paths can remove a session
//! workspace, each logged differently or not at all, so the mechanism could not
//! be identified even after the fact. An audit line that exists only on the
//! success arm of one of those paths is not an audit trail: the one that
//! deleted these left no record at all.
//!
//! What: [`audited_removal`], called from
//! [`remove_session_worktree`](super::decommission::remove_session_worktree) —
//! the single choke point every removal route passes through, including the raw
//! `remove_dir_all` fallback. It resolves a [`RemovalAudit`] from the path
//! itself (resolved path, branch, owning session or agent from the
//! `.trusty-mpm-worktree` sentinel, and the caller's reason), writes an ATTEMPT
//! line before the removal runs, and an OUTCOME line after it returns. Both go
//! to `tracing` at INFO, where prune already logs (no second sink).
//!
//! **Attempt, then outcome (#7885 critic round).** The single line used to read
//! "removing …" and was written before a removal that can still be refused —
//! a git lock, a stale pointer, a spawn failure — so a refusal read as a
//! deletion. The attempt line stays BEFORE the removal, because a removal that
//! dies part-way (the incident's shape) still has to be on record; the outcome
//! line says whether the directory is actually gone.
//!
//! **Best-effort, and never a gate.** Every field falls back to a placeholder
//! rather than failing, because an audit that can refuse a removal is a new
//! failure mode on a path whose whole job is to finish. It reads; it never
//! writes and never decides.
//! Test: `worktree_removal_audit_tests`.

use std::path::{Path, PathBuf};

use tracing::info;

use super::decommission::WorktreeRemoval;
use super::worktree_ownership::{SentinelOwner, read_sentinel_owner};
use super::worktree_safety::git_stdout;

/// What a removal is about to delete, and on whose say-so (#7885).
///
/// Why: a struct rather than four `tracing` fields at each call site, so every
/// removal route is guaranteed to record the same four facts — the divergence
/// between routes is exactly what made the observed deletion untraceable.
/// What: the four fields #7885's closure conditions name. Built by
/// [`Self::resolve`], rendered by [`Self::line`] and [`Self::outcome_line`].
/// Test: `worktree_7885_an_audit_names_the_path_branch_session_and_reason`,
/// `an_audit_for_an_unreadable_path_still_names_the_path_and_reason`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RemovalAudit {
    /// The directory about to be removed, canonicalized where possible.
    pub path: PathBuf,
    /// The branch the worktree has checked out.
    pub branch: String,
    /// The session or agent the ownership sentinel names.
    pub session: String,
    /// Which code path asked for the removal, and why.
    pub reason: String,
}

/// Rendered when git reports no branch name, or cannot be asked.
const NO_BRANCH: &str = "(none)";

/// Rendered when the ownership sentinel names nobody.
const NO_OWNER: &str = "(unowned)";

impl RemovalAudit {
    /// Read the four facts off `path` itself (#7885).
    ///
    /// Why: resolved from the path rather than passed in, so a caller cannot
    /// omit or mis-state them, and so the line describes what is ACTUALLY on
    /// disk at the moment of deletion rather than what a minutes-old survey
    /// believed.
    /// What: the canonical path (raw spelling when canonicalization fails, which
    /// is itself worth recording), `git rev-parse --abbrev-ref HEAD`, and the
    /// `.trusty-mpm-worktree` sentinel's owner. Every read falls back to a
    /// placeholder; none can fail.
    /// Test: `worktree_7885_an_audit_names_the_path_branch_session_and_reason`.
    pub(crate) fn resolve(path: &Path, reason: impl Into<String>) -> Self {
        let branch = git_stdout(path, &["rev-parse", "--abbrev-ref", "HEAD"])
            .map(|s| s.trim().to_string())
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| NO_BRANCH.to_string());
        let session = match read_sentinel_owner(path) {
            SentinelOwner::Known(id, _) => format!("session {id}"),
            SentinelOwner::Agent(owner, _) => format!("agent {}", owner.agent_id),
            SentinelOwner::Unknown => NO_OWNER.to_string(),
        };
        Self {
            path: std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf()),
            branch,
            session,
            reason: reason.into(),
        }
    }

    /// The ATTEMPT line, written before the removal runs.
    ///
    /// #7885 critic round: worded as an attempt. It precedes a removal that can
    /// still be refused, so "removing …" read as a completed deletion.
    /// Test: `worktree_7885_an_audit_names_the_path_branch_session_and_reason`,
    /// `worktree_7885_a_refused_removal_is_never_audited_as_a_deletion`.
    pub(crate) fn line(&self) -> String {
        format!(
            "worktree-removal: attempting removal of {} (branch {}, owner {}) — {} (#7885)",
            self.path.display(),
            self.branch,
            self.session,
            self.reason
        )
    }

    /// The OUTCOME line, written after the removal returns (#7885 critic round).
    ///
    /// Why: the attempt line alone over-reports deletions to anyone counting
    /// them. This line reads `removed` only when the remover reported success
    /// AND the directory is gone; otherwise it reads `kept` and quotes why.
    /// What: `still_on_disk` is observed by the caller after the removal, so a
    /// reported success that left the directory behind is not called a removal.
    /// Test: `worktree_7885_a_refused_removal_is_never_audited_as_a_deletion`,
    /// `worktree_7885_a_completed_removal_is_audited_as_removed`.
    pub(super) fn outcome_line(&self, outcome: &WorktreeRemoval, still_on_disk: bool) -> String {
        let (verb, detail) = match (outcome.reason(), still_on_disk) {
            (None, false) => ("removed", self.reason.clone()),
            (None, true) => (
                "kept",
                format!(
                    "the remover reported success but the directory is still on disk (asked \
                     for: {})",
                    self.reason
                ),
            ),
            (Some(why), _) => (
                "kept",
                format!(
                    "the removal did not complete: {why} (asked for: {})",
                    self.reason
                ),
            ),
        };
        format!(
            "worktree-removal: {verb} {} (branch {}, owner {}) — {detail} (#7885)",
            self.path.display(),
            self.branch,
            self.session,
        )
    }
}

/// Run `remove` between an attempt line and an outcome line (#7885).
///
/// Why: the removal path has exactly one place this belongs. Taking the removal
/// as a closure means no future edit can write the attempt without the outcome,
/// or read the facts after the directory they describe is gone.
/// What: resolves [`RemovalAudit`] from `path`, logs [`RemovalAudit::line`],
/// runs `remove`, logs [`RemovalAudit::outcome_line`] against whether `path`
/// still exists, and returns the removal's own result unchanged.
/// Test: `worktree_7885_a_removal_emits_one_audit_line_before_deleting`,
/// `worktree_7885_a_refused_removal_is_never_audited_as_a_deletion`,
/// `worktree_7885_a_completed_removal_is_audited_as_removed`.
pub(super) fn audited_removal(
    path: &Path,
    reason: &str,
    remove: impl FnOnce() -> WorktreeRemoval,
) -> WorktreeRemoval {
    let audit = RemovalAudit::resolve(path, reason);
    info!(
        path = %audit.path.display(),
        branch = %audit.branch,
        session = %audit.session,
        reason = %audit.reason,
        "{}",
        audit.line()
    );
    let outcome = remove();
    info!(
        path = %audit.path.display(),
        removed = outcome.removed() && !path.exists(),
        "{}",
        audit.outcome_line(&outcome, path.exists())
    );
    outcome
}

#[cfg(test)]
#[path = "worktree_removal_audit_tests.rs"]
mod worktree_removal_audit_tests;
