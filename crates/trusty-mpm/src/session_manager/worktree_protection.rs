//! Does git still hold state at this directory? (issue #4732)
//!
//! Why: `decommission::remove_session_worktree` used to read ANY non-zero
//! `git worktree remove --force` exit as "git could not do it, so do it
//! ourselves" and fall through to `std::fs::remove_dir_all`. Git exits 128 for
//! every fatal condition, and the worst of them is deliberate: `git worktree
//! lock` is an operator's explicit "do not remove this", and git honours it by
//! exiting 128. Locking a worktree to protect it was therefore exactly what
//! caused it to be deleted. The same fall-through swallowed a stale worktree
//! pointer, an unreadable `.git`, and a repository git merely declined to read.
//!
//! What: [`GitProtection`] — three states, never a bool. "git holds state here"
//! and "git could not be asked" are different facts, but they have the SAME
//! safe answer (refuse), while "git positively holds nothing here" is the only
//! state that may proceed to a raw directory removal. Two entry points classify
//! the two ways the removal path can fail to get an answer from git:
//! [`protection_after_failed_removal`] (git ran and declined) and
//! [`protection_without_registry_root`] (git would not even name the checkout
//! that owns the path).
//!
//! This module NEVER writes and NEVER removes. Every git invocation here is a
//! read-only query.
//!
//! Scope note: #4735 will lift this into a shared `GitProbe<T>` alongside
//! `trusty-agents-common`'s `agents::vcs_claim`, whose three-state shape,
//! narrow stderr matching, and filesystem corroboration this module
//! deliberately mirrors. It is one self-contained file so that extraction is a
//! move rather than a rewrite.
//!
//! Test: `worktree_protection_tests`.

use std::path::Path;

use super::worktree_registry::{list_registered_worktrees, registry_root_for};

/// The only `git worktree remove` stderr CONSISTENT with "git has nothing
/// registered at this path".
///
/// Why: the exit code cannot distinguish the cases — git exits 128 for all of
/// them. Measured against git 2.54.0, the three failures this path actually
/// meets are:
///
/// | condition | stderr | exit |
/// |---|---|---|
/// | operator-locked worktree | `cannot remove a locked working tree; use 'remove -f -f' …` | 128 |
/// | worktree with a broken `.git` file | `validation failed, cannot remove working tree: '…/.git' is not a .git file, error code 7` | 128 |
/// | plain directory / already-pruned worktree | `'<path>' is not a working tree` | 128 |
///
/// Only the third means git is holding nothing. Note how close the second one
/// reads — `is not a .git file` — while meaning the opposite; matching on the
/// looser `not a working tree` fragment or on `128` alone deletes a live
/// worktree. Anything that is not this exact phrase is treated as a REFUSAL,
/// so an unrecognized or reworded message fails closed.
///
/// Consistent with, NOT proof of: git emits this same text for a worktree
/// whose admin directory (`.git/worktrees/<name>`) was removed out of band,
/// where the working tree and its uncommitted content are entirely intact —
/// the state ~70 worktrees on this machine were left in on 2026-07-21.
/// [`protection_after_failed_removal`] therefore corroborates it against a
/// filesystem witness and git's own registry before concluding anything.
/// What: verified byte-identical against git 2.54.0.
/// Test: `a_locked_worktree_is_protected`,
/// `a_broken_git_file_is_protected`,
/// `an_unregistered_directory_inside_a_repo_is_unclaimed`.
const NOT_A_WORKING_TREE: &str = "is not a working tree";

/// What git says about a directory the removal path is about to delete (#4732).
///
/// Why: [`Undetermined`](Self::Undetermined) is not a synonym for
/// [`Unclaimed`](Self::Unclaimed). If git could not be consulted, the honest
/// answer is that protected state cannot be ruled out, and the caller must
/// refuse rather than guess. Folding the two together is the fail-open shape
/// that made this function delete locked worktrees.
/// What: [`Protected`](Self::Protected) — git holds state here, or declined to
/// give it up; [`Unclaimed`](Self::Unclaimed) — positively established that no
/// git state exists at or claiming this path; [`Undetermined`](Self::Undetermined)
/// — no conclusion is available. Both non-`Unclaimed` variants carry an
/// operator-facing reason and are reported identically by
/// [`Self::refusal`], so a caller cannot accidentally handle only one of them.
/// Test: `worktree_protection_tests`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum GitProtection {
    /// Git holds state here, or declined to remove it. NEVER delete.
    Protected(String),
    /// No git state exists at or claiming this path. A raw removal is safe.
    Unclaimed,
    /// Git could not be consulted, or gave an answer this code cannot read.
    Undetermined(String),
}

impl GitProtection {
    /// `Some(reason)` when the caller must NOT remove the directory.
    ///
    /// Why: the whole point of the three states is that two of them refuse.
    /// Exposing the refusal as one accessor means a caller cannot match on
    /// `Protected` alone and silently let `Undetermined` through — the exact
    /// collapse this enum exists to prevent.
    /// What: `None` only for [`Unclaimed`](Self::Unclaimed).
    /// Test: `refusal_covers_both_non_unclaimed_states`.
    pub(super) fn refusal(&self) -> Option<&str> {
        match self {
            Self::Unclaimed => None,
            Self::Protected(reason) | Self::Undetermined(reason) => Some(reason),
        }
    }
}

/// Does `path` itself carry a `.git` entry?
///
/// Why: this is a witness git does not consult once its own discovery has
/// failed. `symlink_metadata` needs only the PARENT directory's search bit,
/// which is necessarily present — the caller already stat'd `path`. So a
/// worktree whose `.git` file dangles, is unreadable (mode 000), or names a
/// gitdir that no longer exists still answers `true` here, even though every
/// `git` invocation against it reports "not a git repository".
/// What: `symlink_metadata` on `<path>/.git`, which does not follow the link
/// and does not read the entry's contents.
/// Test: `a_stale_worktree_pointer_is_protected`,
/// `an_unreadable_git_entry_is_protected`.
fn carries_git_entry(path: &Path) -> bool {
    path.join(".git").symlink_metadata().is_ok()
}

/// Does the repository rooted at `root` still register `path` as a worktree?
///
/// Why: `git worktree list --porcelain` is the registry itself — a structured
/// answer, not a message to be pattern-matched. When it disagrees with the
/// removal command's stderr, the registry wins and the path is protected.
/// What: `None` from [`list_registered_worktrees`] is an unanswerable probe,
/// never an empty registry, so it yields
/// [`Undetermined`](GitProtection::Undetermined). Paths are compared
/// canonicalised on both sides because git records resolved paths (`/private/var`
/// on macOS) while callers hold the unresolved spelling; a `path` that cannot be
/// canonicalised is itself [`Undetermined`](GitProtection::Undetermined) rather
/// than compared as a raw string that could never match.
/// Test: `a_locked_worktree_is_protected`,
/// `a_registered_worktree_is_protected_even_when_git_says_otherwise`,
/// `an_unregistered_directory_inside_a_repo_is_unclaimed`.
fn registry_verdict(root: &Path, path: &Path) -> GitProtection {
    let Some(registered) = list_registered_worktrees(root) else {
        return GitProtection::Undetermined(
            "git's worktree registry could not be read for the owning checkout".into(),
        );
    };
    let Ok(canonical) = path.canonicalize() else {
        return GitProtection::Undetermined(
            "the path could not be canonicalised, so git's registry cannot be compared \
             against it"
                .into(),
        );
    };
    let found = registered
        .into_iter()
        .find(|w| w.path.canonicalize().unwrap_or_else(|_| w.path.clone()) == canonical);
    match found {
        Some(w) if w.locked => GitProtection::Protected(
            "the worktree is git-locked — an explicit operator 'do not remove this'".into(),
        ),
        Some(_) => {
            GitProtection::Protected("git still registers this path as one of its worktrees".into())
        }
        None => GitProtection::Unclaimed,
    }
}

/// Classify a NON-ZERO `git worktree remove --force` exit (#4732).
///
/// Why: this decides whether the raw `remove_dir_all` fallback may run at all.
/// Before #4732 it always ran, so `git worktree lock` — the one mechanism an
/// operator has to say "leave this alone" — caused deletion instead of
/// preventing it.
/// What: three gates, each fail-closed.
///   1. stderr is not [`NOT_A_WORKING_TREE`] → git declined for a reason this
///      code does not recognize as "nothing here". Refuse, quoting git.
///   2. `path` carries a `.git` entry → git's message is describing its own
///      failed discovery, not an empty directory. Refuse.
///   3. otherwise the owning repository's registry decides, via
///      [`registry_verdict`].
///
/// Only gate 3 returning [`Unclaimed`](GitProtection::Unclaimed) permits the
/// fallback.
///
/// Test: `a_locked_worktree_is_protected`, `a_broken_git_file_is_protected`,
/// `a_registered_worktree_is_protected_even_when_git_says_otherwise`,
/// `an_unregistered_directory_inside_a_repo_is_unclaimed`,
/// `an_unreadable_registry_is_undetermined`.
pub(super) fn protection_after_failed_removal(
    path: &Path,
    repo_root: &Path,
    stderr: &str,
) -> GitProtection {
    if !stderr.contains(NOT_A_WORKING_TREE) {
        return GitProtection::Protected(format!(
            "git declined to remove it: {}",
            stderr.split_whitespace().collect::<Vec<_>>().join(" ")
        ));
    }
    if carries_git_entry(path) {
        return GitProtection::Protected(
            "the directory carries a .git entry — a worktree git could not read, \
             not an unmanaged directory"
                .into(),
        );
    }
    registry_verdict(repo_root, path)
}

/// Classify the case where git would not name a registry root for `path` (#4732).
///
/// Why: `registry_root_for` returns `None` both when there is genuinely no
/// repository and when git could not resolve one — and the removal path read
/// that `None` as "a plain directory, remove it directly". A worktree whose
/// admin directory was deleted out of band answers exactly that way
/// (`fatal: not a git repository: (null)`) while its working tree, including
/// uncommitted work, is fully intact. That is the state the 2026-07-21 incident
/// left ~70 worktrees in.
/// What: three gates, each fail-closed.
///   1. `path` carries a `.git` entry → it IS a checkout or worktree, however
///      broken. Refuse.
///   2. no ancestor carries a `.git` entry → positively outside any repository,
///      so nothing can be claiming it. [`Unclaimed`](GitProtection::Unclaimed).
///      The path is canonicalised first: a relative path would otherwise walk a
///      truncated ancestor chain, miss the project's `.git`, and land back on
///      the permissive answer.
///   3. an ancestor IS a repository → that repository's registry decides, via
///      [`registry_verdict`] against the root git names for the PARENT
///      directory (asking from `path` itself is what failed).
///
/// Test: `a_stale_worktree_pointer_is_protected`,
/// `an_unreadable_git_entry_is_protected`,
/// `a_directory_no_repository_claims_is_unclaimed`,
/// `an_unregistered_leftover_under_a_repo_is_unclaimed`.
pub(super) fn protection_without_registry_root(path: &Path) -> GitProtection {
    if carries_git_entry(path) {
        return GitProtection::Protected(
            "the directory carries a .git entry that git cannot resolve — a stale or \
             broken worktree pointer, not an unmanaged directory"
                .into(),
        );
    }
    let Ok(canonical) = path.canonicalize() else {
        return GitProtection::Undetermined(
            "the path could not be canonicalised, so its ancestors cannot be inspected".into(),
        );
    };
    let ancestor_repo = canonical
        .ancestors()
        .skip(1)
        .any(|a| a.join(".git").symlink_metadata().is_ok());
    if !ancestor_repo {
        return GitProtection::Unclaimed;
    }
    let Some(parent) = canonical.parent() else {
        return GitProtection::Undetermined(
            "an ancestor carries a .git entry but the path has no parent to ask about".into(),
        );
    };
    let Some(root) = registry_root_for(parent) else {
        return GitProtection::Undetermined(
            "an ancestor carries a .git entry but git would not name the checkout that \
             owns it"
                .into(),
        );
    };
    registry_verdict(&root, &canonical)
}

#[cfg(test)]
#[path = "worktree_protection_tests.rs"]
mod worktree_protection_tests;
