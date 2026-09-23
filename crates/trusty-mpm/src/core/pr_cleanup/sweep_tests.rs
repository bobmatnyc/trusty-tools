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
use crate::core::pr_cleanup::auth_backoff::{AuthBackoff, STRIKES};
use crate::core::pr_cleanup::driver::{ClaimEnder, CmdOut, Gh, Git, Landing};
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

/// A [`Landing`] that names no merged pull request.
///
/// Why: every sweep test here drives a CLEAN or a file-dirty worktree, and the
/// #7275 ahead-count gate only consults this seam for a tree whose sole finding
/// is that it is ahead of its upstream. Answering `None` keeps that unreachable
/// path fail-closed if one ever does.
struct NoLanding;

impl Landing for NoLanding {
    fn merged_pr(
        &self,
        _repo_root: &Path,
        _worktree: &Path,
        _branch: Option<&str>,
        _exact: Option<u64>,
    ) -> Option<u64> {
        None
    }
}

fn entry(pr: u64, cleaned: bool, root: &Path) -> OpenedPr {
    OpenedPr {
        pr,
        repo: REPO.to_string(),
        repo_root: root.to_path_buf(),
        opened_at: Utc::now(),
        cleaned_at: cleaned.then(Utc::now),
        scope: Default::default(),
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
        // #7275 round 2: the repo/checkout reconciliation runs before any
        // destructive step, so every git fake that reaches one answers it.
        .on(
            "config --get remote.origin.url",
            "https://github.com/bobmatnyc/trusty-tools.git\n",
        )
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
        // #7275 round 2: the repo/checkout reconciliation runs before any
        // destructive step, so every git fake that reaches one answers it.
        .on(
            "config --get remote.origin.url",
            "https://github.com/bobmatnyc/trusty-tools.git\n",
        )
        .on("git ls-remote", "")
        .on(
            "git worktree list",
            &format!(
                "worktree /repo\nHEAD 1111\nbranch refs/heads/main\n\n\
                 worktree {tree}\nHEAD {HEAD_OID}\nbranch refs/heads/{BRANCH}\n\n"
            ),
        )
        // #7275 round 4: the tree is re-read immediately before it is removed.
        .on("rev-parse HEAD", &format!("{HEAD_OID}\n"))
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
    let cleaned = run_sweep(
        &gh,
        &git,
        &NoClaims,
        &NoLanding,
        &clean,
        &reg,
        &AuthBackoff::new(),
    )
    .await;

    assert_eq!(cleaned, 1, "the newly merged PR is cleaned once");
    assert!(reg.pending().is_empty(), "and stamped so it never re-runs");

    // A second sweep against the same registry must do nothing at all.
    let gh2 = gh_merged();
    let git2 = git_nothing_left();
    let again = run_sweep(
        &gh2,
        &git2,
        &NoClaims,
        &NoLanding,
        &clean,
        &reg,
        &AuthBackoff::new(),
    )
    .await;
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
    let cleaned = run_sweep(
        &gh,
        &git,
        &NoClaims,
        &NoLanding,
        &dirty,
        &reg,
        &AuthBackoff::new(),
    )
    .await;

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
    let cleaned = run_sweep(
        &gh,
        &git,
        &NoClaims,
        &NoLanding,
        &clean,
        &reg,
        &AuthBackoff::new(),
    )
    .await;

    assert_eq!(cleaned, 0);
    assert_eq!(reg.pending().len(), 1, "an open PR stays pending");
    assert!(
        git.calls().is_empty(),
        "no git command runs for an unmerged PR: {:?}",
        git.calls()
    );
}

/// The #8058 regression: a `gh auth login` failure must stop the calls.
///
/// Why: before the backoff gate, an auth failure was a per-entry `warn!` and a
/// `continue`, so every pending entry spawned its own doomed `gh` on every
/// tick — the tight loop the 2026-09-15 log shows, whose process churn alone
/// degraded `/health`. The assertion is therefore on the CALL COUNT across
/// ticks, not on the log.
/// What: three pending entries, a `gh` that always fails the way `gh` fails
/// without a credential, and five ticks. Un-gated that is fifteen calls; gated
/// it is [`STRIKES`], after which the window holds for five minutes.
#[tokio::test]
async fn sweep_stops_calling_gh_after_repeated_auth_failures() {
    let dir = tempfile::tempdir().expect("tempdir");
    let reg = CleanupRegistry::under_root(dir.path());
    for pr in [7275u64, 7276, 7277] {
        reg.record_open(entry(pr, false, Path::new("/repo")))
            .expect("record");
    }

    // `Scripted` with no route bails with its own message, so this fake answers
    // the auth failure verbatim instead.
    struct AuthDenied {
        calls: RefCell<usize>,
    }
    impl Gh for AuthDenied {
        fn run(&self, _args: &[String]) -> anyhow::Result<CmdOut> {
            *self.calls.borrow_mut() += 1;
            Ok(CmdOut {
                success: false,
                stdout: String::new(),
                stderr: "To get started with GitHub CLI, please run: gh auth login.".to_string(),
            })
        }
    }

    let gh = AuthDenied {
        calls: RefCell::new(0),
    };
    let git = Scripted::new();
    let backoff = AuthBackoff::new();
    const TICKS: usize = 5;
    for _ in 0..TICKS {
        let cleaned = run_sweep(&gh, &git, &NoClaims, &NoLanding, &clean, &reg, &backoff).await;
        assert_eq!(cleaned, 0, "an auth failure cleans nothing");
    }

    let calls = *gh.calls.borrow();
    assert_eq!(
        calls,
        usize::try_from(STRIKES).expect("STRIKES fits usize"),
        "the sweep must stop calling `gh` after {STRIKES} auth failures; it made {calls} over \
         {TICKS} tick(s) against 3 pending entries"
    );
    assert_eq!(
        reg.pending().len(),
        3,
        "nothing is stamped on an auth failure"
    );

    let reason = backoff
        .degraded_reason()
        .expect("a suspended sweep publishes its reason to /health");
    assert!(reason.contains("gh auth login"), "{reason}");
}

/// The #8058 fail-open arm: a NON-auth failure must not suspend anything.
///
/// Why: the backoff is a failure branch, and its dangerous direction is the
/// opposite of the loop it fixes. `run_sweep` downgrades every `view_pr` error
/// to a `continue`, and only the auth arm may also take a strike — so an
/// over-broad [`is_auth_failure`], or the two arms in the wrong order, would
/// turn ONE stale registry row into a host-wide cleanup outage that `/health`
/// then reports as a deliberate suspension. That is strictly worse than the
/// tight loop, because it is silent and self-sustaining.
/// What: three pending entries, a `gh` that always fails the way a deleted pull
/// request fails, and two ticks. Every entry must still be asked on every tick
/// — six calls — with nothing stamped and nothing published to `/health`.
#[tokio::test]
async fn a_non_auth_failure_never_suspends_the_sweep() {
    let dir = tempfile::tempdir().expect("tempdir");
    let reg = CleanupRegistry::under_root(dir.path());
    for pr in [7275u64, 7276, 7277] {
        reg.record_open(entry(pr, false, Path::new("/repo")))
            .expect("record");
    }

    struct NotFound {
        calls: RefCell<usize>,
    }
    impl Gh for NotFound {
        fn run(&self, _args: &[String]) -> anyhow::Result<CmdOut> {
            *self.calls.borrow_mut() += 1;
            Ok(CmdOut {
                success: false,
                stdout: String::new(),
                stderr: "GraphQL: Could not resolve to a PullRequest with the number of 7275."
                    .to_string(),
            })
        }
    }

    let gh = NotFound {
        calls: RefCell::new(0),
    };
    let git = Scripted::new();
    let backoff = AuthBackoff::new();
    const TICKS: usize = 2;
    for _ in 0..TICKS {
        let cleaned = run_sweep(&gh, &git, &NoClaims, &NoLanding, &clean, &reg, &backoff).await;
        assert_eq!(cleaned, 0, "a failed read cleans nothing");
    }

    let calls = *gh.calls.borrow();
    assert_eq!(
        calls, 6,
        "every pending entry is still asked on every tick; a per-entry error must not gate the \
         host-wide sweep (made {calls} over {TICKS} tick(s) against 3 entries)"
    );
    assert!(
        backoff.degraded_reason().is_none(),
        "a per-entry error is not a degraded subsystem: {:?}",
        backoff.degraded_reason()
    );
    assert_eq!(
        reg.pending().len(),
        3,
        "nothing is stamped on a failed read"
    );
}

// ── #8301: the operator's recorded scope ─────────────────────────────────

/// Write one registry entry for PR 7275 with `scope` spelled as raw JSON.
///
/// Why raw JSON: it is the on-disk shape `tm pr merge` writes and the sweep
/// reads, so the test pins the wire format rather than a Rust constructor.
fn registry_with_scope(dir: &Path, scope: &str) -> CleanupRegistry {
    let reg = CleanupRegistry::under_root(dir);
    let body = format!(
        "{{\"entries\":[{{\"pr\":7275,\"repo\":\"{REPO}\",\"repo_root\":\"/repo\",\
         \"opened_at\":\"2026-09-23T00:00:00Z\",\"scope\":\"{scope}\"}}]}}"
    );
    std::fs::write(reg.path(), body).expect("write registry");
    reg
}

const OTHER_TREE: &str = "/repo/.claude/worktrees/agent-cc33";

/// A `git` fake with the PR's head tree plus an unnamed agent tree whose tip is
/// the merged head — the tree the wide cleanup removes and head-only keeps.
fn git_head_and_unnamed_tree() -> Scripted {
    let head_tree = "/repo/.claude/worktrees/agent-aa11";
    Scripted::new()
        .on(
            "config --get remote.origin.url",
            "https://github.com/bobmatnyc/trusty-tools.git\n",
        )
        .on("git ls-remote", "")
        .on(
            "git worktree list",
            &format!(
                "worktree /repo\nHEAD 1111\nbranch refs/heads/main\n\n\
                 worktree {head_tree}\nHEAD {HEAD_OID}\nbranch refs/heads/{BRANCH}\n\n\
                 worktree {OTHER_TREE}\nHEAD {HEAD_OID}\nbranch refs/heads/worktree-agent-cc33\n\n"
            ),
        )
        .on("rev-parse HEAD", &format!("{HEAD_OID}\n"))
        .on("git worktree remove", "")
        .on(
            "git branch --format",
            &format!("main 1111\n{BRANCH} {HEAD_OID}\nworktree-agent-cc33 {HEAD_OID}\n"),
        )
        .on("git branch -D", "")
        .on("git worktree prune", "")
        .on("git fetch --prune", "")
}

/// 🔴 #8301: `tm pr merge --no-cleanup` recorded the entry as deferred, and
/// the sweep never asks about it, let alone removes anything. Fails before the
/// fix, which read the entry as pending and ran the wide cleanup.
#[tokio::test]
async fn sweep_never_touches_a_deferred_entry() {
    let dir = tempfile::tempdir().expect("tempdir");
    let reg = registry_with_scope(dir.path(), "deferred");
    let gh = gh_merged();
    let git = git_head_and_unnamed_tree();

    let cleaned = run_sweep(
        &gh,
        &git,
        &NoClaims,
        &NoLanding,
        &clean,
        &reg,
        &AuthBackoff::new(),
    )
    .await;

    assert_eq!(cleaned, 0, "a deferred entry is never cleaned by the sweep");
    assert!(
        gh.calls().is_empty() && git.calls().is_empty(),
        "a deferred entry is not even asked about: gh={:?} git={:?}",
        gh.calls(),
        git.calls()
    );
    assert!(
        reg.entries()[0].cleaned_at.is_none(),
        "it stays for `tm pr cleanup <n>`"
    );
}

/// 🔴 #8301: a merge-chained cleanup that blocked left a head-only entry, and
/// the sweep's retry stays head-only — the unnamed tree and its branch
/// survive. Fails before the fix, whose sweep always ran wide.
#[tokio::test]
async fn sweep_honours_a_recorded_head_only_scope() {
    let dir = tempfile::tempdir().expect("tempdir");
    let reg = registry_with_scope(dir.path(), "head_only");
    let gh = gh_merged();
    let git = git_head_and_unnamed_tree();

    let cleaned = run_sweep(
        &gh,
        &git,
        &NoClaims,
        &NoLanding,
        &clean,
        &reg,
        &AuthBackoff::new(),
    )
    .await;

    let joined = git.calls().join("\n");
    assert_eq!(cleaned, 1, "the head-only retry completes: {joined}");
    assert!(
        joined.contains("git worktree remove /repo/.claude/worktrees/agent-aa11"),
        "the PR's own head tree is still removed: {joined}"
    );
    assert!(
        !joined.contains(&format!("git worktree remove {OTHER_TREE}")),
        "the unnamed tree must survive a head-only sweep: {joined}"
    );
    assert!(
        !joined.contains("git branch -D worktree-agent-cc33"),
        "its branch must survive too: {joined}"
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
