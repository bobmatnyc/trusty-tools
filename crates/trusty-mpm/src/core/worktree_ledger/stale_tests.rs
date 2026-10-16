//! Tests for PR linking and stale candidates (#8994 S2c criteria 1 and 5).
//!
//! Why: a candidate is what S3a will reclaim, so only an ANSWERED fact may
//! make one; every unresolvable answer must leave the tree off the list.
//! What: a fake [`StaleProbe`] over real directories and a scratch ledger.
//! Test: this file.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::super::fold::fold;
use super::super::{EventKind, LedgerEvent, Origin, WorktreeLedger};
use super::{BranchAnswer, PrAnswer, StaleProbe, StaleReason, link_prs_and_find_stale};

/// Answers keyed by branch name.
#[derive(Default)]
struct FakeProbe {
    prs: BTreeMap<String, PrAnswer>,
    branches: BTreeMap<String, BranchAnswer>,
}

impl StaleProbe for FakeProbe {
    fn pr_for(&self, _repo: &Path, branch: &str) -> PrAnswer {
        self.prs.get(branch).cloned().unwrap_or(PrAnswer::NoPr)
    }
    fn branch_state(&self, _repo: &Path, _tree: &Path, branch: &str) -> BranchAnswer {
        self.branches
            .get(branch)
            .cloned()
            .unwrap_or(BranchAnswer::Present)
    }
}

struct Fixture {
    _dir: tempfile::TempDir,
    root: PathBuf,
    ledger: WorktreeLedger,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = std::fs::canonicalize(dir.path()).expect("canonicalize");
        let ledger = WorktreeLedger::at(root.join("worktrees.jsonl"));
        Self {
            _dir: dir,
            root,
            ledger,
        }
    }

    /// A real directory recorded live on `branch`.
    fn tree(&self, name: &str, branch: &str) -> PathBuf {
        let path = self.root.join("repo/.worktrees").join(name);
        std::fs::create_dir_all(&path).expect("mkdir");
        self.ledger
            .append(&LedgerEvent::now(
                path.clone(),
                EventKind::Created {
                    repo: self.root.join("repo"),
                    branch: Some(branch.to_string()),
                    origin: Origin::TmDaemon,
                    session: None,
                },
            ))
            .expect("seed");
        path
    }

    fn pass(&self, probe: &FakeProbe) -> super::StaleReport {
        let state = fold(&self.ledger.read().expect("read").events);
        link_prs_and_find_stale(&self.ledger, &state, probe).expect("pass")
    }

    fn pr_linked_lines(&self) -> usize {
        self.ledger
            .read()
            .expect("read")
            .events
            .iter()
            .filter(|e| matches!(e.kind, EventKind::PrLinked { .. }))
            .count()
    }
}

/// #8994 S2c criteria 1 and 5: a merged PR is linked once, shown per tree,
/// and makes the tree a candidate; nothing is deleted.
#[test]
fn a_merged_pr_is_linked_once_and_lists_the_tree_as_a_candidate() {
    let fx = Fixture::new();
    let tree = fx.tree("merged", "feat/merged");
    let mut probe = FakeProbe::default();
    probe.prs.insert("feat/merged".into(), PrAnswer::Merged(42));

    let first = fx.pass(&probe);
    let second = fx.pass(&probe);

    for report in [&first, &second] {
        assert_eq!(report.trees.len(), 1);
        assert_eq!(report.trees[0].pr, Some(42));
        assert_eq!(
            report.trees[0].stale,
            Some(StaleReason::MergedPr { pr: 42 })
        );
    }
    assert_eq!((first.pr_linked, second.pr_linked), (1, 0));
    assert_eq!(fx.pr_linked_lines(), 1, "an unchanged PR is not re-linked");
    let state = fold(&fx.ledger.read().expect("read").events);
    assert_eq!(state.live[&tree].pr, Some(42));
    assert!(tree.is_dir(), "a candidate is listed, never removed");
}

/// #8994 S2c criterion 5: only an answered fact makes a candidate. A gone
/// branch with nothing unpushed is one; a present branch, a branch with no
/// upstream, unpushed commits, an open PR, and any unresolvable answer are not.
#[test]
fn only_an_answered_fact_makes_a_stale_candidate() {
    let fx = Fixture::new();
    let cases: Vec<(
        &str,
        PrAnswer,
        BranchAnswer,
        Option<StaleReason>,
        Option<u64>,
    )> = vec![
        (
            "gone",
            PrAnswer::NoPr,
            BranchAnswer::Gone,
            Some(StaleReason::BranchGone),
            None,
        ),
        ("present", PrAnswer::NoPr, BranchAnswer::Present, None, None),
        (
            "no-upstream",
            PrAnswer::NoPr,
            BranchAnswer::NoUpstream,
            None,
            None,
        ),
        (
            "unpushed",
            PrAnswer::NoPr,
            BranchAnswer::Unpushed(2),
            None,
            None,
        ),
        (
            "open",
            PrAnswer::Open(7),
            BranchAnswer::Present,
            None,
            Some(7),
        ),
        (
            "closed",
            PrAnswer::Closed(8),
            BranchAnswer::Present,
            None,
            Some(8),
        ),
        (
            "git-unknown",
            PrAnswer::NoPr,
            BranchAnswer::Unresolved("fetch failed".into()),
            None,
            None,
        ),
        (
            "gh-unknown",
            PrAnswer::Unresolved("gh failed".into()),
            BranchAnswer::Present,
            None,
            None,
        ),
    ];
    let mut probe = FakeProbe::default();
    for (name, pr, branch, ..) in &cases {
        fx.tree(name, name);
        probe.prs.insert((*name).into(), pr.clone());
        probe.branches.insert((*name).into(), branch.clone());
    }

    let report = fx.pass(&probe);

    for (name, _, _, stale, pr) in &cases {
        let view = report
            .trees
            .iter()
            .find(|t| t.branch.as_deref() == Some(*name))
            .unwrap_or_else(|| panic!("{name} is listed"));
        assert_eq!(&view.stale, stale, "{name}");
        assert_eq!(&view.pr, pr, "{name}");
        if name.ends_with("-unknown") {
            assert!(!view.unresolved.is_empty(), "{name} says why");
        }
    }
    assert_eq!(report.candidates().count(), 1);
}

/// #8994 S2c criterion 5: a tree whose directory is gone, or that records no
/// branch, is never asked about and never a candidate.
#[test]
fn a_missing_or_branchless_tree_is_not_a_candidate() {
    let fx = Fixture::new();
    let gone = fx.tree("gone-dir", "feat/gone-dir");
    std::fs::remove_dir(&gone).expect("rmdir");
    let branchless = fx.root.join("repo/.worktrees/detached");
    std::fs::create_dir_all(&branchless).expect("mkdir");
    fx.ledger
        .append(&LedgerEvent::now(
            branchless.clone(),
            EventKind::Observed {
                repo: fx.root.join("repo"),
                branch: None,
                origin: Origin::AgentIsolation,
                session: None,
            },
        ))
        .expect("seed");
    let mut probe = FakeProbe::default();
    probe
        .prs
        .insert("feat/gone-dir".into(), PrAnswer::Merged(1));

    let report = fx.pass(&probe);

    assert_eq!(report.trees.len(), 2);
    assert_eq!(report.candidates().count(), 0);
    assert!(report.trees.iter().all(|t| !t.unresolved.is_empty()));
}
