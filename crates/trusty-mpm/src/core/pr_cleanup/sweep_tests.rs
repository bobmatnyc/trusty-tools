//! Unit tests for the periodic cleanup trigger (#7275, owner scope amendment
//! 2026-09-09).
//!
//! The trigger's whole job is to fire exactly once per PR, at the transition to
//! MERGED, so these tests pin both halves of that: a newly merged PR is cleaned
//! and stamped, and a PR already carrying a stamp is never looked at again.

use std::cell::RefCell;
use std::path::{Path, PathBuf};

use chrono::Utc;

use super::{SweepDecision, run_sweep, sweep_decision};
use crate::core::pr_cleanup::driver::{ClaimEnder, CmdOut, Gh, Git};
use crate::core::pr_cleanup::plan::PrView;
use crate::core::pr_cleanup::registry::{CleanupRegistry, OpenedPr};
use crate::session_manager::DirtyWorktree;

const HEAD_OID: &str = "abc1234def5678000000000000000000000000aa";
const BRANCH: &str = "feat/7275-post-merge-cleanup";
const REPO: &str = "bobmatnyc/trusty-tools";

fn view(state: &str) -> PrView {
    PrView {
        state: state.to_string(),
        head_ref_name: BRANCH.to_string(),
        head_ref_oid: HEAD_OID.to_string(),
        merge_commit: None,
        base_ref_name: "main".to_string(),
    }
}

fn entry(pr: u64, cleaned: bool, root: &Path) -> OpenedPr {
    OpenedPr {
        pr,
        repo: REPO.to_string(),
        repo_root: root.to_path_buf(),
        opened_at: Utc::now(),
        cleaned_at: cleaned.then(Utc::now),
    }
}

// ── the decision ─────────────────────────────────────────────────────────

#[test]
fn sweep_decision_cleans_a_newly_merged_pr() {
    let e = entry(7275, false, Path::new("/repo"));
    assert_eq!(sweep_decision(&e, &view("MERGED")), SweepDecision::Clean);
}

#[test]
fn sweep_decision_never_reruns_a_cleaned_pr() {
    let e = entry(7275, true, Path::new("/repo"));
    assert_eq!(
        sweep_decision(&e, &view("MERGED")),
        SweepDecision::AlreadyCleaned,
        "the stamp outranks the state; re-running a destructive sequence is worse than idling"
    );
}

#[test]
fn sweep_decision_leaves_an_open_pr_alone() {
    let e = entry(7275, false, Path::new("/repo"));
    match sweep_decision(&e, &view("OPEN")) {
        SweepDecision::NotMerged(reason) => assert!(reason.contains("OPEN"), "{reason}"),
        other => panic!("an OPEN PR must not be cleaned: {other:?}"),
    }
}

// ── the loop ─────────────────────────────────────────────────────────────

/// A seam that answers by argv-substring match.
struct Scripted {
    routes: Vec<(String, String)>,
    seen: RefCell<Vec<String>>,
}

impl Scripted {
    fn new() -> Self {
        Self {
            routes: Vec::new(),
            seen: RefCell::new(Vec::new()),
        }
    }

    fn on(mut self, needle: &str, stdout: &str) -> Self {
        self.routes.push((needle.to_string(), stdout.to_string()));
        self
    }

    fn answer(&self, joined: &str) -> anyhow::Result<CmdOut> {
        self.seen.borrow_mut().push(joined.to_string());
        for (needle, stdout) in &self.routes {
            if joined.contains(needle.as_str()) {
                return Ok(CmdOut {
                    success: true,
                    stdout: stdout.clone(),
                    stderr: String::new(),
                });
            }
        }
        anyhow::bail!("Scripted: no route for `{joined}`")
    }

    fn calls(&self) -> Vec<String> {
        self.seen.borrow().clone()
    }
}

impl Gh for Scripted {
    fn run(&self, args: &[String]) -> anyhow::Result<CmdOut> {
        self.answer(&format!("gh {}", args.join(" ")))
    }
}

impl Git for Scripted {
    fn run(&self, _dir: &Path, args: &[String]) -> anyhow::Result<CmdOut> {
        self.answer(&format!("git {}", args.join(" ")))
    }
}

struct NoClaims;

#[async_trait::async_trait]
impl ClaimEnder for NoClaims {
    async fn claims_on(&self, _path: &Path) -> anyhow::Result<Vec<String>> {
        Ok(Vec::new())
    }
    async fn end_claim(&self, _id: &str) -> anyhow::Result<()> {
        Ok(())
    }
}

fn gh_merged() -> Scripted {
    Scripted::new().on(
        "gh pr view 7275",
        &format!(
            "{{\"state\":\"MERGED\",\"headRefName\":\"{BRANCH}\",\"headRefOid\":\"{HEAD_OID}\"}}"
        ),
    )
}

/// A `git` fake where nothing is left to clean — every step completes.
fn git_nothing_left() -> Scripted {
    Scripted::new()
        .on("git ls-remote", "")
        .on(
            "git worktree list",
            "worktree /repo\nHEAD 1111\nbranch refs/heads/main\n\n",
        )
        .on("git branch --format", "main 1111\n")
        .on("git worktree prune", "")
        .on("git fetch --prune", "")
}

/// A `git` fake with one worktree still holding the merged head.
fn git_one_tree(tree: &str) -> Scripted {
    Scripted::new()
        .on("git ls-remote", "")
        .on(
            "git worktree list",
            &format!(
                "worktree /repo\nHEAD 1111\nbranch refs/heads/main\n\n\
                 worktree {tree}\nHEAD {HEAD_OID}\nbranch refs/heads/{BRANCH}\n\n"
            ),
        )
        .on("git worktree remove", "")
        .on("git branch --format", "main 1111\n")
        .on("git worktree prune", "")
        .on("git fetch --prune", "")
}

fn clean(_p: &Path) -> Option<DirtyWorktree> {
    None
}

#[tokio::test]
async fn sweep_stamps_only_a_fully_successful_run() {
    let dir = tempfile::tempdir().expect("tempdir");
    let reg = CleanupRegistry::under_root(dir.path());
    reg.record_open(entry(7275, false, Path::new("/repo")))
        .expect("record");

    let gh = gh_merged();
    let git = git_nothing_left();
    let cleaned = run_sweep(&gh, &git, &NoClaims, &clean, &reg).await;

    assert_eq!(cleaned, 1, "the newly merged PR is cleaned once");
    assert!(reg.pending().is_empty(), "and stamped so it never re-runs");

    // A second sweep against the same registry must do nothing at all.
    let gh2 = gh_merged();
    let git2 = git_nothing_left();
    let again = run_sweep(&gh2, &git2, &NoClaims, &clean, &reg).await;
    assert_eq!(again, 0, "the trigger is idempotent");
    assert!(
        gh2.calls().is_empty() && git2.calls().is_empty(),
        "a stamped entry is not even asked about: gh={:?} git={:?}",
        gh2.calls(),
        git2.calls()
    );
}

#[tokio::test]
async fn sweep_leaves_a_dirty_worktree_pending() {
    let dir = tempfile::tempdir().expect("tempdir");
    let reg = CleanupRegistry::under_root(dir.path());
    reg.record_open(entry(7275, false, Path::new("/repo")))
        .expect("record");

    let gh = gh_merged();
    let git = git_one_tree("/repo/.claude/worktrees/agent-aa11");
    let dirty = |p: &Path| {
        Some(DirtyWorktree {
            path: p.to_path_buf(),
            reason: "1 uncommitted/untracked file(s), 0 unpushed commit(s)".to_string(),
            dirty_files: 1,
            unpushed_commits: 0,
        })
    };
    let cleaned = run_sweep(&gh, &git, &NoClaims, &dirty, &reg).await;

    assert_eq!(cleaned, 0, "a refused run is not a success");
    assert_eq!(
        reg.pending().len(),
        1,
        "it stays pending so it retries once the operator saves their work"
    );
    assert!(
        !git.calls().iter().any(|c| c.contains("worktree remove")),
        "the dirty tree is never removed: {:?}",
        git.calls()
    );
}

#[tokio::test]
async fn sweep_skips_a_pr_that_has_not_merged() {
    let dir = tempfile::tempdir().expect("tempdir");
    let reg = CleanupRegistry::under_root(dir.path());
    reg.record_open(entry(7275, false, Path::new("/repo")))
        .expect("record");

    let gh = Scripted::new().on(
        "gh pr view 7275",
        &format!("{{\"state\":\"OPEN\",\"headRefName\":\"{BRANCH}\"}}"),
    );
    let git = Scripted::new();
    let cleaned = run_sweep(&gh, &git, &NoClaims, &clean, &reg).await;

    assert_eq!(cleaned, 0);
    assert_eq!(reg.pending().len(), 1, "an open PR stays pending");
    assert!(
        git.calls().is_empty(),
        "no git command runs for an unmerged PR: {:?}",
        git.calls()
    );
}

#[test]
fn sweep_registry_path_is_under_the_framework_root() {
    let reg = CleanupRegistry::under_root(PathBuf::from("/root"));
    assert_eq!(
        reg.path(),
        Path::new("/root/pr-cleanup.json"),
        "one registry, at a stable place the supervisor and the CLI both read"
    );
}
