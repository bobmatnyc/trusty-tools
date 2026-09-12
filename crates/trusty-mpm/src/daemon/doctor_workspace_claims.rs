//! Which claimed workspaces are live, and is this project one of them?
//!
//! Why: split out of `daemon/doctor.rs`, which sits AT the 500-SLOC production
//! cap, when the #7673 ancestor-`CLAUDE.md` row was added — the repo's rule is
//! that the split ships inside the PR that next adds to the file. The two
//! helpers here are the one cohesive piece of `doctor.rs` that answers a
//! question about the FLEET rather than running a probe, which is why they are
//! the piece that moved.
//! What: [`live_workspace_paths`] and [`is_managed_workspace`], verbatim. The
//! module is included with `#[path]`, so `doctor_tests.rs`'s `use super::*`
//! still reaches them through `doctor.rs`'s own `use`.
//! Test: `live_workspace_paths_drops_only_claims_a_probe_found_gone`,
//! `unmanaged_cwd_audits_the_operator_home_tier`,
//! `a_registered_workspace_still_gets_the_workspace_layout`,
//! `an_uncanonicalizable_path_is_not_a_managed_workspace`,
//! `an_unreadable_directory_is_not_a_managed_workspace`,
//! `an_absent_path_still_matches_the_recorded_spelling_of_itself`.

use std::path::{Path, PathBuf};

use crate::session_manager::worktree_reclaim::{ClaimLiveness, LiveClaims};

/// The claimed workspace paths whose session is still live (#7259).
///
/// Why: four probes — the managed-workspace tier decision, the base-clone
/// identity check, and the two hooks-hygiene checks — ask only "which
/// workspaces belong to a running session". A tombstoned record answers that
/// question with a directory nobody occupies, which is how a `deleted` adopted
/// pane's org-level path kept counting as active.
/// What: every claim except the ones a liveness probe answered for and found
/// gone. `Live` covers both "the session is running" and "nothing could
/// establish that it is not", so an unobservable tmux yields the full set —
/// the same fail-closed direction gate 2 takes.
/// Test: `live_workspace_paths_drops_only_claims_a_probe_found_gone`.
pub(super) fn live_workspace_paths(active: &LiveClaims) -> Vec<PathBuf> {
    active
        .claims
        .iter()
        .filter(|c| c.liveness != ClaimLiveness::SessionGone)
        .map(|c| c.path.clone())
        .collect()
}

/// Is `project_dir` a workspace some live session was provisioned into?
///
/// Why (#5867): [`FrameworkPaths::for_managed_workspace`] rewrites the SKILL
/// deploy destination to `<dir>/.claude/skills`, which is true only of a
/// managed session's own workspace. Every other production call site already
/// passes one; `run_doctor` is the only one handed an arbitrary process cwd,
/// and applying the workspace layout there collapsed the operator-home tier
/// onto the project tier. `active_workspace_paths` is exactly the set of
/// provisioned workspaces — `daemon::api::doctor` builds it from every session
/// record's `workspace_path` — so it is the only input that can answer this
/// without inventing a heuristic.
/// What: canonicalizes both sides (a workspace under `/tmp` resolves through a
/// symlink on macOS, so a raw `==` would miss the match) and reports whether
/// `project_dir` appears in the set. A path that cannot be canonicalized —
/// absent, dangling symlink, unreadable parent alike — falls back to its own
/// raw spelling, so it is then compared verbatim. That is the safe answer in
/// the sense that matters: it can still match a recorded workspace under the
/// exact name the session recorded, but it can never match an unregistered
/// directory, which is the promotion #5867 is about.
/// Test: `unmanaged_cwd_audits_the_operator_home_tier`,
/// `a_registered_workspace_still_gets_the_workspace_layout`,
/// `an_uncanonicalizable_path_is_not_a_managed_workspace`,
/// `an_unreadable_directory_is_not_a_managed_workspace`,
/// `an_absent_path_still_matches_the_recorded_spelling_of_itself`.
pub(super) fn is_managed_workspace(project_dir: &Path, active_workspace_paths: &[PathBuf]) -> bool {
    fn resolve(path: &Path) -> PathBuf {
        std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
    }
    let target = resolve(project_dir);
    active_workspace_paths
        .iter()
        .any(|candidate| resolve(candidate) == target)
}
