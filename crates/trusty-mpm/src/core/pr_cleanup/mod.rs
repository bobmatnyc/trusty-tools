//! Deterministic post-merge cleanup: `tm pr cleanup <n>` (#7275).
//!
//! Why: the delivery chain ends with a squash merge, and everything the merge
//! makes obsolete — the remote head branch, every agent worktree that carried
//! it, the local branches pointing at it, and the session records claiming
//! those directories — was reclaimed by hand or not at all. The owner's ruling
//! (2026-09-09) is that the sequence is mechanical: "let's script post merge
//! cleanup, it should be deterministic unless it errors. It should be run as
//! the final step after merge confirmation." A merged pull request is also the
//! most reliable determiner that its worktree and claim are obsolete, so the
//! merge — not the claim — decides.
//!
//! What: [`run`] executes five steps against the [`driver`] seams and reports
//! one [`StepLine`] each.
//!
//! | step | what it does | fails when |
//! |---|---|---|
//! | `pr` | `gh pr view <n> --json state,headRefName,headRefOid,mergeCommit` | the state is not `MERGED` |
//! | `remote-branch` | deletes `origin/<head>` when `git ls-remote` still lists it | the delete errors |
//! | `worktree` | ends any session claim, then `git worktree remove` each tree holding the head | a tree holds unsaved work, or the claim store cannot be read |
//!
//! **"Unsaved work" is not an ahead-of-upstream count (#7275 round 3).** Every
//! merge here is a squash, so a landed branch's commits are never ancestors of
//! `main` and `inspect_dirt` reports "N unpushed commit(s)" for every worktree
//! cleanup exists to reclaim — seven were refused that way on 2026-09-09. A
//! reading whose ONLY finding is that count defers to [`landed::landed`], which
//! requires a MERGED pull request matching the tree AND a merge that changes
//! nothing. Uncommitted or untracked files still refuse outright.
//!
//! **That merge test reads a ref this run refreshes itself (#7275 round 4).**
//! The comparison is against `origin/<base>` in the local ref store and the
//! only fetch is step 5's, which runs after the worktree step — so the first
//! cleanup after a merge would compare against a base predating it. [`landed::Merge`]
//! refreshes that one ref, once per run, before the first comparison.
//!
//! **The unsaved-work probe is taken twice (#7275 round 4).** Several `gh` and
//! `git` round trips separate the first probe from the removal it authorises,
//! and a claim is record-only — so [`recheck`] re-reads the tree immediately
//! before `git worktree remove` and refuses on any difference. The claims end
//! only after that re-read passes (#8301).
//! | `local-branch` | `git branch -D` the head branch and each `worktree-agent-*` at the head commit | a delete errors |
//! | `prune` | `git worktree prune`, then `git fetch --prune origin` | either errors |
//!
//! Two properties are load-bearing. `git branch -D` is defensible only because
//! step 1 confirmed the merge: a squash merge leaves the branch looking
//! unmerged to git, so `-d` would refuse every time and prove nothing. And a
//! dirty worktree is the error arm — cleanup never passes `--force` to
//! `git worktree remove`, so a tree holding unsaved work stops the run with a
//! nonzero exit instead of losing the work.
//!
//! **Only pull requests `tm pr open` recorded are swept.** The registry is
//! written at open time, so a PR opened by hand, by `gh pr create`, or by a
//! `tm` predating #7275 has no entry and the periodic sweep never considers it.
//! Cleaning one up is `tm pr cleanup <n>` by hand, which takes the PR number
//! directly and needs no entry. Under `--auto` the sweep is the only trigger,
//! so an unrecorded PR merged that way is not cleaned up at all.
//!
//! **The sweep and a hand-run `tm pr cleanup` can race, and that is safe.**
//! Every step is idempotent by construction: `remote-branch` deletes only what
//! `git ls-remote` still lists, `worktree` removes only trees git still
//! registers, `local-branch` deletes only branches `git branch` still reports,
//! and `prune` is idempotent outright. A step whose target is already gone
//! reports nothing to do rather than failing. The registry stamp and
//! [`SweepDecision::AlreadyCleaned`] then keep the sweep from re-running a
//! sequence a hand run already completed — see `sweep.rs`.
//!
//! **Ownership (#8301).** [`remove::remove_one`] ends only the claims of the calling
//! session or of a provably ended one, and applies the #7771 rule — harness
//! lock, owner-file session, a process standing in the tree — before any claim
//! is ended. See `ownership`.
//!
//! **KNOWN GAP — cleanup does not consult the daemon's live-writer registry
//! (#7275 round 2, critic finding 3).** It never asks the question the
//! pm-guard's `CHECK_SOLE_OWNER` asks — whether a live dispatched agent is
//! writing in that tree right now. The harness lock covers an
//! isolation-dispatched agent; an agent pointed at a tree by `cwd` alone is
//! still invisible here. The
//! answer lives in the daemon's in-memory delegation map and is reachable only
//! over the session-scoped `shared-tree-dispatch` route; the supervisor that
//! runs the sweep is a SEPARATE PROCESS holding a `SessionManager` and no
//! delegation state, and the HTTP client exposes no path-keyed live-writer
//! query. Closing it needs a new daemon route plus a client method plus wiring
//! into both `ClaimEnder` implementations — a cross-crate API change, not a
//! fix inside this one. Tracked for the parent rather than done here.
//!
//! Test: the sibling `tests.rs`.

// #8058: the sweep's `gh`-authentication backoff — see its module doc for the
// loop it stops.
pub mod auth_backoff;
pub mod driver;
mod landed;
// #8301: the ownership gate `remove_one` applies before ending any claim.
mod ownership;
pub mod plan;
mod recheck;
// #7771: `remove_one`, split out for the SLOC cap.
pub mod registry;
mod remove;
mod repo_check;
pub mod sweep;

#[cfg(test)]
#[path = "tests.rs"]
mod tests;

use std::path::{Path, PathBuf};

use tracing::info;

pub use driver::{
    ClaimEnder, CmdOut, Gh, Git, Landing, RealGh, RealGit, RealLanding, UnavailableClaims,
};
pub(crate) use ownership::{
    CallerOwnership, ClaimOwnership, HolderState, holder_state_of, host_tree_gate,
};
pub use plan::{PrView, StepLine, StepStatus};
pub use registry::{CleanupRegistry, CleanupScope, OpenedPr};
use repo_check::repo_mismatch;
pub use sweep::{SweepDecision, sweep_decision};

use crate::session_manager::DirtyWorktree;

/// The `--json` field set step 1 reads.
const VIEW_FIELDS: &str = "state,headRefName,headRefOid,mergeCommit,baseRefName";

/// The base branch used when a PR payload names none.
const FALLBACK_BASE: &str = "main";

/// One `tm pr cleanup` invocation's inputs.
#[derive(Debug, Clone)]
pub struct CleanupRequest {
    /// The pull-request number.
    pub pr: u64,
    /// `owner/repo`, when the caller pinned one.
    pub repo: Option<String>,
    /// The main checkout every git command runs in.
    pub repo_root: PathBuf,
    /// Print the plan and make no mutating call.
    pub dry_run: bool,
    /// Remove only the PR's own head worktree and head branch (#8301).
    ///
    /// `tm pr merge` sets it: a merge names one PR, so trees that merely sit on
    /// its head commit, round-N siblings and `worktree-agent-*` branches are
    /// reported and left for an explicit `tm pr cleanup <n>`.
    pub head_only: bool,
}

/// What one cleanup run did.
#[derive(Debug, Clone)]
pub struct CleanupReport {
    /// The pull request this run was about.
    pub pr: u64,
    /// One line per step, in execution order.
    pub lines: Vec<StepLine>,
}

impl CleanupReport {
    /// Whether any step failed — the caller's nonzero exit.
    pub fn failed(&self) -> bool {
        self.lines.iter().any(StepLine::is_failure)
    }

    /// Every step line, newline-joined.
    pub fn render(&self) -> String {
        self.lines
            .iter()
            .map(StepLine::render)
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// The unsaved-work probe, injected so the executor is testable.
///
/// Why: passing a precomputed answer would let a caller reach the worktree
/// step with a `None` meaning "not checked" instead of "checked and clean" —
/// the same reason `worktree_reclaim::classify` takes its probe as a closure.
/// Production passes [`crate::session_manager::worktree_safety::inspect_dirt`],
/// which fails toward DIRTY on every error.
pub type DirtProbe<'a> = &'a dyn Fn(&Path) -> Option<DirtyWorktree>;

/// Run the five cleanup steps and report each one.
///
/// Why: one function, not five call sites — `tm pr cleanup`, `tm pr merge`'s
/// final step and the supervisor's periodic sweep all land here, so the three
/// triggers cannot disagree about what cleanup means or which safety check it
/// applies.
/// What: executes the table in the module doc. Step 1 refusing short-circuits
/// the run: nothing else is attempted, because every later step is authorised
/// by the merge. Under `dry_run` every READ still runs — so the reported plan
/// is the real one — and no mutating command is issued.
/// Test: `cleanup_refuses_an_open_pr`, `cleanup_clean_path_removes_everything`,
/// `cleanup_refuses_a_dirty_worktree`, `cleanup_dry_run_makes_no_mutating_call`.
///
/// #8301: ownership is [`CallerOwnership::from_env`] — this process's own
/// session ids, so another session's tree or claim is kept.
pub async fn run<G: Gh, T: Git, C: ClaimEnder>(
    gh: &G,
    git: &T,
    claims: &C,
    landing: &dyn Landing,
    probe_dirt: DirtProbe<'_>,
    req: &CleanupRequest,
) -> CleanupReport {
    let ownership = CallerOwnership::from_env();
    run_with(gh, git, claims, landing, probe_dirt, req, &ownership).await
}

/// [`run`], with the #8301 ownership answers supplied.
///
/// Test: `cleanup_keeps_a_tree_another_live_session_claims`.
pub(crate) async fn run_with<G: Gh, T: Git, C: ClaimEnder>(
    gh: &G,
    git: &T,
    claims: &C,
    landing: &dyn Landing,
    probe_dirt: DirtProbe<'_>,
    req: &CleanupRequest,
    ownership: &dyn ClaimOwnership,
) -> CleanupReport {
    let mut lines = Vec::new();

    let view = match view_pr(gh, req) {
        Ok(v) => v,
        Err(e) => {
            lines.push(StepLine::failed("pr", format!("{e:#}")));
            return CleanupReport { pr: req.pr, lines };
        }
    };
    if let Some(reason) = plan::merge_refusal(&view, req.pr) {
        lines.push(StepLine::failed("pr", reason));
        return CleanupReport { pr: req.pr, lines };
    }
    let merged_at = view
        .merge_commit
        .as_ref()
        .map(|c| c.oid.trim())
        .filter(|o| !o.is_empty())
        .unwrap_or("<no merge commit reported>")
        .to_string();
    lines.push(StepLine::ok(
        "pr",
        format!(
            "#{} is MERGED (head {} at {}, merge commit {merged_at})",
            req.pr,
            view.head_ref_name,
            short(&view.head_ref_oid)
        ),
    ));

    // #7275 round 2: `gh` is aimed by `--repo` — correct by construction — but
    // every git command below is aimed by `repo_root`, and a registry entry
    // carries the two independently. Reconciled here, after the merge is
    // confirmed and before the first destructive command, so a MERGED pull
    // request in one repository can never authorise deletions in another.
    if let Some(reason) = repo_mismatch(git, req) {
        lines.push(StepLine::failed("repo", reason));
        return CleanupReport { pr: req.pr, lines };
    }

    // #7275 round 4: every merge test below compares against `origin/<base>`,
    // and the run's own fetch is step 5. One `Merge` shared across the steps
    // refreshes that ref on first use, so the whole run pays one round trip.
    let merged = MergedPr {
        merge: landed::Merge::new(&view),
        view: &view,
    };

    lines.push(step_remote_branch(git, req, &view));
    step_worktrees(
        git, claims, ownership, landing, probe_dirt, req, &merged, &mut lines,
    )
    .await;
    // The head branch cannot be deleted while a worktree still has it checked
    // out, so branch deletion always follows the removals above.
    lines.push(step_local_branches(git, req, &merged));
    lines.push(step_prune(git, req));

    let report = CleanupReport { pr: req.pr, lines };
    info!(
        pr = req.pr,
        dry_run = req.dry_run,
        failed = report.failed(),
        "pr cleanup finished"
    );
    report
}

/// Step 1: read the PR once.
fn view_pr<G: Gh>(gh: &G, req: &CleanupRequest) -> anyhow::Result<PrView> {
    let n = req.pr.to_string();
    let mut a = vec!["pr".to_string(), "view".to_string(), n];
    if let Some(repo) = req.repo.as_deref().map(str::trim).filter(|r| !r.is_empty()) {
        a.push("--repo".to_string());
        a.push(repo.to_string());
    }
    a.push("--json".to_string());
    a.push(VIEW_FIELDS.to_string());
    let stdout = gh.run(&a)?.stdout_ok(&format!("gh {}", a.join(" ")))?;
    serde_json::from_str(&stdout)
        .map_err(|e| anyhow::anyhow!("cannot parse `gh pr view {}` JSON: {e}", req.pr))
}

/// Step 2: delete the remote head branch when the remote still lists it.
fn step_remote_branch<T: Git>(git: &T, req: &CleanupRequest, view: &PrView) -> StepLine {
    const STEP: &str = "remote-branch";
    let branch = view.head_ref_name.trim();
    if branch.is_empty() {
        return StepLine::ok(STEP, "the PR names no head branch; nothing to delete");
    }
    let listing = match run_git(git, req, &["ls-remote", "--heads", "origin", branch]) {
        Ok(s) => s,
        Err(e) => return StepLine::failed(STEP, format!("{e:#}")),
    };
    if listing.trim().is_empty() {
        return StepLine::ok(STEP, format!("origin/{branch} is already gone"));
    }
    if req.dry_run {
        return StepLine::ok(STEP, format!("would delete origin/{branch}"));
    }
    match run_git(git, req, &["push", "origin", "--delete", branch]) {
        Ok(_) => StepLine::ok(STEP, format!("deleted origin/{branch}")),
        Err(e) => StepLine::failed(STEP, format!("{e:#}")),
    }
}

/// The merged pull request as steps 3 and 4 read it (#7275 round 4).
///
/// Why: those steps need both the `gh` payload and the refreshed base ref the
/// merge tests compare against, and passing the two separately put
/// [`step_worktrees`] one argument over the lint's limit. Grouping them also
/// says the truer thing: the base ref is derived from this pull request's own
/// `baseRefName`, so the two travel together or not at all.
/// What: a borrow of step 1's payload plus the one [`landed::Merge`] every
/// landing test in the run shares — which is what keeps a sweep over five
/// worktrees to a single `git fetch`.
/// Test: `cleanup_reclaims_a_tree_whose_base_ref_was_stale` (one fetch, before
/// the first comparison), `cleanup_clean_path_removes_everything`.
struct MergedPr<'a> {
    /// Step 1's `gh` payload — head branch, head commit, base branch.
    view: &'a PrView,
    /// The base ref every landing test is measured against, refreshed once.
    merge: landed::Merge,
}

/// Step 3: end each claim, then remove each worktree holding the merged head.
// #8301: `ownership` is the eighth argument; it travels with `claims`.
#[allow(clippy::too_many_arguments)]
async fn step_worktrees<T: Git, C: ClaimEnder>(
    git: &T,
    claims: &C,
    ownership: &dyn ClaimOwnership,
    landing: &dyn Landing,
    probe_dirt: DirtProbe<'_>,
    req: &CleanupRequest,
    merged: &MergedPr<'_>,
    lines: &mut Vec<StepLine>,
) {
    let view = merged.view;
    const STEP: &str = "worktree";
    let porcelain = match run_git(git, req, &["worktree", "list", "--porcelain"]) {
        Ok(s) => s,
        Err(e) => {
            lines.push(StepLine::failed(STEP, format!("{e:#}")));
            return;
        }
    };
    let entries = plan::parse_worktree_list(&porcelain);
    let mut targets = plan::worktree_targets(
        &entries,
        &view.head_ref_name,
        &view.head_ref_oid,
        &req.repo_root,
    );
    // #8301: a merge-chained run removes only the tree holding the head branch.
    if req.head_only {
        let (named, left) = plan::split_head_only(targets, &view.head_ref_name);
        if !left.is_empty() {
            lines.push(StepLine::ok(STEP, plan::left_in_place(&left, req.pr)));
        }
        // #8301: the trees left in place DO hold the head — never say none does.
        if named.is_empty() && !left.is_empty() {
            return;
        }
        targets = named;
    }
    if targets.is_empty() {
        lines.push(StepLine::ok(
            STEP,
            format!("no worktree holds {}", view.head_ref_name),
        ));
        return;
    }
    for t in targets {
        // A round-N sibling carries no merged pull request of its own, so its
        // content is what proves the merge made it obsolete. The head branch,
        // and any tree sitting on the merged head commit, already carry that
        // proof from step 1 and are not asked again.
        if !is_merged_head(view, t)
            && let Some(branch) = t.branch.as_deref()
            && let Some(refusal) = landed::unlanded(git, req, &merged.merge, branch)
        {
            lines.push(StepLine::failed(
                STEP,
                format!("{}: {refusal}", t.path.display()),
            ));
            continue;
        }
        lines.push(
            remove::remove_one(
                git,
                claims,
                ownership,
                landing,
                probe_dirt,
                req,
                &merged.merge,
                t,
            )
            .await,
        );
    }
}

/// Is this worktree the PR's own head — by branch name, or by sitting on the
/// merged head commit?
///
/// Test: `cleanup_removes_a_round_sibling_whose_content_landed` (the sibling is
/// asked), `cleanup_clean_path_removes_everything` (the head is not).
fn is_merged_head(view: &PrView, entry: &plan::WorktreeEntry) -> bool {
    let head = view.head_ref_name.trim();
    let oid = view.head_ref_oid.trim();
    (!head.is_empty() && entry.branch.as_deref().map(str::trim) == Some(head))
        || (!oid.is_empty() && entry.head.eq_ignore_ascii_case(oid))
}

/// Build an owned argv from string slices.
fn owned(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|s| (*s).to_string()).collect()
}

/// Step 4: delete the local head branch and every agent branch at its tip.
fn step_local_branches<T: Git>(git: &T, req: &CleanupRequest, merged: &MergedPr<'_>) -> StepLine {
    const STEP: &str = "local-branch";
    let view = merged.view;
    let listing = match run_git(
        git,
        req,
        &["branch", "--format=%(refname:short) %(objectname)"],
    ) {
        Ok(s) => s,
        Err(e) => return StepLine::failed(STEP, format!("{e:#}")),
    };
    let head = view.head_ref_name.trim();
    // #8301: a merge-chained run deletes the head branch and nothing else.
    if req.head_only {
        let wanted = plan::pr_branches(&listing, head)
            .into_iter()
            .filter(|b| b == head)
            .collect();
        return delete_branches(git, req, wanted);
    }
    // The head branch and its round-N siblings first, then every
    // `worktree-agent-*` branch still sitting on the merged head commit.
    let siblings = plan::pr_branches(&listing, head);
    // #7275: a sibling holding work the merge did not carry stays, and says so.
    // The head itself, and every `worktree-agent-*` branch still ON the merged
    // head commit, carry step 1's proof already and are not asked again.
    let mut refusals = Vec::new();
    let mut wanted: Vec<String> = siblings
        .into_iter()
        .filter(|b| {
            if b == head {
                return true;
            }
            match landed::unlanded(git, req, &merged.merge, b) {
                None => true,
                Some(reason) => {
                    refusals.push(reason);
                    false
                }
            }
        })
        .collect();
    for agent in plan::agent_branches_at(&listing, &view.head_ref_oid) {
        if !wanted.contains(&agent) {
            wanted.push(agent);
        }
    }
    if !refusals.is_empty() {
        return StepLine::failed(STEP, refusals.join("; "));
    }
    delete_branches(git, req, wanted)
}

/// Step 4's deletion half: `git branch -D` each of `wanted`, stopping at the
/// first failure.
fn delete_branches<T: Git>(git: &T, req: &CleanupRequest, wanted: Vec<String>) -> StepLine {
    const STEP: &str = "local-branch";
    if wanted.is_empty() {
        return StepLine::ok(STEP, "no local branch left to delete");
    }
    if req.dry_run {
        return StepLine::ok(STEP, format!("would delete {}", wanted.join(", ")));
    }
    let mut deleted = Vec::new();
    for b in &wanted {
        // `-D`, not `-d`: the squash merge means git sees this branch as
        // unmerged. Step 1 already proved otherwise.
        if let Err(e) = run_git(git, req, &["branch", "-D", b]) {
            return StepLine::failed(
                STEP,
                format!(
                    "deleted [{}]; `git branch -D {b}` failed: {e:#}",
                    deleted.join(", ")
                ),
            );
        }
        deleted.push(b.clone());
    }
    StepLine::ok(STEP, format!("deleted {}", deleted.join(", ")))
}

/// Step 5: prune git's worktree records and the stale remote refs.
fn step_prune<T: Git>(git: &T, req: &CleanupRequest) -> StepLine {
    const STEP: &str = "prune";
    if req.dry_run {
        return StepLine::ok(
            STEP,
            "would run `git worktree prune` and `git fetch --prune origin`",
        );
    }
    if let Err(e) = run_git(git, req, &["worktree", "prune"]) {
        return StepLine::failed(STEP, format!("{e:#}"));
    }
    match run_git(git, req, &["fetch", "--prune", "origin"]) {
        Ok(_) => StepLine::ok(STEP, "pruned worktree records and fetched with --prune"),
        Err(e) => StepLine::failed(STEP, format!("{e:#}")),
    }
}

/// Run one git command in the request's checkout, erroring on a nonzero exit.
fn run_git<T: Git>(git: &T, req: &CleanupRequest, args: &[&str]) -> anyhow::Result<String> {
    let owned: Vec<String> = args.iter().map(|s| (*s).to_string()).collect();
    git.run(&req.repo_root, &owned)?
        .stdout_ok(&format!("git {}", args.join(" ")))
}

/// The first 8 characters of an object name, for a log line.
fn short(oid: &str) -> &str {
    let o = oid.trim();
    o.get(..8).unwrap_or(o)
}
