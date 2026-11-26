//! The `gh` reads the PR watch makes (#11673 AC2).
//!
//! Why: the daemon loop must read outbound only, inside a bound, through the
//! workspace's one `gh` entry point, and stay testable without a network.
//! What: [`PrReader`] is the seam: one PR's [`PrSnapshot`], and the required
//! contexts on a base branch. [`GhPrReader`] is the production reader over
//! `GhCommand`, with the caller's GitHub identity and each call killed at its
//! budget. [`parse_pr_view`] is shared with `tm pr watch add`.
//! Test: `parse_pr_view_reads_head_base_and_rollup`.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;
use serde::Deserialize;

use crate::core::gh_identity::GhEnv;
use crate::core::pr_required::{combine, parse_required, parse_ruleset_required};
use crate::core::pr_rollup::RollupEntry;

/// The `gh pr view --json` fields a snapshot needs.
pub const PR_VIEW_FIELDS: &str = "state,headRefOid,baseRefName,statusCheckRollup";

/// One PR as a tick sees it.
#[derive(Debug)]
pub struct PrSnapshot {
    /// `OPEN`, `MERGED` or `CLOSED`.
    pub state: String,
    /// The head SHA.
    pub head: String,
    /// The base branch.
    pub base: String,
    /// The check rollup; empty when none has registered.
    pub rollup: Vec<RollupEntry>,
}

impl PrSnapshot {
    /// Whether the PR is merged or closed.
    pub fn is_done(&self) -> bool {
        ["MERGED", "CLOSED"]
            .iter()
            .any(|s| self.state.eq_ignore_ascii_case(s))
    }
}

/// `gh pr view --json` with [`PR_VIEW_FIELDS`].
#[derive(Debug, Deserialize)]
struct PrView {
    #[serde(default)]
    state: Option<String>,
    #[serde(default, rename = "headRefOid")]
    head: Option<String>,
    #[serde(default, rename = "baseRefName")]
    base: Option<String>,
    #[serde(default, rename = "statusCheckRollup")]
    rollup: Option<Vec<RollupEntry>>,
}

/// Parse `gh pr view --json` output into a [`PrSnapshot`].
///
/// # Errors
/// Not JSON, or no `state`, `headRefOid` or `baseRefName`.
///
/// Test: `parse_pr_view_reads_head_base_and_rollup`.
pub fn parse_pr_view(json: &str) -> anyhow::Result<PrSnapshot> {
    let view: PrView = serde_json::from_str(json).context("cannot parse `gh pr view` JSON")?;
    let field = |v: Option<String>, name: &str| {
        v.map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .with_context(|| format!("`gh pr view` returned no `{name}`"))
    };
    Ok(PrSnapshot {
        state: field(view.state, "state")?,
        head: field(view.head, "headRefOid")?,
        base: field(view.base, "baseRefName")?,
        rollup: view.rollup.unwrap_or_default(),
    })
}

/// The reads one tick makes.
pub trait PrReader: Send + Sync {
    /// One PR's snapshot.
    ///
    /// # Errors
    /// The read failed; a shim rate-limit refusal keeps its line in the chain.
    fn view(&self, repo: &str, pr: u64) -> anyhow::Result<PrSnapshot>;

    /// The required contexts on `base`; `None` when protection is unknown.
    ///
    /// # Errors
    /// Either read failed or did not parse.
    fn required(&self, repo: &str, base: &str) -> anyhow::Result<Option<Vec<String>>>;
}

/// `gh`'s exit code for "authentication required" (`gh auth login`).
pub const GH_EXIT_AUTH: i32 = 4;

/// Resolves the `gh` identity for one `owner/repo`; `Err` is why none binds.
pub type IdentityResolver = Arc<dyn Fn(&str) -> Result<GhEnv, String> + Send + Sync>;

/// A read that failed for want of a usable GitHub identity (#11673).
///
/// Why: under launchd every PR shares the one cause, so the loop reports it
/// once per tick instead of once per PR, and never as an ordinary read error.
/// What: the resolver refused (no `gh` ran), or `gh` exited
/// [`GH_EXIT_AUTH`] under the identity it was given.
#[derive(Debug, thiserror::Error)]
#[error("no usable GitHub identity for {repo}: {reason}")]
pub struct IdentityError {
    /// The `owner/repo` the read was for.
    pub repo: String,
    /// The resolver's refusal, or `gh`'s exit-4 line.
    pub reason: String,
}

/// Production [`PrReader`] over `trusty_common::gh::GhCommand`.
///
/// Why: `GhCommand` is the single `gh` entry point (#5475); the budget stops a
/// stalled `gh` from holding a daemon tick (#7956's bounded runner). #11673:
/// a fixed overlay resolved at daemon start was empty under launchd, so every
/// read exited 4.
/// What: resolves the identity for the row's repo on EVERY call, so a re-pin
/// applies on the next tick; applies it (removals first, #6668), binds the
/// real `gh` (#9750), and runs each call under `run_bounded`. A resolver
/// refusal spawns nothing; it and an exit 4 are [`IdentityError`].
/// Test: `a_pinned_repo_reads_through_its_bound_identity`,
/// `a_registry_refusal_spawns_no_gh`.
pub struct GhPrReader {
    identity: IdentityResolver,
    budget: Duration,
}

impl GhPrReader {
    /// A reader resolving each call's identity through `identity`.
    pub fn new(identity: IdentityResolver, budget: Duration) -> Self {
        Self { identity, budget }
    }

    /// Run `gh <args>` for `repo`; stdout on a zero exit.
    fn run(&self, repo: &str, args: &[&str]) -> anyhow::Result<String> {
        // #11673: per call, never the empty overlay launchd's env resolves to.
        let gh_env = (self.identity)(repo).map_err(|reason| IdentityError {
            repo: repo.to_string(),
            reason,
        })?;
        let mut cmd = trusty_common::gh::GhCommand::new(args);
        for k in gh_env.unset_vars() {
            cmd = cmd.env_remove(k);
        }
        for (k, v) in gh_env.vars() {
            cmd = cmd.env(k, v);
        }
        let cmd = crate::core::gh_shim_path::bind_gh_path(cmd, gh_env.vars(), gh_env.unset_vars())
            .to_std_command();
        let label = format!("gh {}", args.join(" "));
        let out = crate::core::bounded_proc::run_bounded(cmd, self.budget)
            .map_err(|e| anyhow::anyhow!("`{label}` {e} ({:?})", self.budget))?;
        if out.status.code() == Some(GH_EXIT_AUTH) {
            // #11673: an auth refusal is the identity's fault, not this PR's.
            return Err(IdentityError {
                repo: repo.to_string(),
                reason: format!(
                    "`{label}` exited {GH_EXIT_AUTH} under [{}]: {}",
                    gh_env.describe(),
                    out.stderr.trim()
                ),
            }
            .into());
        }
        anyhow::ensure!(
            out.status.success(),
            "`{label}` failed ({}): {}",
            out.status,
            out.stderr.trim()
        );
        Ok(out.stdout)
    }
}

impl PrReader for GhPrReader {
    fn view(&self, repo: &str, pr: u64) -> anyhow::Result<PrSnapshot> {
        let n = pr.to_string();
        let json = self.run(
            repo,
            &["pr", "view", &n, "--repo", repo, "--json", PR_VIEW_FIELDS],
        )?;
        parse_pr_view(&json)
    }

    fn required(&self, repo: &str, base: &str) -> anyhow::Result<Option<Vec<String>>> {
        let branch =
            parse_required(&self.run(repo, &["api", &format!("repos/{repo}/branches/{base}")])?)?;
        let rules = parse_ruleset_required(&self.run(
            repo,
            &[
                "api",
                "--paginate",
                &format!("repos/{repo}/rules/branches/{base}"),
            ],
        )?)?;
        Ok(combine(branch, &rules))
    }
}
