//! Unit tests for the post-merge cleanup engine (#7275).
//!
//! Every test drives the `Gh`, `Git` and `ClaimEnder` seams with a scripted
//! fake, so nothing here touches the network, a live `gh`, or a real worktree.

use std::cell::RefCell;
use std::path::{Path, PathBuf};

use super::driver::{ClaimEnder, CmdOut, Gh, Git, Landing};
use super::plan::{
    AGENT_BRANCH_PREFIX, MergeCommit, PrView, StepLine, StepStatus, agent_branches_at,
    merge_refusal, parse_worktree_list, worktree_targets,
};
use super::registry::{CleanupRegistry, OpenedPr};
use super::{CleanupRequest, run};
use crate::session_manager::DirtyWorktree;
use crate::session_manager::worktree_reclaim::BranchPrState;
use crate::session_manager::worktree_reclaim_pr_match::worktree_reclaim_pr_match_tests::FakeProbe;

// ── fakes ────────────────────────────────────────────────────────────────

/// A seam that answers by argv-substring match, in registration order.
struct Scripted {
    /// (substring that must appear in the joined argv, response).
    routes: Vec<(String, CmdOut)>,
    /// Every argv this fake was asked to run, joined, in order.
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
        self.routes.push((
            needle.to_string(),
            CmdOut {
                success: true,
                stdout: stdout.to_string(),
                stderr: String::new(),
            },
        ));
        self
    }

    fn on_fail(mut self, needle: &str, stderr: &str) -> Self {
        self.routes.push((
            needle.to_string(),
            CmdOut {
                success: false,
                stdout: String::new(),
                stderr: stderr.to_string(),
            },
        ));
        self
    }

    fn answer(&self, joined: &str) -> anyhow::Result<CmdOut> {
        self.seen.borrow_mut().push(joined.to_string());
        for (needle, out) in &self.routes {
            if joined.contains(needle.as_str()) {
                return Ok(out.clone());
            }
        }
        // An unrouted call is a scripting mistake, not a command failure —
        // failing loudly here is what keeps a test from silently exercising a
        // path it never meant to.
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

impl Scripted {
    /// The head this fake's own worktree listing gives `dir` (#7275 round 4).
    ///
    /// Why: the pre-removal re-read asks each WORKTREE for its HEAD, and a
    /// substring route cannot tell two trees apart. Answering out of the
    /// porcelain listing this fake already serves keeps the two consistent by
    /// construction — a real `git` cannot disagree with itself either — and
    /// costs no route per test.
    fn head_of(&self, dir: &Path) -> anyhow::Result<CmdOut> {
        let listing = self
            .routes
            .iter()
            .find(|(needle, _)| needle.contains("worktree list"))
            .map(|(_, out)| out.stdout.clone())
            .unwrap_or_default();
        let head = parse_worktree_list(&listing)
            .into_iter()
            .find(|e| e.path == dir)
            .map(|e| e.head);
        match head {
            Some(h) => Ok(CmdOut {
                success: true,
                stdout: format!("{h}\n"),
                stderr: String::new(),
            }),
            None => anyhow::bail!("Scripted: no worktree listed at {}", dir.display()),
        }
    }
}

impl Git for Scripted {
    fn run(&self, dir: &Path, args: &[String]) -> anyhow::Result<CmdOut> {
        let joined = format!("git {}", args.join(" "));
        if args == ["rev-parse", "HEAD"] {
            self.seen.borrow_mut().push(joined);
            return self.head_of(dir);
        }
        self.answer(&joined)
    }
}

/// A [`ClaimEnder`] with a pinned answer and a record of what it ended.
struct FakeClaims {
    /// Session ids every path is reported as claimed by.
    holders: Vec<String>,
    /// Ids passed to `end_claim`, in order.
    ended: std::sync::Mutex<Vec<String>>,
}

impl FakeClaims {
    fn none() -> Self {
        Self {
            holders: Vec::new(),
            ended: std::sync::Mutex::new(Vec::new()),
        }
    }

    fn held_by(id: &str) -> Self {
        Self {
            holders: vec![id.to_string()],
            ended: std::sync::Mutex::new(Vec::new()),
        }
    }
}

#[async_trait::async_trait]
impl ClaimEnder for FakeClaims {
    async fn claims_on(&self, _path: &Path) -> anyhow::Result<Vec<String>> {
        Ok(self.holders.clone())
    }
    async fn end_claim(&self, id: &str) -> anyhow::Result<()> {
        self.ended.lock().expect("claims lock").push(id.to_string());
        Ok(())
    }
}

// ── fixtures ─────────────────────────────────────────────────────────────

const HEAD_OID: &str = "abc1234def5678000000000000000000000000aa";
const BRANCH: &str = "feat/7275-post-merge-cleanup";
const TREE: &str = "/repo/.claude/worktrees/agent-aa11";

fn root() -> PathBuf {
    PathBuf::from("/repo")
}

fn req(dry_run: bool) -> CleanupRequest {
    CleanupRequest {
        pr: 7275,
        repo: Some("bobmatnyc/trusty-tools".to_string()),
        repo_root: root(),
        dry_run,
    }
}

fn merged_json() -> String {
    format!(
        "{{\"state\":\"MERGED\",\"headRefName\":\"{BRANCH}\",\"headRefOid\":\"{HEAD_OID}\",\
         \"mergeCommit\":{{\"oid\":\"9999888877776666555544443333222211110000\"}}}}"
    )
}

fn worktree_listing() -> String {
    format!(
        "worktree /repo\nHEAD 1111111111111111111111111111111111111111\nbranch refs/heads/main\n\n\
         worktree {TREE}\nHEAD {HEAD_OID}\nbranch refs/heads/{BRANCH}\n\n"
    )
}

fn branch_listing() -> String {
    format!(
        "main 1111111111111111111111111111111111111111\n\
         {BRANCH} {HEAD_OID}\n\
         {AGENT_BRANCH_PREFIX}aa11 {HEAD_OID}\n\
         {AGENT_BRANCH_PREFIX}bb22 2222222222222222222222222222222222222222\n"
    )
}

/// #7275 round 2: the argv the repo/checkout reconciliation runs, and the
/// answer that AGREES with `req()`'s stated repository.
const ORIGIN_QUERY: &str = "config --get remote.origin.url";
/// See [`ORIGIN_QUERY`]. An https URL, so no test reads an SSH alias table.
const ORIGIN_URL: &str = "https://github.com/bobmatnyc/trusty-tools.git\n";

/// A `gh` fake that reports the PR merged.
fn gh_merged() -> Scripted {
    Scripted::new().on("gh pr view 7275", &merged_json())
}

/// A `git` fake with the remote branch present and one matching worktree.
fn git_full() -> Scripted {
    Scripted::new()
        .on(ORIGIN_QUERY, ORIGIN_URL)
        .on(
            "git ls-remote",
            &format!("{HEAD_OID}\trefs/heads/{BRANCH}\n"),
        )
        .on("git push origin --delete", "")
        .on("git worktree list", &worktree_listing())
        .on("git worktree remove", "")
        .on("git branch --format", &branch_listing())
        .on("git branch -D", "")
        .on("git worktree prune", "")
        .on("git fetch --prune", "")
}

/// Nothing in any worktree is dirty.
fn clean(_p: &Path) -> Option<DirtyWorktree> {
    None
}

/// A [`Landing`] over #7267's own `LandingProbe` fake (#7275 round 3).
///
/// Why: what is under test is the ahead-count gate's POLICY, not `gh`. Routing
/// through `driver::resolve_merged_pr` means these tests run the production
/// seed-then-ladder body rather than a second copy of it, and the probe below
/// is the fake `worktree_reclaim_pr_match_tests` already drives.
struct FakeLanding {
    /// The scripted merged-pull-request answers.
    probe: FakeProbe,
}

impl FakeLanding {
    /// A probe that knows of no pull request at all — the refusal arm.
    fn nothing_merged() -> Self {
        Self {
            probe: FakeProbe::default(),
        }
    }

    /// A probe reporting `branch` as carried by a MERGED pull request.
    fn merged(branch: &str, pr: u64) -> Self {
        Self {
            probe: FakeProbe::default().with_head(branch, BranchPrState::Merged { pr }),
        }
    }
}

impl Landing for FakeLanding {
    fn merged_pr(
        &self,
        repo_root: &Path,
        worktree: &Path,
        branch: Option<&str>,
        exact: Option<u64>,
    ) -> Option<u64> {
        super::driver::resolve_merged_pr(&self.probe, repo_root, worktree, branch, exact)
    }
}

/// A dirt probe reporting `n` commits ahead of the upstream and nothing else —
/// exactly what `inspect_dirt` says about every squash-merged worktree.
fn ahead_by(n: usize) -> impl Fn(&Path) -> Option<DirtyWorktree> {
    move |p: &Path| {
        Some(DirtyWorktree {
            path: p.to_path_buf(),
            reason: format!("0 uncommitted/untracked file(s), {n} unpushed commit(s)"),
            dirty_files: 0,
            unpushed_commits: n,
        })
    }
}

fn rendered(lines: &[StepLine]) -> String {
    lines
        .iter()
        .map(StepLine::render)
        .collect::<Vec<_>>()
        .join("\n")
}

// ── step 1: the merge gate ───────────────────────────────────────────────

#[test]
fn merge_refusal_accepts_merged() {
    let view = PrView {
        state: "MERGED".to_string(),
        head_ref_name: BRANCH.to_string(),
        head_ref_oid: HEAD_OID.to_string(),
        merge_commit: Some(MergeCommit {
            oid: "deadbeef".to_string(),
        }),
        base_ref_name: "main".to_string(),
    };
    assert_eq!(merge_refusal(&view, 7275), None);
}

#[test]
fn merge_refusal_names_the_state() {
    let view = PrView {
        state: "OPEN".to_string(),
        head_ref_name: BRANCH.to_string(),
        ..PrView::default()
    };
    let reason = merge_refusal(&view, 7275).expect("an OPEN PR must be refused");
    assert!(
        reason.contains("OPEN"),
        "reason must name the state: {reason}"
    );
    assert!(reason.contains("7275"), "reason must name the PR: {reason}");
}

#[test]
fn merge_refusal_rejects_a_payload_with_no_head() {
    let view = PrView {
        state: "MERGED".to_string(),
        ..PrView::default()
    };
    assert!(
        merge_refusal(&view, 7275).is_some(),
        "MERGED with no head names nothing to clean up"
    );
}

// ── worktree listing ─────────────────────────────────────────────────────

#[test]
fn parse_worktree_list_reads_branch_and_head() {
    let entries = parse_worktree_list(&worktree_listing());
    assert_eq!(entries.len(), 2, "{entries:?}");
    assert_eq!(entries[1].path, PathBuf::from(TREE));
    assert_eq!(entries[1].head, HEAD_OID);
    assert_eq!(entries[1].branch.as_deref(), Some(BRANCH));
}

#[test]
fn parse_worktree_list_drops_the_bare_record() {
    let entries = parse_worktree_list("worktree /repo/.git\nbare\n\n");
    assert!(entries.is_empty(), "{entries:?}");
}

#[test]
fn parse_worktree_list_keeps_a_detached_worktree() {
    let entries = parse_worktree_list(&format!("worktree {TREE}\nHEAD {HEAD_OID}\ndetached\n\n"));
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].branch, None);
}

#[test]
fn worktree_targets_matches_branch_and_detached_head() {
    let listing = format!(
        "worktree /repo\nHEAD 1111111111111111111111111111111111111111\nbranch refs/heads/main\n\n\
         worktree /repo/wt-branch\nHEAD 3333333333333333333333333333333333333333\n\
         branch refs/heads/{BRANCH}\n\n\
         worktree /repo/wt-detached\nHEAD {HEAD_OID}\ndetached\n\n"
    );
    let entries = parse_worktree_list(&listing);
    let hits = worktree_targets(&entries, BRANCH, HEAD_OID, &root());
    let paths: Vec<_> = hits.iter().map(|e| e.path.clone()).collect();
    assert_eq!(
        paths,
        vec![
            PathBuf::from("/repo/wt-branch"),
            PathBuf::from("/repo/wt-detached")
        ],
        "both the branch match and the detached head match are targets"
    );
}

#[test]
fn worktree_targets_never_returns_the_main_checkout() {
    let listing = format!("worktree /repo\nHEAD {HEAD_OID}\nbranch refs/heads/{BRANCH}\n\n");
    let entries = parse_worktree_list(&listing);
    assert!(
        worktree_targets(&entries, BRANCH, HEAD_OID, &root()).is_empty(),
        "cleanup runs from the main checkout; it is never its own target"
    );
}

#[test]
fn worktree_targets_ignores_an_empty_head_oid() {
    let listing = format!("worktree {TREE}\nHEAD \ndetached\n\n");
    let entries = parse_worktree_list(&listing);
    assert!(
        worktree_targets(&entries, "", "", &root()).is_empty(),
        "an empty branch and OID must match nothing"
    );
}

// ── branch selection ─────────────────────────────────────────────────────

#[test]
fn agent_branches_at_matches_only_the_agent_prefix() {
    let hits = agent_branches_at(&branch_listing(), HEAD_OID);
    assert_eq!(hits, vec![format!("{AGENT_BRANCH_PREFIX}aa11")]);
}

#[test]
fn agent_branches_at_ignores_a_branch_that_moved_on() {
    let listing = format!("{AGENT_BRANCH_PREFIX}bb22 2222222222222222222222222222222222222222\n");
    assert!(
        agent_branches_at(&listing, HEAD_OID).is_empty(),
        "a tip that is not the merged head carries commits the PR never had"
    );
}

#[test]
fn agent_branches_at_returns_nothing_for_an_empty_oid() {
    assert!(agent_branches_at(&branch_listing(), "  ").is_empty());
}

#[test]
fn step_line_renders_ok_and_failed() {
    assert_eq!(StepLine::ok("pr", "fine").render(), "pr: ok — fine");
    assert_eq!(StepLine::failed("pr", "nope").render(), "pr: FAILED — nope");
    assert_eq!(StepLine::ok("pr", "fine").status, StepStatus::Ok);
}

// ── the executor ─────────────────────────────────────────────────────────

#[tokio::test]
async fn cleanup_refuses_an_open_pr() {
    let gh = Scripted::new().on(
        "gh pr view 7275",
        &format!("{{\"state\":\"OPEN\",\"headRefName\":\"{BRANCH}\"}}"),
    );
    let git = Scripted::new();
    let claims = FakeClaims::none();
    let report = run(
        &gh,
        &git,
        &claims,
        &FakeLanding::nothing_merged(),
        &clean,
        &req(false),
    )
    .await;

    assert!(report.failed(), "{}", report.render());
    assert_eq!(report.lines.len(), 1, "the refusal short-circuits the run");
    assert!(report.lines[0].detail.contains("OPEN"));
    assert!(
        git.calls().is_empty(),
        "no git command may run for an unmerged PR: {:?}",
        git.calls()
    );
}

#[tokio::test]
async fn cleanup_refuses_when_gh_view_fails() {
    let gh = Scripted::new().on_fail("gh pr view 7275", "no pull requests found");
    let git = Scripted::new();
    let claims = FakeClaims::none();
    let report = run(
        &gh,
        &git,
        &claims,
        &FakeLanding::nothing_merged(),
        &clean,
        &req(false),
    )
    .await;

    assert!(report.failed());
    assert!(report.lines[0].detail.contains("no pull requests found"));
    assert!(git.calls().is_empty());
}

#[tokio::test]
async fn cleanup_clean_path_removes_everything() {
    let gh = gh_merged();
    let git = git_full();
    let claims = FakeClaims::none();
    let report = run(
        &gh,
        &git,
        &claims,
        &FakeLanding::nothing_merged(),
        &clean,
        &req(false),
    )
    .await;

    assert!(!report.failed(), "{}", report.render());
    let calls = git.calls();
    let joined = calls.join("\n");
    assert!(
        joined.contains("git push origin --delete"),
        "the remote branch must be deleted: {joined}"
    );
    assert!(
        joined.contains(&format!("git worktree remove {TREE}")),
        "the worktree must be removed: {joined}"
    );
    assert!(
        !joined.contains("--force"),
        "cleanup never forces a worktree removal: {joined}"
    );
    assert!(
        joined.contains(&format!("git branch -D {BRANCH}")),
        "the head branch must be deleted: {joined}"
    );
    assert!(
        joined.contains(&format!("git branch -D {AGENT_BRANCH_PREFIX}aa11")),
        "the agent branch at the merged head must be deleted: {joined}"
    );
    assert!(
        !joined.contains(&format!("git branch -D {AGENT_BRANCH_PREFIX}bb22")),
        "an agent branch that moved on must be left alone: {joined}"
    );
    assert!(joined.contains("git worktree prune"), "{joined}");
    assert!(joined.contains("git fetch --prune origin"), "{joined}");
}

/// 🔴 REGRESSION (#7275 round 2): a registry entry whose `repo` and
/// `repo_root` name different repositories REFUSES.
///
/// Why: `--repo` aims `gh` and `repo_root` aims every git command, and the two
/// are recorded independently at open time. A directory later reused for a
/// different clone would let a MERGED pull request in one repository authorise
/// branch deletion and worktree removal in another. Fails on round 1, where the
/// two were never compared and the deletions ran.
#[tokio::test]
async fn cleanup_refuses_when_the_registry_repo_and_checkout_disagree() {
    let gh = gh_merged();
    let git = Scripted::new()
        .on(
            ORIGIN_QUERY,
            "https://github.com/someone-else/other-repo.git\n",
        )
        .on("git ls-remote", "");
    let claims = FakeClaims::none();
    let report = run(
        &gh,
        &git,
        &claims,
        &FakeLanding::nothing_merged(),
        &clean,
        &req(false),
    )
    .await;

    assert!(report.failed(), "{}", report.render());
    let rendered = report.render();
    assert!(rendered.contains("someone-else/other-repo"), "{rendered}");
    assert!(rendered.contains("bobmatnyc/trusty-tools"), "{rendered}");
    let joined = git.calls().join("\n");
    assert!(
        !joined.contains("push origin --delete") && !joined.contains("branch -D"),
        "nothing destructive may run against a repository this cleanup is not for: {joined}"
    );
}

/// #7275 round 2: an `origin` that cannot be read is undeterminable, so the
/// destructive steps are refused rather than run on an unverified checkout.
#[tokio::test]
async fn cleanup_refuses_when_the_checkout_origin_cannot_be_read() {
    let gh = gh_merged();
    let git = Scripted::new()
        .on_fail(ORIGIN_QUERY, "not a git repository")
        .on("git ls-remote", "");
    let claims = FakeClaims::none();
    let report = run(
        &gh,
        &git,
        &claims,
        &FakeLanding::nothing_merged(),
        &clean,
        &req(false),
    )
    .await;

    assert!(report.failed(), "{}", report.render());
    assert!(
        report.render().contains("could not be read"),
        "{}",
        report.render()
    );
}

#[tokio::test]
async fn cleanup_clean_path_with_the_remote_branch_already_gone() {
    let gh = gh_merged();
    let git = Scripted::new()
        .on(ORIGIN_QUERY, ORIGIN_URL)
        .on("git ls-remote", "")
        .on("git worktree list", &worktree_listing())
        .on("git worktree remove", "")
        .on("git branch --format", &branch_listing())
        .on("git branch -D", "")
        .on("git worktree prune", "")
        .on("git fetch --prune", "");
    let claims = FakeClaims::none();
    let report = run(
        &gh,
        &git,
        &claims,
        &FakeLanding::nothing_merged(),
        &clean,
        &req(false),
    )
    .await;

    assert!(!report.failed(), "{}", report.render());
    assert!(
        !git.calls()
            .iter()
            .any(|c| c.contains("push origin --delete")),
        "an absent remote branch is a completed step, not a delete: {:?}",
        git.calls()
    );
    assert!(
        rendered(&report.lines).contains("already gone"),
        "{}",
        report.render()
    );
}

#[tokio::test]
async fn cleanup_refuses_a_dirty_worktree() {
    let gh = gh_merged();
    let git = git_full();
    let claims = FakeClaims::none();
    let dirty = |p: &Path| {
        Some(DirtyWorktree {
            path: p.to_path_buf(),
            reason: "3 uncommitted/untracked file(s), 1 unpushed commit(s)".to_string(),
            dirty_files: 3,
            unpushed_commits: 1,
        })
    };
    let report = run(
        &gh,
        &git,
        &claims,
        &FakeLanding::nothing_merged(),
        &dirty,
        &req(false),
    )
    .await;

    assert!(report.failed(), "{}", report.render());
    assert!(
        !git.calls().iter().any(|c| c.contains("worktree remove")),
        "a dirty tree is never removed: {:?}",
        git.calls()
    );
    assert!(
        rendered(&report.lines).contains("unsaved work"),
        "{}",
        report.render()
    );
}

#[tokio::test]
async fn cleanup_ends_a_session_claim_before_removing_the_worktree() {
    let gh = gh_merged();
    let git = git_full();
    let claims = FakeClaims::held_by("tm-bobmatnyc-01");
    let report = run(
        &gh,
        &git,
        &claims,
        &FakeLanding::nothing_merged(),
        &clean,
        &req(false),
    )
    .await;

    assert!(!report.failed(), "{}", report.render());
    assert_eq!(
        claims.ended.lock().expect("claims lock").as_slice(),
        ["tm-bobmatnyc-01"],
        "a merged PR makes the claim obsolete; cleanup ends it and proceeds"
    );
    assert!(
        git.calls().iter().any(|c| c.contains("worktree remove")),
        "the claim must not block the removal: {:?}",
        git.calls()
    );
}

#[tokio::test]
async fn cleanup_fails_the_worktree_step_when_claims_cannot_be_read() {
    let gh = gh_merged();
    let git = git_full();
    let claims = super::UnavailableClaims::new("the daemon is not reachable");
    let report = run(
        &gh,
        &git,
        &claims,
        &FakeLanding::nothing_merged(),
        &clean,
        &req(false),
    )
    .await;

    assert!(report.failed(), "{}", report.render());
    assert!(
        !git.calls().iter().any(|c| c.contains("worktree remove")),
        "an unanswerable claim question must not advance toward a delete: {:?}",
        git.calls()
    );
}

#[tokio::test]
async fn cleanup_dry_run_makes_no_mutating_call() {
    let gh = gh_merged();
    let git = git_full();
    let claims = FakeClaims::held_by("tm-bobmatnyc-01");
    let report = run(
        &gh,
        &git,
        &claims,
        &FakeLanding::nothing_merged(),
        &clean,
        &req(true),
    )
    .await;

    assert!(!report.failed(), "{}", report.render());
    for call in git.calls() {
        assert!(
            !(call.contains("push origin --delete")
                || call.contains("worktree remove")
                || call.contains("branch -D")
                || call.contains("worktree prune")
                || call.contains("fetch --prune")),
            "--dry-run issued a mutating command: {call}"
        );
    }
    assert!(
        claims.ended.lock().expect("claims lock").is_empty(),
        "--dry-run must not end a claim"
    );
    let text = report.render();
    assert!(text.contains("would delete"), "{text}");
    assert!(text.contains("would remove"), "{text}");
}

// ── round-N siblings ─────────────────────────────────────────────────────

#[test]
fn strip_round_suffix_removes_only_a_numbered_round() {
    use super::plan::strip_round_suffix;
    assert_eq!(strip_round_suffix("fix/7232-thing-r2"), "fix/7232-thing");
    assert_eq!(strip_round_suffix("fix/7232-thing-r13"), "fix/7232-thing");
    assert_eq!(strip_round_suffix("fix/7232-thing"), "fix/7232-thing");
    assert_eq!(
        strip_round_suffix("feat/add-r-flag"),
        "feat/add-r-flag",
        "`-r` followed by letters is part of the name, not a round"
    );
    assert_eq!(
        strip_round_suffix("-r2"),
        "-r2",
        "a bare suffix is the name"
    );
}

#[test]
fn is_pr_branch_relates_round_siblings() {
    use super::plan::is_pr_branch;
    // The PR merged under the unsuffixed name; `-r2` is its sibling.
    assert!(is_pr_branch("fix/7232-thing", "fix/7232-thing-r2"));
    // …and the reverse: the PR opened from `-r2`, round 1 never had one.
    assert!(is_pr_branch("fix/7232-thing-r2", "fix/7232-thing"));
    // …and two rounds of the same work.
    assert!(is_pr_branch("fix/7232-thing-r2", "fix/7232-thing-r3"));
    assert!(is_pr_branch("fix/7232-thing", "fix/7232-thing"));
}

#[test]
fn is_pr_branch_rejects_an_unrelated_branch() {
    use super::plan::is_pr_branch;
    assert!(!is_pr_branch("fix/7232-thing", "fix/7233-other"));
    assert!(!is_pr_branch("fix/7232-thing", "main"));
    assert!(!is_pr_branch("", "fix/7232-thing"));
    assert!(!is_pr_branch("fix/7232-thing", ""));
}

#[test]
fn worktree_targets_includes_a_round_sibling() {
    let listing = format!(
        "worktree /repo\nHEAD 1111111111111111111111111111111111111111\nbranch refs/heads/main\n\n\
         worktree /repo/wt-r2\nHEAD 3333333333333333333333333333333333333333\n\
         branch refs/heads/{BRANCH}-r2\n\n"
    );
    let entries = parse_worktree_list(&listing);
    let hits = worktree_targets(&entries, BRANCH, HEAD_OID, &root());
    assert_eq!(
        hits.iter().map(|e| e.path.clone()).collect::<Vec<_>>(),
        vec![PathBuf::from("/repo/wt-r2")],
        "a `-r2` tree carries no merged PR of its own and nothing else reclaims it"
    );
}

#[test]
fn pr_branches_lists_head_then_siblings() {
    use super::plan::pr_branches;
    let listing = format!("main 1111\n{BRANCH}-r2 2222\n{BRANCH} {HEAD_OID}\nfix/other-r2 3333\n");
    assert_eq!(
        pr_branches(&listing, BRANCH),
        vec![BRANCH.to_string(), format!("{BRANCH}-r2")],
        "the head is deleted first, then its siblings"
    );
}

#[test]
fn pr_branches_ignores_an_unrelated_branch() {
    use super::plan::pr_branches;
    assert!(pr_branches("main 1111\nfix/other 2222\n", BRANCH).is_empty());
}

/// The round-N sibling `git` fake: the merge into the base is a no-op.
fn git_with_sibling(landed: bool) -> Scripted {
    let listing = format!(
        "worktree /repo\nHEAD 1111111111111111111111111111111111111111\nbranch refs/heads/main\n\n\
         worktree {TREE}\nHEAD {HEAD_OID}\nbranch refs/heads/{BRANCH}\n\n\
         worktree /repo/wt-r2\nHEAD 3333333333333333333333333333333333333333\n\
         branch refs/heads/{BRANCH}-r2\n\n"
    );
    let branches = format!("main 1111\n{BRANCH} {HEAD_OID}\n{BRANCH}-r2 3333\n");
    let s = Scripted::new()
        .on(ORIGIN_QUERY, ORIGIN_URL)
        .on("git ls-remote", "")
        // The sibling's tip is NOT an ancestor of the merged head, so the
        // merge-tree test is what decides.
        .on_fail("git merge-base --is-ancestor", "not an ancestor")
        // #7275 round 4: the base ref is refreshed once, before the first
        // merge-tree comparison that reads it.
        .on("git fetch origin", "")
        .on("git merge-tree --write-tree", "aaaabbbbccccdddd\n")
        .on("git worktree list", &listing)
        .on("git worktree remove", "")
        .on("git branch --format", &branches)
        .on("git branch -D", "")
        .on("git worktree prune", "")
        .on("git fetch --prune", "");
    if landed {
        s.on("git diff --name-only", "")
    } else {
        s.on(
            "git diff --name-only",
            "crates/a/src/lib.rs\ncrates/a/src/x.rs\n",
        )
    }
}

/// REGRESSION (#7275): a `-r2` sibling whose content the squash carried is
/// reclaimed along with the head — nothing else in the delivery chain deletes
/// it, and the ADR-0057 guard refused two such trees on 2026-09-09.
#[tokio::test]
async fn cleanup_removes_a_round_sibling_whose_content_landed() {
    let gh = gh_merged();
    let git = git_with_sibling(true);
    let claims = FakeClaims::none();
    let report = run(
        &gh,
        &git,
        &claims,
        &FakeLanding::nothing_merged(),
        &clean,
        &req(false),
    )
    .await;

    assert!(!report.failed(), "{}", report.render());
    let joined = git.calls().join("\n");
    assert!(
        joined.contains("git worktree remove /repo/wt-r2"),
        "the sibling tree must be removed: {joined}"
    );
    assert!(
        joined.contains(&format!("git branch -D {BRANCH}-r2")),
        "and its branch with it: {joined}"
    );
    assert!(!joined.contains("--force"), "still never forced: {joined}");
}

/// REGRESSION (#7275): a sibling holding work the merge did not carry is
/// refused, and the refusal NAMES the residue files so a person can look.
#[tokio::test]
async fn cleanup_refuses_a_sibling_whose_content_is_not_on_the_base() {
    let gh = gh_merged();
    let git = git_with_sibling(false);
    let claims = FakeClaims::none();
    let report = run(
        &gh,
        &git,
        &claims,
        &FakeLanding::nothing_merged(),
        &clean,
        &req(false),
    )
    .await;

    assert!(report.failed(), "{}", report.render());
    let text = report.render();
    assert!(text.contains("crates/a/src/lib.rs"), "{text}");
    assert!(text.contains("2 files"), "{text}");
    assert!(
        !git.calls()
            .iter()
            .any(|c| c.contains("worktree remove /repo/wt-r2")),
        "the sibling must not be removed: {:?}",
        git.calls()
    );
}

/// REGRESSION (#7275): a round-1 branch that never had a pull request of its
/// own — superseded by the `-r2` that opened the PR — is reclaimed when its tip
/// is an ancestor of the merged head, without a merge-tree round trip.
#[tokio::test]
async fn cleanup_removes_a_round_one_branch_that_never_had_its_own_pr() {
    let head_r2 = format!("{BRANCH}-r2");
    let gh = Scripted::new().on(
        "gh pr view 7275",
        &format!(
            "{{\"state\":\"MERGED\",\"headRefName\":\"{head_r2}\",\"headRefOid\":\"{HEAD_OID}\",\
             \"baseRefName\":\"main\"}}"
        ),
    );
    let listing = format!(
        "worktree /repo\nHEAD 1111\nbranch refs/heads/main\n\n\
         worktree /repo/wt-r1\nHEAD 4444\nbranch refs/heads/{BRANCH}\n\n"
    );
    let git = Scripted::new()
        .on(ORIGIN_QUERY, ORIGIN_URL)
        .on("git ls-remote", "")
        .on("git merge-base --is-ancestor", "")
        .on("git worktree list", &listing)
        .on("git worktree remove", "")
        .on(
            "git branch --format",
            &format!("main 1111\n{BRANCH} 4444\n"),
        )
        .on("git branch -D", "")
        .on("git worktree prune", "")
        .on("git fetch --prune", "");
    let claims = FakeClaims::none();
    let report = run(
        &gh,
        &git,
        &claims,
        &FakeLanding::nothing_merged(),
        &clean,
        &req(false),
    )
    .await;

    assert!(!report.failed(), "{}", report.render());
    let joined = git.calls().join("\n");
    assert!(
        joined.contains("git worktree remove /repo/wt-r1"),
        "the round-1 tree has no PR of its own and nothing else reclaims it: {joined}"
    );
    assert!(
        joined.contains(&format!("git branch -D {BRANCH}")),
        "and its branch with it: {joined}"
    );
    assert!(
        !joined.contains("merge-tree"),
        "an ancestor tip needs no merge test: {joined}"
    );
}

// ── the ahead-count gate (#7275 round 3) ─────────────────────────────────

/// A commit that is not the merged head — a sibling branch's own tip.
const SIB_OID: &str = "5555555555555555555555555555555555555555";

/// The squash-merge shape: `origin/<head>` is already gone, the tip is an
/// ancestor of nothing, and the merge into the base is or is not a no-op.
///
/// `merge-base --is-ancestor` FAILS on purpose, so every test below reaches the
/// `git merge-tree --write-tree` comparison the owner's round-3 brief names
/// rather than the ancestor shortcut.
fn git_squash(listing: &str, branches: &str, landed: bool) -> Scripted {
    let s = Scripted::new()
        .on(ORIGIN_QUERY, ORIGIN_URL)
        .on("git ls-remote", "")
        .on_fail("git merge-base --is-ancestor", "not an ancestor")
        // #7275 round 4: the base ref is refreshed once, before the first
        // merge-tree comparison that reads it.
        .on("git fetch origin", "")
        .on("git merge-tree --write-tree", "aaaabbbbccccdddd\n")
        .on("git worktree list", listing)
        .on("git worktree remove", "")
        .on("git branch --format", branches)
        .on("git branch -D", "")
        .on("git worktree prune", "")
        .on("git fetch --prune", "");
    if landed {
        s.on("git diff --name-only", "")
    } else {
        s.on("git diff --name-only", "crates/a/src/lib.rs\n")
    }
}

/// 🔴 REGRESSION (#7275 round 3): a squash-merged worktree is landed and
/// removable, however many commits it is "ahead" by.
///
/// Why: a squash merge puts the branch's content on `main` as a NEW commit, so
/// nothing the branch holds is an ancestor of `main` and `inspect_dirt` counts
/// every one of its commits as unpushed. Seven worktrees were refused that way
/// on 2026-09-09 — the exact population cleanup exists to reclaim. FAILS on
/// origin/main, where the count alone refused.
#[tokio::test]
async fn cleanup_removes_a_squash_merged_worktree() {
    let gh = gh_merged();
    let git = git_squash(&worktree_listing(), &branch_listing(), true);
    let claims = FakeClaims::none();
    let probe = ahead_by(2);
    let report = run(
        &gh,
        &git,
        &claims,
        &FakeLanding::nothing_merged(),
        &probe,
        &req(false),
    )
    .await;

    assert!(!report.failed(), "{}", report.render());
    let joined = git.calls().join("\n");
    assert!(
        joined.contains(&format!("git worktree remove {TREE}")),
        "a squash-merged tree must be removed, not refused for being ahead: {joined}"
    );
    assert!(
        joined.contains("git merge-tree --write-tree origin/main"),
        "the merge-tree no-op is what proves it landed: {joined}"
    );
    assert!(!joined.contains("--force"), "still never forced: {joined}");
    assert!(
        report.render().contains("landed"),
        "the line must say the ahead-count was accounted for: {}",
        report.render()
    );
}

/// 🔴 REGRESSION (#7275 round 3): a branch with a merged pull request whose
/// merge-tree still ADDS content is refused, and the refusal names what.
///
/// Why: the merged pull request proves the branch's NAME landed, not that this
/// checkout holds only what landed. Without the merge-tree half, a tree whose
/// branch gained commits after the merge would be removed with them.
#[tokio::test]
async fn cleanup_refuses_a_squash_merged_tree_that_moved_on() {
    let gh = gh_merged();
    let git = git_squash(&worktree_listing(), &branch_listing(), false);
    let claims = FakeClaims::none();
    let probe = ahead_by(3);
    let report = run(
        &gh,
        &git,
        &claims,
        &FakeLanding::nothing_merged(),
        &probe,
        &req(false),
    )
    .await;

    assert!(report.failed(), "{}", report.render());
    let text = report.render();
    assert!(
        text.contains("crates/a/src/lib.rs"),
        "the refusal must name the divergence: {text}"
    );
    assert!(
        text.contains("3 unpushed commit(s)"),
        "and still report what the probe saw: {text}"
    );
    assert!(
        !git.calls()
            .iter()
            .any(|c| c.contains(&format!("worktree remove {TREE}"))),
        "a tree that moved on past the merge is never removed: {:?}",
        git.calls()
    );
}

/// 🔴 REGRESSION (#7275 round 3): an ahead-of-upstream tree that NO merged pull
/// request matches is refused.
///
/// Why: the ahead count stops being a refusal only because a merge accounts for
/// it. With no merged pull request under the branch name, its round stem or its
/// head commit, nothing does — and today's refusal must stand.
#[tokio::test]
async fn cleanup_refuses_an_ahead_tree_with_no_merged_pr() {
    let gh = gh_merged();
    // The tree carries the PR's branch NAME but sits on a different commit, so
    // step 1's own proof does not settle it and the matcher is asked.
    let listing = format!(
        "worktree /repo\nHEAD 1111\nbranch refs/heads/main\n\n\
         worktree {TREE}\nHEAD {SIB_OID}\nbranch refs/heads/{BRANCH}\n\n"
    );
    let git = git_squash(&listing, &format!("main 1111\n{BRANCH} {SIB_OID}\n"), true);
    let claims = FakeClaims::none();
    let probe = ahead_by(1);
    let report = run(
        &gh,
        &git,
        &claims,
        &FakeLanding::nothing_merged(),
        &probe,
        &req(false),
    )
    .await;

    assert!(report.failed(), "{}", report.render());
    let text = report.render();
    assert!(
        text.contains("no MERGED pull request matches"),
        "the refusal must name the missing evidence: {text}"
    );
    assert!(
        !git.calls().iter().any(|c| c.contains("worktree remove")),
        "no merged pull request means no removal: {:?}",
        git.calls()
    );
}

/// REGRESSION (#7275 round 3): a round sibling reaches its merged pull request
/// through the #7267 round-stem rung, not through its own name.
#[tokio::test]
async fn cleanup_removes_a_squash_merged_round_sibling() {
    let gh = gh_merged();
    let listing = format!(
        "worktree /repo\nHEAD 1111\nbranch refs/heads/main\n\n\
         worktree /repo/wt-r2\nHEAD {SIB_OID}\nbranch refs/heads/{BRANCH}-r2\n\n"
    );
    let branches = format!("main 1111\n{BRANCH}-r2 {SIB_OID}\n");
    let git = git_squash(&listing, &branches, true);
    let claims = FakeClaims::none();
    let probe = ahead_by(1);
    let report = run(
        &gh,
        &git,
        &claims,
        &FakeLanding::merged(BRANCH, 7275),
        &probe,
        &req(false),
    )
    .await;

    assert!(!report.failed(), "{}", report.render());
    assert!(
        git.calls()
            .iter()
            .any(|c| c.contains("worktree remove /repo/wt-r2")),
        "the stem's merged pull request vouches for the sibling: {:?}",
        git.calls()
    );
}

/// 🔴 FAIL-CLOSED (#7275 round 3): a `git merge-tree` that cannot run leaves
/// the refusal standing.
///
/// Why: the gate's grant deletes a checkout, so an unanswerable question must
/// never read as "landed" (ADR-0045).
#[tokio::test]
async fn cleanup_refuses_when_the_merge_test_cannot_run() {
    let gh = gh_merged();
    let git = Scripted::new()
        .on(ORIGIN_QUERY, ORIGIN_URL)
        .on("git ls-remote", "")
        .on_fail("git merge-base --is-ancestor", "not an ancestor")
        .on("git fetch origin", "")
        .on_fail("git merge-tree --write-tree", "fatal: not a valid object")
        .on("git worktree list", &worktree_listing())
        .on("git worktree remove", "")
        .on("git branch --format", &branch_listing())
        .on("git branch -D", "")
        .on("git worktree prune", "")
        .on("git fetch --prune", "");
    let claims = FakeClaims::none();
    let probe = ahead_by(1);
    let report = run(
        &gh,
        &git,
        &claims,
        &FakeLanding::nothing_merged(),
        &probe,
        &req(false),
    )
    .await;

    assert!(report.failed(), "{}", report.render());
    assert!(
        !git.calls().iter().any(|c| c.contains("worktree remove")),
        "a failed merge test must not advance toward a delete: {:?}",
        git.calls()
    );
}

/// 🔴 FAIL-CLOSED (#7275 round 3): an `inspect_dirt` that could not read the
/// tree — both counts zero, its error arm — still refuses.
#[tokio::test]
async fn cleanup_refuses_an_unreadable_worktree() {
    let gh = gh_merged();
    let git = git_full();
    let claims = FakeClaims::none();
    let unreadable = |p: &Path| {
        Some(DirtyWorktree {
            path: p.to_path_buf(),
            reason: "`git status` could not be run".to_string(),
            dirty_files: 0,
            unpushed_commits: 0,
        })
    };
    let report = run(
        &gh,
        &git,
        &claims,
        &FakeLanding::nothing_merged(),
        &unreadable,
        &req(false),
    )
    .await;

    assert!(report.failed(), "{}", report.render());
    assert!(
        report.render().contains("unsaved work"),
        "an unreadable tree is dirty, not ahead: {}",
        report.render()
    );
    assert!(
        !git.calls().iter().any(|c| c.contains("worktree remove")),
        "{:?}",
        git.calls()
    );
}

// ── the base ref and the removal window (#7275 round 4) ──────────────────

/// A `git` fake whose `origin/main` is STALE until a targeted fetch lands.
///
/// Why: this is the shape of a real checkout the moment after a merge. The
/// squash commit is on GitHub and not in the local ref store, so the merge test
/// diffs the branch against a base that predates it and NAMES every file the
/// branch touched as residue. Only a fetch of that ref makes the same
/// comparison empty.
struct StaleBase {
    /// The routes for everything the run does besides the base comparison.
    inner: Scripted,
    /// Set once `git fetch origin <base>` has run.
    fetched: RefCell<bool>,
}

impl StaleBase {
    fn new(inner: Scripted) -> Self {
        Self {
            inner,
            fetched: RefCell::new(false),
        }
    }
}

impl Git for StaleBase {
    fn run(&self, dir: &Path, args: &[String]) -> anyhow::Result<CmdOut> {
        let joined = args.join(" ");
        if joined.starts_with("fetch origin ") {
            *self.fetched.borrow_mut() = true;
        }
        if joined.starts_with("diff --name-only") && !*self.fetched.borrow() {
            self.inner.seen.borrow_mut().push(format!("git {joined}"));
            return Ok(CmdOut {
                success: true,
                stdout: "crates/a/src/lib.rs\n".to_string(),
                stderr: String::new(),
            });
        }
        Git::run(&self.inner, dir, args)
    }
}

/// 🔴 REGRESSION (#7275 round 4): a tree whose `origin/<base>` had not yet been
/// fetched is reclaimed in ONE pass, not the pass after next.
///
/// Why: the merge test compares against the local `origin/<base>`, and the only
/// fetch in the run is step 5's — which runs AFTER the worktree step. So the
/// first cleanup following a merge compared against a base predating it, found
/// the branch's content missing, and refused a tree that had genuinely landed;
/// under `--auto` that is a whole sweep interval of delay, and by hand it looks
/// like a false refusal. FAILS on round 3, where no fetch precedes the compare.
#[tokio::test]
async fn cleanup_reclaims_a_tree_whose_base_ref_was_stale() {
    let gh = gh_merged();
    let git = StaleBase::new(git_squash(&worktree_listing(), &branch_listing(), true));
    let claims = FakeClaims::none();
    let probe = ahead_by(2);
    let report = run(
        &gh,
        &git,
        &claims,
        &FakeLanding::nothing_merged(),
        &probe,
        &req(false),
    )
    .await;

    assert!(!report.failed(), "{}", report.render());
    let calls = git.inner.calls();
    let joined = calls.join("\n");
    assert!(
        joined.contains("git fetch origin main"),
        "the base ref must be refreshed before it is compared against: {joined}"
    );
    let fetch_at = calls
        .iter()
        .position(|c| c.contains("fetch origin main"))
        .expect("the fetch must have run");
    let diff_at = calls
        .iter()
        .position(|c| c.contains("diff --name-only"))
        .expect("the merge comparison must have run");
    assert!(
        fetch_at < diff_at,
        "the refresh must precede the comparison it feeds: {calls:?}"
    );
    assert!(
        joined.contains(&format!("git worktree remove {TREE}")),
        "one pass must reclaim it: {joined}"
    );
}

/// 🔴 FAIL-CLOSED (#7275 round 4): a base ref that cannot be refreshed refuses.
///
/// Why: the alternative is comparing against a ref that may predate the merge,
/// and that comparison's only two answers are "refuse" and "delete a checkout".
/// An unanswerable question must never take the second (ADR-0045).
#[tokio::test]
async fn cleanup_refuses_when_the_base_ref_cannot_be_refreshed() {
    let gh = gh_merged();
    // Every other route says landed; only the refresh fails.
    let git = Scripted::new()
        .on(ORIGIN_QUERY, ORIGIN_URL)
        .on("git ls-remote", "")
        .on_fail("git merge-base --is-ancestor", "not an ancestor")
        .on_fail(
            "git fetch origin",
            "fatal: could not read from remote repository",
        )
        .on("git merge-tree --write-tree", "aaaabbbbccccdddd\n")
        .on("git diff --name-only", "")
        .on("git worktree list", &worktree_listing())
        .on("git worktree remove", "")
        .on("git branch --format", &branch_listing())
        .on("git branch -D", "")
        .on("git worktree prune", "")
        .on("git fetch --prune", "");
    let claims = FakeClaims::none();
    let probe = ahead_by(1);
    let report = run(
        &gh,
        &git,
        &claims,
        &FakeLanding::nothing_merged(),
        &probe,
        &req(false),
    )
    .await;

    assert!(report.failed(), "{}", report.render());
    let text = report.render();
    assert!(
        text.contains("could not read from remote repository"),
        "the refusal must name what failed: {text}"
    );
    assert!(
        !git.calls().iter().any(|c| c.contains("worktree remove")),
        "a base ref that may predate the merge must not authorise a delete: {:?}",
        git.calls()
    );
}

/// A dirt probe that answers once, then differently — an agent that wrote in
/// the window between cleanup's two reads.
fn dirt_then(later: Option<DirtyWorktree>) -> impl Fn(&Path) -> Option<DirtyWorktree> {
    let calls = std::cell::Cell::new(0usize);
    move |p: &Path| {
        let n = calls.get();
        calls.set(n + 1);
        if n == 0 {
            return None;
        }
        later.clone().map(|mut d| {
            d.path = p.to_path_buf();
            d
        })
    }
}

/// 🔴 REGRESSION (#7275 round 4): a tree that gains unsaved work WHILE cleanup
/// is checking it is refused, not removed on the stale first reading.
///
/// Why: the first probe and the removal are separated by the merged-pull-request
/// lookup, the merge-tree comparison, the claim read and the claim tombstone —
/// several `gh` and `git` round trips. Ending a claim is record-only, so the
/// agent that held it keeps writing; on round 3 the removal ran on an answer
/// taken before all of that. FAILS on round 3, where the tree is removed with
/// the file in it.
#[tokio::test]
async fn cleanup_refuses_a_tree_that_changed_while_cleanup_was_checking_it() {
    let gh = gh_merged();
    let git = git_full();
    let claims = FakeClaims::held_by("tm-bobmatnyc-01");
    let probe = dirt_then(Some(DirtyWorktree {
        path: PathBuf::from(TREE),
        reason: "1 uncommitted/untracked file(s), 0 unpushed commit(s)".to_string(),
        dirty_files: 1,
        unpushed_commits: 0,
    }));
    let report = run(
        &gh,
        &git,
        &claims,
        &FakeLanding::nothing_merged(),
        &probe,
        &req(false),
    )
    .await;

    assert!(report.failed(), "{}", report.render());
    let text = report.render();
    assert!(
        text.contains("changed while cleanup was checking it"),
        "the refusal must say the reading went stale: {text}"
    );
    assert!(
        text.contains("uncommitted/untracked files went from 0 to 1"),
        "and name what changed: {text}"
    );
    assert!(
        !git.calls().iter().any(|c| c.contains("worktree remove")),
        "work written during the window is never deleted: {:?}",
        git.calls()
    );
}

// ── the registry ─────────────────────────────────────────────────────────

fn entry(pr: u64, cleaned: bool) -> OpenedPr {
    OpenedPr {
        pr,
        repo: "bobmatnyc/trusty-tools".to_string(),
        repo_root: root(),
        opened_at: chrono::Utc::now(),
        cleaned_at: cleaned.then(chrono::Utc::now),
    }
}

#[test]
fn registry_round_trips_an_entry() {
    let dir = tempfile::tempdir().expect("tempdir");
    let reg = CleanupRegistry::under_root(dir.path());
    reg.record_open(entry(7275, false)).expect("record");
    let back = reg.entries();
    assert_eq!(back.len(), 1);
    assert_eq!(back[0].pr, 7275);
    assert!(back[0].pending());
}

#[test]
fn registry_record_open_is_idempotent() {
    let dir = tempfile::tempdir().expect("tempdir");
    let reg = CleanupRegistry::under_root(dir.path());
    reg.record_open(entry(7275, false)).expect("first");
    reg.record_open(entry(7275, false)).expect("second");
    assert_eq!(
        reg.entries().len(),
        1,
        "recording the same PR twice would clean it twice"
    );
}

#[test]
fn registry_pending_excludes_a_cleaned_entry() {
    let dir = tempfile::tempdir().expect("tempdir");
    let reg = CleanupRegistry::under_root(dir.path());
    reg.record_open(entry(7275, false)).expect("pending");
    reg.record_open(entry(7276, true)).expect("cleaned");
    let pending: Vec<u64> = reg.pending().into_iter().map(|e| e.pr).collect();
    assert_eq!(pending, vec![7275]);
}

#[test]
fn registry_mark_cleaned_stamps_the_entry() {
    let dir = tempfile::tempdir().expect("tempdir");
    let reg = CleanupRegistry::under_root(dir.path());
    reg.record_open(entry(7275, false)).expect("record");
    reg.mark_cleaned("bobmatnyc/trusty-tools", 7275, chrono::Utc::now())
        .expect("stamp");
    assert!(
        reg.pending().is_empty(),
        "a stamped entry never sweeps again"
    );
}

#[test]
fn registry_mark_cleaned_ignores_an_unknown_pr() {
    let dir = tempfile::tempdir().expect("tempdir");
    let reg = CleanupRegistry::under_root(dir.path());
    reg.mark_cleaned("bobmatnyc/trusty-tools", 999, chrono::Utc::now())
        .expect("stamping an unknown PR is a no-op, not an error");
    assert!(reg.entries().is_empty());
}

#[test]
fn registry_unreadable_file_reads_as_empty() {
    let dir = tempfile::tempdir().expect("tempdir");
    let reg = CleanupRegistry::under_root(dir.path());
    std::fs::write(reg.path(), "{ this is not json").expect("write garbage");
    assert!(
        reg.entries().is_empty(),
        "a corrupt registry makes the sweep idle, never guess"
    );
}

// ── #7185: the harness marker git counts and this engine's gate does not ─────

/// 🔴 REGRESSION (#7185, recurrence 2026-09-11): the worktree step clears the
/// harness ownership marker before it removes the tree.
///
/// Why: `count_dirty_files` excuses `.trusty-mpm-worktree` (it is the harness's
/// marker, not the agent's work), so the dirt gate above authorises the
/// removal — but `git worktree remove` runs git's OWN clean check, which counts
/// every untracked entry. Verified against git 2.54.0: a worktree whose only
/// untracked file is that marker is refused with `contains modified or
/// untracked files, use --force to delete it`. The two answers agree only where
/// the project gitignores the marker (this repo does; the project that hit the
/// recurrence does not), so `tm pr merge`'s cleanup left EVERY merged worktree
/// on disk there. Fails on 4cc327dbc, where nothing clears the marker.
/// What this does NOT assert: `--force`. Forcing would override the dirt gate
/// itself, which is the one thing standing between this step and unsaved work.
#[tokio::test]
async fn cleanup_clears_the_harness_marker_before_removing_the_tree() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let tree = tmp.path().join("agent-aa11");
    std::fs::create_dir_all(&tree).expect("tree");
    let marker = tree.join(crate::session_manager::decommission::WORKTREE_SENTINEL_FILE);
    std::fs::write(&marker, b"{}").expect("marker");
    let kept = tree.join("notes.md");
    std::fs::write(&kept, "left alone\n").expect("sibling");
    let shown = tree.display().to_string();

    let listing = format!(
        "worktree /repo\nHEAD 1111111111111111111111111111111111111111\n\
         branch refs/heads/main\n\n\
         worktree {shown}\nHEAD {HEAD_OID}\nbranch refs/heads/{BRANCH}\n\n"
    );
    let git = Scripted::new()
        .on(ORIGIN_QUERY, ORIGIN_URL)
        .on(
            "git ls-remote",
            &format!("{HEAD_OID}\trefs/heads/{BRANCH}\n"),
        )
        .on("git push origin --delete", "")
        .on("git worktree list", &listing)
        .on("git worktree remove", "")
        .on("git branch --format", &branch_listing())
        .on("git branch -D", "")
        .on("git worktree prune", "")
        .on("git fetch --prune", "");

    let report = run(
        &gh_merged(),
        &git,
        &FakeClaims::none(),
        &FakeLanding::nothing_merged(),
        &clean,
        &req(false),
    )
    .await;

    assert!(!report.failed(), "{}", report.render());
    assert!(
        !marker.exists(),
        "the harness marker must be cleared before the removal, or git refuses it"
    );
    assert!(
        kept.exists(),
        "clearing the marker must touch nothing else in the tree"
    );
    let joined = git.calls().join("\n");
    assert!(
        joined.contains(&format!("git worktree remove {shown}")),
        "the tree must still be removed: {joined}"
    );
    assert!(
        !joined.contains("--force"),
        "clearing the marker is not licence to force: {joined}"
    );
}

/// 🔴 REGRESSION (#7511 review, MEDIUM 1): a removal that fails AFTER the clear
/// puts the marker back.
///
/// Why: taking the marker is safe only because the removal that follows deletes
/// the directory. When the removal fails the tree survives, and without the
/// marker it is unattributed — `agent_ownership_blocks` refuses it and
/// `prune_orphaned_worktrees` reports it `owner_unknown`, so it is stranded
/// rather than lost, which is the #7185 symptom in a rarer branch. The failure
/// is forced by leaving `git worktree remove` unrouted on the fake, which is
/// what `Scripted` turns into an `Err` — the same arm a locked worktree or a
/// permission error reaches in production.
#[tokio::test]
async fn cleanup_restores_the_harness_marker_when_the_removal_fails() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let tree = tmp.path().join("agent-aa11");
    std::fs::create_dir_all(&tree).expect("tree");
    let marker = tree.join(crate::session_manager::decommission::WORKTREE_SENTINEL_FILE);
    // Real sentinel bytes, so the restore is proved faithful and not merely present.
    let original = br#"{"owner_session_id":"s-1","created_at":"2026-09-11T00:00:00Z"}"#;
    std::fs::write(&marker, original).expect("marker");
    let shown = tree.display().to_string();

    let listing = format!(
        "worktree /repo\nHEAD 1111111111111111111111111111111111111111\n\
         branch refs/heads/main\n\n\
         worktree {shown}\nHEAD {HEAD_OID}\nbranch refs/heads/{BRANCH}\n\n"
    );
    // Every route the run needs EXCEPT `worktree remove`, which therefore errors.
    let git = Scripted::new()
        .on(ORIGIN_QUERY, ORIGIN_URL)
        .on(
            "git ls-remote",
            &format!("{HEAD_OID}\trefs/heads/{BRANCH}\n"),
        )
        .on("git push origin --delete", "")
        .on("git worktree list", &listing)
        .on("git branch --format", &branch_listing())
        .on("git branch -D", "")
        .on("git worktree prune", "")
        .on("git fetch --prune", "");

    let report = run(
        &gh_merged(),
        &git,
        &FakeClaims::none(),
        &FakeLanding::nothing_merged(),
        &clean,
        &req(false),
    )
    .await;

    assert!(
        report.failed(),
        "an unroutable removal must fail the step: {}",
        report.render()
    );
    assert!(
        marker.exists(),
        "a removal that did not happen must leave the tree attributed"
    );
    assert_eq!(
        std::fs::read(&marker).expect("read restored marker"),
        original,
        "the restore must write back the ORIGINAL bytes, not a re-serialised payload"
    );
    assert!(
        !report.render().contains("could not be restored"),
        "a successful restore must add no note: {}",
        report.render()
    );
}

/// 🔴 A dry run inspects and reports; it must not touch the tree.
///
/// Why: `--dry-run` exists so an operator can see what cleanup would do. A
/// marker deleted by a preview is a write the preview promised not to make, and
/// the tree stays registered afterwards — so the next real run would find it
/// unmarked and owner-unknown.
#[tokio::test]
async fn a_dry_run_leaves_the_harness_marker_in_place() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let tree = tmp.path().join("agent-aa11");
    std::fs::create_dir_all(&tree).expect("tree");
    let marker = tree.join(crate::session_manager::decommission::WORKTREE_SENTINEL_FILE);
    std::fs::write(&marker, b"{}").expect("marker");
    let shown = tree.display().to_string();

    let listing = format!(
        "worktree /repo\nHEAD 1111111111111111111111111111111111111111\n\
         branch refs/heads/main\n\n\
         worktree {shown}\nHEAD {HEAD_OID}\nbranch refs/heads/{BRANCH}\n\n"
    );
    let git = Scripted::new()
        .on(ORIGIN_QUERY, ORIGIN_URL)
        .on(
            "git ls-remote",
            &format!("{HEAD_OID}\trefs/heads/{BRANCH}\n"),
        )
        .on("git worktree list", &listing)
        .on("git branch --format", &branch_listing());

    let report = run(
        &gh_merged(),
        &git,
        &FakeClaims::none(),
        &FakeLanding::nothing_merged(),
        &clean,
        &req(true),
    )
    .await;

    assert!(!report.failed(), "{}", report.render());
    assert!(
        marker.exists(),
        "a dry run must leave the tree exactly as it found it"
    );
}
