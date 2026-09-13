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
//! `git init` only ever runs through [`trusty_common::git::command`] — the
//! workspace's one `git` subprocess entry point — never a second
//! `Command::new("git")`. An accepted offer whose `git init` fails refuses the
//! seed rather than reading as a decline (#7774 review).
//! Test: `claude_md_seed_git_tests.rs`.

use std::path::Path;

use crate::core::child_repo_scan::{ChildRepoScan, scan_for_child_repo};
use crate::core::claude_md_seed::{SeedRefusal, refuse_seed_at};

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

/// May tm seed a `CLAUDE.md` at `dir`, and should it run `git init` first?
///
/// Why: see the module header.
/// What: `Err(refusal)` when [`refuse_seed_at`] already refuses `dir` (Home /
/// AboveHome — unchanged), or when `dir` is a workspace parent
/// ([`SeedRefusal::WorkspaceParent`], naming the child repository found), or
/// when the downward scan could not finish ([`SeedRefusal::ScanIncomplete`]).
/// `Ok(false)` with `should_init` never called when `dir` is already inside a
/// repository — `harness_root_for` is checked before the downward scan, and
/// [`has_git_ancestor`] only after it, so an ancestor `.git` git itself rejects
/// cannot skip the scan — because running `git init` there would create a
/// nested repository. Otherwise `Ok(true)` only when `should_init` is `Some`
/// and returns `true`, in which case [`run_git_init`] has left a git work tree
/// in `dir`; `Ok(false)` for a `None` seam (no prompt capability — every
/// non-interactive caller) or a declined one. An accepted offer that does not
/// produce a work tree in `dir` is `Err` carrying
/// [`SeedRefusal::GitInitFailed`], never `Ok(false)`. Either `Ok` variant means
/// "go ahead and seed"; only `Err` means "do not write the file".
/// Test: `a_workspace_parent_is_refused_naming_the_child`,
/// `a_wide_node_modules_sibling_never_lets_a_workspace_parent_seed`,
/// `an_exhausted_scan_refuses_to_seed`,
/// `an_unreadable_child_directory_refuses_to_seed`,
/// `git_init_is_never_offered_inside_an_existing_repository`,
/// `a_non_interactive_caller_declines_git_init_and_still_may_seed`,
/// `an_accepted_offer_runs_git_init`,
/// `an_accepted_offer_on_a_first_touch_directory_runs_git_init`,
/// `a_failed_git_init_refuses_to_seed`,
/// `a_stale_ancestor_git_file_cannot_bypass_the_workspace_parent_scan`.
pub fn offer_git_init(
    dir: &Path,
    home: Option<&Path>,
    should_init: Option<&mut dyn FnMut() -> bool>,
) -> Result<bool, SeedRefusal> {
    if let Some(refusal) = refuse_seed_at(dir, home) {
        return Err(refusal);
    }
    if crate::core::harness_root::harness_root_for(dir).is_some() {
        // Already tracked — never offer `git init` here, because that would
        // nest a second repository inside the one that already owns `dir`.
        return Ok(false);
    }
    // #7673: only a scan that checked everything may reach the offer or the seed.
    // #7774 review: the scan runs BEFORE the filesystem-only ancestor check, so
    // a stale or broken ancestor `.git` cannot skip the workspace-parent refusal.
    match scan_for_child_repo(dir) {
        ChildRepoScan::Clear => {}
        ChildRepoScan::Found(child) => return Err(SeedRefusal::WorkspaceParent(child)),
        ChildRepoScan::Incomplete(stop) => return Err(SeedRefusal::ScanIncomplete(stop)),
    }
    if has_git_ancestor(dir) {
        // The same nested-repository rule, for an ancestor git cannot see.
        return Ok(false);
    }
    let wants_init = should_init.is_some_and(|f| f());
    if !wants_init {
        return Ok(false);
    }
    run_git_init(dir)?;
    Ok(true)
}

/// Run `git init` for `dir` and prove it left a git work tree there.
///
/// Why: a failed `git init` is not a decline — the operator asked for a
/// repository — and git's exit status alone does not prove one exists in
/// `dir`: an inherited `GIT_DIR` puts it elsewhere, a `git` on `PATH` may do
/// nothing, and a `-`-prefixed `dir` parses as an option (#7774 review).
/// What: `git init -q -- <dir>` through [`trusty_common::git::command`] with
/// `GIT_DIR` and `GIT_WORK_TREE` removed from the child environment. `dir` is
/// positional, so git creates a first-touch directory the pipeline has not
/// created yet, and `--` keeps it a path. `Ok(())` only when git exits 0 AND
/// [`is_initialised_work_tree`] holds; otherwise
/// [`SeedRefusal::GitInitFailed`].
/// Test: `a_failed_git_init_refuses_to_seed`,
/// `an_inherited_git_dir_cannot_redirect_git_init`,
/// `a_git_that_exits_zero_without_initialising_refuses_to_seed`,
/// `a_dash_prefixed_directory_is_initialised_as_a_path`.
fn run_git_init(dir: &Path) -> Result<(), SeedRefusal> {
    let out = trusty_common::git::command()
        .args(["init", "-q", "--"])
        .arg(dir)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .output()
        .map_err(|e| SeedRefusal::GitInitFailed(format!("could not run git: {e}")))?;
    if !out.status.success() {
        return Err(SeedRefusal::GitInitFailed(init_failure_reason(&out)));
    }
    if !is_initialised_work_tree(dir) {
        return Err(SeedRefusal::GitInitFailed(format!(
            "git init exited 0 but {} has no .git/HEAD",
            dir.display()
        )));
    }
    Ok(())
}

/// Does `dir` hold a non-bare repository's `.git/HEAD` file — what `git init`
/// writes into the directory it initialises?
fn is_initialised_work_tree(dir: &Path) -> bool {
    std::fs::metadata(dir.join(".git").join("HEAD")).is_ok_and(|m| m.is_file())
}

/// Why `git init` failed, for [`SeedRefusal::GitInitFailed`]: git's own
/// trimmed stderr, or the exit status when git wrote nothing.
fn init_failure_reason(out: &std::process::Output) -> String {
    let stderr = String::from_utf8_lossy(&out.stderr);
    match stderr.trim() {
        "" => format!("git init exited with {}", out.status),
        text => text.to_string(),
    }
}

#[cfg(test)]
#[path = "claude_md_seed_git_tests.rs"]
mod tests;
