//! Shared per-session git-worktree branch naming convention (issue #2032).
//!
//! Why: both `daemon::managed_routes::inproject` (feature = `"daemon"`;
//! creates per-session worktrees/branches) and `session_manager::decommission`
//! (unconditional — compiled with or without the `daemon` feature; removes
//! them) must agree on EXACTLY the same `session/<name>` branch convention.
//! Placing the convention here, in `core` (always compiled, no feature gate),
//! lets the unconditional `session_manager` module depend on it WITHOUT
//! pulling in the feature-gated `daemon` module — the reverse dependency
//! direction is what caused the bug this module fixes: `decommission.rs`
//! previously hardcoded `git branch -D <leaf>` (missing the `session/` prefix
//! `create_session_worktree` actually uses), so the branch-delete step always
//! targeted a nonexistent ref and silently no-opped — every session's branch
//! leaked forever.
//! What: [`worktree_branch_for`] builds the `session/<name>` branch name for a
//! given worktree leaf/session name.
//! Test: `worktree_branch_for_adds_session_prefix`.

/// Compute the per-session branch name for a worktree leaf/session name.
///
/// Why: single source of truth for the `session/<name>` convention, shared by
/// `daemon::managed_routes::inproject::create_session_worktree` (which
/// creates the branch) and `session_manager::decommission::remove_session_worktree`
/// (which must delete the SAME branch — see the #2032 branch-prefix fix in
/// that module's doc comment).
/// What: `format!("session/{name}")`.
/// Test: `worktree_branch_for_adds_session_prefix`.
pub fn worktree_branch_for(name: &str) -> String {
    format!("session/{name}")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The branch convention must always be `session/<name>`, for both
    /// pre-#2032 raw-UUID leaves and the new semantic-tmux-name leaves — the
    /// convention is name-format-agnostic.
    /// Test: this function IS the test.
    #[test]
    fn worktree_branch_for_adds_session_prefix() {
        assert_eq!(worktree_branch_for("tm-foo-01"), "session/tm-foo-01");
        assert_eq!(
            worktree_branch_for("00000000-old-session-uuid"),
            "session/00000000-old-session-uuid"
        );
    }
}
