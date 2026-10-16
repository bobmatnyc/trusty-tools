//! Tests for the ledger reclaim plan (#8994 S3a criteria 2, 3 and 8).
//!
//! Why: the plan decides which candidates may reach the removal gates at all,
//! so every rule and every unanswered probe needs a test that keeps the tree.
//! What: a fake [`ReclaimProbe`] over hand-built stale reports.
//! Test: this file.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::super::stale::{StaleReason, StaleReport, TreeView};
use super::{HeadAnswer, PlanAction, PostMerge, ReclaimProbe, f1_refusal, plan_reclaim};

const HEAD: &str = "0123456789abcdef0123456789abcdef01234567";

/// Answers keyed by tree path; unlisted trees sit on their own branch.
#[derive(Default)]
struct FakeProbe {
    heads: BTreeMap<PathBuf, HeadAnswer>,
    merged: BTreeMap<u64, Result<String, String>>,
    post: BTreeMap<PathBuf, PostMerge>,
    asked_merged: RefCell<usize>,
}

impl ReclaimProbe for FakeProbe {
    fn head_of(&self, tree: &Path) -> HeadAnswer {
        self.heads.get(tree).cloned().unwrap_or_else(|| {
            let name = tree.file_name().map(|n| n.to_string_lossy().to_string());
            HeadAnswer::Ref(format!("refs/heads/wt/{}", name.unwrap_or_default()))
        })
    }
    fn merged_head(&self, _repo: &Path, pr: u64) -> Result<String, String> {
        *self.asked_merged.borrow_mut() += 1;
        self.merged.get(&pr).cloned().unwrap_or(Ok(HEAD.into()))
    }
    fn post_merge(&self, tree: &Path, _branch: &str, pr_head: &str) -> PostMerge {
        assert_eq!(pr_head, HEAD, "the merged PR's head is what is compared");
        self.post
            .get(tree)
            .cloned()
            .unwrap_or(PostMerge::NothingAfter)
    }
}

fn tree(name: &str, stale: Option<StaleReason>) -> TreeView {
    TreeView {
        path: PathBuf::from("/r/.worktrees").join(name),
        repo: PathBuf::from("/r"),
        branch: Some(format!("wt/{name}")),
        pr: match stale {
            Some(StaleReason::MergedPr { pr }) => Some(pr),
            _ => None,
        },
        stale,
        unresolved: Vec::new(),
    }
}

fn report(trees: Vec<TreeView>) -> StaleReport {
    StaleReport {
        trees,
        pr_linked: 0,
    }
}

fn action_of<'a>(plan: &'a [super::PlannedTree], name: &str) -> &'a PlanAction {
    &plan
        .iter()
        .find(|p| p.path.ends_with(name))
        .unwrap_or_else(|| panic!("{name} planned: {plan:?}"))
        .action
}

fn kept_because(action: &PlanAction, needle: &str) {
    match action {
        PlanAction::Keep { reason } => assert!(reason.contains(needle), "{reason}"),
        other => panic!("expected keep ({needle}), got {other:?}"),
    }
}

/// #8994 S3a criterion 3: F1 keeps a detached HEAD, another branch and an
/// unanswered `symbolic-ref`; only the recorded branch passes.
#[test]
fn f1_keeps_a_detached_or_other_branch_or_unanswered_head() {
    assert_eq!(
        f1_refusal(&HeadAnswer::Ref("refs/heads/a".into()), "a"),
        None
    );
    for head in [
        HeadAnswer::Detached,
        HeadAnswer::Ref("refs/heads/b".into()),
        HeadAnswer::Ref("refs/heads/a-r2".into()),
        HeadAnswer::Unresolved("git exited 128".into()),
    ] {
        let why = f1_refusal(&head, "a").expect("refused");
        assert!(why.starts_with("F1:"), "{why}");
    }
}

/// #8994 S3a criteria 2 and 3: a candidate whose tree left its branch is kept;
/// a clean branch-gone candidate is eligible with no PR head.
#[test]
fn a_candidate_whose_head_moved_is_kept_and_a_clean_one_is_eligible() {
    let probe = FakeProbe {
        heads: BTreeMap::from([
            (
                PathBuf::from("/r/.worktrees/detached"),
                HeadAnswer::Detached,
            ),
            (
                PathBuf::from("/r/.worktrees/switched"),
                HeadAnswer::Ref("refs/heads/main".into()),
            ),
        ]),
        ..FakeProbe::default()
    };
    let plan = plan_reclaim(
        &report(vec![
            tree("detached", Some(StaleReason::MergedPr { pr: 4 })),
            tree("switched", Some(StaleReason::BranchGone)),
            tree("gone", Some(StaleReason::BranchGone)),
            tree("live", None),
        ]),
        &probe,
    );
    assert_eq!(plan.len(), 3, "a tree with no stale fact is not planned");
    kept_because(action_of(&plan, "detached"), "detached");
    kept_because(action_of(&plan, "switched"), "refs/heads/main");
    assert_eq!(
        action_of(&plan, "gone"),
        &PlanAction::Eligible { pr_head: None }
    );
    assert_eq!(*probe.asked_merged.borrow(), 0, "F1 is asked first");
}

/// #8994 S3a criterion 2: a merged-PR candidate whose branch gained commits
/// after the merge is kept; one at the PR's head is eligible.
#[test]
fn a_merged_candidate_with_commits_after_the_merge_is_kept() {
    let probe = FakeProbe {
        post: BTreeMap::from([(
            PathBuf::from("/r/.worktrees/moved-on"),
            PostMerge::CommitsAfter,
        )]),
        ..FakeProbe::default()
    };
    let plan = plan_reclaim(
        &report(vec![
            tree("moved-on", Some(StaleReason::MergedPr { pr: 7 })),
            tree("landed", Some(StaleReason::MergedPr { pr: 8 })),
        ]),
        &probe,
    );
    kept_because(action_of(&plan, "moved-on"), "gained commits after PR #7");
    assert_eq!(
        action_of(&plan, "landed"),
        &PlanAction::Eligible {
            pr_head: Some(HEAD.into())
        }
    );
}

/// #8994 S3a criterion 8: a failed `gh` lookup of the PR head, an unanswered
/// ancestry check and an unresolved stale lookup (fetch or `gh` failed) each
/// keep the tree and say why.
#[test]
fn every_unanswered_probe_keeps_the_tree() {
    let probe = FakeProbe {
        merged: BTreeMap::from([(9, Err("gh timed out".into()))]),
        post: BTreeMap::from([(
            PathBuf::from("/r/.worktrees/no-ancestry"),
            PostMerge::Unresolved("bad object".into()),
        )]),
        heads: BTreeMap::from([(
            PathBuf::from("/r/.worktrees/no-head"),
            HeadAnswer::Unresolved("git exited 128".into()),
        )]),
        ..FakeProbe::default()
    };
    let mut unresolved = tree("no-fetch", None);
    unresolved.unresolved = vec!["remote branch: fetch failed".into()];
    let plan = plan_reclaim(
        &report(vec![
            tree("no-gh", Some(StaleReason::MergedPr { pr: 9 })),
            tree("no-ancestry", Some(StaleReason::MergedPr { pr: 10 })),
            tree("no-head", Some(StaleReason::BranchGone)),
            unresolved,
        ]),
        &probe,
    );
    kept_because(action_of(&plan, "no-gh"), "gh timed out");
    kept_because(action_of(&plan, "no-ancestry"), "bad object");
    kept_because(action_of(&plan, "no-head"), "could not be read");
    kept_because(action_of(&plan, "no-fetch"), "fetch failed");
    assert!(
        plan.iter()
            .all(|p| matches!(p.action, PlanAction::Keep { .. })),
        "{plan:?}"
    );
}
