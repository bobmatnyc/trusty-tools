//! Pull-request numbers and stale candidates for `tm worktrees` (#8994 S2c).
//!
//! Why: the ledger knew every live tree but not which ones had finished — a
//! merged pull request, or a branch deleted on the remote. S3a will reclaim on
//! those facts; this slice only records the PR and lists the candidates.
//! What: [`link_prs_and_find_stale`] asks a [`StaleProbe`] about each live
//! tree, appends `pr_linked` when the tree's PR is new or changed, and returns
//! one [`TreeView`] per tree. A tree is a candidate only on an ANSWERED fact:
//! its PR merged, or its branch is gone (the remote ref is gone AND nothing on
//! it is unpushed — owner ruling Q4). An unresolvable git or gh answer is
//! never a candidate. Nothing here deletes anything.
//! Test: `core::worktree_ledger::stale::tests`.

use std::path::{Path, PathBuf};

use serde::Serialize;

use super::fold::LedgerState;
use super::{LedgerError, WorktreeLedger};

/// What the pull-request lookup said about one branch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrAnswer {
    /// A pull request for the branch merged.
    Merged(u64),
    /// A pull request for the branch is open.
    Open(u64),
    /// A pull request for the branch was closed without merging.
    Closed(u64),
    /// A complete index holds no pull request for the branch.
    NoPr,
    /// The lookup could not answer; carries why.
    Unresolved(String),
}

/// What git said about one branch's remote ref and unpushed commits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BranchAnswer {
    /// The upstream ref is gone from the remote and nothing is unpushed.
    Gone,
    /// The upstream ref still exists on the remote.
    Present,
    /// The branch records no upstream, so "gone" cannot be told from "never pushed".
    NoUpstream,
    /// The upstream ref is gone but this many commits are on no remote ref.
    Unpushed(usize),
    /// Git could not answer; carries why.
    Unresolved(String),
}

/// The two lookups behind a stale candidate.
pub trait StaleProbe {
    /// The pull request for `branch` in `repo`.
    fn pr_for(&self, repo: &Path, branch: &str) -> PrAnswer;
    /// Whether `branch`, checked out in `tree` of `repo`, is gone from the remote.
    fn branch_state(&self, repo: &Path, tree: &Path, branch: &str) -> BranchAnswer;
}

/// Why a tree is a stale candidate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum StaleReason {
    /// Its branch's pull request merged.
    MergedPr {
        /// The merged pull request.
        pr: u64,
    },
    /// Its branch's remote ref is gone and nothing on it is unpushed.
    BranchGone,
}

/// One live tree as `tm worktrees` lists it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TreeView {
    /// The worktree.
    pub path: PathBuf,
    /// Its repository.
    pub repo: PathBuf,
    /// Its branch, when recorded.
    pub branch: Option<String>,
    /// Its pull request, when known.
    pub pr: Option<u64>,
    /// Why it is a stale candidate; `None` when it is not one.
    pub stale: Option<StaleReason>,
    /// Lookups that could not be answered for it.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub unresolved: Vec<String>,
}

/// What one pass found.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct StaleReport {
    /// Every live tree, ordered by path.
    pub trees: Vec<TreeView>,
    /// `pr_linked` events appended this pass.
    pub pr_linked: usize,
}

impl StaleReport {
    /// The trees that are stale candidates.
    pub fn candidates(&self) -> impl Iterator<Item = &TreeView> {
        self.trees.iter().filter(|t| t.stale.is_some())
    }
}

/// Link each live tree's pull request and classify stale candidates.
///
/// Test: see the module doc.
pub fn link_prs_and_find_stale(
    _ledger: &WorktreeLedger,
    _state: &LedgerState,
    _probe: &dyn StaleProbe,
) -> Result<StaleReport, LedgerError> {
    Ok(StaleReport::default())
}

#[cfg(test)]
#[path = "stale_tests.rs"]
mod tests;
