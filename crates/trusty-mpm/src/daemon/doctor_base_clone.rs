//! `tm doctor`'s `base_clone` probe — the git identity of the clone a live
//! session's worktree hangs off (issue #3605).
//!
//! Why: a linked worktree stores no object database of its own. Its `.git` is a
//! one-line FILE pointing at admin data inside the base clone, and every git
//! command run inside it reads through that pointer. When the base clone loses
//! its git internals the worktree keeps every source file it had and stops
//! being a git repository, which surfaces to whoever is working in it as
//! `fatal: not a git repository: (null)` — and, second-hand, as test failures
//! that have nothing to do with the code under test. On 2026-07-21 that state
//! went unnoticed for over half an hour across 70 worktrees, discovered only
//! when two agents died in it. Nothing in doctor looked at the base clone:
//! `doctor_worktrees` counts orphaned worktree DIRECTORIES, which is the
//! opposite condition (a directory nothing owns, rather than an owner that has
//! stopped resolving).
//!
//! What: [`check_base_clones`] reads each live session workspace's `.git`
//! pointer, resolves the base clone behind it, and Fails when that clone can no
//! longer answer for the worktree — naming the base path and how many live
//! worktrees hang off it, so the blast radius is in the report rather than
//! inferred later.
//!
//! DETECTION ONLY. It reads; it never repairs, moves, or deletes.
//! `provisioner::workspace::base_lock::stale_base_dir_error` already settles
//! the repair side by refusing to touch a directory that may hold live
//! worktrees, and this probe's remediation text points at the same quarantine
//! discipline rather than inventing a second answer.
//! Test: `base_clone_ok_with_no_live_workspaces`,
//! `base_clone_ok_for_a_healthy_worktree`,
//! `base_clone_fails_when_the_admin_dir_is_gone`,
//! `base_clone_fails_when_the_object_database_is_gone`,
//! `base_clone_counts_every_worktree_behind_one_base`,
//! `base_clone_ignores_a_plain_checkout` (all in `doctor_base_clone_tests.rs`).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::core::doctor::{CheckStatus, DoctorCheck};

/// What a live workspace's `.git` pointer established about its base clone.
///
/// Why: "no base clone" and "a base clone that stopped resolving" must not
/// collapse into one value — the first is the normal state of a plain checkout
/// and the second is the incident. Making them separate variants is what keeps
/// [`check_base_clones`] from reporting a project that simply is not a linked
/// worktree.
/// What: `NotLinked` for a workspace whose `.git` is a directory or absent;
/// `Healthy` carries the base clone root; `Severed` carries the root plus the
/// reason it no longer answers.
#[derive(Debug, PartialEq, Eq)]
enum BaseCloneState {
    /// The workspace is not a linked worktree — it has no base clone.
    NotLinked,
    /// The base clone at this path still answers for the workspace.
    Healthy(PathBuf),
    /// The base clone at this path can no longer answer for the workspace.
    Severed {
        /// The base clone's root directory.
        base: PathBuf,
        /// What is missing, in one clause, for the report.
        reason: &'static str,
    },
}

/// The admin directory a linked worktree's `.git` file points at.
///
/// Why: `git worktree add` writes `gitdir: <path>` into the worktree's `.git`
/// FILE, and that pointer is the whole of the worktree's git identity. Reading
/// it directly — rather than shelling out to `git` — is what lets this probe
/// report the severed state at all: once the target is gone every `git` command
/// run there fails with the same opaque error and tells us nothing about which
/// path it was reaching for.
/// What: reads `<workspace>/.git` as a file, returns the `gitdir:` value
/// resolved against `workspace` when relative. `None` when `.git` is a
/// directory (a plain clone), absent, unreadable, or carries no `gitdir:` line.
fn worktree_gitdir(workspace: &Path) -> Option<PathBuf> {
    let dot_git = workspace.join(".git");
    if dot_git.is_dir() {
        return None;
    }
    let contents = std::fs::read_to_string(&dot_git).ok()?;
    let target = contents
        .lines()
        .find_map(|line| line.trim().strip_prefix("gitdir:"))?
        .trim();
    if target.is_empty() {
        return None;
    }
    let path = Path::new(target);
    Some(if path.is_absolute() {
        path.to_path_buf()
    } else {
        workspace.join(path)
    })
}

/// The base clone root, and its common git directory, for a worktree admin
/// path.
///
/// Why: git writes the admin path as `<common>/worktrees/<name>`, where
/// `<common>` is `<base>/.git` for an ordinary clone and `<base>` itself for
/// the legacy bare shape #4270 retired. Both spellings still exist on disk on
/// any machine that ran an older build, so the report must name the right base
/// for either.
/// What: pops `<name>` and `worktrees`, yielding the common dir; the base root
/// is that with a trailing `.git` popped when present. `None` when the path is
/// not shaped like a worktree admin directory.
fn base_and_common_dir(gitdir: &Path) -> Option<(PathBuf, PathBuf)> {
    let worktrees_dir = gitdir.parent()?;
    if worktrees_dir.file_name()? != "worktrees" {
        return None;
    }
    let common = worktrees_dir.parent()?.to_path_buf();
    let base = match common.file_name() {
        Some(name) if name == ".git" => common.parent()?.to_path_buf(),
        _ => common.clone(),
    };
    Some((base, common))
}

/// Classify one live workspace's base clone.
///
/// What: resolves the `.git` pointer, then asks three questions of what it
/// found — does the worktree's own admin directory still exist, does the common
/// git directory still hold a `HEAD`, and does it still hold an object
/// database. Any "no" is the severed state; a partial deletion that spares
/// `worktrees/` is caught by the second and third.
/// Test: `base_clone_fails_when_the_admin_dir_is_gone`,
/// `base_clone_fails_when_the_object_database_is_gone`,
/// `base_clone_ignores_a_plain_checkout`.
fn classify(workspace: &Path) -> BaseCloneState {
    let Some(gitdir) = worktree_gitdir(workspace) else {
        return BaseCloneState::NotLinked;
    };
    let Some((base, common)) = base_and_common_dir(&gitdir) else {
        return BaseCloneState::NotLinked;
    };
    if !gitdir.is_dir() {
        return BaseCloneState::Severed {
            base,
            reason: "its per-worktree admin directory is gone",
        };
    }
    if !common.join("HEAD").is_file() {
        return BaseCloneState::Severed {
            base,
            reason: "it has no HEAD",
        };
    }
    if !common.join("objects").is_dir() {
        return BaseCloneState::Severed {
            base,
            reason: "it has no object database",
        };
    }
    BaseCloneState::Healthy(base)
}

/// Probe every live session workspace for a base clone that stopped resolving.
///
/// Why: see the module doc — this is the detection half of #3605, and it is a
/// hard `Fail` rather than a `Warn` deliberately. The condition is not degraded
/// service: every git operation in every affected worktree is already failing,
/// and unpushed commits that lived only in that clone are already unreachable.
/// A quiet row is what made the original incident expensive.
/// What: classifies each path in `active_workspace_paths`, groups the severed
/// ones by base clone, and reports `Ok` when none is severed (naming how many
/// linked worktrees were examined) or `Fail` naming each broken base, the
/// reason, and how many live worktrees hang off it. Reads only.
/// Test: `base_clone_ok_with_no_live_workspaces`,
/// `base_clone_ok_for_a_healthy_worktree`,
/// `base_clone_fails_when_the_admin_dir_is_gone`,
/// `base_clone_counts_every_worktree_behind_one_base`.
pub(super) fn check_base_clones(active_workspace_paths: &[PathBuf]) -> DoctorCheck {
    let mut linked = 0usize;
    // BTreeMap so a multi-base failure reports in a stable order.
    let mut severed: BTreeMap<PathBuf, (&'static str, usize)> = BTreeMap::new();
    for workspace in active_workspace_paths {
        match classify(workspace) {
            BaseCloneState::NotLinked => {}
            BaseCloneState::Healthy(_) => linked += 1,
            BaseCloneState::Severed { base, reason } => {
                linked += 1;
                let entry = severed.entry(base).or_insert((reason, 0));
                entry.1 += 1;
            }
        }
    }

    if severed.is_empty() {
        return DoctorCheck::new(
            "base_clone",
            CheckStatus::Ok,
            format!(
                "{linked} live worktree(s) resolve through their base clone \
                 ({} session workspace(s) examined)",
                active_workspace_paths.len()
            ),
        );
    }

    let detail = severed
        .iter()
        .map(|(base, (reason, count))| {
            format!("{} — {reason} ({count} live worktree(s))", base.display())
        })
        .collect::<Vec<_>>()
        .join("; ");
    DoctorCheck::new(
        "base_clone",
        CheckStatus::Fail,
        format!(
            "base clone lost its git identity while live worktrees still point at it: \
             {detail}. Every git command in those worktrees fails with `fatal: not a \
             git repository`, and commits that lived only in that clone are \
             unreachable. Do NOT recursively delete the base — quarantine it (`mv` it \
             aside) and re-clone, so an orphaned worktree can still be repointed at \
             the copy (issue #3605)."
        ),
    )
}

#[cfg(test)]
#[path = "doctor_base_clone_tests.rs"]
mod tests;
