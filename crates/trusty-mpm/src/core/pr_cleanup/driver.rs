//! The three seams `tm pr cleanup` runs through: `gh`, `git`, and the session
//! claim store (#7275).
//!
//! Why: post-merge cleanup is a sequence of destructive commands, so the
//! decision to run each one has to be testable without a network, a live `gh`,
//! or a real worktree to delete. Hiding every spawn behind a trait is what lets
//! [`super::plan`] be a pure function of scripted command output. The
//! production implementations here spawn through this workspace's single
//! entry points — `trusty_common::gh::GhCommand` (#5475) and
//! `trusty_common::git::command_in` (#7171) — so cleanup adds no new spawn
//! site, per the repo's common-entry-point rule.
//!
//! What: [`CmdOut`] (one completed subprocess), the [`Gh`] and [`Git`] seams
//! with [`RealGh`] / [`RealGit`] behind them, and [`ClaimEnder`] — the seam
//! that answers which session records claim a worktree and tombstones one.
//!
//! Test: `super::tests` drives scripted fakes for all three.

use std::path::Path;

use anyhow::Context as _;

/// One completed subprocess, as cleanup needs it.
///
/// Why: a `gh pr view` on a missing PR and a `git push --delete` on an absent
/// branch both exit non-zero and want different handling, so the seam hands
/// back the triple rather than deciding.
/// What: exit success, verbatim stdout, verbatim stderr.
/// Test: the fakes in `super::tests` construct these directly.
#[derive(Debug, Clone)]
pub struct CmdOut {
    /// Whether the command exited zero.
    pub success: bool,
    /// Verbatim stdout.
    pub stdout: String,
    /// Verbatim stderr.
    pub stderr: String,
}

impl CmdOut {
    /// A successful run's stdout, or an error naming `what` and the stderr.
    ///
    /// Test: `cleanup_refuses_when_gh_view_fails`.
    pub fn stdout_ok(self, what: &str) -> anyhow::Result<String> {
        if self.success {
            return Ok(self.stdout);
        }
        anyhow::bail!("`{what}` failed: {}", self.stderr.trim())
    }
}

/// The `gh` seam every cleanup call goes through.
///
/// Test: `FakeGh` in `super::tests`.
pub trait Gh {
    /// Run `gh <args>` to completion.
    fn run(&self, args: &[String]) -> anyhow::Result<CmdOut>;
}

/// The `git` seam every cleanup call goes through, scoped to one checkout.
///
/// Test: `FakeGit` in `super::tests`.
pub trait Git {
    /// Run `git -C <dir> <args>` to completion.
    fn run(&self, dir: &Path, args: &[String]) -> anyhow::Result<CmdOut>;
}

/// Who claims a worktree, and how that claim is ended (#7275).
///
/// Why: the owner's ruling is that a MERGED pull request makes its worktree
/// obsolete, claim included — so cleanup ends the claim and proceeds rather
/// than refusing. Ending it must be RECORD-ONLY: cleanup tombstones the
/// session record, and never kills a process it does not own. Both halves are
/// behind this trait so the executor can be tested without a daemon and so the
/// two production callers (the CLI over HTTP, the supervisor over
/// `SessionManager`) share one decision path.
/// What: [`claims_on`](Self::claims_on) names the session ids claiming a path;
/// [`end_claim`](Self::end_claim) tombstones one. An error from either is a
/// step FAILURE, never a silent proceed — see [`super::run`].
/// Test: `FakeClaims` in `super::tests`;
/// `cleanup_ends_a_session_claim_before_removing_the_worktree`.
#[async_trait::async_trait]
pub trait ClaimEnder {
    /// Session ids whose record claims `path`, in a stable order.
    async fn claims_on(&self, path: &Path) -> anyhow::Result<Vec<String>>;

    /// Tombstone `id`'s record — record-only, never a process kill.
    async fn end_claim(&self, id: &str) -> anyhow::Result<()>;
}

/// A [`ClaimEnder`] that reports no claims and can end none.
///
/// Why: `--dry-run` never needs to end a claim, and a caller with no session
/// store to consult must be able to say so explicitly rather than by passing
/// something that silently answers "unclaimed".
/// What: `claims_on` returns an error naming `reason`, so the worktree step
/// FAILS rather than proceeding on an unanswerable question.
/// Test: `cleanup_fails_the_worktree_step_when_claims_cannot_be_read`.
pub struct UnavailableClaims {
    /// Why the claim store could not be consulted, quoted in the failure.
    reason: String,
}

impl UnavailableClaims {
    /// Build one, naming why the claim store is unavailable.
    pub fn new(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
        }
    }
}

#[async_trait::async_trait]
impl ClaimEnder for UnavailableClaims {
    async fn claims_on(&self, path: &Path) -> anyhow::Result<Vec<String>> {
        anyhow::bail!(
            "cannot tell whether a session claims {}: {}",
            path.display(),
            self.reason
        )
    }

    async fn end_claim(&self, id: &str) -> anyhow::Result<()> {
        anyhow::bail!("cannot end the claim held by {id}: {}", self.reason)
    }
}

/// Production [`Gh`] over `trusty_common::gh::GhCommand`.
///
/// Why: `GhCommand` is this workspace's single `gh` entry point (#5475) — it
/// decides which binary, renders the argv for errors, and never goes through a
/// shell. A second `Command::new("gh")` here would be a defect.
/// What: overlays a caller-supplied GitHub identity (`tm`'s `gh_identity`,
/// #1265) onto the child, removals first (#6668), then runs blocking.
/// Test: exercised live; the logic it feeds is covered against [`Gh`] fakes.
pub struct RealGh {
    /// `GH_*` overrides to apply to the child.
    env: Vec<(String, String)>,
    /// Inherited identity vars removed before `env` is applied (#6668).
    unset: Vec<String>,
}

impl RealGh {
    /// Build a runner bound to an already-resolved GitHub identity.
    pub fn new(env: Vec<(String, String)>, unset: Vec<String>) -> Self {
        Self { env, unset }
    }
}

impl Gh for RealGh {
    fn run(&self, args: &[String]) -> anyhow::Result<CmdOut> {
        let mut cmd = trusty_common::gh::GhCommand::new(args);
        // #6668: removals first, then the overrides.
        for k in &self.unset {
            cmd = cmd.env_remove(k);
        }
        for (k, v) in &self.env {
            cmd = cmd.env(k, v);
        }
        let out = cmd
            .output_blocking()
            .with_context(|| format!("`gh {}` could not be run", args.join(" ")))?;
        Ok(CmdOut {
            success: out.success,
            stdout: out.stdout,
            stderr: out.stderr,
        })
    }
}

/// Production [`Git`] over `trusty_common::git::command_in`.
///
/// Why: `command_in` is this workspace's single synchronous git entry point
/// (#7171) — it carries `-c maintenance.auto=false -c gc.auto=0`, which is what
/// keeps a sweep over many worktrees from touching off the detached-repack
/// pile-up that has flattened this machine before.
/// What: `git -C <dir> <args>`, run blocking.
/// Test: exercised live; the logic it feeds is covered against [`Git`] fakes.
pub struct RealGit;

impl Git for RealGit {
    fn run(&self, dir: &Path, args: &[String]) -> anyhow::Result<CmdOut> {
        let out = trusty_common::git::command_in(dir)
            .args(args)
            .output()
            .with_context(|| {
                format!(
                    "`git -C {} {}` could not be run",
                    dir.display(),
                    args.join(" ")
                )
            })?;
        Ok(CmdOut {
            success: out.status.success(),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        })
    }
}

/// Which MERGED pull request carried a worktree's work (#7275 round 3).
///
/// Why: every merge here is a squash, so a landed branch's commits are never
/// ancestors of `main` and an ahead-of-upstream count reads every landed
/// worktree as live work — seven trees were refused as holding "1–3 unpushed
/// commit(s)" on 2026-09-09, which is the population cleanup exists to
/// reclaim. The question that answers it is which merged pull request carried
/// this tree, and `worktree_reclaim_pr_match::resolve_landing` already decides
/// that by exact branch name, round stem, and head-commit ancestry. This seam
/// exists so the engine — which is `pub` and wired from the `tm` binary — can
/// ask through a trait its tests can fake, without exporting the matcher's own
/// `pub(crate)` types.
/// What: [`merged_pr`](Self::merged_pr) names the pull request, or `None` when
/// none matches. FAIL-CLOSED: every failure — no `gh`, a timeout, an
/// unparseable reply — answers `None`, which leaves today's refusal standing.
/// Test: `FakeLanding` in `super::tests` drives the real matcher over #7267's
/// `LandingProbe` fake; `cleanup_removes_a_squash_merged_worktree`.
pub trait Landing {
    /// The MERGED pull request that carried `worktree`, or `None`.
    ///
    /// `repo_root` is the main checkout, whose `origin` names the repository to
    /// ask about — the sweep visits entries in different checkouts, so it is a
    /// per-call input rather than state. `exact` is what the caller already
    /// proved about this branch — the pull request whose head it matches by
    /// name — and seeds the matcher's first rung, exactly as the reclaim sweep
    /// seeds it from its bulk index.
    fn merged_pr(
        &self,
        repo_root: &Path,
        worktree: &Path,
        branch: Option<&str>,
        exact: Option<u64>,
    ) -> Option<u64>;
}

/// Production [`Landing`] over the #7267 merged-pull-request matcher.
///
/// Why: one implementation of "which merged pull request carried this tree",
/// shared with `tm session prune-worktrees --merged-prs`. A second matcher here
/// would be the defect the repo's common-entry-point rule names.
/// What: seeds `resolve_landing`'s first rung with the caller's `exact` answer
/// when there is one and otherwise asks GitHub about the branch itself, then
/// lets the round-stem and head-commit rungs widen it. Only a `Merged` verdict
/// becomes `Some`.
/// Test: exercised live; the policy it delegates to is unit-tested in
/// `worktree_reclaim_pr_match_tests`.
pub struct RealLanding;

impl Landing for RealLanding {
    fn merged_pr(
        &self,
        repo_root: &Path,
        worktree: &Path,
        branch: Option<&str>,
        exact: Option<u64>,
    ) -> Option<u64> {
        resolve_merged_pr(
            &crate::session_manager::worktree_reclaim_pr_match::GhLandingProbe,
            repo_root,
            worktree,
            branch,
            exact,
        )
    }
}

/// [`RealLanding`]'s whole body, over any [`LandingProbe`] (#7275 round 3).
///
/// Why: the seed-then-ladder shape IS the policy, so a test that drove a
/// different shape would prove nothing about what production does. Taking the
/// probe as a parameter lets `super::tests` run this exact code over #7267's
/// own `LandingProbe` fake.
/// What: `exact` becomes rung 1's settled answer; otherwise the branch's own
/// pull requests are asked, and `resolve_landing` widens by round stem and head
/// commit. Only `Merged` becomes `Some`.
/// Test: `cleanup_removes_a_squash_merged_round_sibling`,
/// `cleanup_refuses_an_ahead_tree_with_no_merged_pr`.
///
/// [`LandingProbe`]: crate::session_manager::worktree_reclaim_pr_match::LandingProbe
pub(crate) fn resolve_merged_pr(
    probe: &dyn crate::session_manager::worktree_reclaim_pr_match::LandingProbe,
    repo_root: &Path,
    worktree: &Path,
    branch: Option<&str>,
    exact: Option<u64>,
) -> Option<u64> {
    use crate::session_manager::worktree_reclaim::BranchPrState;
    use crate::session_manager::worktree_reclaim_pr_match::resolve_landing;

    let seed = match exact {
        Some(pr) => BranchPrState::Merged { pr },
        None => branch.map_or(BranchPrState::NoPr, |b| probe.state_for_head(repo_root, b)),
    };
    match resolve_landing(worktree, repo_root, branch, seed, probe) {
        BranchPrState::Merged { pr } => Some(pr),
        _ => None,
    }
}
