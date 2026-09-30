//! One answer to "does this repository have a remote?" (#8934).
//!
//! Why: the owner ruled on 2026-09-30 that a managed session must run in a git
//! repository with no `origin` remote — local-only worktrees, no push and no
//! PR, merges kept local. Every tm path that spawns, fetches, pushes, opens a
//! PR or pins a gh account has to ask the same question, and two predicates
//! would drift. A local-only repository also has no GitHub identity to prove,
//! so #8914's rule that an unprovable pin fails closed means its gh is
//! disabled outright rather than left to the machine's active account.
//! What: [`remote_mode`] classifies a directory; [`LOCAL_ONLY_SKIP`] is the
//! one-line notice a skipped push/PR step prints; [`local_only_gh_vars`] is
//! the explicit "no remote, no gh" spawn pin. A local-only worktree is cut from
//! the repository root's `HEAD` — the local default branch — because there is
//! no `origin/<default>` to fetch.
//! Test: `remote_mode_tests.rs`.

use std::path::Path;

/// The notice every skipped push, PR, fetch or remote-branch step prints.
pub const LOCAL_ONLY_SKIP: &str = "local-only repo: no remote; skipping push/PR";

/// The `GH_CONFIG_DIR` a local-only session is pinned to.
///
/// Why: gh cannot read or create a directory under `/dev/null`, so every gh
/// command exits before it authenticates ("failed to read configuration: …
/// not a directory", measured with the installed gh on 2026-09-30) — whatever
/// token the session later sets, and with no directory to write a login into.
pub const LOCAL_ONLY_GH_CONFIG_DIR: &str = "/dev/null/tm-local-only-repo-no-gh";

/// The value a local-only session gets in `GH_TOKEN` and `GH_ENTERPRISE_TOKEN`.
///
/// Why: an env token outranks every other gh credential source, so filling
/// both keeps an inherited real token from reaching gh; it authenticates as
/// nobody and is not a secret.
pub const LOCAL_ONLY_GH_TOKEN: &str = "tm-local-only-repo-no-gh";

/// Whether a directory's repository has an `origin` remote.
///
/// Why: see the module docs — the three answers need different handling, and
/// an `Option` would read "not a repository" the same as "no remote".
/// What: `Origin` carries the configured URL; `LocalOnly` is a git work tree
/// with no `origin` (other remotes are ignored: tm pushes only to `origin`);
/// `NotARepository` is a directory git finds no work tree for.
/// Test: `remote_mode_reads_origin_local_only_and_non_repo`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoteMode {
    /// `origin` is configured; the value is its URL.
    Origin(String),
    /// A git work tree with no `origin` remote.
    LocalOnly,
    /// Not inside a git work tree.
    NotARepository,
}

impl RemoteMode {
    /// Whether this is a repository with no `origin` remote.
    pub fn is_local_only(&self) -> bool {
        matches!(self, RemoteMode::LocalOnly)
    }
}

/// Classify `path` by its `origin` remote (#8934).
///
/// Why: the single predicate every remote-touching tm path shares.
/// What: reads `origin` through
/// [`crate::daemon::managed_routes::inproject::get_origin_url`] (which keeps a
/// git failure apart from an absent remote, #4734, and propagates it as
/// `Err`); with no `origin`, asks git whether `path` is inside a work tree.
/// Test: `remote_mode_reads_origin_local_only_and_non_repo`,
/// `remote_mode_propagates_an_unreadable_git_dir`.
pub fn remote_mode(path: &Path) -> Result<RemoteMode, String> {
    if let Some(url) = crate::daemon::managed_routes::inproject::get_origin_url(path)? {
        return Ok(RemoteMode::Origin(url));
    }
    let inside = trusty_common::git::command()
        .arg("-C")
        .arg(path)
        .args(["rev-parse", "--is-inside-work-tree"])
        .output()
        .is_ok_and(|out| out.status.success() && out.stdout.trim_ascii() == b"true");
    Ok(if inside {
        RemoteMode::LocalOnly
    } else {
        RemoteMode::NotARepository
    })
}

/// Whether `path` is the ROOT of a local-only repository — a `.git` entry of
/// its own and no `origin` remote.
///
/// Why: the daemon's local-only spawn serves a repository root only, the same
/// rule the in-project path applies to a GitHub checkout (ADR-0055).
/// What: `path/.git` exists and [`remote_mode`] says `LocalOnly`.
/// Test: `local_only_root_is_the_repo_root_only`.
pub fn is_local_only_root(path: &Path) -> Result<bool, String> {
    if !path.join(".git").exists() {
        return Ok(false);
    }
    Ok(remote_mode(path)?.is_local_only())
}

/// The spawn-env pin for a session in a local-only repository (#8934).
///
/// Why: an absent pin hands the session the machine's active gh account — the
/// wrong-identity outcome #8914 closed — so "no remote" is pinned explicitly.
/// What: `GH_CONFIG_DIR` = [`LOCAL_ONLY_GH_CONFIG_DIR`] and both token vars =
/// [`LOCAL_ONLY_GH_TOKEN`]. Setting `GH_CONFIG_DIR` also makes
/// [`crate::core::gh_identity::inherited_identity_to_clear`] strip an inherited
/// `GITHUB_TOKEN` / `GITHUB_ENTERPRISE_TOKEN` from the child.
/// Test: `a_local_only_repo_spawns_with_gh_disabled`,
/// `local_only_gh_env_strips_every_inherited_token`.
pub fn local_only_gh_vars() -> Vec<(String, String)> {
    [
        ("GH_CONFIG_DIR", LOCAL_ONLY_GH_CONFIG_DIR),
        ("GH_TOKEN", LOCAL_ONLY_GH_TOKEN),
        ("GH_ENTERPRISE_TOKEN", LOCAL_ONLY_GH_TOKEN),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect()
}

#[cfg(test)]
#[path = "remote_mode_tests.rs"]
mod tests;
