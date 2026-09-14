//! The one audit line every worktree removal writes before it deletes (#7885).
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
//! What: [`RemovalAudit`], resolved from the path itself immediately before the
//! first destructive call in
//! [`remove_session_worktree`](super::decommission::remove_session_worktree) —
//! the single choke point every removal route passes through, including the raw
//! `remove_dir_all` fallback. It carries the resolved path, the branch, the
//! owning session or agent from the `.trusty-mpm-worktree` sentinel, and the
//! caller's reason, and it goes to `tracing` at INFO, which is where prune
//! already logs (no second sink).
//!
//! **Best-effort, and never a gate.** Every field falls back to a placeholder
//! rather than failing, because an audit that can refuse a removal is a new
//! failure mode on a path whose whole job is to finish. It reads; it never
//! writes and never decides.
//! Test: `worktree_removal_audit_tests`.

use std::path::{Path, PathBuf};

use tracing::info;

use super::worktree_ownership::{SentinelOwner, read_sentinel_owner};
use super::worktree_safety::git_stdout;

/// What a removal is about to delete, and on whose say-so (#7885).
///
/// Why: a struct rather than four `tracing` fields at each call site, so every
/// removal route is guaranteed to record the same four facts — the divergence
/// between routes is exactly what made the observed deletion untraceable.
/// What: the four fields #7885's closure conditions name. Built by
/// [`Self::resolve`], rendered by [`Self::line`], emitted by [`Self::emit`].
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

    /// The one line, as an operator reads it.
    ///
    /// Test: `worktree_7885_an_audit_names_the_path_branch_session_and_reason`.
    pub(crate) fn line(&self) -> String {
        format!(
            "worktree-removal: removing {} (branch {}, owner {}) — {} (#7885)",
            self.path.display(),
            self.branch,
            self.session,
            self.reason
        )
    }

    /// Write [`Self::line`] to `tracing` at INFO, where prune already logs.
    pub(crate) fn emit(&self) {
        info!(
            path = %self.path.display(),
            branch = %self.branch,
            session = %self.session,
            reason = %self.reason,
            "{}",
            self.line()
        );
    }
}

/// Resolve and emit one audit line in a single call.
///
/// Why: the removal path has exactly one place this belongs, and a two-step
/// `resolve` + `emit` there invites a future edit that keeps one and drops the
/// other.
/// Test: `worktree_7885_a_removal_emits_one_audit_line_before_deleting`.
pub(crate) fn audit_removal(path: &Path, reason: &str) {
    RemovalAudit::resolve(path, reason).emit();
}

#[cfg(test)]
#[path = "worktree_removal_audit_tests.rs"]
mod worktree_removal_audit_tests;
