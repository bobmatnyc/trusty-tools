//! The Disk dashboard's worktree classification and payload (#6927).
//!
//! Why: DOC-73 §16.2 renders every worktree in one of three tiers, and §16.4
//! states console reaches trusty-mpm only through MCP tools. This module is the
//! tier decision plus the JSON shape §16.5 specifies; [`super::survey_run`]
//! gathers the facts it judges.
//! What: [`classify_tier`] turns one worktree's facts into a
//! [`WorktreeTier`] and the [`Reason`]s behind it, and the `Disk*` structs are
//! the serialized payload.
//! Test: `super::survey_tests`.
//!
//! # Where this differs from `worktree_reclaim::classify`, and why
//!
//! The two answer different questions and the difference is deliberate.
//! `classify` answers "may this be DELETED?" and is the only input to the
//! delete loop; this module answers "what should the operator SEE?" and deletes
//! nothing. Two consequences:
//!
//! - **A branchless worktree can read `stale` here and still be `Blocked`
//!   there.** The owner's ruling for this view is that a worktree with no
//!   branch counts as landed when it is clean AND holds nothing unpushed —
//!   where "nothing unpushed" is `inspect_dirt`'s own no-upstream arm,
//!   `rev-list HEAD --not --remotes`, i.e. every commit on `HEAD` is already
//!   reachable from a remote. A tree that is contained in the mainline holds
//!   nothing a deletion could destroy. `classify`'s gate 5 still demands a
//!   MERGED pull request, and this change does not widen it: widening a
//!   destructive gate is not this issue's to do, and #6930's clear action must
//!   apply `classify`, never this tier.
//! - **Reasons are a LIST, not a first-gate-wins verdict.** `classify` returns
//!   the first gate that refused, which is right for a refusal and wrong for a
//!   detail panel: an operator looking at a KEEP worktree wants every reason it
//!   is kept. The tier itself is still first-match-wins, in the order below.
//!
//! What the two share is direction: every condition this module reports as KEEP
//! is one `classify` also refuses on, so nothing shown `stale` is narrower than
//! what the deleter will accept — asserted by
//! `a_reclaimable_verdict_is_always_shown_stale`.

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::session_manager::worktree_keep_list::KeepList;
use crate::session_manager::worktree_reclaim::{BranchPrState, ReclaimGate, ReclaimVerdict};
use crate::session_manager::worktree_reclaim_claim::ClaimState;
use crate::session_manager::worktree_registry::Admission;
use crate::session_manager::worktree_safety::DirtyWorktree;

/// The tier a worktree renders in (DOC-73 §16.2, plus `missing`).
///
/// Why: colour in the sunburst means exactly one thing, so the tier set is
/// closed and small. `Missing` is the fourth because a stale POINTER — a
/// `git worktree list` record whose directory is gone — is not a stale
/// worktree: there is nothing on disk to clear, and showing it green would
/// advertise bytes that do not exist.
/// What: `Stale` (safe to clear), `Review`, `Keep`, `Missing`.
/// Test: `a_merged_clean_worktree_is_stale`, `a_missing_pointer_is_missing`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum WorktreeTier {
    /// Landed, clean, unclaimed, removable, and not kept.
    Stale,
    /// Nothing forbids clearing it, but nothing proves its work landed.
    Review,
    /// Something forbids clearing it — see the reasons.
    Keep,
    /// Git lists it, but the directory is gone.
    Missing,
}

/// Why a worktree landed in its tier.
///
/// Why: the owner's ruling names these by name — a worktree with uncommitted or
/// unpushed work is never stale, and the tool must return WHY. Codes rather
/// than prose so the console can style them and a test can assert on them
/// without matching a message string, which is the coupling `ReclaimGate`
/// exists to avoid.
/// Test: `a_dirty_worktree_is_kept_and_says_dirty`,
/// `an_unpushed_worktree_is_kept_and_says_unpushed`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum ReasonCode {
    /// The operator's keep-list names it.
    KeepList,
    /// The working tree holds uncommitted changes.
    Dirty,
    /// `HEAD` holds commits that are on no remote.
    Unpushed,
    /// A live session claims the directory.
    LiveSession,
    /// A dispatched agent owns the directory.
    AgentOwned,
    /// The branch's pull-request state does not prove the work landed.
    UnknownBranchState,
    /// `prune-worktrees` could not remove it even if asked.
    NotRemovable,
    /// Git itself excludes it — the main checkout, bare, or operator-locked.
    NotAdmitted,
    /// Git lists the path; the directory is not there.
    Missing,
    /// A merged pull request proves the branch's work landed.
    MergedPr,
    /// No branch, and every commit on `HEAD` is already on a remote.
    ContainedInMainline,
}

/// One reason, with the detail that makes it actionable.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct Reason {
    /// The machine-readable code.
    pub code: ReasonCode,
    /// One line an operator can act on.
    pub detail: String,
}

impl Reason {
    /// Build a reason.
    fn new(code: ReasonCode, detail: impl Into<String>) -> Self {
        Self {
            code,
            detail: detail.into(),
        }
    }
}

/// The tier plus every reason established for it.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct Classification {
    /// Which tier this worktree renders in.
    pub tier: WorktreeTier,
    /// Every reason established, in the order they were checked.
    pub reasons: Vec<Reason>,
}

/// Everything [`classify_tier`] judges, gathered by [`super::survey_run`].
///
/// Why: a struct rather than eight parameters, because the whole point of the
/// split is that the decision is PURE and a test can state one worktree's
/// situation in one literal.
/// What: the facts the git/`gh` probes produced, plus the reclaim verdict
/// `worktree_reclaim::classify` reached from the same facts.
pub(crate) struct WorktreeFacts<'a> {
    /// The worktree directory git reports.
    pub path: &'a Path,
    /// Short branch name, or `None` when detached.
    pub branch: Option<&'a str>,
    /// Git's own admission verdict.
    pub admission: Admission,
    /// Which session, if any, claims this path.
    pub claim: &'a ClaimState,
    /// What the pull-request index said about the branch.
    pub pr: &'a BranchPrState,
    /// The reclaim verdict reached from these same facts.
    pub verdict: &'a ReclaimVerdict,
}

/// Decide one worktree's tier and the reasons behind it (#6927).
///
/// Why: this is the owner's staleness ruling, written so each conjunct is its
/// own testable branch. "Stale" requires ALL of: landed (a merged pull request,
/// or no branch at all with every commit already on a remote); a clean working
/// tree; nothing unpushed; no live session claim; no live agent; git admits it;
/// trusty-mpm could remove it; and it is not on the keep-list. Anything short
/// of that is `Keep` when something FORBIDS clearing it and `Review` when
/// nothing forbids it but nothing proves it landed either.
/// What: first-match-wins over the tiers, in this order — keep-list, missing
/// directory, unsaved work, live claim, agent ownership, git's admission,
/// removability, landing evidence. `probe_dirt` is a closure for the same
/// reason `classify`'s is: a caller with no answer must say so by probing, not
/// by passing a `None` that reads as "checked and clean".
/// Test: `a_merged_clean_worktree_is_stale`,
/// `a_branchless_contained_worktree_is_stale`,
/// `a_dirty_worktree_is_kept_and_says_dirty`,
/// `an_unpushed_worktree_is_kept_and_says_unpushed`,
/// `a_live_sessions_worktree_is_kept`,
/// `a_keep_listed_merged_worktree_is_never_stale`,
/// `a_missing_pointer_is_missing`, `an_open_pr_is_review`,
/// `a_reclaimable_verdict_is_always_shown_stale`.
pub(crate) fn classify_tier(
    facts: &WorktreeFacts<'_>,
    keep_list: &KeepList,
    probe_dirt: &dyn Fn(&Path) -> Option<DirtyWorktree>,
) -> Classification {
    // The keep-list outranks every other answer (DOC-73 §16.4), and costs no
    // subprocess, so it is asked first here exactly as it is in `classify`.
    if let Some(kept) = keep_list.keeps(facts.path) {
        return keep(vec![Reason::new(ReasonCode::KeepList, kept.detail())]);
    }
    // A pointer whose directory is gone holds no bytes to clear, so it is
    // neither stale nor kept — `git worktree prune` is its remedy, not this
    // dashboard's clear action.
    if facts.admission == Admission::Prunable || !facts.path.exists() {
        return Classification {
            tier: WorktreeTier::Missing,
            reasons: vec![Reason::new(
                ReasonCode::Missing,
                "git lists this worktree but the directory is gone — `git worktree prune` \
                 removes the registration",
            )],
        };
    }
    let mut reasons = Vec::new();
    // Unsaved work outranks everything below it: the owner's ruling is that a
    // worktree holding uncommitted or unpushed work is NEVER stale.
    if let Some(dirt) = probe_dirt(facts.path) {
        if dirt.dirty_files > 0 {
            reasons.push(Reason::new(
                ReasonCode::Dirty,
                format!("{} uncommitted working-tree entries", dirt.dirty_files),
            ));
        }
        if dirt.unpushed_commits > 0 {
            reasons.push(Reason::new(
                ReasonCode::Unpushed,
                format!("{} commits are on no remote", dirt.unpushed_commits),
            ));
        }
        // `inspect_dirt` fails toward dirty, so a probe that could not answer
        // returns `Some` with both counts at zero. That is unsaved work as far
        // as this view is concerned, and it keeps its own reason string.
        if dirt.dirty_files == 0 && dirt.unpushed_commits == 0 {
            reasons.push(Reason::new(ReasonCode::Dirty, dirt.reason.clone()));
        }
    }
    if let Some(session) = claiming_session(facts.claim) {
        reasons.push(Reason::new(
            ReasonCode::LiveSession,
            format!("session {session} claims this workspace"),
        ));
    }
    if let ReclaimVerdict::BlockedByAgent { reason, .. } = facts.verdict {
        reasons.push(Reason::new(ReasonCode::AgentOwned, reason.clone()));
    }
    if facts.admission != Admission::Admitted {
        reasons.push(Reason::new(
            ReasonCode::NotAdmitted,
            facts.admission.reason(),
        ));
    }
    if let ReclaimVerdict::Blocked {
        gate: ReclaimGate::Removability,
        reason,
    } = facts.verdict
    {
        reasons.push(Reason::new(ReasonCode::NotRemovable, reason.clone()));
    }
    if !reasons.is_empty() {
        return keep(reasons);
    }
    // Nothing forbids clearing it. Does anything prove its work landed?
    match (facts.pr, facts.branch) {
        (BranchPrState::Merged { pr }, _) => Classification {
            tier: WorktreeTier::Stale,
            reasons: vec![Reason::new(
                ReasonCode::MergedPr,
                format!("PR #{pr} merged; tree clean and fully pushed"),
            )],
        },
        // The owner's branchless case: nothing to attribute a pull request to,
        // but the dirt probe already proved every commit on `HEAD` is on a
        // remote, so the directory holds nothing the mainline does not.
        (_, None) => Classification {
            tier: WorktreeTier::Stale,
            reasons: vec![Reason::new(
                ReasonCode::ContainedInMainline,
                "no branch, tree clean, and every commit on HEAD is already on a remote",
            )],
        },
        (pr, Some(branch)) => Classification {
            tier: WorktreeTier::Review,
            reasons: vec![Reason::new(
                ReasonCode::UnknownBranchState,
                unlanded_detail(pr, branch),
            )],
        },
    }
}

/// A `Keep` classification carrying `reasons`.
fn keep(reasons: Vec<Reason>) -> Classification {
    Classification {
        tier: WorktreeTier::Keep,
        reasons,
    }
}

/// The session whose claim FORBIDS clearing, or `None`.
///
/// Why: `ClaimState::CallerNested` is a claim that permits — the #6806 case
/// where a session prunes worktrees it created inside its own workspace — so
/// treating "a claim exists" as "kept" would show a session its own reclaimable
/// worktrees as KEEP. The four-way state already resolves this; this reads it.
/// Test: `a_live_sessions_worktree_is_kept`,
/// `a_callers_own_nested_worktree_is_not_kept_for_liveness`.
fn claiming_session(claim: &ClaimState) -> Option<&str> {
    match claim {
        ClaimState::Unclaimed | ClaimState::CallerNested { .. } => None,
        ClaimState::CallerWorkspace { session } | ClaimState::Foreign { session, .. } => {
            Some(session)
        }
    }
}

/// One line saying why a branch's state is not landing evidence.
fn unlanded_detail(pr: &BranchPrState, branch: &str) -> String {
    match pr {
        BranchPrState::Open { pr } => format!("PR #{pr} for `{branch}` is still open"),
        BranchPrState::ClosedUnmerged { pr } => {
            format!("PR #{pr} for `{branch}` was closed without merging")
        }
        BranchPrState::NoPr => format!("no pull request found for `{branch}`"),
        BranchPrState::Unknown => {
            format!("the pull-request state of `{branch}` could not be determined")
        }
        BranchPrState::LookupFailed { reason } => {
            format!("the pull-request lookup for `{branch}` failed: {reason}")
        }
        // Unreachable: the caller matched `Merged` first. Worded rather than
        // `unreachable!` so a future reorder degrades into a readable string
        // instead of a panic inside a read-only report.
        BranchPrState::Merged { pr } => format!("PR #{pr} for `{branch}` merged"),
    }
}

/// The pull request a worktree's branch is attached to, when there is one.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct PrRef {
    /// The pull-request number.
    pub number: u64,
    /// `merged` / `open` / `closed_unmerged`.
    pub state: &'static str,
}

impl PrRef {
    /// The renderable pull-request reference for a branch state, if any.
    pub(crate) fn from_state(pr: &BranchPrState) -> Option<Self> {
        match pr {
            BranchPrState::Merged { pr } => Some(Self {
                number: *pr,
                state: "merged",
            }),
            BranchPrState::Open { pr } => Some(Self {
                number: *pr,
                state: "open",
            }),
            BranchPrState::ClosedUnmerged { pr } => Some(Self {
                number: *pr,
                state: "closed_unmerged",
            }),
            BranchPrState::NoPr | BranchPrState::Unknown | BranchPrState::LookupFailed { .. } => {
                None
            }
        }
    }
}

/// How a worktree's byte figure was obtained (#6926 passthrough).
///
/// Why: DOC-73 §16.5's `generated_at` exists because a cached figure must
/// disclose its own age, and the index's own bounds can cut a walk short. Both
/// ride on the row rather than on the survey, because they differ per worktree.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct SizeNote {
    /// Served from the index's cache with no filesystem access.
    pub from_cache: bool,
    /// A depth or wall-clock bound cut the walk short; `bytes` is a floor.
    pub truncated: bool,
    /// When the walk behind the figure ran, RFC 3339.
    pub measured_at: String,
    /// How many directories the OS refused, whose contents are uncounted.
    pub unreadable: usize,
}

/// One worktree row, as DOC-73 §16.5 shapes it.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct DiskWorktree {
    /// Stable identity. The PATH is the id: git's registry is keyed by it and
    /// no second identifier space exists to disagree with.
    pub id: String,
    /// The worktree directory.
    pub path: PathBuf,
    /// Short branch name, or `None` when detached.
    pub branch: Option<String>,
    /// Which tier this renders in.
    pub tier: WorktreeTier,
    /// Every reason established for that tier.
    pub reasons: Vec<Reason>,
    /// The gate `worktree_reclaim::classify` refused at, or `None` when it
    /// found the worktree reclaimable.
    pub gate: Option<ReclaimGate>,
    /// That refusal's own wording, verbatim.
    pub reason: Option<String>,
    /// Whether the DELETE path would accept this worktree today.
    pub reclaimable: bool,
    /// Bytes on disk, or `None` when the index refused or could not measure.
    pub bytes: Option<u64>,
    /// How that figure was obtained, absent when there is no figure.
    pub size: Option<SizeNote>,
    /// The branch's pull request, when it has one.
    pub pr: Option<PrRef>,
    /// The session claiming this workspace, when one does.
    pub session: Option<String>,
}

/// One project and the worktrees under it.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct DiskProject {
    /// `<owner>/<repo>`, derived from the managed project directory.
    pub name: String,
    /// The managed project directory.
    pub path: PathBuf,
    /// Bytes under `path`, including the main checkout and `.base`.
    pub bytes: Option<u64>,
    /// How that figure was obtained.
    pub size: Option<SizeNote>,
    /// Every worktree both anchors register, in path order.
    pub worktrees: Vec<DiskWorktree>,
}

/// Per-tier counts, so a caller need not fold the rows to render a legend.
#[derive(Debug, Clone, Copy, Default, Serialize)]
pub(crate) struct TierCounts {
    /// Worktrees safe to clear.
    pub stale: usize,
    /// Worktrees needing an operator decision.
    pub review: usize,
    /// Worktrees something forbids clearing.
    pub keep: usize,
    /// Registrations whose directory is gone.
    pub missing: usize,
}

/// The workspace root and everything under it.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct DiskRoot {
    /// The managed workspace root that was scanned.
    pub path: PathBuf,
    /// Bytes under `path`, or `None` when the index refused it.
    pub bytes: Option<u64>,
    /// How that figure was obtained.
    pub size: Option<SizeNote>,
    /// Registered projects, in path order.
    pub projects: Vec<DiskProject>,
    /// Per-tier counts across every project.
    pub counts: TierCounts,
    /// Bytes held by the `stale` worktrees that were measured.
    pub stale_bytes: u64,
    /// How many `stale` worktrees contributed to `stale_bytes`.
    ///
    /// Why: `stale_bytes` is a sum over the MEASURED subset. Reporting it
    /// without saying how much of the set it covers is the
    /// `reclaimable_bytes` mistake #2919 spent a review round on.
    pub stale_measured: usize,
}

/// What the operator's keep-list contained, and what of it did not take effect.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct KeepListReport {
    /// The patterns as configured.
    pub patterns: Vec<String>,
    /// Patterns that would not compile, as `"<pattern>: <error>"`. A non-empty
    /// list means the operator believes something is protected that is not.
    pub invalid: Vec<String>,
    /// Why the config could not be read at all, when it could not be (#6927).
    ///
    /// Why the console must render this: a `Some` here means the keep-list is
    /// in its fail-closed state — EVERY worktree is kept and nothing can be
    /// reclaimed until the operator fixes their config. Without the field the
    /// dashboard would show a plausible all-`keep` view with no way to tell it
    /// apart from a workspace that genuinely has nothing to clear.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// The whole Disk survey — DOC-73 §16.5's `GET /api/console/disk/tree` body.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct DiskSurvey {
    /// When this survey ran, RFC 3339. Byte figures may be older; each row's
    /// `size.measured_at` says how much older.
    pub generated_at: String,
    /// The keep-list this survey applied.
    pub keep_list: KeepListReport,
    /// The scanned workspace root.
    pub root: DiskRoot,
}

#[cfg(test)]
#[path = "survey_tests.rs"]
mod survey_tests;
