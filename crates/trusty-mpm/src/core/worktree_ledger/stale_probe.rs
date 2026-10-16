//! The production [`StaleProbe`]: `gh` for pull requests, git for branches
//! (#8994 S2c).
//!
//! Why: `tm worktrees` must answer "merged?" and "branch gone?" with the same
//! sources the reclaim sweep trusts, and must say "unresolved" whenever they
//! cannot answer — never guess.
//! What: [`GitGhStaleProbe`]. `pr_for` reads the reclaim sweep's per-repository
//! `gh pr list` index ([`PrIndex`]), built once per repository per pass.
//! `branch_state` applies owner ruling Q4 — the branch's recorded upstream is
//! gone from `origin` after a `git fetch --prune` (once per repository per
//! pass) AND no commit on the local branch is off every `origin` ref. Every
//! git call is bounded and goes through the crate's hardened spawn. Read-only
//! apart from the fetch's remote-tracking refs.
//! Test: `core::worktree_ledger::stale_probe::tests`.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::stale::{BranchAnswer, PrAnswer, StaleProbe};
use crate::session_manager::git_ceiling::bounded_git_output;
use crate::session_manager::worktree_landing_refresh::refresh_landing_refs_within;
use crate::session_manager::worktree_reclaim::{BranchPrState, PrIndex};
use crate::session_manager::worktree_safety::{git_command, git_stdout};

/// How long one repository's `git fetch --prune origin` may run.
const FETCH_TIMEOUT: Duration = Duration::from_secs(10);

/// [`StaleProbe`] over the reclaim sweep's `gh` index and plain git.
///
/// What: memoizes one PR index and one fetch result per repository, so a
/// pass over many trees of one repository costs one `gh` call and one fetch.
/// Test: `branch_state_reads_gone_present_unpushed_and_no_upstream_from_git`,
/// `branch_state_is_unresolved_when_git_cannot_answer`,
/// `pr_for_a_repository_without_a_github_origin_is_unresolved`,
/// `an_offline_probe_never_answers_gone_or_merged`.
#[derive(Debug, Default)]
pub struct GitGhStaleProbe {
    indexes: RefCell<BTreeMap<PathBuf, PrIndex>>,
    fetched: RefCell<BTreeMap<PathBuf, Result<(), String>>>,
    /// `tm worktrees --no-fetch`: no fetch and no `gh` call (#8994 S3a).
    offline: bool,
}

/// Why an offline probe leaves a lookup unanswered.
pub const OFFLINE_REASON: &str = "--no-fetch: the remote and GitHub were not asked";

impl GitGhStaleProbe {
    /// A probe that runs no `git fetch` and no `gh` (#8994 S3a).
    ///
    /// Why: `tm worktrees --no-fetch` must not touch the network, and a local
    /// remote-tracking ref may be stale, so "gone" and "merged" cannot be told.
    /// What: every PR lookup, and every branch lookup that would fetch, is
    /// unresolved with [`OFFLINE_REASON`]; never gone, never merged.
    /// Test: `an_offline_probe_never_answers_gone_or_merged`.
    pub fn offline() -> Self {
        Self {
            offline: true,
            ..Self::default()
        }
    }

    /// `repo`'s `git fetch --prune origin`, once per probe.
    fn fetch(&self, repo: &Path) -> Result<(), String> {
        // #8994 S3a: offline answers nothing that needs the remote.
        if self.offline {
            return Err(OFFLINE_REASON.into());
        }
        self.fetched
            .borrow_mut()
            .entry(repo.to_path_buf())
            .or_insert_with(|| refresh_landing_refs_within(repo, FETCH_TIMEOUT))
            .clone()
    }
}

impl StaleProbe for GitGhStaleProbe {
    fn pr_for(&self, repo: &Path, branch: &str) -> PrAnswer {
        // #8994 S3a: `--no-fetch` makes no `gh` call.
        if self.offline {
            return PrAnswer::Unresolved(OFFLINE_REASON.into());
        }
        let mut indexes = self.indexes.borrow_mut();
        let index = indexes
            .entry(repo.to_path_buf())
            .or_insert_with(|| PrIndex::from_gh(repo));
        match index.state_for(Some(branch)) {
            BranchPrState::Merged { pr } => PrAnswer::Merged(pr),
            BranchPrState::Open { pr } => PrAnswer::Open(pr),
            BranchPrState::ClosedUnmerged { pr } => PrAnswer::Closed(pr),
            BranchPrState::NoPr => PrAnswer::NoPr,
            BranchPrState::Unknown => PrAnswer::Unresolved(
                "the pull-request index is truncated, so this branch's PR is unknown".into(),
            ),
            BranchPrState::LookupFailed { reason } => PrAnswer::Unresolved(reason),
        }
    }

    fn branch_state(&self, repo: &Path, tree: &Path, branch: &str) -> BranchAnswer {
        match branch_state_in(self, repo, tree, branch) {
            Ok(answer) => answer,
            Err(why) => BranchAnswer::Unresolved(why),
        }
    }
}

/// [`StaleProbe::branch_state`], with every unanswerable step an `Err`.
fn branch_state_in(
    probe: &GitGhStaleProbe,
    repo: &Path,
    tree: &Path,
    branch: &str,
) -> Result<BranchAnswer, String> {
    // #8994 S2c: the name comes from the ledger and lands in argv and refs.
    if !is_branch_name(branch) {
        return Err(format!("`{branch}` is not a branch name"));
    }
    let local = format!("refs/heads/{branch}");
    if !ref_exists(tree, &local)? {
        return Err(format!("no local branch `{branch}` in {}", tree.display()));
    }
    let (Some(merge), Some(remote)) = (
        config_value(tree, &format!("branch.{branch}.merge"))?,
        config_value(tree, &format!("branch.{branch}.remote"))?,
    ) else {
        return Ok(BranchAnswer::NoUpstream);
    };
    if remote != "origin" {
        return Err(format!("the upstream remote is `{remote}`, not `origin`"));
    }
    let Some(upstream) = merge.strip_prefix("refs/heads/") else {
        return Err(format!("the upstream `{merge}` is not a branch"));
    };
    probe.fetch(repo)?;
    if ref_exists(tree, &format!("refs/remotes/origin/{upstream}"))? {
        return Ok(BranchAnswer::Present);
    }
    let count = git_stdout(
        tree,
        &["rev-list", "--count", &local, "--not", "--remotes=origin"],
    )?;
    match count.trim().parse::<usize>() {
        Ok(0) => Ok(BranchAnswer::Gone),
        Ok(n) => Ok(BranchAnswer::Unpushed(n)),
        Err(e) => Err(format!(
            "`git rev-list --count` printed {:?}: {e}",
            count.trim()
        )),
    }
}

/// A plain branch name: no option shape, no revision syntax, no whitespace.
pub(super) fn is_branch_name(branch: &str) -> bool {
    !branch.is_empty()
        && !branch.starts_with('-')
        && !branch.contains("..")
        && !branch
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || "~^:?*[\\".contains(c))
}

/// `git rev-parse --verify --quiet <name>` in `dir`: exit 0 yes, exit 1 no.
fn ref_exists(dir: &Path, name: &str) -> Result<bool, String> {
    match exit_code(dir, &["rev-parse", "--verify", "--quiet", name])? {
        0 => Ok(true),
        1 => Ok(false),
        code => Err(format!("`git rev-parse --verify {name}` exited {code}")),
    }
}

/// `git config --get <key>` in `dir`: `None` when unset (exit 1).
fn config_value(dir: &Path, key: &str) -> Result<Option<String>, String> {
    let args = ["config", "--get", key];
    let out = bounded_git_output(git_command(dir, &args))
        .map_err(|e| format!("`git config --get {key}` could not be run: {e}"))?;
    match out.status.code() {
        Some(0) => Ok(Some(
            String::from_utf8_lossy(&out.stdout).trim().to_string(),
        )),
        Some(1) => Ok(None),
        _ => Err(format!("`git config --get {key}` failed ({})", out.status)),
    }
}

/// The exit code of a bounded `git -C <dir> <args>`.
fn exit_code(dir: &Path, args: &[&str]) -> Result<i32, String> {
    let out = bounded_git_output(git_command(dir, args))
        .map_err(|e| format!("`git {}` could not be run: {e}", args.join(" ")))?;
    out.status
        .code()
        .ok_or_else(|| format!("`git {}` was killed ({})", args.join(" "), out.status))
}

#[cfg(test)]
#[path = "stale_probe_tests.rs"]
mod tests;
