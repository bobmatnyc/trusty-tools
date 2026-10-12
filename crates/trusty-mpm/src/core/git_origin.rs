//! Read a checkout's `origin` remote without mistaking a git failure for none (#4734).
//!
//! Why: [`get_origin_url`] is plain git plumbing, but it lived in
//! `daemon::managed_routes::inproject`, which the `daemon` feature gates, while
//! the always-on `core::remote_mode` calls it — so a plain
//! `--no-default-features` build failed to compile (#9776). `inproject`
//! re-exports it, so every existing path still resolves.
//! What: [`get_origin_url`] and its guard `ensure_repo_rooted_at`.
//! Test: `get_origin_url_returns_none_for_non_git`,
//! `get_origin_url_errors_when_discovery_escapes_to_a_parent_repo` (in
//! `inproject`'s tests).

use std::path::{Path, PathBuf};

/// Confirm git reads the repository rooted at `path`, when one is rooted there.
///
/// Why: `git config --get` answers from whatever repository DISCOVERY finds,
/// and discovery walks straight past a `.git` it cannot read — which hides two
/// faults the exit code cannot express (#4734). A `.git` directory at mode 000
/// with no repo above it exits **1 with empty stderr**, identical to "no origin
/// remote"; the same `.git` nested inside another repo exits **0 with the
/// PARENT repo's origin**, which would have the daemon provision a managed
/// clone of the wrong repository. Neither shows up in the exit code or in
/// stderr, so the exit-code split alone cannot catch them and no amount of
/// stderr matching would either.
/// What: `path/.git` existing means a repository is rooted at `path`, so git's
/// own `rev-parse --show-toplevel` must name `path` back. A failure to answer,
/// or an answer naming a different directory, is `Err`. When `path/.git` does
/// NOT exist the check is skipped, which keeps the two legitimate cases
/// working: `path` is not in a repo at all (the caller's read exits 1, a true
/// `Ok(None)`), or `path` is a subdirectory of a repo, where discovering the
/// enclosing repo is exactly what callers want. `--show-toplevel` rather than
/// the git dir because a linked worktree's git dir lives under the main
/// checkout while its toplevel is the worktree itself — and tm creates linked
/// worktrees, so that case is load-bearing.
/// Test: `get_origin_url_errors_when_the_git_dir_is_unreadable`,
/// `get_origin_url_errors_when_discovery_escapes_to_a_parent_repo`,
/// `get_origin_url_reads_a_linked_worktree`.
fn ensure_repo_rooted_at(path: &Path) -> Result<(), String> {
    if !path.join(".git").exists() {
        return Ok(());
    }

    let out = trusty_common::git::command()
        .arg("-C")
        .arg(path)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .map_err(|e| format!("could not run `git rev-parse` in '{}': {e}", path.display()))?;

    if !out.status.success() {
        return Err(format!(
            "'{}' has a .git entry but git cannot read a repository there: {}",
            path.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }

    let toplevel = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim());
    let rooted_here = std::fs::canonicalize(&toplevel)
        .ok()
        .zip(std::fs::canonicalize(path).ok())
        .is_some_and(|(found, want)| found == want);
    if !rooted_here {
        return Err(format!(
            "'{}' has a .git entry but git resolved the repository to '{}'; refusing to \
             answer with another repository's remote",
            path.display(),
            toplevel.display()
        ));
    }
    Ok(())
}

/// Read the `remote.origin.url` from a git repository at `path`.
///
/// Why: the in-project spawn path needs the remote URL to determine the
/// `owner/repo` identity and locate the matching base clone. Before #4734 this
/// returned a bare `Option`, so "this repo has no `origin` remote" and "git
/// could not answer at all" were the same value, and every caller read it as
/// the former — `tm launch` and the guided default told the operator a repo
/// they could not read had no remote, or was not a GitHub project, and sent
/// them to `tm connect` for a checkout that is fine.
/// What: guards with [`ensure_repo_rooted_at`], then runs
/// `git -C <path> config --get remote.origin.url` and keeps apart the outcomes
/// git distinguishes by exit code. Exit 0 yields `Ok(Some(url))`, or `Ok(None)`
/// when the configured value is empty. Exit 1 is git-config's "the key is not
/// set" answer, so it yields `Ok(None)` — both a plain non-git directory and a
/// repo with no `origin` land there, which is why this split changes no
/// existing caller's behaviour on a healthy machine (a multi-valued `origin`,
/// the one working setup that could have regressed into `Err`, exits 0).
/// Anything else — a spawn failure, or a fatal exit such as 128 for an
/// unreadable `.git/config`, a dangling gitdir pointer, or a `safe.directory`
/// ownership refusal — is `Err`, for the caller to fail closed on.
/// Test: `get_origin_url_returns_none_for_non_git`,
/// `get_origin_url_returns_none_for_repo_without_origin`,
/// `get_origin_url_handles_a_multi_valued_origin`,
/// `get_origin_url_reads_a_linked_worktree`,
/// `get_origin_url_errors_when_git_config_is_unreadable`,
/// `get_origin_url_errors_when_the_git_dir_is_unreadable`,
/// `get_origin_url_errors_when_discovery_escapes_to_a_parent_repo` (unit);
/// `try_inproject_spawn_errors_when_git_cannot_read_the_remote`
/// (`tests/inproject_git_failure.rs`).
pub fn get_origin_url(path: &Path) -> Result<Option<String>, String> {
    // #4734: the exit code alone cannot separate "no origin remote" from "git
    // never read the repo rooted here" — see `ensure_repo_rooted_at`.
    ensure_repo_rooted_at(path)?;

    let out = trusty_common::git::command()
        .arg("-C")
        .arg(path)
        .args(["config", "--get", "remote.origin.url"])
        .output()
        .map_err(|e| {
            format!(
                "could not run `git config --get remote.origin.url` in '{}': {e}",
                path.display()
            )
        })?;

    match out.status.code() {
        Some(0) => {
            let url = String::from_utf8_lossy(&out.stdout).trim().to_string();
            Ok(if url.is_empty() { None } else { Some(url) })
        }
        Some(1) => Ok(None),
        Some(code) => Err(format!(
            "`git config --get remote.origin.url` failed in '{}' (exit {code}): {}",
            path.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        )),
        None => Err(format!(
            "`git config --get remote.origin.url` was killed by a signal in '{}'",
            path.display()
        )),
    }
}
