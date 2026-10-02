//! One answer to "does this repository have a remote?" (#8934).
//!
//! Why: the owner ruled on 2026-09-30 that a managed session must run in a git
//! repository with no `origin` remote — local-only worktrees, no push and no
//! PR, merges kept local. Every tm path that spawns, fetches, pushes, opens a
//! PR or pins a gh account has to ask the same question, and two predicates
//! would drift. A local-only repository also has no GitHub identity to prove,
//! so #8914's rule that an unprovable pin fails closed means its gh is
//! disabled outright rather than left to the machine's active account — except
//! for an allow-listed supervisor (the Architect), whose directory has no
//! origin by design and which needs the machine's gh accounts (07:47Z ruling).
//! What: [`remote_mode`] classifies a directory; [`gh_disabled_for`] decides
//! whether a local-only root gets the no-gh pin; [`local_only_gh_vars`] is that
//! pin; [`local_default_branch`] names the branch a local-only worktree is cut
//! from (there is no `origin/<default>` to fetch); [`LOCAL_ONLY_SKIP`] is the
//! one-line notice a skipped push/PR step prints.
//! Test: `remote_mode_tests.rs`.

use std::path::{Path, PathBuf};

use crate::core::config::MpmConfig;

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
/// with no `origin` (other remotes are ignored: tm pushes only to `origin`),
/// carrying the work tree's top level; `NotARepository` is a directory git
/// positively reports is not in a repository.
/// Test: `remote_mode_reads_origin_local_only_and_non_repo`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoteMode {
    /// `origin` is configured; the value is its URL.
    Origin(String),
    /// A git work tree with no `origin` remote, rooted at `root`.
    LocalOnly {
        /// `git rev-parse --show-toplevel` of the directory asked about.
        root: PathBuf,
    },
    /// Not inside a git work tree.
    NotARepository,
}

impl RemoteMode {
    /// Whether this is a repository with no `origin` remote.
    pub fn is_local_only(&self) -> bool {
        matches!(self, RemoteMode::LocalOnly { .. })
    }
}

/// Classify `path` by its `origin` remote (#8934).
///
/// Why: the single predicate every remote-touching tm path shares.
/// What: reads `origin` through
/// [`crate::daemon::managed_routes::inproject::get_origin_url`] (which keeps a
/// git failure apart from an absent remote, #4734, and propagates it as
/// `Err`); with no `origin`, asks `git rev-parse --show-toplevel`. Success is
/// `LocalOnly`; a failure whose stderr says "not a git repository" is
/// `NotARepository`; any other failure (a `safe.directory` "dubious
/// ownership" refusal, an unrunnable git) is `Err`, never a guess.
/// Test: `remote_mode_reads_origin_local_only_and_non_repo`,
/// `remote_mode_propagates_an_unreadable_git_dir`,
/// `a_git_failure_that_is_not_not_a_repository_is_an_error`.
pub fn remote_mode(path: &Path) -> Result<RemoteMode, String> {
    if let Some(url) = crate::daemon::managed_routes::inproject::get_origin_url(path)? {
        return Ok(RemoteMode::Origin(url));
    }
    let out = trusty_common::git::command()
        .arg("-C")
        .arg(path)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .map_err(|e| format!("could not run `git rev-parse` in '{}': {e}", path.display()))?;
    classify_toplevel(path, out.status.success(), &out.stdout, &out.stderr)
}

/// [`remote_mode`]'s reading of a `git rev-parse --show-toplevel` result for a
/// directory with no `origin` (#8934 LOW 8).
/// Test: `a_git_failure_that_is_not_not_a_repository_is_an_error`.
fn classify_toplevel(
    path: &Path,
    success: bool,
    stdout: &[u8],
    stderr: &[u8],
) -> Result<RemoteMode, String> {
    if success {
        let root = String::from_utf8_lossy(stdout).trim().to_string();
        return Ok(RemoteMode::LocalOnly { root: root.into() });
    }
    let stderr = String::from_utf8_lossy(stderr);
    if stderr.contains("not a git repository") {
        return Ok(RemoteMode::NotARepository);
    }
    Err(format!(
        "`git rev-parse --show-toplevel` failed in '{}': {}",
        path.display(),
        stderr.trim()
    ))
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

/// Whether a local-only repository rooted at `root` gets the no-gh pin.
///
/// Why: the Architect's directory has no origin by design and it needs the
/// machine's gh accounts (07:47Z ruling). Only the allow-listed profile
/// decides that — a project's own `.trusty-mpm.toml` alone cannot (#8453).
/// What: `true` unless [`crate::core::session_profile::resolve`] says
/// supervisor for `root` under `config`.
/// Test: `an_allow_listed_supervisor_keeps_gh`,
/// `a_self_declared_supervisor_does_not_keep_gh`.
pub fn gh_disabled_for(root: &Path, config: &MpmConfig) -> bool {
    !crate::core::session_profile::resolve(root, config).is_supervisor()
}

/// The spawn-env pin for a session in a local-only repository (#8934).
///
/// Why: an absent pin hands the session the machine's active gh account — the
/// wrong-identity outcome #8914 closed — so "no remote" is pinned explicitly.
/// What: `GH_CONFIG_DIR` = [`LOCAL_ONLY_GH_CONFIG_DIR`], both token vars =
/// [`LOCAL_ONLY_GH_TOKEN`], and a git credential clamp like #8914's
/// `git_credential_pin`: `credential.helper` reset to an empty list and
/// `GIT_TERMINAL_PROMPT=0`, so HTTPS git cannot reach a stored credential
/// either. Setting `GH_CONFIG_DIR` also makes
/// [`crate::core::gh_identity::inherited_identity_to_clear`] strip an
/// inherited `GITHUB_TOKEN` / `GITHUB_ENTERPRISE_TOKEN` from the child.
/// Test: `a_local_only_repo_spawns_with_gh_disabled`,
/// `local_only_gh_env_strips_every_inherited_token`.
pub fn local_only_gh_vars() -> Vec<(String, String)> {
    [
        ("GH_CONFIG_DIR", LOCAL_ONLY_GH_CONFIG_DIR),
        ("GH_TOKEN", LOCAL_ONLY_GH_TOKEN),
        ("GH_ENTERPRISE_TOKEN", LOCAL_ONLY_GH_TOKEN),
        ("GIT_CONFIG_COUNT", "1"),
        ("GIT_CONFIG_KEY_0", "credential.helper"),
        ("GIT_CONFIG_VALUE_0", ""),
        ("GIT_TERMINAL_PROMPT", "0"),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect()
}

/// The gh spawn vars for a session in the local-only repository at `root`.
///
/// Why: every daemon spawn, resume and relaunch path reaches this through
/// `core::gh_account::resolve_gh_account_env_for_registry`, so the supervisor
/// exemption is decided once.
/// What: [`local_only_gh_vars`] when [`gh_disabled_for`]; otherwise no vars —
/// the allow-listed supervisor keeps the machine's gh. Logs which at `info`.
/// Test: `an_allow_listed_supervisor_keeps_gh`,
/// `a_self_declared_supervisor_does_not_keep_gh`.
pub fn local_only_spawn_vars(root: &Path, config: &MpmConfig) -> Vec<(String, String)> {
    if gh_disabled_for(root, config) {
        tracing::info!(root = %root.display(), "{LOCAL_ONLY_SKIP}; gh is disabled for this session");
        return local_only_gh_vars();
    }
    tracing::info!(root = %root.display(), "{LOCAL_ONLY_SKIP}; allow-listed supervisor keeps gh");
    Vec::new()
}

/// The branch a local-only worktree is cut from (#8934).
///
/// Why: with no `origin/<default>`, the root's checked-out branch is whatever
/// the operator last switched to; a session must start from the repository's
/// default branch, not from a feature branch.
/// What: the first of `init.defaultBranch` (as git resolves it from `repo`),
/// `main`, `master` that exists as a local branch. `Err` naming the repository
/// when none does.
/// Test: `local_default_branch_ignores_a_checked_out_feature_branch`,
/// `local_default_branch_refuses_when_none_exists`.
pub fn local_default_branch(repo: &Path) -> Result<String, String> {
    let git = |args: &[&str]| {
        trusty_common::git::command()
            .arg("-C")
            .arg(repo)
            .args(args)
            .output()
            .ok()
            .filter(|out| out.status.success())
            .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string())
    };
    let configured = git(&["config", "--get", "init.defaultBranch"]).filter(|b| !b.is_empty());
    configured
        .into_iter()
        .chain(["main".to_string(), "master".to_string()])
        .find(|b| {
            git(&[
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("refs/heads/{b}"),
            ])
            .is_some()
        })
        .ok_or_else(|| {
            format!(
                "local-only repo '{}' has no local default branch (no `init.defaultBranch` \
                 branch, no `main`, no `master`); create one before starting a worktree session",
                repo.display()
            )
        })
}

#[cfg(test)]
#[path = "remote_mode_tests.rs"]
mod tests;
