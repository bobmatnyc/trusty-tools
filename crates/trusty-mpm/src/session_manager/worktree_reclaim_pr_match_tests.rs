//! Tests for the merged-pull-request matcher (#7267).
//!
//! Why: this ladder's grant is what authorises deleting a checkout, so every
//! rung needs a test that fails if the rung is removed, and every FAILURE path
//! needs one proving it refuses rather than reclaims. The probe is faked
//! because the real one is `gh` and `git`; what is under test is the policy —
//! which answers count as landing evidence — not the subprocesses.
//! What: one test per rung, one per failure mode of each rung, and the
//! `resolve_with_index` wiring that decides when a rung may run at all.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use super::*;

/// The worktree's HEAD in every test that does not override it.
const HEAD_SHA: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
/// A commit that is not this worktree's HEAD.
const OTHER_SHA: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

/// The worktree directory under test — the fake probe never touches disk.
fn worktree() -> &'static Path {
    Path::new("/nonexistent/worktree-7267")
}

/// The registry root under test — likewise never touched.
fn root() -> &'static Path {
    Path::new("/nonexistent/root-7267")
}

/// A [`LandingProbe`] whose every answer is stated by the test.
struct FakeProbe {
    /// Branch name → what GitHub says about it.
    heads: BTreeMap<String, BranchPrState>,
    /// What `git rev-parse HEAD` answers.
    head_commit: Result<String, String>,
    /// What the commit search answers.
    search: Result<Vec<MergedPrHead>, String>,
    /// Ordered pairs for which `is_ancestor` answers true.
    ancestors: BTreeSet<(String, String)>,
    /// When set, `is_ancestor` fails with this reason instead of answering.
    ancestry_error: Option<String>,
    /// Every probe method called, in order — so a test can prove a rung was
    /// NOT reached.
    calls: RefCell<Vec<String>>,
}

impl Default for FakeProbe {
    fn default() -> Self {
        Self {
            heads: BTreeMap::new(),
            head_commit: Ok(HEAD_SHA.to_string()),
            search: Ok(Vec::new()),
            ancestors: BTreeSet::new(),
            ancestry_error: None,
            calls: RefCell::new(Vec::new()),
        }
    }
}

impl FakeProbe {
    /// State `branch`'s pull-request answer.
    fn with_head(mut self, branch: &str, state: BranchPrState) -> Self {
        self.heads.insert(branch.to_string(), state);
        self
    }

    /// State what the commit search returns.
    fn with_search(mut self, rows: Vec<MergedPrHead>) -> Self {
        self.search = Ok(rows);
        self
    }

    /// State that the commit search FAILS.
    fn with_search_error(mut self, reason: &str) -> Self {
        self.search = Err(reason.to_string());
        self
    }

    /// State what `git rev-parse HEAD` answers.
    fn with_head_commit(mut self, answer: Result<String, String>) -> Self {
        self.head_commit = answer;
        self
    }

    /// State that the ancestry probe FAILS — what an object this checkout does
    /// not hold looks like.
    fn with_ancestry_error(mut self, reason: &str) -> Self {
        self.ancestry_error = Some(reason.to_string());
        self
    }

    /// State that `ancestor` is an ancestor of `descendant`.
    fn with_ancestor(mut self, ancestor: &str, descendant: &str) -> Self {
        self.ancestors
            .insert((ancestor.to_string(), descendant.to_string()));
        self
    }

    /// Did any probe method get called?
    fn calls(&self) -> Vec<String> {
        self.calls.borrow().clone()
    }
}

impl LandingProbe for FakeProbe {
    fn state_for_head(&self, _registry_root: &Path, branch: &str) -> BranchPrState {
        self.calls.borrow_mut().push(format!("head:{branch}"));
        self.heads
            .get(branch)
            .cloned()
            .unwrap_or(BranchPrState::NoPr)
    }

    fn head_commit(&self, _worktree: &Path) -> Result<String, String> {
        self.calls.borrow_mut().push("head_commit".to_string());
        self.head_commit.clone()
    }

    fn merged_prs_containing(
        &self,
        _registry_root: &Path,
        sha: &str,
    ) -> Result<Vec<MergedPrHead>, String> {
        self.calls.borrow_mut().push(format!("search:{sha}"));
        self.search.clone()
    }

    fn is_ancestor(
        &self,
        _worktree: &Path,
        ancestor: &str,
        descendant: &str,
    ) -> Result<bool, String> {
        self.calls
            .borrow_mut()
            .push(format!("ancestor:{ancestor}:{descendant}"));
        if let Some(reason) = &self.ancestry_error {
            return Err(reason.clone());
        }
        Ok(self
            .ancestors
            .contains(&(ancestor.to_string(), descendant.to_string())))
    }
}

/// One row of the commit search, from this repository.
fn row(number: u64, head_ref_oid: &str) -> MergedPrHead {
    MergedPrHead {
        number,
        head_ref_oid: head_ref_oid.to_string(),
        is_cross_repository: false,
    }
}

// ---------------------------------------------------------------------------
// Rung 2 — the round-stem sibling
// ---------------------------------------------------------------------------

/// The reported defect, one rung at a time: a worktree left on `<branch>-r2`
/// after a review round, whose pull request merged under `<branch>` (#7267).
#[test]
fn a_round_sibling_with_a_merged_pr_is_reclaimable() {
    let probe = FakeProbe::default().with_head("fix/7267-thing", BranchPrState::Merged { pr: 91 });
    let out = resolve_landing(
        worktree(),
        root(),
        Some("fix/7267-thing-r2"),
        BranchPrState::NoPr,
        &probe,
    );
    assert_eq!(out, BranchPrState::Merged { pr: 91 });
    assert_eq!(probe.calls(), vec!["head:fix/7267-thing".to_string()]);
}

/// A sibling whose pull request is still OPEN is work in flight — the rung
/// produces nothing and the refusal stands (#7267).
#[test]
fn a_round_sibling_whose_stem_pr_is_open_is_not_widened() {
    let probe = FakeProbe::default().with_head("fix/7267-thing", BranchPrState::Open { pr: 92 });
    let out = resolve_landing(
        worktree(),
        root(),
        Some("fix/7267-thing-r2"),
        BranchPrState::NoPr,
        &probe,
    );
    assert_eq!(out, BranchPrState::NoPr, "an open sibling grants nothing");
}

/// A branch with no round suffix costs no sibling lookup at all (#7267).
#[test]
fn a_branch_without_a_round_suffix_asks_for_no_sibling() {
    let probe = FakeProbe::default();
    let _ = resolve_landing(
        worktree(),
        root(),
        Some("fix/7267-thing"),
        BranchPrState::NoPr,
        &probe,
    );
    assert!(
        !probe.calls().iter().any(|c| c.starts_with("head:")),
        "no stem to ask about: {:?}",
        probe.calls()
    );
}

// ---------------------------------------------------------------------------
// Rung 3 — the head commit
// ---------------------------------------------------------------------------

/// A branch re-cut under a new name before its pull request opened: no name
/// relates the two, but the COMMIT does (#7267).
#[test]
fn a_renamed_branch_matches_the_pr_opened_from_its_head_commit() {
    let probe = FakeProbe::default().with_search(vec![row(93, HEAD_SHA)]);
    let out = resolve_landing(
        worktree(),
        root(),
        Some("fix/7267-recut"),
        BranchPrState::NoPr,
        &probe,
    );
    assert_eq!(out, BranchPrState::Merged { pr: 93 });
}

/// A pull request opened from a DESCENDANT of this HEAD carried everything
/// here, so it vouches for the tree too (#7267).
#[test]
fn a_pr_opened_from_a_descendant_of_this_head_also_vouches() {
    let probe = FakeProbe::default()
        .with_search(vec![row(94, OTHER_SHA)])
        .with_ancestor(HEAD_SHA, OTHER_SHA);
    let out = resolve_landing(
        worktree(),
        root(),
        Some("fix/7267-recut"),
        BranchPrState::NoPr,
        &probe,
    );
    assert_eq!(out, BranchPrState::Merged { pr: 94 });
}

/// A detached worktree has no branch to widen by name, and still reaches the
/// commit rung (#7267).
#[test]
fn a_detached_worktree_can_still_match_by_head_commit() {
    let probe = FakeProbe::default().with_search(vec![row(95, HEAD_SHA)]);
    let out = resolve_landing(worktree(), root(), None, BranchPrState::Unknown, &probe);
    assert_eq!(out, BranchPrState::Merged { pr: 95 });
}

/// A merged pull request the search returned whose head stands in no ancestry
/// relationship to this HEAD vouches for nothing (#7267).
#[test]
fn an_unrelated_merged_pr_containing_no_ancestor_is_not_a_match() {
    let probe = FakeProbe::default().with_search(vec![row(96, OTHER_SHA)]);
    let out = resolve_landing(
        worktree(),
        root(),
        Some("fix/7267-recut"),
        BranchPrState::NoPr,
        &probe,
    );
    assert_eq!(out, BranchPrState::NoPr);
}

/// A fork's pull request that happens to contain a commit of this name is not
/// this repository's landing evidence (#7267, the #2919 rule).
#[test]
fn a_fork_pull_request_containing_the_commit_is_ignored() {
    let mut fork = row(97, HEAD_SHA);
    fork.is_cross_repository = true;
    let probe = FakeProbe::default().with_search(vec![fork]);
    let out = resolve_landing(
        worktree(),
        root(),
        Some("fix/7267-recut"),
        BranchPrState::NoPr,
        &probe,
    );
    assert_eq!(out, BranchPrState::NoPr);
}

// ---------------------------------------------------------------------------
// The refusals — every one of these would be a deleted checkout if it granted
// ---------------------------------------------------------------------------

/// The acceptance condition: no merged pull request under ANY route leaves the
/// worktree refused, `--force` or not (#7267).
#[test]
fn no_merged_pr_under_any_stem_is_still_refused() {
    let probe = FakeProbe::default();
    for (branch, exact) in [
        (Some("fix/7267-thing-r2"), BranchPrState::NoPr),
        (Some("fix/7267-thing"), BranchPrState::NoPr),
        (None, BranchPrState::Unknown),
    ] {
        let out = resolve_landing(worktree(), root(), branch, exact.clone(), &probe);
        assert_eq!(out, exact, "branch {branch:?} must stay refused");
    }
}

/// A settled answer about the branch ITSELF is never widened past — an open
/// pull request means work in flight (#7267).
#[test]
fn an_open_pr_on_the_branch_is_never_widened() {
    let probe = FakeProbe::default()
        .with_head("fix/7267-thing", BranchPrState::Merged { pr: 98 })
        .with_search(vec![row(99, HEAD_SHA)]);
    for exact in [
        BranchPrState::Open { pr: 1 },
        BranchPrState::ClosedUnmerged { pr: 2 },
        BranchPrState::LookupFailed {
            reason: "`gh` exited 4".to_string(),
        },
    ] {
        let out = resolve_landing(
            worktree(),
            root(),
            Some("fix/7267-thing-r2"),
            exact.clone(),
            &probe,
        );
        assert_eq!(out, exact, "a settled answer is the answer");
    }
    assert!(probe.calls().is_empty(), "and costs no widening call");
}

/// A HEAD that git could not read denies — the branch-name refusal stands and
/// nothing is reclaimed (#7267 fail-open check).
#[test]
fn a_failed_head_commit_read_leaves_the_refusal_standing() {
    let probe = FakeProbe::default()
        .with_search(vec![row(100, HEAD_SHA)])
        .with_head_commit(Err("`git rev-parse HEAD` failed (128)".to_string()));
    let out = resolve_landing(
        worktree(),
        root(),
        Some("fix/7267-recut"),
        BranchPrState::NoPr,
        &probe,
    );
    assert_eq!(out, BranchPrState::NoPr);
    assert!(
        !probe.calls().iter().any(|c| c.starts_with("search:")),
        "and never searches on a HEAD it could not read: {:?}",
        probe.calls()
    );
}

/// A `gh` that could not answer the commit search denies (#7267 fail-open
/// check).
#[test]
fn a_failed_commit_search_leaves_the_refusal_standing() {
    let probe = FakeProbe::default().with_search_error("`gh` exited 4: gh auth login");
    let out = resolve_landing(
        worktree(),
        root(),
        Some("fix/7267-recut"),
        BranchPrState::NoPr,
        &probe,
    );
    assert_eq!(out, BranchPrState::NoPr);
}

/// A git that could not settle the ancestry denies — an unresolvable object is
/// exactly what a pull request head this checkout never fetched looks like
/// (#7267 fail-open check).
#[test]
fn a_failed_ancestry_probe_leaves_the_refusal_standing() {
    let probe = FakeProbe::default()
        .with_search(vec![row(101, OTHER_SHA)])
        .with_ancestry_error("bad object bbbbbbb");
    let out = resolve_landing(
        worktree(),
        root(),
        Some("fix/7267-recut"),
        BranchPrState::NoPr,
        &probe,
    );
    assert_eq!(out, BranchPrState::NoPr);
}

/// A HEAD the probe reports as empty is not a commit to search for (#7267).
#[test]
fn an_empty_head_commit_reaches_no_search() {
    let probe = FakeProbe::default().with_head_commit(Ok("   ".to_string()));
    let out = resolve_landing(
        worktree(),
        root(),
        Some("fix/7267-recut"),
        BranchPrState::NoPr,
        &probe,
    );
    assert_eq!(out, BranchPrState::NoPr);
    assert_eq!(probe.calls(), vec!["head_commit".to_string()]);
}

// ---------------------------------------------------------------------------
// resolve_with_index — when a rung is allowed to run at all
// ---------------------------------------------------------------------------

/// A merged row keyed by a branch name.
fn index(branch: &str, pr: u64, state: &str) -> PrIndex {
    PrIndex::from_json(
        &format!(r#"[{{"number": {pr}, "headRefName": "{branch}", "state": "{state}"}}]"#),
        400,
    )
}

/// The `tm doctor` probe passes `per_branch_fallback: false` and must pay for
/// no network call — the index's own answer is the whole answer (#7267).
#[test]
fn resolve_with_index_without_fallback_never_calls_the_probe() {
    let probe = FakeProbe::default().with_head("fix/7267-thing", BranchPrState::Merged { pr: 102 });
    let out = resolve_with_index(
        worktree(),
        root(),
        Some("fix/7267-unrelated"),
        &index("fix/7267-thing", 102, "MERGED"),
        false,
        &probe,
    );
    assert_eq!(out, BranchPrState::NoPr);
    assert!(probe.calls().is_empty(), "{:?}", probe.calls());
}

/// #6561's per-branch retry survives the move into this function: a TRUNCATED
/// index still asks `gh` about the branch itself before any widening.
#[test]
fn resolve_with_index_retries_a_truncated_index_per_branch() {
    // A one-row reply against a limit of 1 is indistinguishable from a
    // truncated page, so the index reports incomplete.
    let truncated = PrIndex::from_json(
        r#"[{"number": 103, "headRefName": "someone/else", "state": "MERGED"}]"#,
        1,
    );
    let probe = FakeProbe::default().with_head("fix/7267-mine", BranchPrState::Merged { pr: 104 });
    let out = resolve_with_index(
        worktree(),
        root(),
        Some("fix/7267-mine"),
        &truncated,
        true,
        &probe,
    );
    assert_eq!(out, BranchPrState::Merged { pr: 104 });
    assert_eq!(probe.calls(), vec!["head:fix/7267-mine".to_string()]);
}

/// A COMPLETE index that answers `NoPr` is still widened — the renamed-branch
/// case has nothing to do with the index being truncated (#7267).
#[test]
fn resolve_with_index_widens_a_complete_index_no_pr_answer() {
    let probe = FakeProbe::default().with_search(vec![row(105, HEAD_SHA)]);
    let out = resolve_with_index(
        worktree(),
        root(),
        Some("fix/7267-recut"),
        &index("someone/else", 103, "MERGED"),
        true,
        &probe,
    );
    assert_eq!(out, BranchPrState::Merged { pr: 105 });
}
