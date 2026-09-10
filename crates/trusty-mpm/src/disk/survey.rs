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

use std::collections::BTreeMap;
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
        // #7232: a discarded dead claim forbids nothing — the session it names
        // no longer exists, so showing the worktree as KEEP would be as wrong
        // as the `CallerNested` case above.
        ClaimState::Unclaimed
        | ClaimState::CallerNested { .. }
        | ClaimState::DeadClaimsDiscarded { .. } => None,
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
    ///
    /// This is the LIVE claim alone, and it stays that way: a console built
    /// against #6927 reads it as "a running session is sitting here". #7313's
    /// attribution is [`Self::owning_session`], beside it rather than in it.
    pub session: Option<String>,
    /// The session this worktree's bytes are charged to (#7313).
    ///
    /// Why: [`Self::session`] answers only for a LIVE claim, so every worktree
    /// an ended session left behind was attributed to nobody — which is the
    /// whole of what #7313 reports. A worktree the harness created for a
    /// dispatched agent carries the dispatching session in its durable
    /// `.trusty-mpm-worktree` sentinel, and that record outlives the session,
    /// so an ended session's leftovers are still attributable.
    /// What: the path-overlap claim's session when a live one holds this path;
    /// otherwise the sentinel's owner — an agent worktree's
    /// `agent.parent_session_id`, or a session worktree's `owner_session_id`;
    /// otherwise `None`, which the grouping renders as its unattributed bucket.
    /// Test: `a_sentinel_attributes_an_ended_sessions_worktree`,
    /// `a_live_claim_outranks_the_sentinel_for_attribution`.
    pub owning_session: Option<String>,
    /// Bytes held by this worktree's top-level `target*` build directories
    /// (#7313), or `None` when no complete figure could be obtained.
    ///
    /// Why: a build directory is the reclaimable majority of a worktree's
    /// bytes, and an operator deciding what to clear wants that split rather
    /// than one total. The convention this matches is
    /// [worktree-discipline.md](../../../../docs/reference/worktree-discipline.md):
    /// an agent is given an absolute `CARGO_TARGET_DIR` inside its own
    /// worktree, so the directories are `target`, `target-worktree`, and
    /// `target-<issue-number>` — a prefix match, never a fixed name.
    /// What: the sum of [`DiskProbes::measure`](super::survey_run::DiskProbes)
    /// over the top-level entries whose name starts with `target`, so the
    /// figures come from the SAME budgeted, cached index that produced
    /// [`Self::bytes`] — never a second walker. `Some(0)` when the worktree has
    /// no such directory; `None` when the directory could not be listed, a
    /// matched entry produced no figure, or the deadline was spent, because a
    /// short sum presented as a total is worse than no sum. These directories
    /// are inside the worktree, so they are already part of `bytes`, and
    /// [`Self::size`]'s provenance covers both walks.
    /// Test: `build_dir_bytes_counts_target_dirs_and_nothing_else`.
    pub build_dir_bytes: Option<u64>,
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

/// What roll-up the caller asked for, beside the per-project tree (#7313).
///
/// Why an enum rather than a bool: the tool's `group_by` argument is a string
/// with one accepted value today, and a second grouping (by project, by tier)
/// is the obvious next ask. A bool would have to be renamed to add one.
/// What: [`None`](Self::None) leaves [`DiskSurvey::by_session`] absent, so a
/// console built against #6927 sees a byte-identical payload;
/// [`Session`](Self::Session) fills it, possibly with an empty list.
/// Test: `by_session_is_absent_unless_group_by_is_asked_for`,
/// `a_sentinel_attributes_an_ended_sessions_worktree`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GroupBy {
    /// No roll-up — the per-project tree alone.
    None,
    /// Roll worktrees up by the session that owns them.
    Session,
}

/// One session's whole footprint, rolled up across every project (#7313).
///
/// Why: the owner's ask is "report disk usage by session", and a session's
/// worktrees are not confined to one project — an agent isolation worktree, an
/// install/verify throwaway tree, and a `jobs/<id>/tmp/` tree can sit under
/// three different repositories while belonging to one dispatching session. The
/// per-project tree cannot express that, so this is a second index over the same
/// rows rather than a reshaping of them.
/// What: the totals and the tier split for one `session_id`, plus the paths that
/// produced them so a caller can drill back into the tree without re-folding it.
/// A `None` `session_id` is the bucket for worktrees nothing attributed.
/// Test: `a_sentinel_attributes_an_ended_sessions_worktree`,
/// `by_session_sorts_by_bytes_and_buckets_the_unattributed`.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct DiskSessionGroup {
    /// The owning session, or `None` for the unattributed bucket.
    pub session_id: Option<String>,
    /// Bytes across this session's MEASURED worktrees. A worktree carrying no
    /// figure contributes nothing and is still counted in `worktree_count`, so
    /// the two disagreeing is the signal that the pass was budget-limited.
    pub bytes: u64,
    /// Build-directory bytes across the worktrees that reported a figure.
    pub build_dir_bytes: u64,
    /// How many worktrees this session owns, measured or not.
    pub worktree_count: usize,
    /// The tier split, so a caller renders a legend without folding the rows.
    pub tiers: TierCounts,
    /// The worktree paths behind these totals, in path order.
    pub worktree_paths: Vec<PathBuf>,
}

/// Roll every worktree row up by its owning session (#7313).
///
/// Why: pure, so the ordering contract and the unattributed bucket are testable
/// without a filesystem — the attribution itself is [`super::survey_run`]'s, and
/// this only folds what it decided.
/// What: one group per distinct [`DiskWorktree::owning_session`], sorted by
/// `bytes` descending so the console's first row is the session worth
/// reclaiming; ties break on the session id, with the `None` bucket last, so the
/// order is total rather than merely mostly-determined.
/// Test: `by_session_sorts_by_bytes_and_buckets_the_unattributed`.
pub(crate) fn group_by_session(projects: &[DiskProject]) -> Vec<DiskSessionGroup> {
    let mut groups: BTreeMap<Option<String>, DiskSessionGroup> = BTreeMap::new();
    for wt in projects.iter().flat_map(|p| p.worktrees.iter()) {
        let group = groups
            .entry(wt.owning_session.clone())
            .or_insert_with(|| DiskSessionGroup {
                session_id: wt.owning_session.clone(),
                bytes: 0,
                build_dir_bytes: 0,
                worktree_count: 0,
                tiers: TierCounts::default(),
                worktree_paths: Vec::new(),
            });
        group.bytes = group.bytes.saturating_add(wt.bytes.unwrap_or(0));
        group.build_dir_bytes = group
            .build_dir_bytes
            .saturating_add(wt.build_dir_bytes.unwrap_or(0));
        group.worktree_count += 1;
        match wt.tier {
            WorktreeTier::Stale => group.tiers.stale += 1,
            WorktreeTier::Review => group.tiers.review += 1,
            WorktreeTier::Keep => group.tiers.keep += 1,
            WorktreeTier::Missing => group.tiers.missing += 1,
        }
        group.worktree_paths.push(wt.path.clone());
    }
    let mut out: Vec<DiskSessionGroup> = groups.into_values().collect();
    out.sort_by(|a, b| {
        b.bytes
            .cmp(&a.bytes)
            // `None` sorts after every `Some`, which is where the unattributed
            // bucket belongs when its byte total ties a real session's.
            .then_with(|| a.session_id.is_none().cmp(&b.session_id.is_none()))
            .then_with(|| a.session_id.cmp(&b.session_id))
    });
    out
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
    /// Whether the deadline stopped the pass before it finished (#6929).
    ///
    /// Why the console must render this: a truncated pass is still a 200 with
    /// every worktree listed, which is the point — but the rows it ran out of
    /// time on read `review` / `unknown-branch-state`, and a project or root it
    /// ran out of time on reports no bytes at all. Without this flag those are
    /// indistinguishable from a fleet that genuinely has nothing stale and
    /// nothing measurable, and an operator would read a partial answer as the
    /// whole one. `false` means every worktree was inspected and every
    /// aggregate was offered to the index inside the budget.
    /// Test: `a_survey_reports_whether_its_deadline_truncated_the_pass`.
    pub partial: bool,
    /// The keep-list this survey applied.
    pub keep_list: KeepListReport,
    /// The scanned workspace root.
    pub root: DiskRoot,
    /// Per-session roll-up, present only when the caller asked for it (#7313).
    ///
    /// Why it is absent rather than empty by default: #6927's console reads this
    /// payload today, and the survey already costs a git and `gh` pass per
    /// worktree. Serializing a second index nobody asked for would change every
    /// existing consumer's payload to pay for a view none of them render.
    /// Test: `by_session_is_absent_unless_group_by_is_asked_for`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub by_session: Option<Vec<DiskSessionGroup>>,
}

#[cfg(test)]
#[path = "survey_tests.rs"]
mod survey_tests;
