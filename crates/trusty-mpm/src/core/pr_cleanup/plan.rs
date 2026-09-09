//! The pure decisions behind `tm pr cleanup` (#7275).
//!
//! Why: every mutating step of the cleanup is guarded by a judgment made from
//! command output — is this PR merged, does the remote still list the branch,
//! which worktrees hold this head, which local branches point at it. Keeping
//! those judgments as pure functions of scripted text is what makes the
//! destructive command they authorise testable without running it.
//!
//! What: [`PrView`] and [`merge_refusal`] (step 1), [`WorktreeEntry`] /
//! [`parse_worktree_list`] / [`worktree_targets`] (step 3),
//! [`agent_branches_at`] (step 4), and [`StepLine`] — the one-line-per-step
//! result shape the whole command reports in.
//!
//! Test: the sibling `tests.rs` — `merge_refusal_*`, `parse_worktree_list_*`,
//! `worktree_targets_*`, `agent_branches_at_*`.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The prefix of a branch the harness mints for a dispatched agent's worktree.
///
/// Why: those branches are named after the agent, not the workstream, so
/// nothing else in the delivery chain deletes them; they accumulate one per
/// dispatch. They are safe to delete exactly when the PR that carried their
/// commits has merged, which is the only moment this command runs.
/// Test: `agent_branches_at_matches_only_the_agent_prefix`.
pub const AGENT_BRANCH_PREFIX: &str = "worktree-agent-";

/// The `gh pr view --json` fields cleanup decides from.
///
/// Why: one read answers the whole gate — `state` is the refusal, and
/// `headRefName` / `headRefOid` are what every later step matches against, so
/// no second round trip can observe a different PR than the one that was
/// judged.
/// What: every field defaulted, so a payload missing one still parses.
/// Test: `merge_refusal_accepts_merged`, `merge_refusal_names_the_state`.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct PrView {
    /// `MERGED`, `OPEN` or `CLOSED`.
    #[serde(default)]
    pub state: String,
    /// The head branch name, without a `refs/heads/` prefix.
    #[serde(default, rename = "headRefName")]
    pub head_ref_name: String,
    /// The head commit the PR was merged from.
    #[serde(default, rename = "headRefOid")]
    pub head_ref_oid: String,
    /// The squash commit on the base branch, when GitHub reports one.
    #[serde(default, rename = "mergeCommit")]
    pub merge_commit: Option<MergeCommit>,
    /// The base branch the PR merged into — `origin/<this>` is what a sibling's
    /// content is checked against (#7275 round-N siblings).
    #[serde(default, rename = "baseRefName")]
    pub base_ref_name: String,
}

/// The `mergeCommit` sub-object of a `gh pr view --json` payload.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct MergeCommit {
    /// The squash commit's SHA.
    #[serde(default)]
    pub oid: String,
}

/// Why this PR may NOT be cleaned up, or `None` when it may.
///
/// Why: cleanup deletes a remote branch, several worktrees and several local
/// branches, and `git branch -D` is only defensible because the PR is known
/// MERGED — a squash merge leaves the branch looking unmerged to git, so the
/// ordinary `-d` safety net reports nothing useful here. That makes the state
/// check the load-bearing gate, and the reason it refuses must name what it
/// actually saw so an operator is not left guessing between "not merged yet"
/// and "no such PR".
/// What: refuses on any `state` other than `MERGED` (case-insensitive), and on
/// a payload that names neither a head branch nor a head OID — with nothing to
/// match on, every later step would silently find no targets and report a
/// clean sweep it never performed.
/// Test: `merge_refusal_accepts_merged`, `merge_refusal_names_the_state`,
/// `merge_refusal_rejects_a_payload_with_no_head`.
pub fn merge_refusal(view: &PrView, pr: u64) -> Option<String> {
    let state = view.state.trim();
    if !state.eq_ignore_ascii_case("MERGED") {
        let seen = if state.is_empty() { "<absent>" } else { state };
        return Some(format!(
            "#{pr} is {seen}, not MERGED — cleanup deletes branches and worktrees and runs only \
             after a merge is confirmed"
        ));
    }
    if view.head_ref_name.trim().is_empty() && view.head_ref_oid.trim().is_empty() {
        return Some(format!(
            "#{pr} reports MERGED but names neither a head branch nor a head commit, so nothing \
             identifies what to clean up"
        ));
    }
    None
}

/// One entry of `git worktree list --porcelain`.
///
/// Test: `parse_worktree_list_reads_branch_and_head`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeEntry {
    /// The worktree's directory.
    pub path: PathBuf,
    /// The commit its HEAD points at, lowercase hex, or empty when detached
    /// with no reported HEAD.
    pub head: String,
    /// The branch it has checked out, short form, or `None` when detached.
    pub branch: Option<String>,
}

/// Parse `git worktree list --porcelain` into entries.
///
/// Why: the porcelain form is the only listing that reports the branch and the
/// HEAD alongside the path; the human form collapses them into one line that
/// cannot be split reliably when a path contains a space.
/// What: a `worktree <path>` line opens a record, `HEAD <oid>` and
/// `branch refs/heads/<name>` fill it, and a blank line closes it. A `bare`
/// record has no path of interest and is dropped, as is any record whose
/// `worktree` line was absent.
/// Test: `parse_worktree_list_reads_branch_and_head`,
/// `parse_worktree_list_drops_the_bare_record`,
/// `parse_worktree_list_keeps_a_detached_worktree`.
pub fn parse_worktree_list(porcelain: &str) -> Vec<WorktreeEntry> {
    let mut out = Vec::new();
    let mut path: Option<PathBuf> = None;
    let mut head = String::new();
    let mut branch: Option<String> = None;
    let mut bare = false;

    let flush = |path: &mut Option<PathBuf>,
                 head: &mut String,
                 branch: &mut Option<String>,
                 bare: &mut bool,
                 out: &mut Vec<WorktreeEntry>| {
        if let Some(p) = path.take()
            && !*bare
        {
            out.push(WorktreeEntry {
                path: p,
                head: std::mem::take(head),
                branch: branch.take(),
            });
        }
        head.clear();
        *branch = None;
        *bare = false;
    };

    for line in porcelain.lines() {
        let line = line.trim_end();
        if line.is_empty() {
            flush(&mut path, &mut head, &mut branch, &mut bare, &mut out);
        } else if let Some(p) = line.strip_prefix("worktree ") {
            flush(&mut path, &mut head, &mut branch, &mut bare, &mut out);
            path = Some(PathBuf::from(p.trim()));
        } else if let Some(oid) = line.strip_prefix("HEAD ") {
            head = oid.trim().to_ascii_lowercase();
        } else if let Some(r) = line.strip_prefix("branch ") {
            branch = Some(short_branch(r.trim()).to_string());
        } else if line == "bare" {
            bare = true;
        }
    }
    flush(&mut path, &mut head, &mut branch, &mut bare, &mut out);
    out
}

/// A `refs/heads/foo` ref as `foo`; anything else unchanged.
fn short_branch(r: &str) -> &str {
    r.strip_prefix("refs/heads/").unwrap_or(r)
}

/// A branch name with a trailing review-round suffix removed.
///
/// Why (#7275, round-N siblings): an engineer's second review round lands on
/// `<branch>-r2`, and `version-control` pushes that onto the PR's own head
/// name — so the PR merges as `<branch>` while `<branch>-r2` and its
/// `worktree-agent-*` tree survive with no merged pull request carrying their
/// name. Reducing both spellings to the same stem is what lets one rule relate
/// them, in either direction: the PR's head may be the suffixed one.
/// What: strips a trailing `-r<digits>`; anything else is returned unchanged.
/// Test: `strip_round_suffix_removes_only_a_numbered_round`.
pub fn strip_round_suffix(name: &str) -> &str {
    let Some((stem, round)) = name.rsplit_once("-r") else {
        return name;
    };
    if stem.is_empty() || round.is_empty() || !round.bytes().all(|b| b.is_ascii_digit()) {
        return name;
    }
    stem
}

/// Does `name` belong to the pull request whose head branch is `head`?
///
/// Why: cleanup must reclaim the round-N siblings the merge made obsolete
/// WITHOUT widening into a machine-wide sweep — `tm session prune-worktrees
/// --merged-prs` already does the unscoped thing, and doing it again here would
/// let one PR's cleanup delete another PR's tree. Relating names by their
/// round-stripped stem keeps the scope to this pull request's own branches.
/// What: true when both names reduce to the same stem under
/// [`strip_round_suffix`] — so `foo`, `foo-r2` and `foo-r3` all belong to each
/// other, and nothing else does. An empty `head` matches nothing.
/// Test: `is_pr_branch_relates_round_siblings`,
/// `is_pr_branch_rejects_an_unrelated_branch`.
pub fn is_pr_branch(head: &str, name: &str) -> bool {
    let head = head.trim();
    let name = name.trim();
    if head.is_empty() || name.is_empty() {
        return false;
    }
    strip_round_suffix(head) == strip_round_suffix(name)
}

/// The worktrees this PR's merge makes obsolete.
///
/// Why: an agent's tree is found either by the branch it has checked out or —
/// when the harness left it detached, which is the shape after a rebase — by
/// the commit its HEAD sits on. Matching only the branch missed the detached
/// half and left those trees on disk with no automated route to reclaiming
/// them.
/// What: every entry whose branch belongs to this PR under [`is_pr_branch`] —
/// the head branch itself and its round-N siblings — or whose HEAD equals
/// `head_oid`, EXCEPT the checkout at `repo_root`: cleanup runs from the main
/// checkout, which is never its own target however its HEAD happens to sit.
/// Comparison on the OID is case-insensitive and requires a non-empty OID, so
/// two entries with no reported HEAD never match each other.
/// Test: `worktree_targets_matches_branch_and_detached_head`,
/// `worktree_targets_never_returns_the_main_checkout`,
/// `worktree_targets_ignores_an_empty_head_oid`,
/// `worktree_targets_includes_a_round_sibling`.
pub fn worktree_targets<'a>(
    entries: &'a [WorktreeEntry],
    branch: &str,
    head_oid: &str,
    repo_root: &Path,
) -> Vec<&'a WorktreeEntry> {
    let branch = branch.trim();
    let oid = head_oid.trim();
    entries
        .iter()
        .filter(|e| e.path != repo_root)
        .filter(|e| {
            let by_branch = e.branch.as_deref().is_some_and(|b| is_pr_branch(branch, b));
            let by_head = !oid.is_empty() && e.head.eq_ignore_ascii_case(oid);
            by_branch || by_head
        })
        .collect()
}

/// The local branches this pull request owns, head and round-N siblings.
///
/// Why: the sibling branch is the other half of what a round-2 fix leaves
/// behind — the worktree is one, `<branch>-r2` in `git branch` is the other,
/// and deleting only the tree leaves a branch nothing will ever delete.
/// What: reads `git branch --format='%(refname:short) %(objectname)'` output
/// and returns the names [`is_pr_branch`] relates to `head`, head first so the
/// caller deletes it before its siblings.
/// Test: `pr_branches_lists_head_then_siblings`,
/// `pr_branches_ignores_an_unrelated_branch`.
pub fn pr_branches(listing: &str, head: &str) -> Vec<String> {
    let head = head.trim();
    let mut out: Vec<String> = listing
        .lines()
        .filter_map(|l| l.split_whitespace().next())
        .filter(|name| is_pr_branch(head, name))
        .map(str::to_string)
        .collect();
    out.sort_by_key(|n| (n != head, n.clone()));
    out
}

/// The `worktree-agent-*` branches whose tip is this PR's head commit.
///
/// Why: the harness mints one of these per dispatched agent and nothing else
/// deletes them. Requiring the tip to EQUAL the merged head is what keeps the
/// match narrow: an agent branch that has moved on since the merge carries
/// commits the PR never contained, and deleting it would discard them.
/// What: reads `git branch --format='%(refname:short) %(objectname)'` output;
/// returns the short names carrying [`AGENT_BRANCH_PREFIX`] whose object name
/// matches `head_oid` case-insensitively. An empty `head_oid` matches nothing.
/// Test: `agent_branches_at_matches_only_the_agent_prefix`,
/// `agent_branches_at_ignores_a_branch_that_moved_on`,
/// `agent_branches_at_returns_nothing_for_an_empty_oid`.
pub fn agent_branches_at(listing: &str, head_oid: &str) -> Vec<String> {
    let oid = head_oid.trim();
    if oid.is_empty() {
        return Vec::new();
    }
    listing
        .lines()
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            let name = parts.next()?;
            let object = parts.next()?;
            (name.starts_with(AGENT_BRANCH_PREFIX) && object.eq_ignore_ascii_case(oid))
                .then(|| name.to_string())
        })
        .collect()
}

/// The outcome of one cleanup step.
///
/// Why: the owner's ruling is that cleanup is deterministic unless it errors,
/// so a step has exactly two outcomes and there is deliberately no third
/// "warning" variant — a step that could not do its job is a failure, and
/// "there was nothing to do" is a success carrying that detail.
/// Test: `step_line_renders_ok_and_failed`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum StepStatus {
    /// The step did its job, or found nothing left to do.
    Ok,
    /// The step could not do its job.
    Failed,
}

/// One printed line of a cleanup run.
///
/// Test: `step_line_renders_ok_and_failed`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StepLine {
    /// The step's stable name, e.g. `worktree`.
    pub step: &'static str,
    /// Whether it succeeded.
    pub status: StepStatus,
    /// What it did, or why it could not.
    pub detail: String,
}

impl StepLine {
    /// A step that did its job.
    pub fn ok(step: &'static str, detail: impl Into<String>) -> Self {
        Self {
            step,
            status: StepStatus::Ok,
            detail: detail.into(),
        }
    }

    /// A step that could not do its job.
    pub fn failed(step: &'static str, detail: impl Into<String>) -> Self {
        Self {
            step,
            status: StepStatus::Failed,
            detail: detail.into(),
        }
    }

    /// Whether this line reports a failure.
    pub fn is_failure(&self) -> bool {
        self.status == StepStatus::Failed
    }

    /// The one line an operator sees.
    ///
    /// Test: `step_line_renders_ok_and_failed`.
    pub fn render(&self) -> String {
        let tag = match self.status {
            StepStatus::Ok => "ok",
            StepStatus::Failed => "FAILED",
        };
        format!("{}: {tag} — {}", self.step, self.detail)
    }
}
