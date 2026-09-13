//! Should tm run `git init` before seeding a `CLAUDE.md`? (#7673 round 3)
//!
//! Why: the seed-site guard ([`crate::core::claude_md_seed::refuse_seed_at`])
//! closed the `$HOME` case, but left a gap the owner ruled on 2026-09-13: a
//! marker-less directory below `$HOME` that is not in any git repository —
//! `~/projects`, say — is still seedable, and the seeded file then becomes an
//! ancestor `CLAUDE.md` for every project beneath it. Two scans close it:
//! UPWARD (is `dir` already inside a repository — possibly one
//! [`harness_root::harness_root_for`][crate::core::harness_root::harness_root_for]
//! missed because `git` itself is unavailable?) and DOWNWARD (is `dir` itself a
//! WORKSPACE PARENT that would inject the seed into every child repository
//! beneath it?).
//!
//! What: [`offer_git_init`] is the one function the seed call site runs right
//! after [`crate::core::claude_md_seed::refuse_seed_at`] returns `None`. It
//! never offers `git init` inside an existing repository — that would create a
//! nested repository (an accidental submodule) — and it refuses outright when
//! `dir` is a workspace parent. Otherwise it asks the caller's `should_init`
//! seam, defaulting to declined (no `git init`, seed anyway) for every
//! non-interactive caller: `None` means "no prompt capability", and a real
//! `git init` only ever runs through [`trusty_common::git::command_in`] — the
//! workspace's one `git` subprocess entry point — never a second
//! `Command::new("git")`.
//! Test: `claude_md_seed_git_tests.rs`.

use std::path::{Path, PathBuf};

use crate::core::claude_md_seed::{SeedRefusal, refuse_seed_at};

/// Bound on the downward workspace-parent scan: `dir`'s immediate children,
/// plus one further level beneath each.
///
/// Why: an unbounded recursive walk could hang on a huge, deeply-nested, or
/// symlink-cyclic tree, and this check runs on every non-git seed attempt — it
/// has to stay cheap. Two levels covers the shape that actually motivated the
/// ruling (a monorepo workspace whose child projects sit at `<parent>/<name>`
/// or one level deeper, e.g. `<parent>/packages/<name>`) without walking the
/// whole subtree.
const WORKSPACE_SCAN_DEPTH: u8 = 2;

/// Does any strict ancestor of `dir` contain a `.git` entry?
///
/// Why: [`harness_root::harness_root_for`][crate::core::harness_root::harness_root_for]
/// answers the same question through a `git` subprocess, which is right for
/// the ordinary case but blind when `git` itself is unavailable while a
/// `.git` directory genuinely sits on disk. This is the filesystem-only
/// fallback the owner's ruling asks for; it never replaces the git-backed
/// check, only backs it up.
/// Test: `an_ancestor_git_directory_is_found_without_the_git_binary`.
fn has_git_ancestor(dir: &Path) -> bool {
    dir.ancestors().skip(1).any(|a| a.join(".git").exists())
}

/// The first child (or grandchild) git repository found beneath `dir`, if any.
///
/// What: `dir`'s immediate children, then — for every child that is itself a
/// plain directory — that child's own immediate children, each checked for a
/// `.git` entry. See [`WORKSPACE_SCAN_DEPTH`] for why the walk stops there. An
/// unreadable directory contributes nothing rather than failing the scan.
/// Test: `a_workspace_parent_is_refused_naming_the_child`,
/// `a_grandchild_repository_is_found`,
/// `a_repository_three_levels_down_is_not_found`.
fn find_child_git_repo(dir: &Path) -> Option<PathBuf> {
    debug_assert_eq!(
        WORKSPACE_SCAN_DEPTH, 2,
        "the doc comment above names this bound"
    );
    let Ok(children) = std::fs::read_dir(dir) else {
        return None;
    };
    let mut second_level = Vec::new();
    for entry in children.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        if path.join(".git").exists() {
            return Some(path);
        }
        second_level.push(path);
    }
    for child in second_level {
        let Ok(grandchildren) = std::fs::read_dir(&child) else {
            continue;
        };
        for entry in grandchildren.flatten() {
            let path = entry.path();
            if path.is_dir() && path.join(".git").exists() {
                return Some(path);
            }
        }
    }
    None
}

/// May tm seed a `CLAUDE.md` at `dir`, and should it run `git init` first?
///
/// Why: see the module header.
/// What: `Err(refusal)` when [`refuse_seed_at`] already refuses `dir` (Home /
/// AboveHome — unchanged), or when `dir` is a workspace parent
/// ([`SeedRefusal::WorkspaceParent`], naming the child repository found).
/// `Ok(false)` with `should_init` never called when `dir` is already inside a
/// repository (through `harness_root_for` or [`has_git_ancestor`]) — running
/// `git init` there would create a nested repository. Otherwise `Ok(true)`
/// only when `should_init` is `Some` and returns `true`, in which case `git
/// init` has already run (via [`trusty_common::git::command_in`]); `Ok(false)`
/// for a `None` seam (no prompt capability — every non-interactive caller) or
/// a declined one. Either `Ok` variant means "go ahead and seed"; only `Err`
/// means "do not write the file".
/// Test: `a_workspace_parent_is_refused_naming_the_child`,
/// `git_init_is_never_offered_inside_an_existing_repository`,
/// `a_non_interactive_caller_declines_git_init_and_still_may_seed`,
/// `an_accepted_offer_runs_git_init`.
pub fn offer_git_init(
    dir: &Path,
    home: Option<&Path>,
    should_init: Option<&mut dyn FnMut() -> bool>,
) -> Result<bool, SeedRefusal> {
    if let Some(refusal) = refuse_seed_at(dir, home) {
        return Err(refusal);
    }
    if crate::core::harness_root::harness_root_for(dir).is_some() || has_git_ancestor(dir) {
        // Already tracked — never offer `git init` here, because that would
        // nest a second repository inside the one that already owns `dir`.
        return Ok(false);
    }
    if let Some(child) = find_child_git_repo(dir) {
        return Err(SeedRefusal::WorkspaceParent(child));
    }
    let wants_init = should_init.is_some_and(|f| f());
    if !wants_init {
        return Ok(false);
    }
    let out = trusty_common::git::command_in(dir)
        .args(["init", "-q"])
        .output();
    Ok(out.is_ok_and(|o| o.status.success()))
}

#[cfg(test)]
#[path = "claude_md_seed_git_tests.rs"]
mod tests;
