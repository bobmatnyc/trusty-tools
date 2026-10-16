//! The production [`StaleProbe`]: `gh` for pull requests, git for branches
//! (#8994 S2c).
//!
//! Test: `core::worktree_ledger::stale_probe::tests`.

use std::path::Path;

use super::stale::{BranchAnswer, PrAnswer, StaleProbe};

/// [`StaleProbe`] over the reclaim sweep's `gh` index and plain git.
#[derive(Debug, Default)]
pub struct GitGhStaleProbe;

impl StaleProbe for GitGhStaleProbe {
    fn pr_for(&self, _repo: &Path, _branch: &str) -> PrAnswer {
        PrAnswer::Unresolved("not implemented".into())
    }

    fn branch_state(&self, _repo: &Path, _tree: &Path, _branch: &str) -> BranchAnswer {
        BranchAnswer::Unresolved("not implemented".into())
    }
}

#[cfg(test)]
#[path = "stale_probe_tests.rs"]
mod tests;
