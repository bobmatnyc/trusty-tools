//! The gate that spares a worktree a live process was LAUNCHED FROM (#7504).
//!
//! Why: every other reclaim gate asks about the tree's CONTENT (dirt, landing
//! evidence) or about who CLAIMS it (a session record, an agent sentinel, the
//! operator's keep-list). None of them notices that the running process's own
//! working directory, or the executable serving this sweep, sits inside the
//! candidate. Deleting that tree pulls the ground out from under the process
//! doing the deleting: its working directory becomes a dangling inode, every
//! subsequent `git -C .` fails with ENOENT, and on macOS a later exec of the
//! deleted binary is SIGKILLed as an invalid signature rather than reported as
//! missing — which reads as an OOM kill. #7185 named this as the
//! worktree-launched-daemon trap; #7262 is the same shape one layer up, a
//! settings hook pointing at a Cargo build-tree binary.
//!
//! What: [`launch_refusal`] is the whole gate as a pure function over one
//! candidate and the directories processes were launched from.
//! [`process_launch_dirs`] is the production collector — the current working
//! directory and the running executable, both read with no subprocess and no
//! process-table walk, so the gate costs nothing per candidate.
//!
//! # Direction of the containment test
//!
//! A launch directory INSIDE the candidate refuses; a launch directory that
//! merely CONTAINS the candidate does not. Deleting `<repo>/.claude/worktrees/x`
//! does not disturb a process launched from `<repo>`, and treating an ancestor
//! as a refusal would spare every worktree in the repository the daemon was
//! started from — which is all of them.
//!
//! # Fail direction
//!
//! Toward keeping. A path that will not canonicalize is compared by its literal
//! spelling rather than dropped, exactly as
//! [`KeepList`](super::worktree_keep_list::KeepList) does, so an unresolvable
//! launch directory still protects the tree it names.
//!
//! Test: `worktree_reclaim_launch_tests`.

use std::path::{Path, PathBuf};

use super::worktree_keep_list::resolve;

/// Why this candidate may not be removed, or `None` when no process lives in it.
///
/// Why: the gate is pure so "a process was launched from inside it" is testable
/// without starting a process inside a tempdir, and so the reclaim loop can
/// apply it before the slow `gh` lookups rather than after them.
/// What: refuses when any `launched_from` entry IS the candidate or sits BENEATH
/// it, compared on canonicalized AND literal forms (see the module docs for why
/// both). The reason names the offending directory so the audit line says which
/// process the refusal protected.
/// Test: `a_launch_dir_inside_the_candidate_refuses`,
/// `the_candidate_itself_as_a_launch_dir_refuses`,
/// `a_launch_dir_containing_the_candidate_does_not_refuse`,
/// `an_unrelated_launch_dir_does_not_refuse`,
/// `an_unresolvable_launch_dir_still_refuses_by_its_literal_spelling`.
pub(crate) fn launch_refusal(path: &Path, launched_from: &[PathBuf]) -> Option<String> {
    let resolved_candidate = resolve(path);
    for dir in launched_from {
        let resolved_dir = resolve(dir);
        if resolved_dir.starts_with(&resolved_candidate) || dir.starts_with(path) {
            return Some(format!(
                "a live process was launched from inside this worktree ({}) — \
                 removing it would strip that process of its working directory \
                 or its executable (#7504)",
                dir.display()
            ));
        }
    }
    None
}

/// The directories THIS process was launched from, for [`launch_refusal`].
///
/// Why: the daemon running the sweep is the process most likely to be sitting
/// inside a candidate — a `cargo run` from a worktree, or a binary still living
/// in that worktree's `target-worktree/`. Reading its own cwd and exe answers
/// that deterministically, with no process-table scan to go stale or to need a
/// subprocess the gate would then have to fail closed around.
/// What: the current working directory, plus the directory holding the running
/// executable. Either read may fail (a deleted cwd, a platform that will not
/// report `current_exe`); a failure contributes nothing rather than aborting the
/// sweep, because this gate is additive to the five that already refuse.
/// Test: `process_launch_dirs_reports_the_current_directory`.
pub(crate) fn process_launch_dirs() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(cwd) = std::env::current_dir() {
        out.push(cwd);
    }
    if let Ok(exe) = std::env::current_exe()
        && let Some(parent) = exe.parent()
    {
        out.push(parent.to_path_buf());
    }
    out
}

#[cfg(test)]
#[path = "worktree_reclaim_launch_tests.rs"]
mod worktree_reclaim_launch_tests;
