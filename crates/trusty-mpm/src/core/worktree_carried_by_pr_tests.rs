//! Unit coverage for route (c) of the #7889 admission.
//!
//! Why: the decision is which direction of ancestry counts, and each failure
//! arm must refuse. A fake probe states GitHub's rows and git's ancestry
//! answers, so every arm is reachable without a network.

use std::path::Path;

use super::{CarriedByPr, MERGED_PR_ANCESTRY_CHECK, carried_by_merged_pr};
use crate::session_manager::worktree_reclaim::BranchPrState;
use crate::session_manager::worktree_reclaim_pr_match::{LandingProbe, MergedPrHead};

const HEAD: &str = "1111111111111111111111111111111111111111";
const PR_HEAD: &str = "2222222222222222222222222222222222222222";

/// A probe whose every answer the test states.
struct Fake {
    head: Result<String, String>,
    rows: Result<Vec<MergedPrHead>, String>,
    /// `(ancestor, descendant)` pairs git would answer "yes" for.
    ancestry: Vec<(&'static str, &'static str)>,
    ancestry_error: Option<String>,
}

impl Fake {
    fn with_rows(rows: Vec<MergedPrHead>) -> Self {
        Self {
            head: Ok(HEAD.to_string()),
            rows: Ok(rows),
            ancestry: vec![(HEAD, PR_HEAD)],
            ancestry_error: None,
        }
    }
}

impl LandingProbe for Fake {
    fn state_for_head(&self, _root: &Path, _branch: &str) -> BranchPrState {
        BranchPrState::Unknown
    }
    fn head_commit(&self, _wt: &Path) -> Result<String, String> {
        self.head.clone()
    }
    fn merged_prs_containing(&self, _root: &Path, _sha: &str) -> Result<Vec<MergedPrHead>, String> {
        self.rows.clone()
    }
    fn is_ancestor(&self, _wt: &Path, ancestor: &str, descendant: &str) -> Result<bool, String> {
        if let Some(e) = &self.ancestry_error {
            return Err(e.clone());
        }
        Ok(self.ancestry.contains(&(ancestor, descendant)))
    }
}

fn row(number: u64, oid: &str, fork: bool) -> MergedPrHead {
    MergedPrHead {
        number,
        head_ref_oid: oid.to_string(),
        is_cross_repository: fork,
    }
}

fn verdict(fake: &Fake) -> CarriedByPr {
    carried_by_merged_pr(Path::new("/repo/.claude/worktrees/donor"), fake)
}

/// 🔴 REGRESSION (#7889, route (c)): the donor shape — HEAD is an ancestor of
/// a merged pull request's head, so every commit here was inside what merged.
#[test]
fn a_head_inside_a_merged_prs_history_is_carried() {
    let v = verdict(&Fake::with_rows(vec![row(8328, PR_HEAD, false)]));
    assert_eq!(
        v,
        CarriedByPr::Carried {
            pr: 8328,
            pr_head: PR_HEAD.to_string()
        }
    );
    assert!(v.is_carried());
    // The exact head needs no ancestry answer at all.
    let exact = Fake {
        ancestry: Vec::new(),
        ..Fake::with_rows(vec![row(7, HEAD, false)])
    };
    assert!(verdict(&exact).is_carried());
}

/// 🔴 #7889: the reverse direction leaves commits here the merge never saw,
/// so a pull request whose head is BEHIND HEAD is not a match.
#[test]
fn a_merged_pr_head_behind_head_is_not_a_match() {
    let fake = Fake {
        ancestry: vec![(PR_HEAD, HEAD)],
        ..Fake::with_rows(vec![row(8328, PR_HEAD, false)])
    };
    let v = verdict(&fake);
    assert_eq!(
        v,
        CarriedByPr::NotCarried {
            head: HEAD.to_string()
        }
    );
    assert!(!v.is_carried());
    assert!(v.note().contains(HEAD), "{}", v.note());
}

/// 🔴 #7889: a fork's pull request says nothing about this checkout, and a row
/// with no head commit proves nothing.
#[test]
fn a_fork_row_is_never_a_match() {
    let fake = Fake::with_rows(vec![row(1, PR_HEAD, true), row(2, "", false)]);
    assert!(!verdict(&fake).is_carried());
}

/// 🔴 #7889, ADR-0045: an ancestry probe that errored — a head commit this
/// checkout does not hold — is undeterminable, never "not carried" and never
/// a grant.
#[test]
fn an_ancestry_probe_error_is_unavailable() {
    let fake = Fake {
        ancestry_error: Some("fatal: Not a valid commit name".to_string()),
        ..Fake::with_rows(vec![row(8328, PR_HEAD, false)])
    };
    let v = verdict(&fake);
    assert!(matches!(v, CarriedByPr::Unavailable { .. }), "{v:?}");
    assert!(v.note().contains("Not a valid commit name"), "{}", v.note());
}

/// 🔴 #7889: HEAD that cannot be read never reaches GitHub, and refuses.
#[test]
fn an_unreadable_head_is_unavailable() {
    let fake = Fake {
        head: Err("not a git repository".to_string()),
        ..Fake::with_rows(vec![row(8328, PR_HEAD, false)])
    };
    let v = verdict(&fake);
    assert!(matches!(v, CarriedByPr::Unavailable { .. }), "{v:?}");
    assert!(v.note().contains("not a git repository"), "{}", v.note());
}

/// 🔴 #7889: a commit search that did not answer establishes nothing.
#[test]
fn a_failed_search_is_unavailable() {
    let fake = Fake {
        rows: Err("gh timed out after 10s".to_string()),
        ..Fake::with_rows(Vec::new())
    };
    let v = verdict(&fake);
    assert!(matches!(v, CarriedByPr::Unavailable { .. }), "{v:?}");
    assert!(v.note().contains("timed out"), "{}", v.note());
}

/// Every arm's sentence names the admission, so a refusal can be grepped.
#[test]
fn every_arm_names_the_admission() {
    for v in [
        CarriedByPr::Carried {
            pr: 1,
            pr_head: PR_HEAD.to_string(),
        },
        CarriedByPr::NotCarried {
            head: HEAD.to_string(),
        },
        CarriedByPr::Unavailable {
            detail: "x".to_string(),
        },
    ] {
        assert!(v.note().contains(MERGED_PR_ANCESTRY_CHECK), "{}", v.note());
    }
}
