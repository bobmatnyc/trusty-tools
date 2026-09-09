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
//! Test: the sibling `tests.rs`.

pub mod driver;
pub mod plan;
pub mod registry;
pub mod sweep;

#[cfg(test)]
#[path = "tests.rs"]
mod tests;

use std::path::{Path, PathBuf};

use tracing::info;

pub use driver::{ClaimEnder, CmdOut, Gh, Git, RealGh, RealGit, UnavailableClaims};
pub use plan::{PrView, StepLine, StepStatus};
pub use registry::{CleanupRegistry, OpenedPr};
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
pub async fn run<G: Gh, T: Git, C: ClaimEnder>(
    gh: &G,
    git: &T,
    claims: &C,
    probe_dirt: DirtProbe<'_>,
    req: &CleanupRequest,
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

    lines.push(step_remote_branch(git, req, &view));
    step_worktrees(git, claims, probe_dirt, req, &view, &mut lines).await;
    // The head branch cannot be deleted while a worktree still has it checked
    // out, so branch deletion always follows the removals above.
    lines.push(step_local_branches(git, req, &view));
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

/// Step 3: end each claim, then remove each worktree holding the merged head.
async fn step_worktrees<T: Git, C: ClaimEnder>(
    git: &T,
    claims: &C,
    probe_dirt: DirtProbe<'_>,
    req: &CleanupRequest,
    view: &PrView,
    lines: &mut Vec<StepLine>,
) {
    const STEP: &str = "worktree";
    let porcelain = match run_git(git, req, &["worktree", "list", "--porcelain"]) {
        Ok(s) => s,
        Err(e) => {
            lines.push(StepLine::failed(STEP, format!("{e:#}")));
            return;
        }
    };
    let entries = plan::parse_worktree_list(&porcelain);
    let targets = plan::worktree_targets(
        &entries,
        &view.head_ref_name,
        &view.head_ref_oid,
        &req.repo_root,
    );
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
            && let Some(refusal) = unlanded(git, req, view, branch)
        {
            lines.push(StepLine::failed(
                STEP,
                format!("{}: {refusal}", t.path.display()),
            ));
            continue;
        }
        lines.push(remove_one(git, claims, probe_dirt, req, &t.path).await);
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

/// Why `tip` is NOT safely deletable as this PR's sibling, or `None` when it is.
///
/// Why (#7275, owner correction 2026-09-09): every merge here is a squash, so
/// `git cherry`'s per-commit patch-id comparison reports `+` for content that
/// IS on the base — it said so for #7258. Merging the tip into the base and
/// asking whether the result differs from the base answers the real question:
/// would landing this branch change anything? An empty diff means no, which is
/// the ownership proof a sibling needs and also what makes a stacked branch
/// whose base already merged a no-op.
/// What: accepts immediately when `tip` is an ancestor of the merged head —
/// a round-1 branch superseded by `-r2` usually is — and otherwise runs
/// `git merge-tree --write-tree origin/<base> <tip>` for the merged tree, then
/// `git diff --name-only origin/<base> <tree>`. An empty answer is the no-op; a
/// non-empty one NAMES the residue files, so a person can look at what the
/// merge did not carry instead of being told only that something remains.
/// FAIL-SAFE: a conflict, an unreadable tree, and any error all REFUSE.
/// Test: `cleanup_removes_a_round_sibling_whose_content_landed`,
/// `cleanup_refuses_a_sibling_whose_content_is_not_on_the_base`,
/// `cleanup_removes_a_round_one_branch_that_never_had_its_own_pr`.
fn unlanded<T: Git>(git: &T, req: &CleanupRequest, view: &PrView, tip: &str) -> Option<String> {
    let head_oid = view.head_ref_oid.trim();
    if !head_oid.is_empty()
        && let Ok(out) = git.run(
            &req.repo_root,
            &owned(&["merge-base", "--is-ancestor", tip, head_oid]),
        )
        && out.success
    {
        return None;
    }
    let base = match view.base_ref_name.trim() {
        "" => FALLBACK_BASE,
        b => b,
    };
    let base_ref = format!("origin/{base}");
    let merged = match git.run(
        &req.repo_root,
        &owned(&["merge-tree", "--write-tree", &base_ref, tip]),
    ) {
        Ok(out) if out.success => out.stdout,
        Ok(out) => {
            return Some(format!(
                "`{tip}` does not merge cleanly into {base_ref}, so the merge cannot have \
                 carried it: {}",
                out.stderr.trim()
            ));
        }
        Err(e) => {
            return Some(format!(
                "cannot merge-test `{tip}` against {base_ref}: {e:#}"
            ));
        }
    };
    let Some(tree) = merged
        .lines()
        .next()
        .map(str::trim)
        .filter(|t| !t.is_empty())
    else {
        return Some(format!(
            "`git merge-tree --write-tree {base_ref} {tip}` named no tree"
        ));
    };
    match git.run(
        &req.repo_root,
        &owned(&["diff", "--name-only", &base_ref, tree]),
    ) {
        Ok(out) if out.success && out.stdout.trim().is_empty() => None,
        Ok(out) if out.success => Some(format!(
            "`{tip}` is this PR's round sibling, but merging it into {base_ref} would still \
             change {}: {} — it holds work the merge did not carry, so cleanup leaves it alone \
             (#7275)",
            file_count(&out.stdout),
            out.stdout.split_whitespace().collect::<Vec<_>>().join(", ")
        )),
        Ok(out) => Some(format!(
            "cannot compare the merge of `{tip}` against {base_ref}: {}",
            out.stderr.trim()
        )),
        Err(e) => Some(format!(
            "cannot compare the merge of `{tip}` against {base_ref}: {e:#}"
        )),
    }
}

/// `"1 file"` / `"3 files"` for a newline-separated listing.
fn file_count(listing: &str) -> String {
    let n = listing.split_whitespace().count();
    if n == 1 {
        "1 file".to_string()
    } else {
        format!("{n} files")
    }
}

/// Build an owned argv from string slices.
fn owned(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|s| (*s).to_string()).collect()
}

/// End the claims on one worktree and remove it, or say why it stayed.
async fn remove_one<T: Git, C: ClaimEnder>(
    git: &T,
    claims: &C,
    probe_dirt: DirtProbe<'_>,
    req: &CleanupRequest,
    path: &Path,
) -> StepLine {
    const STEP: &str = "worktree";
    let shown = path.display().to_string();
    // The dirty check runs FIRST and is the one refusal: a merged PR makes the
    // claim obsolete, but it says nothing about work that was never committed.
    if let Some(dirt) = probe_dirt(path) {
        return StepLine::failed(
            STEP,
            format!(
                "{shown} holds unsaved work ({}) — refusing to remove it; cleanup never forces \
                 (#7275)",
                dirt.reason
            ),
        );
    }
    let holders = match claims.claims_on(path).await {
        Ok(h) => h,
        Err(e) => return StepLine::failed(STEP, format!("{e:#}")),
    };
    if req.dry_run {
        return StepLine::ok(
            STEP,
            format!("would remove {shown}{}", claim_note(&holders, "end ")),
        );
    }
    for id in &holders {
        if let Err(e) = claims.end_claim(id).await {
            return StepLine::failed(
                STEP,
                format!(
                    "{shown} is claimed by session {id} and the claim could not be ended: {e:#}"
                ),
            );
        }
    }
    // Never `--force`: the dirty gate above is the only thing standing between
    // this call and an operator's unsaved work.
    match run_git(git, req, &["worktree", "remove", &shown]) {
        Ok(_) => StepLine::ok(
            STEP,
            format!("removed {shown}{}", claim_note(&holders, "ended ")),
        ),
        Err(e) => StepLine::failed(STEP, format!("{e:#}")),
    }
}

/// ` (<verb>claim held by a, b)`, or empty when nothing claimed the tree.
fn claim_note(holders: &[String], verb: &str) -> String {
    if holders.is_empty() {
        return String::new();
    }
    format!(" ({verb}claim held by {})", holders.join(", "))
}

/// Step 4: delete the local head branch and every agent branch at its tip.
fn step_local_branches<T: Git>(git: &T, req: &CleanupRequest, view: &PrView) -> StepLine {
    const STEP: &str = "local-branch";
    let listing = match run_git(
        git,
        req,
        &["branch", "--format=%(refname:short) %(objectname)"],
    ) {
        Ok(s) => s,
        Err(e) => return StepLine::failed(STEP, format!("{e:#}")),
    };
    let head = view.head_ref_name.trim();
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
            match unlanded(git, req, view, b) {
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
