//! Positive-evidence liveness classification for a managed session's workspace
//! directory (issue #4204).
//!
//! Why: two independent code paths — [`super::agent_reset_workspace::
//! reset_active_workspace_agents`] and the `tm` binary's
//! `commands::guided_inplace::run_inplace_relaunch` — gated on
//! `workspace_path.is_dir()` alone and then acted on the directory as if it
//! were live. `is_dir()` is blind to the failure mode #4204 observed on
//! 2026-07-27: worktree `.base/.worktrees/f443c12d-…` had its `.git` and its
//! entire source tree stripped and was absent from `git worktree list`, yet the
//! directory node survived — so `is_dir()` returned `true`, and 45 files were
//! recomposed into the husk's `.claude/agents/` while a `claude` process sat on
//! the dead cwd. This module is the ONE predicate both call sites now share, so
//! "is this workspace alive?" has a single definition rather than a per-call-
//! site reinvention.
//!
//! THE INVARIANT — SKIP ONLY ON POSITIVE EVIDENCE OF DEATH. The obvious patch,
//! `path.join(".git").exists()`, is wrong twice over and this module deliberately
//! avoids both traps:
//!
//! 1. `.git` IS NOT A DIRECTORY IN A LINKED WORKTREE. `git worktree add` writes
//!    `.git` as a FILE containing a `gitdir:` pointer (mirrored by
//!    `provisioner::workspace::FakeGitBackend::worktree_add`). Every workspace
//!    the provisioner creates is a linked worktree under
//!    `<project>/.base/.worktrees/<session-id>`, so a guard spelled
//!    `.join(".git").is_dir()` would reject EVERY legitimate managed workspace —
//!    the exact inverse of the bug. This module stats the `.git` name and
//!    accepts whatever it finds: file, directory, or symlink.
//! 2. NOT EVERY WORKSPACE IS A GIT CHECKOUT, AND A FAILED OBSERVATION IS NOT A
//!    DEATH. A missing `.git` is evidence of death only where a `.git` is
//!    KNOWN to be owed (see [`workspace_liveness`]'s expectation rule), and an
//!    I/O failure — permission denied, a stalled network mount, ENOTDIR — is
//!    an ABSENCE OF EVIDENCE, reported as [`WorkspaceLiveness::Indeterminate`]
//!    and explicitly NOT dead ([`WorkspaceLiveness::is_dead`] returns `false`
//!    for it). Silently declining to serve a live workspace is a far quieter
//!    failure than the bug being fixed, and is the over-refusal trap PR #4202's
//!    first round fell into.
//!
//! What: [`WorkspaceLiveness`] — a four-valued classification with the
//! dead/not-dead decision encoded in [`WorkspaceLiveness::is_dead`] so no caller
//! has to re-derive it — and [`workspace_liveness`], which computes it.
//! Test: `liveness_accepts_linked_worktree_git_file`,
//! `liveness_accepts_clone_git_directory`,
//! `liveness_reports_gutted_worktree_missing_git`,
//! `liveness_never_requires_git_for_unowned_non_worktree_path`,
//! `liveness_reports_absent_directory`, `liveness_indeterminate_is_never_dead`.

use std::path::Path;

use crate::session_manager::decommission::is_session_worktree;

/// What an observation of a workspace directory established about its liveness.
///
/// Why: a two-valued `bool` cannot express the distinction this guard exists to
/// preserve — "confirmed dead" versus "could not tell". Collapsing the two is
/// precisely how a liveness guard turns into an over-refusal bug, so the
/// ambiguous case gets its own variant and [`Self::is_dead`] answers `false`
/// for it.
/// What: `Live` (no evidence of death), `Absent` (the path itself is gone or is
/// not a directory), `Gutted` (the directory survives but a `.git` that is owed
/// is confirmed missing — the #4204 husk), and `Indeterminate` (the filesystem
/// could not be interrogated; carries the reason for logging).
/// Test: `liveness_indeterminate_is_never_dead` pins the invariant that only
/// `Absent`/`Gutted` count as death.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkspaceLiveness {
    /// Nothing observed suggests this workspace is dead — act on it.
    Live,
    /// The path does not exist, or exists but is not a directory.
    Absent,
    /// The directory node survives but its git checkout has been stripped
    /// (#4204). Positive evidence of death.
    Gutted,
    /// The filesystem could not be interrogated (permission denied, I/O error,
    /// a stalled network mount). NEVER treated as death — see [`Self::is_dead`].
    Indeterminate(String),
}

impl WorkspaceLiveness {
    /// `true` only when the observation POSITIVELY established the workspace is
    /// dead.
    ///
    /// Why: callers must skip on evidence of death, never on the absence of
    /// evidence. Putting the rule here (rather than in each caller's `match`)
    /// makes it impossible for one call site to quietly widen "dead" to include
    /// [`Self::Indeterminate`].
    /// What: `true` for [`Self::Absent`] and [`Self::Gutted`]; `false` for
    /// [`Self::Live`] and [`Self::Indeterminate`].
    /// Test: `liveness_indeterminate_is_never_dead`,
    /// `liveness_reports_gutted_worktree_missing_git`,
    /// `liveness_reports_absent_directory`.
    pub fn is_dead(&self) -> bool {
        matches!(self, Self::Absent | Self::Gutted)
    }
}

impl std::fmt::Display for WorkspaceLiveness {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Live => f.write_str("live"),
            Self::Absent => f.write_str("no longer exists on disk"),
            Self::Gutted => f.write_str(
                "gutted — the directory survives but its git checkout (.git) has been stripped",
            ),
            Self::Indeterminate(reason) => {
                write!(
                    f,
                    "liveness could not be determined ({reason}) — treated as live"
                )
            }
        }
    }
}

/// Classify whether `path` is a workspace that is still safe to act on.
///
/// Why: see the module doc — this is the single #4204 predicate replacing the
/// bare `is_dir()` check at both offending call sites.
/// What, in order: (1) `path.is_dir()` — preserved verbatim from the code this
/// replaces, so the pre-existing "gone / not a directory" skip keeps its exact
/// semantics and this change adds no new abort paths; a `false` here is
/// [`WorkspaceLiveness::Absent`]. (2) THE EXPECTATION RULE — a missing `.git`
/// is evidence of death only where one is owed, which is true iff
/// `workspace_owned || is_session_worktree(path)`: `workspace_owned` means
/// `WorkspaceProvisioner::provision_in` created this directory (it takes a
/// mandatory `repo_url` and always finishes with `GitBackend::worktree_add`, so
/// an owned workspace is ALWAYS a checkout), and `is_session_worktree`'s
/// `.worktrees/<id>` path shape is only ever produced by `git worktree add`.
/// This is deliberately the SAME predicate the #1511 ownership gate uses — it
/// adds no third notion of ownership — and outside it the answer is
/// [`WorkspaceLiveness::Live`], so an adopted, non-git workspace is never
/// refused service. (3) `symlink_metadata` on `path.join(".git")`: `Ok(_)`
/// (file — the linked-worktree case — directory, or symlink) is
/// [`WorkspaceLiveness::Live`]; `NotFound` is the confirmed absence that makes
/// [`WorkspaceLiveness::Gutted`]; ANY OTHER error is
/// [`WorkspaceLiveness::Indeterminate`], never death.
///
/// `is_session_worktree` is intentionally left as the pure path-shape heuristic
/// it is: it answers "may trusty-mpm DELETE this?" (ownership) on paths that may
/// legitimately be absent — `is_session_worktree_absent_path_is_noop` pins that
/// — and teaching it about `.git` would make `decommission`/`search_gc` refuse
/// to clean up the very husks #4204 is about. Ownership and liveness are
/// orthogonal axes; this function composes them rather than conflating them.
///
/// Test: `liveness_accepts_linked_worktree_git_file`,
/// `liveness_accepts_clone_git_directory`,
/// `liveness_reports_gutted_worktree_missing_git`,
/// `liveness_never_requires_git_for_unowned_non_worktree_path`,
/// `liveness_reports_absent_directory`.
pub fn workspace_liveness(path: &Path, workspace_owned: bool) -> WorkspaceLiveness {
    if !path.is_dir() {
        return WorkspaceLiveness::Absent;
    }

    if !(workspace_owned || is_session_worktree(path)) {
        return WorkspaceLiveness::Live;
    }

    match std::fs::symlink_metadata(path.join(".git")) {
        Ok(_) => WorkspaceLiveness::Live,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => WorkspaceLiveness::Gutted,
        Err(e) => WorkspaceLiveness::Indeterminate(format!("cannot stat .git: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    /// Build `<tmp>/.worktrees/<leaf>` so `is_session_worktree` matches, exactly
    /// as `WorkspaceProvisioner::provision_in` lays a managed worktree out.
    fn worktree_shaped(base: &TempDir, leaf: &str) -> std::path::PathBuf {
        let p = base
            .path()
            .join(crate::session_manager::decommission::WORKTREES_DIRNAME)
            .join(leaf);
        fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn liveness_accepts_linked_worktree_git_file() {
        // THE ANTI-OVER-REFUSAL CASE. `git worktree add` writes `.git` as a
        // FILE holding a `gitdir:` pointer, which is what EVERY managed
        // workspace looks like. A guard spelled `.join(".git").is_dir()` would
        // reject all of them.
        let base = TempDir::new().unwrap();
        let wt = worktree_shaped(&base, "f443c12d-2fb6-4ce1-9f70-2e7695306e47");
        fs::write(
            wt.join(".git"),
            "gitdir: /repo/.base/.git/worktrees/f443c12d\n",
        )
        .unwrap();

        let liveness = workspace_liveness(&wt, true);
        assert_eq!(liveness, WorkspaceLiveness::Live, "{liveness}");
        assert!(!liveness.is_dead());
        assert!(
            !wt.join(".git").is_dir(),
            "fixture must model a .git FILE, not a directory — otherwise this \
             test cannot catch an is_dir()-shaped guard"
        );
    }

    #[test]
    fn liveness_accepts_clone_git_directory() {
        // A full clone (`.git` as a directory) is equally live.
        let base = TempDir::new().unwrap();
        let clone = base.path().join("checkout");
        fs::create_dir_all(clone.join(".git")).unwrap();

        assert_eq!(workspace_liveness(&clone, true), WorkspaceLiveness::Live);
    }

    #[test]
    fn liveness_reports_gutted_worktree_missing_git() {
        // The #4204 husk: the directory node survives (and even carries a
        // `.claude/` tree) while `.git` and the source are gone.
        let base = TempDir::new().unwrap();
        let wt = worktree_shaped(&base, "f443c12d-2fb6-4ce1-9f70-2e7695306e47");
        fs::create_dir_all(wt.join(".claude").join("agents")).unwrap();

        assert!(wt.is_dir(), "the old is_dir() gate would have passed this");
        let liveness = workspace_liveness(&wt, true);
        assert_eq!(liveness, WorkspaceLiveness::Gutted);
        assert!(liveness.is_dead());
    }

    #[test]
    fn liveness_never_requires_git_for_unowned_non_worktree_path() {
        // THE OTHER ANTI-OVER-REFUSAL CASE. A directory trusty-mpm did not
        // provision and whose shape is not `.worktrees/<id>` is owed no `.git`
        // at all — an operator may legitimately have adopted a plain,
        // non-git directory. It must classify Live, not Gutted.
        let plain = TempDir::new().unwrap();
        assert!(!plain.path().join(".git").exists());

        let liveness = workspace_liveness(plain.path(), false);
        assert_eq!(liveness, WorkspaceLiveness::Live, "{liveness}");
        assert!(!liveness.is_dead());
    }

    #[test]
    fn liveness_reports_absent_directory() {
        let base = TempDir::new().unwrap();
        let gone = base.path().join("never-existed");

        let liveness = workspace_liveness(&gone, true);
        assert_eq!(liveness, WorkspaceLiveness::Absent);
        assert!(liveness.is_dead());
    }

    #[test]
    fn liveness_indeterminate_is_never_dead() {
        // THE INVARIANT: an observation that FAILED is not evidence of death.
        // Treating a permission/IO/network-mount failure as "dead" would
        // silently stop serving live workspaces.
        let unknown = WorkspaceLiveness::Indeterminate("permission denied".into());
        assert!(
            !unknown.is_dead(),
            "an ambiguous observation must never be treated as death"
        );
        assert!(unknown.to_string().contains("treated as live"));
    }
}
