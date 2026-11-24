//! The `gh` reads the PR watch makes (#11673 AC2).
//!
//! Why: the daemon loop must read outbound only, inside a bound, through the
//! workspace's one `gh` entry point, and stay testable without a network.
//! What: [`PrReader`] is the seam: one PR's [`PrSnapshot`], and the required
//! contexts on a base branch. [`GhPrReader`] is the production reader over
//! `GhCommand`, with the caller's GitHub identity and each call killed at its
//! budget. [`parse_pr_view`] is shared with `tm pr watch add`.
//! Test: `parse_pr_view_reads_head_base_and_rollup`.

use std::time::Duration;

use anyhow::Context as _;
use serde::Deserialize;

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

/// Production [`PrReader`] over `trusty_common::gh::GhCommand`.
///
/// Why: `GhCommand` is the single `gh` entry point (#5475); the budget stops a
/// stalled `gh` from holding a daemon tick (#7956's bounded runner).
/// What: applies the identity overlay (removals first, #6668), binds the real
/// `gh` (#9750), and runs each call under `run_bounded`.
/// Test: exercised live; the logic it feeds is tested against fakes.
pub struct GhPrReader {
    env: Vec<(String, String)>,
    unset: Vec<String>,
    budget: Duration,
}

impl GhPrReader {
    /// A reader with this identity overlay and per-call budget.
    pub fn new(env: Vec<(String, String)>, unset: Vec<String>, budget: Duration) -> Self {
        Self { env, unset, budget }
    }

    /// Run `gh <args>`; stdout on a zero exit.
    fn run(&self, args: &[&str]) -> anyhow::Result<String> {
        let mut cmd = trusty_common::gh::GhCommand::new(args);
        for k in &self.unset {
            cmd = cmd.env_remove(k);
        }
        for (k, v) in &self.env {
            cmd = cmd.env(k, v);
        }
        let cmd =
            crate::core::gh_shim_path::bind_gh_path(cmd, &self.env, &self.unset).to_std_command();
        let label = format!("gh {}", args.join(" "));
        let out = crate::core::bounded_proc::run_bounded(cmd, self.budget)
            .map_err(|e| anyhow::anyhow!("`{label}` {e} ({:?})", self.budget))?;
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
        let json = self.run(&["pr", "view", &n, "--repo", repo, "--json", PR_VIEW_FIELDS])?;
        parse_pr_view(&json)
    }

    fn required(&self, repo: &str, base: &str) -> anyhow::Result<Option<Vec<String>>> {
        let branch =
            parse_required(&self.run(&["api", &format!("repos/{repo}/branches/{base}")])?)?;
        let rules = parse_ruleset_required(&self.run(&[
            "api",
            "--paginate",
            &format!("repos/{repo}/rules/branches/{base}"),
        ])?)?;
        Ok(combine(branch, &rules))
    }
}
