//! Merged-PR worktree reclamation and disk accounting (#2919).
//!
//! Why: `.base/.claude/worktrees` reached **1.1 TiB across 77 entries** on
//! 2026-07-21, four days after a manual sweep had reclaimed the same 1.1 TiB.
//! Nothing was leaking — worktrees were simply never reclaimed, because the
//! merge of a workstream's PR (DOC-52 §3.4: "closing a workstream is THE
//! trigger for resource cleanup") fired no cleanup at all. `prune.rs` already
//! reclaims worktrees whose *session* is provably gone; it has no notion of a
//! *branch* whose PR merged, which is the far more common terminal state.
//! This module adds that missing signal, plus the disk accounting the
//! post-mortem asked for ("report real before/after numbers").
//!
//! What: a git-authoritative survey (ADR-0023 point 1 — `git worktree list
//! --porcelain` decides existence, never a directory walk) that pairs every
//! registered worktree with its branch's pull-request state and its on-disk
//! byte count, and classifies it [`Reclaimable`](ReclaimVerdict::Reclaimable)
//! ONLY when six independent gates all pass. Removal is a separate,
//! non-default [`ReclaimMode::Remove`] opt-in; the survey itself never deletes.
//!
//! FAIL-CLOSED, by construction: [`classify`] reaches `Reclaimable` only by
//! falling off the end of every gate. Each gate `return`s a `Blocked` verdict,
//! so a gate that errors, times out, or cannot answer produces a refusal — the
//! failure branch can never advance to deletion. In particular a PR state that
//! could not be determined ([`BranchPrState::Unknown`]) blocks, a lookup that
//! FAILED ([`BranchPrState::LookupFailed`], #6561) blocks and says why, and so
//! does an index that may have been TRUNCATED (an absent branch is only `NoPr`
//! when the index is known complete).
//!
//! Test: `worktree_reclaim_tests` — one refusal test per gate, each of which
//! fails if its gate is deleted.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde::{Deserialize, Serialize};

// #6561: the `gh` runner lives next door so this file stays under the SLOC cap;
// the re-import keeps every call site (and `super::*` in the tests) unchanged.
use super::worktree_ownership::{
    AgentDelegationState, AgentWorktreeOwner, SentinelOwner, is_harness_agent_worktree,
    read_sentinel_owner,
};
use super::worktree_reclaim_gh::{
    GH_TIMEOUT, PR_JSON_FIELDS, gh_command, resolve_daemon_gh_env, run_with_timeout,
};
use super::worktree_registry::{Admission, HarnessLockState, harness_lock_state};
use super::worktree_safety::DirtyWorktree;
// #6507: the verdict vocabulary lives next door so this file stays under the
// SLOC cap; the re-export keeps every call site (and `super::*` in the tests)
// unchanged.
pub(crate) use super::worktree_reclaim_verdict::{ReclaimGate, ReclaimVerdict};

/// Resolves the delegation registry's answer for the agent a sentinel names.
///
/// Why: named so the four call sites that pass one around agree on the shape,
/// and so a caller that has no registry to consult has to say so explicitly by
/// returning [`AgentDelegationState::Unknown`] rather than by passing an empty
/// list that reads as "nobody claims it".
pub(crate) type AgentStateProbe<'a> = &'a dyn Fn(&AgentWorktreeOwner) -> AgentDelegationState;

/// How many pull requests one `gh pr list` call retrieves (#2919).
///
/// Why: the index is built once per repository rather than once per worktree —
/// 77 worktrees would otherwise mean 77 network round-trips. The limit has to
/// be large enough that a normal repository's whole PR history fits, because a
/// TRUNCATED index cannot tell "this branch has no PR" from "this branch's PR
/// fell off the end", and the safe reading of the latter is
/// [`BranchPrState::Unknown`].
/// What: 400. [`PrIndex::from_json`] marks the index INCOMPLETE when the reply
/// holds exactly this many rows, which downgrades every absent branch to
/// `Unknown` (i.e. blocks) instead of to `NoPr`.
/// Test: `pr_index_truncated_reply_makes_absent_branches_unknown`.
pub(crate) const PR_INDEX_LIMIT: usize = 400;

/// The pull-request state of the branch a worktree has checked out (#2919).
///
/// Why: only ONE of these six states is evidence that the worktree's work has
/// landed and the directory is disposable. Modelling the other five explicitly
/// — rather than as `Option<bool>` — is what keeps "we could not find out"
/// from collapsing into "there is nothing here", which is the fail-open shape
/// this repository keeps re-encountering.
/// What: `Merged` carries the PR number that proves the branch landed. `Open`
/// and `ClosedUnmerged` are active/abandoned-but-unlanded branches. `NoPr` is
/// asserted only against a KNOWN-COMPLETE index. `Unknown` is the answerable-
/// but-unanswered case: a truncated index, or a detached HEAD with no branch to
/// attribute a PR to. `LookupFailed` is the BROKEN case — `gh` missing,
/// unauthenticated, timing out, or erroring — and carries its reason (#6561).
/// Test: `classify_blocks_open_pr`, `classify_blocks_closed_unmerged_pr`,
/// `classify_blocks_no_pr`, `classify_blocks_unknown_pr_state`,
/// `classify_blocks_a_failed_lookup_and_names_the_reason`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum BranchPrState {
    /// A pull request for this branch was merged.
    Merged {
        /// The merged pull request's number.
        pr: u64,
    },
    /// A pull request for this branch is still open — work in flight.
    Open {
        /// The open pull request's number.
        pr: u64,
    },
    /// A pull request for this branch was closed WITHOUT merging.
    ClosedUnmerged {
        /// The closed pull request's number.
        pr: u64,
    },
    /// A known-complete index holds no pull request for this branch.
    NoPr,
    /// The state could not be determined — always blocks reclamation.
    Unknown,
    /// The lookup itself FAILED, carrying `gh`'s own one-line complaint (#6561).
    ///
    /// Why: this used to be `Unknown`, and the collapse hid a whole-workspace
    /// outage. The daemon carries neither `GH_TOKEN` nor `GH_CONFIG_DIR`, so
    /// `gh pr list` exited 4 with `To get started with GitHub CLI, please run:
    /// gh auth login` for all 261 registered worktrees; the survey reported
    /// `0 reclaimable` and named no cause. An unanswerable branch (a detached
    /// HEAD, a truncated index) and a broken lookup need opposite operator
    /// actions, so they are now separate states.
    /// Test: `pr_state_for_branch_reports_a_failed_call_as_lookup_failed`,
    /// `survey_counts_a_failed_lookup_apart_from_an_unknown_state`.
    LookupFailed {
        /// One line: the exit code plus `gh`'s first stderr line.
        reason: String,
    },
}

impl BranchPrState {
    /// How strongly this state BLOCKS reclamation — higher wins a collision.
    ///
    /// Why: a branch can carry several pull requests over its life (merge PR
    /// #1, push again, open PR #2). Reclaiming on the merged one while a newer
    /// one is open would delete live work, so when one branch maps to several
    /// rows the most blocking state must win, not the newest or the first.
    /// What: `Open` (3) > `ClosedUnmerged` (2) > `Merged` (1); `NoPr`,
    /// `Unknown` and `LookupFailed` never come from a row so they rank 0.
    /// Test: `pr_index_open_pr_beats_a_merged_one_on_the_same_branch`.
    fn block_rank(&self) -> u8 {
        match self {
            Self::Open { .. } => 3,
            Self::ClosedUnmerged { .. } => 2,
            Self::Merged { .. } => 1,
            Self::NoPr | Self::Unknown | Self::LookupFailed { .. } => 0,
        }
    }
}

/// One row of `gh pr list --json number,headRefName,state,isCrossRepository`
/// (#2919).
#[derive(Debug, Deserialize)]
struct PrRow {
    /// The pull request number.
    number: u64,
    /// The branch the pull request was opened FROM.
    #[serde(rename = "headRefName")]
    head_ref_name: String,
    /// `OPEN`, `CLOSED`, or `MERGED`.
    state: String,
    /// True when the head branch lives in a FORK, not this repository.
    ///
    /// Why: `headRefName` alone is not an identity. A fork's PR from a branch
    /// called `fix/foo` says nothing about a LOCAL worktree on `fix/foo` — they
    /// are different branches in different repositories that happen to share a
    /// name. Attributing a fork's merge to a local worktree would authorise
    /// deleting work that never landed anywhere. `#[serde(default)]` means a
    /// `gh` too old to report the field leaves this `false`, which is the
    /// pre-existing behaviour and is caught by the same-repo assumption the
    /// field exists to break — so the field is also requested explicitly in
    /// [`PrIndex::from_gh`], and a `gh` that cannot supply it fails the whole
    /// call, yielding [`PrIndex::unavailable_because`].
    #[serde(default, rename = "isCrossRepository")]
    is_cross_repository: bool,
}

/// Branch → pull-request state for one repository (#2919).
///
/// Why: one `gh` call per repository instead of one per worktree, and — more
/// importantly — one place that knows whether the answer set is trustworthy.
/// An index that failed to build, or that may have been truncated, must not
/// let an absent branch read as "no PR", because "no PR" would otherwise be
/// indistinguishable from a merged PR that fell off the end of the page. Since
/// #6561 the two are also kept apart from each other: a failed build carries
/// `gh`'s own reason and reports it, a truncated one does not.
/// What: the branch map plus a `complete` flag. [`state_for`](Self::state_for)
/// returns `NoPr` for an absent branch ONLY when `complete` holds; otherwise
/// `Unknown`.
/// Test: `pr_index_absent_branch_is_no_pr_when_complete`,
/// `pr_index_truncated_reply_makes_absent_branches_unknown`,
/// `pr_index_reports_a_failed_lookup_with_its_reason`.
#[derive(Debug, Default)]
pub(crate) struct PrIndex {
    /// Branch name → the most blocking state observed for it.
    by_branch: BTreeMap<String, BranchPrState>,
    /// True only when the reply is known to hold every pull request.
    complete: bool,
    /// Why the lookup FAILED, when it did — `None` means it answered (#6561).
    ///
    /// Distinguishes an index that could not be built from one that was merely
    /// truncated: both leave a branch unresolved, but only the first is a fault
    /// the operator can fix.
    failure: Option<String>,
}

impl PrIndex {
    /// An index that answers nothing BECAUSE the lookup failed (#6561).
    ///
    /// Why: [`unavailable`](Self::unavailable) says "no answers" and nothing
    /// about why. Every branch it cannot resolve now reads
    /// [`BranchPrState::LookupFailed`] carrying `reason`, so the survey can
    /// count and print the cause instead of reporting a bare zero.
    /// Test: `pr_index_reports_a_failed_lookup_with_its_reason`.
    pub(crate) fn unavailable_because(reason: String) -> Self {
        Self {
            by_branch: BTreeMap::new(),
            complete: false,
            failure: Some(reason),
        }
    }

    /// Parse a `gh pr list --json …` reply into an index (#2919).
    ///
    /// Why: keeping the parse pure means the truncation rule and the
    /// most-blocking-state-wins rule are testable without a network or a `gh`
    /// binary, which is the only way the refusal paths get real coverage.
    /// What: unparsable JSON yields [`unavailable`](Self::unavailable). A reply
    /// holding exactly `limit` rows is treated as TRUNCATED (`complete:
    /// false`). Rows collapse per branch by [`BranchPrState::block_rank`]; an
    /// unrecognised `state` string is skipped rather than guessed at, which
    /// leaves its branch absent and therefore blocking under a truncated index
    /// and `NoPr` under a complete one — the latter is safe because an
    /// unrecognised state is by definition not proof of a merge.
    /// Test: `pr_index_reads_merged_open_and_closed_rows`,
    /// `pr_index_truncated_reply_makes_absent_branches_unknown`,
    /// `pr_index_malformed_json_is_unavailable`.
    pub(crate) fn from_json(stdout: &str, limit: usize) -> Self {
        let Ok(rows) = serde_json::from_str::<Vec<PrRow>>(stdout) else {
            // #6561: a zero exit that printed something other than the JSON is
            // a failed lookup, not an empty repository — say which.
            return Self::unavailable_because(
                "`gh` printed output that is not the pull-request JSON".to_string(),
            );
        };
        // #2919: a full page is indistinguishable from a truncated one, so it
        // is treated as truncated. Erring the other way would let a merged PR
        // that fell off the page read as `NoPr` — which blocks too, but only
        // by luck; this makes it blocking by rule.
        let complete = rows.len() < limit;
        let mut by_branch: BTreeMap<String, BranchPrState> = BTreeMap::new();
        for row in rows {
            // #2919: a fork's branch is not this repository's branch. Skipping
            // the row leaves the local branch absent, which resolves to `NoPr`
            // (complete index) or `Unknown` (truncated) — both refuse.
            if row.is_cross_repository {
                continue;
            }
            let state = match row.state.as_str() {
                "MERGED" => BranchPrState::Merged { pr: row.number },
                "OPEN" => BranchPrState::Open { pr: row.number },
                "CLOSED" => BranchPrState::ClosedUnmerged { pr: row.number },
                _ => continue,
            };
            let entry = by_branch.entry(row.head_ref_name).or_insert(state.clone());
            if state.block_rank() > entry.block_rank() {
                *entry = state;
            }
        }
        Self {
            by_branch,
            complete,
            failure: None,
        }
    }

    /// Build the index by asking `gh` about `registry_root`'s repository.
    ///
    /// Why: this is the ONLY I/O in the module's classification path, isolated
    /// here so every gate above it is a pure function.
    /// What: `gh pr list --state all --limit <PR_INDEX_LIMIT> --json
    /// number,headRefName,state,isCrossRepository`, run with its WORKING
    /// DIRECTORY set to `registry_root` (never `-C`, which `gh` does not
    /// have — see [`gh_command`]) and with the repository-redirecting
    /// environment stripped. A failure to spawn, a timeout, a non-zero exit, or
    /// unparsable output all yield
    /// [`unavailable_because`](Self::unavailable_because) carrying `gh`'s own
    /// one-line complaint (#6561) — never an empty-but-complete index, and
    /// since #6561 never a cause-free one either.
    /// Test: `gh_command_passes_no_dash_c_flag`,
    /// `gh_command_runs_in_the_requested_directory`,
    /// `gh_command_strips_repository_redirecting_env` (argv/env shape);
    /// `pr_index_from_gh_reads_this_repository` (a real successful call);
    /// `pr_index_malformed_json_is_unavailable` (failure-to-unavailable).
    pub(crate) fn from_gh(registry_root: &Path) -> Self {
        // #6623: resolved once per registry root — the daemon's own gh
        // identity, since launchd hands it neither `GH_TOKEN` nor
        // `GH_CONFIG_DIR`.
        let gh_env = resolve_daemon_gh_env(registry_root);
        let identity = gh_env.describe();
        let mut cmd = gh_command(registry_root, &gh_env);
        cmd.args(["pr", "list", "--state", "all", "--limit"])
            .arg(PR_INDEX_LIMIT.to_string())
            .args(["--json", PR_JSON_FIELDS]);
        match run_with_timeout(cmd, GH_TIMEOUT) {
            Ok(stdout) => {
                let index = Self::from_json(&stdout, PR_INDEX_LIMIT);
                // #2919: logged because "resolved 0 branches" is the signature
                // of a call that ran but answered nothing — the shape the bogus
                // `-C` flag produced, which was otherwise invisible.
                tracing::debug!(
                    root = %registry_root.display(),
                    branches = index.branch_count(),
                    complete = index.is_complete(),
                    "worktree-reclaim: built pull-request index (#2919)"
                );
                index
            }
            Err(reason) => {
                // #6561: WARN, not debug, and carrying the reason. A silent
                // debug line is why an auth failure across all 261 worktrees
                // surfaced only as "0 reclaimable". #6623: the resolved
                // identity rides along, so "used the wrong config dir" reads
                // differently from "used none at all".
                tracing::warn!(
                    root = %registry_root.display(),
                    reason = %reason,
                    identity = %identity,
                    "worktree-reclaim: the pull-request lookup failed — every branch \
                     will block, and the survey reports this reason (#6561, #6623)"
                );
                Self::unavailable_because(format!("{reason} (resolved gh identity: {identity})"))
            }
        }
    }

    /// Is this index known to hold every pull request (#2919)?
    ///
    /// Why: the operator-facing output has to disclose when the answer set was
    /// truncated, because on this repository (4526 pull requests against a
    /// [`PR_INDEX_LIMIT`] of 400) it always is — so any worktree whose PR is
    /// older than the last 400 reads `Unknown` and can never be reclaimed from
    /// the bulk index alone. Silence about that would look like "nothing is
    /// reclaimable" rather than "we did not look far enough back".
    /// Test: `pr_index_truncated_reply_makes_absent_branches_unknown`.
    pub(crate) fn is_complete(&self) -> bool {
        self.complete
    }

    /// How many branches this index resolved (#2919).
    ///
    /// Why: a successful `gh` call against a repository with pull requests
    /// yields a non-zero count, and a FAILED one yields zero — which is what
    /// makes `pr_index_from_gh_reads_this_repository` able to tell a working
    /// call from the silently-blocking one the `-C` bug produced.
    /// Test: `pr_index_from_gh_reads_this_repository`.
    pub(crate) fn branch_count(&self) -> usize {
        self.by_branch.len()
    }

    /// The state this index reports for `branch`.
    ///
    /// Why: the one place the "absent means nothing only if we saw everything"
    /// rule lives.
    /// What: a detached worktree (`None`) is `Unknown` — there is no branch to
    /// attribute a pull request to, so nothing can prove its work landed. A
    /// present branch returns its recorded state; an absent one returns `NoPr`
    /// when the index is complete, [`BranchPrState::LookupFailed`] when the
    /// lookup itself failed (#6561), and `Unknown` when the index merely ran
    /// past its page limit.
    /// Test: `pr_index_detached_worktree_is_unknown`,
    /// `pr_index_absent_branch_is_no_pr_when_complete`,
    /// `pr_index_reports_a_failed_lookup_with_its_reason`.
    pub(crate) fn state_for(&self, branch: Option<&str>) -> BranchPrState {
        let Some(branch) = branch else {
            return BranchPrState::Unknown;
        };
        match self.by_branch.get(branch) {
            Some(state) => state.clone(),
            None if self.complete => BranchPrState::NoPr,
            // #6561: a branch the index could not answer BECAUSE the lookup
            // failed reports the failure; a merely truncated index still
            // reports `Unknown`.
            None => match &self.failure {
                Some(reason) => BranchPrState::LookupFailed {
                    reason: reason.clone(),
                },
                None => BranchPrState::Unknown,
            },
        }
    }
}

/// Ask `gh` about ONE branch, for when the bulk index was truncated (#2919).
///
/// Why: [`PR_INDEX_LIMIT`] is always exceeded on this repository (4526 pull
/// requests), so the bulk index cannot reach the OLD worktrees — which are
/// exactly the ones a reclamation sweep is for. Falling back to a targeted
/// query for the branches the bulk index could not answer removes that ceiling
/// without paying one network call per worktree in the common case. It costs
/// one call per unresolved branch, so only the operator-invoked reclaim path
/// uses it; the `tm doctor` probe has a 3-second budget and discloses the
/// truncation instead.
/// What: `gh pr list --head <branch> --state all`. Fork rows are dropped by
/// [`PrIndex::from_json`] exactly as in the bulk path. A failure or timeout
/// yields [`BranchPrState::LookupFailed`] carrying `gh`'s own first stderr line
/// (#6561); it used to yield a cause-free `Unknown`. Either blocks.
///
/// #6561: this resolves a SQUASH-MERGED pull request whose head branch was
/// deleted at merge. `--head` matches the pull request's recorded
/// `headRefName`, which outlives the branch, so all three worktrees the issue
/// names answer `MERGED` here. The deleted branch was never the reason they
/// read unknown — the `gh` call was.
/// Test: `pr_state_for_branch_reports_a_failed_call_as_lookup_failed`,
/// `pr_index_resolves_a_squash_merged_pr_whose_head_branch_was_deleted`.
pub(crate) fn pr_state_for_branch(registry_root: &Path, branch: &str) -> BranchPrState {
    const PER_BRANCH_LIMIT: usize = 50;
    // #6623: same resolution as the bulk index — this call has its own
    // working directory and must not rely on the daemon's bare launchd
    // environment either.
    let gh_env = resolve_daemon_gh_env(registry_root);
    let identity = gh_env.describe();
    let mut cmd = gh_command(registry_root, &gh_env);
    cmd.args(["pr", "list", "--head", branch, "--state", "all", "--limit"])
        .arg(PER_BRANCH_LIMIT.to_string())
        .args(["--json", PR_JSON_FIELDS]);
    match run_with_timeout(cmd, GH_TIMEOUT) {
        Ok(stdout) => PrIndex::from_json(&stdout, PER_BRANCH_LIMIT).state_for(Some(branch)),
        Err(reason) => {
            // #6507: WARN, matching `PrIndex::from_gh`. This arm used to be
            // silent at every level, so a per-branch failure — the path this
            // repository's 4526-PR history forces every older branch through —
            // left no trace in the log at all and the survey reported only a
            // count.
            tracing::warn!(
                root = %registry_root.display(),
                branch = %branch,
                reason = %reason,
                identity = %identity,
                "worktree-reclaim: the per-branch pull-request lookup failed — this \
                 branch will block, and the survey reports this reason (#6507)"
            );
            BranchPrState::LookupFailed {
                reason: format!("{reason} (resolved gh identity: {identity})"),
            }
        }
    }
}

/// Is `path` a worktree trusty-mpm provisioned and can therefore remove
/// (#2919)?
///
/// Why: `decommission::remove_session_worktree` refuses any path that carries
/// no ownership sentinel and does not sit under `.worktrees/` — so the
/// harness-owned `.claude/worktrees/` store (out of scope per ADR-0020) can
/// never actually be deleted by this path. Without this gate the survey
/// classified those worktrees `Reclaimable`, counted their bytes into
/// `reclaimable_bytes`, and `tm doctor` told the operator to run a command that
/// then failed with `removal_failed` and left every one of them on disk. On
/// this machine that is MOST of the 1.1 TiB. Advertising a remedy that cannot
/// fire is worse than reporting nothing, so the classifier now applies exactly
/// the predicate the remover applies.
/// What: CALLS the remover's own predicate
/// ([`super::decommission::removal_permitted`]) rather than restating it. It
/// used to restate it, and the restatement drifted (#6561): both copies excluded
/// the harness `.claude/worktrees/` store, so `--merged-prs` reported
/// `0 of 0 measured` against a store full of merged, clean agent worktrees while
/// `agent_worktree_reap` was removing trees from that same store on every agent
/// exit. The tier that admits them lives in `removal_permitted`; this function
/// is now a one-line forward so the classifier and the remover cannot disagree
/// again.
/// Test: `classify_blocks_a_worktree_trusty_mpm_does_not_own`,
/// `tm_provisioned_matches_the_removers_own_predicate`,
/// `an_unattributed_agent_store_worktree_is_never_reclaimable`.
pub(crate) fn tm_provisioned(path: &Path) -> bool {
    super::decommission::removal_permitted(path)
}

/// Why a DISPATCHED AGENT's ownership forbids reclaiming `path`, or `None` when
/// it does not (#5661).
///
/// Why: `SentinelOwner::Agent` exists to stop an agent-owned worktree being
/// reclaimed by a sweep that has no way to tell whether the agent is still
/// working. `prune_orphaned_worktrees` and `worktree_reconcile::classify` both
/// apply it; the merged-PR reclaim path never read the sentinel at all, so a
/// worktree that carried an agent sentinel, had no `SessionRecord`, and sat on a
/// branch whose PR had merged passed every gate and was deleted out from under
/// the agent holding it. That happened three times on 2026-08-15/16, twice
/// against trees holding unpushed commits.
///
/// What: reads the sentinel through [`read_sentinel_owner`] — the same tolerant
/// parse the orphan path uses, not a second one — and refuses on two answers.
///
/// 1. [`SentinelOwner::Agent`] whose agent the registry calls
///    [`Live`](AgentDelegationState::Live).
///
///    [`Unknown`](AgentDelegationState::Unknown) used to refuse outright,
///    because the delegation map is rebuilt empty at every daemon boot: after a
///    restart it reports nothing for an agent that is still working, and an
///    unanswerable liveness question must never resolve to "free" (ADR-0045).
///    That reasoning is intact; what changed in #6561 is that the question is no
///    longer unanswerable. Git holds a second, DURABLE record of the same fact:
///    the Claude Code harness locks an agent's worktree for the life of that
///    agent and releases the lock when the agent ends, and that lock is a file
///    under `.git/worktrees/<id>/` which no daemon writes and no daemon restart
///    clears. So an `Unknown` registry answer now consults
///    [`harness_lock_state`]: `Released` is POSITIVE evidence the harness let go
///    and permits; `Held` and `Undeterminable` both refuse. Two silences still
///    do not make an answer — only a positive `Released` does.
/// 2. [`SentinelOwner::Unknown`] for ANY path inside the harness agent store
///    ([`is_harness_agent_worktree`]) — whether the sentinel is unreadable or
///    absent entirely. Undeterminable, not absent.
///
///    The first cut of #6561 split those two spellings and refused only the
///    unreadable one, reasoning that a path with no sentinel carries no claim to
///    hide. That is backwards, and the critic round caught it: a missing
///    sentinel is not weaker evidence of a claim, it is the ABSENCE OF ANY
///    ATTRIBUTION — trusty-mpm knows neither who owns the tree nor whether
///    anyone is working in it, which is the exact question ADR-0045 forbids
///    resolving toward "free" on a destructive path. The sentinel is written
///    only after `PostToolUse` teaches an `agent_id`, so #6556's own
///    lost-`PostToolUse` population has neither a sentinel nor a
///    `worktree_path`; a `version-control` agent squash-merging while that agent
///    is still finishing leaves the tree merged and clean, and gates 5 and 6
///    would then be the only things standing in front of a delete of a live
///    agent's tree. That is the #5661 shape, reintroduced.
///
///    What this costs: the historical backlog of unattributed agent worktrees
///    stays unreclaimable, which is the pre-existing state and is stated in
///    #6561 rather than silently fixed. What it does not cost: a tree whose
///    agent DID register (sentinel written, delegation terminal) is answered
///    `Ended` by the probe above and reclaims normally.
///
/// # What this deliberately does NOT change
///
/// A worktree outside the agent store whose sentinel is absent, empty or
/// unparsable keeps today's behaviour: the merged PR is its landing evidence and
/// `tm_provisioned` its ownership evidence. Widening the refusal to every
/// owner-unknown sentinel would make the `.worktrees/` population — the 1.1 TiB
/// this module was written to reclaim — permanently unreclaimable, which is the
/// opposite failure and not #5661's.
/// Test: `classify_blocks_a_live_agents_worktree`,
/// `classify_blocks_an_agent_the_harness_still_holds_after_a_restart`,
/// `classify_allows_a_finished_agents_merged_worktree`,
/// `classify_blocks_an_agent_store_worktree_with_an_unreadable_sentinel`,
/// `classify_leaves_a_session_owned_worktree_alone`,
/// `an_unattributed_agent_store_worktree_is_never_reclaimable`,
/// `an_unreadable_agent_sentinel_still_blocks_an_agent_store_worktree`,
/// `classify_allows_a_merged_agent_tree_the_harness_released`,
/// `classify_blocks_an_agent_tree_git_cannot_be_asked_about`.
pub(crate) fn agent_ownership_blocks(
    path: &Path,
    agent_state: AgentStateProbe<'_>,
) -> Option<String> {
    match read_sentinel_owner(path) {
        SentinelOwner::Agent(owner, _) => match agent_state(&owner) {
            AgentDelegationState::Live => Some(format!(
                "owned by dispatched agent {} — a delegation naming it has not ended, so it is \
                 still working in this tree (#5661)",
                owner.agent_id
            )),
            // #6561: the registry's silence is not the last word — ask git.
            AgentDelegationState::Unknown => match harness_lock_state(path) {
                HarnessLockState::Released => None,
                HarnessLockState::Held => Some(format!(
                    "owned by dispatched agent {} and git still reports the harness's \
                     agent-lifetime lock on this worktree (#6561)",
                    owner.agent_id
                )),
                HarnessLockState::Undeterminable => Some(format!(
                    "owned by dispatched agent {} — the delegation registry holds no record of \
                     that agent, and git could not be asked whether the harness still holds the \
                     worktree. Two silences are not an answer: undeterminable, not absent \
                     (#5661, #6561, ADR-0045)",
                    owner.agent_id
                )),
            },
            AgentDelegationState::Ended => None,
        },
        // #6561 critic round: absent and unreadable are BOTH undeterminable
        // here — see this function's doc, refusal 2.
        SentinelOwner::Unknown if is_harness_agent_worktree(path) => Some(
            "names no owner inside the harness agent-worktree store — the sentinel is absent, \
             empty, malformed or unreadable, so nothing attributes this tree to an agent and \
             nothing says whether one is still working in it. An unanswerable ownership \
             question on a destructive path is undeterminable, not absent (#5661, #6561, \
             ADR-0045)"
                .to_string(),
        ),
        SentinelOwner::Known(..) | SentinelOwner::Unknown => None,
    }
}

/// The refusal reason a survey records for a worktree it ran out of time to
/// inspect (#2919).
///
/// Why: named rather than inlined so [`ReclaimSurvey::not_inspected`] can count
/// these WITHOUT the count and the message drifting apart — the whole point is
/// to distinguish "the probe ran out of time" from "the pull-request lookup
/// failed", which read identically before.
/// Test: `survey_separates_deadline_skips_from_lookup_failures`.
pub(crate) const NOT_INSPECTED_REASON: &str = "survey deadline reached before inspection";

/// Decide whether one worktree may be reclaimed (#2919).
///
/// Why: this is the whole safety argument in one function, deliberately
/// written so that `Reclaimable` is reachable ONLY by falling off the end.
/// Every gate `return`s, so no failure branch can advance toward deletion —
/// the fail-open/cursor-advance shape that has bitten this repository
/// repeatedly is structurally impossible here.
/// What: five gates, in this order.
/// 1. **Existence/eligibility** — git's own [`Admission`] verdict (ADR-0023
///    point 1). Excludes the main checkout, bare records, operator-LOCKED
///    worktrees, and anything outside the managed project.
/// 2. **Liveness** — a path any session record still claims is never touched,
///    whatever that record's persisted state says (#4288).
/// 3. **Removability** — [`tm_provisioned`]: the classifier applies exactly the
///    ownership predicate the REMOVER applies, so nothing is ever advertised as
///    reclaimable that `remove_session_worktree` would refuse.
/// 4. **Agent ownership** (#5661) — [`agent_ownership_blocks`]. Gate 2 reads
///    session records only, and a dispatched agent has none, so a live agent's
///    worktree was invisible to every gate above this one.
/// 5. **Landing evidence** — only [`BranchPrState::Merged`] proceeds.
/// 6. **Unsaved work** — `probe_dirt` is a closure rather than a precomputed
///    `Option` on purpose: passing the value would let a caller reach this gate
///    with a `None` that means "not checked" instead of "checked and clean".
///    The probe is [`inspect_dirt`], which fails toward DIRTY on every error.
///
/// `agent_state` is a closure for the same reason `probe_dirt` is: a caller with
/// no delegation registry to consult must say so by returning
/// [`AgentDelegationState::Unknown`], which refuses, rather than by handing over
/// an empty list that would read as "no agent claims this".
///
/// Test: one refusal test per gate — `classify_blocks_non_admitted_worktree`,
/// `classify_blocks_live_session_workspace`,
/// `classify_blocks_a_worktree_trusty_mpm_does_not_own`,
/// `classify_blocks_a_live_agents_worktree`,
/// `classify_blocks_an_agent_the_harness_still_holds_after_a_restart`,
/// `classify_blocks_an_agent_store_worktree_with_an_unreadable_sentinel`,
/// `classify_records_an_agent_refusal_as_its_own_verdict_kind`,
/// `classify_blocks_open_pr`,
/// `classify_blocks_closed_unmerged_pr`, `classify_blocks_no_pr`,
/// `classify_blocks_unknown_pr_state`, `classify_blocks_dirty_worktree` —
/// plus `classify_allows_clean_pushed_merged_worktree` and
/// `classify_allows_a_finished_agents_merged_worktree` for the paths that say
/// yes.
pub(crate) fn classify(
    path: &Path,
    admission: Admission,
    live: bool,
    pr: &BranchPrState,
    probe_dirt: &dyn Fn(&Path) -> Option<DirtyWorktree>,
    agent_state: AgentStateProbe<'_>,
) -> ReclaimVerdict {
    // Gate 1 (#2919): git decides existence and eligibility, per ADR-0023.
    // #6561: a harness agent lock is a refusal the operator must be TOLD about
    // — it means an agent is working in that tree — so it leaves gate 1 as its
    // own verdict kind and reaches `ReclaimSurvey::agent_owned`. Every other
    // non-admitted verdict is an ordinary block, as before.
    if admission == Admission::HarnessAgentLock {
        return ReclaimVerdict::blocked_by_agent(ReclaimGate::Admission, admission.reason());
    }
    if admission != Admission::Admitted {
        return ReclaimVerdict::blocked(ReclaimGate::Admission, admission.reason());
    }
    // Gate 2 (#2919): a live session can occupy a directory whose record reads
    // terminal — measured on this repo 2026-07-28. Never trust the state field.
    if live {
        return ReclaimVerdict::blocked(
            ReclaimGate::Liveness,
            "a session still claims this workspace",
        );
    }
    // Gate 3 (#2919): only worktrees trusty-mpm provisioned can actually be
    // removed. Classifying one it cannot remove as `Reclaimable` made
    // `tm doctor` advertise a command that then failed and left the directory
    // on disk — see `tm_provisioned`.
    if !tm_provisioned(path) {
        // #6561: the harness `.claude/worktrees/` store is no longer named here
        // as out of scope — `removal_permitted` admits it and the remover can
        // now act on it. What remains excluded is a path with none of the three
        // ownership marks.
        return ReclaimVerdict::blocked(
            ReclaimGate::Removability,
            "not a trusty-mpm-removable worktree — no ownership sentinel, not under \
             `.worktrees/`, and not in the harness `.claude/worktrees/` store, so \
             `prune-worktrees` cannot remove it",
        );
    }
    // Gate 4 (#5661): a dispatched agent has no session record, so gate 2 is
    // blind to it, and gate 3 reads only whether a sentinel FILE exists — never
    // whose claim it carries. See `agent_ownership_blocks`.
    // #5829: recorded as its OWN verdict kind, because sparing a live agent's
    // tree is the one refusal the operator must be told about by name — see
    // `ReclaimVerdict::BlockedByAgent`.
    if let Some(reason) = agent_ownership_blocks(path, agent_state) {
        return ReclaimVerdict::blocked_by_agent(ReclaimGate::AgentOwnership, reason);
    }
    // Gate 5 (#2919): the merged PR is the landing evidence DOC-52 §3.4 makes
    // the reclamation trigger. Everything else — including "we could not find
    // out" — refuses.
    let merged_pr = match pr {
        BranchPrState::Merged { pr } => *pr,
        BranchPrState::Open { pr } => {
            return ReclaimVerdict::blocked(
                ReclaimGate::PrState,
                format!("PR #{pr} is still open"),
            );
        }
        BranchPrState::ClosedUnmerged { pr } => {
            return ReclaimVerdict::blocked(
                ReclaimGate::PrState,
                format!("PR #{pr} was closed without merging"),
            );
        }
        BranchPrState::NoPr => {
            return ReclaimVerdict::blocked(
                ReclaimGate::PrState,
                "no pull request found for this branch",
            );
        }
        BranchPrState::Unknown => {
            return ReclaimVerdict::blocked(
                ReclaimGate::PrState,
                "pull-request state could not be determined (is `gh` installed \
                 and authenticated?)",
            );
        }
        // #6561: the lookup broke. Same refusal, but naming the cause — the
        // operator can act on `gh exited 4: … gh auth login`; they cannot act
        // on "could not be determined".
        BranchPrState::LookupFailed { reason } => {
            return ReclaimVerdict::blocked(
                ReclaimGate::PrState,
                format!("the pull-request lookup failed: {reason}"),
            );
        }
    };
    // Gate 6 (#2919): a merged PR does NOT prove the directory holds nothing
    // novel — the 2026-07-21 salvage found merged-PR worktrees carrying real
    // unpushed source. This is the last gate and it fails toward dirty.
    if let Some(dirt) = probe_dirt(path) {
        return ReclaimVerdict::blocked(
            ReclaimGate::UnsavedWork,
            format!("holds unsaved work: {}", dirt.reason),
        );
    }
    ReclaimVerdict::Reclaimable { pr: merged_pr }
}

/// One surveyed worktree and everything the survey learned about it (#2919).
#[derive(Debug, Clone, Serialize)]
pub(crate) struct ReclaimCandidate {
    /// The worktree directory, as git reports it.
    pub path: PathBuf,
    /// Short branch name, or `None` when detached.
    pub branch: Option<String>,
    /// The checkout whose git registry listed this worktree.
    ///
    /// Carried so the reclaim loop can rebuild that repository's pull-request
    /// index FRESH before deleting, rather than reusing the survey's (#2919).
    #[serde(skip)]
    pub registry_root: PathBuf,
    /// Bytes on disk, or `None` when the directory could not be measured.
    pub bytes: Option<u64>,
    /// What the pull-request index said about [`branch`](Self::branch).
    pub pr: BranchPrState,
    /// Whether this worktree may be reclaimed, and if not, why not.
    pub verdict: ReclaimVerdict,
}

/// The whole-workspace disk and reclaimability picture (#2919).
///
/// Why: the post-mortem's eighth design constraint is "report real before/after
/// numbers … not merely a claim". These are those numbers.
/// What: every surveyed worktree plus the totals `tm doctor` renders.
/// `total_bytes` sums only what was measurable, so `unmeasured` must be read
/// alongside it — a large `unmeasured` means the total is an UNDERCOUNT.
/// Test: `survey_past_its_measure_deadline_still_classifies`.
#[derive(Debug, Clone, Default, Serialize)]
pub(crate) struct ReclaimSurvey {
    /// Every git-registered worktree under the managed repos root.
    pub candidates: Vec<ReclaimCandidate>,
    /// Sum of `bytes` over candidates that could be measured.
    pub total_bytes: u64,
    /// Sum of `bytes` over candidates whose verdict is `Reclaimable`.
    pub reclaimable_bytes: u64,
    /// How many candidates are reclaimable.
    pub reclaimable: usize,
    /// How many of the RECLAIMABLE candidates actually had their bytes measured.
    ///
    /// Why: `reclaimable_bytes` is a sum over this subset, not over
    /// `reclaimable`. When measurement is starved — and a single 17.8 GiB
    /// worktree can consume a 20-second budget by itself — the sum is a floor
    /// wearing the whole set's label. The `unmeasured` count does NOT cover
    /// this: it is a whole-survey figure, and a reader cannot subtract it to
    /// recover how much of the reclaimable set was measured. Every surface that
    /// prints `reclaimable_bytes` must print this beside it.
    /// Test: `survey_discloses_a_partially_measured_reclaimable_set`.
    pub reclaimable_measured: usize,
    /// How many candidates were refused.
    pub blocked: usize,
    /// How many candidates could not be measured (their bytes are missing).
    pub unmeasured: usize,
    /// How many candidates had an indeterminate pull-request state.
    ///
    /// This counts BOTH causes, so read it with `not_inspected`: subtracting
    /// that leaves the ones where the pull-request lookup itself could not
    /// answer. Separating them is not pedantry — a review round was spent
    /// establishing that 169 unknowns against the real store were repos the
    /// authenticated account cannot see plus detached HEADs, not a second bug.
    pub pr_state_unknown: usize,
    /// Worktrees spared because a DISPATCHED AGENT owns them, as
    /// `"<path>: <reason>"` (#5829).
    ///
    /// Why: gate 4 has refused these since #5661, but the refusal never left
    /// the daemon — the prune route returns counts plus the removed and
    /// re-check-refused lists, and a candidate blocked during CLASSIFICATION
    /// appears in none of them. So `--merged-prs --force` printed
    /// `reclaimed 0 worktree(s)` and named neither the worktree it protected
    /// nor the agent it protected it for. A guard the operator cannot observe
    /// cannot be trusted or debugged in the field, and #5829 asks for the skip
    /// to say why.
    /// Test: `survey_discloses_a_live_agents_spared_worktree`.
    pub agent_owned: Vec<String>,
    /// How many candidates the survey never inspected, because its classify
    /// deadline expired first.
    ///
    /// Why: these are `Unknown` for a completely different reason from a `gh`
    /// failure — the probe ran out of time, not out of answers. Reporting them
    /// as "pull-request state could not be determined" sends an operator to
    /// `gh auth status` for a problem that is really "raise the budget".
    /// Test: `survey_separates_deadline_skips_from_lookup_failures`.
    pub not_inspected: usize,
    /// How many candidates the pull-request lookup FAILED for (#6561).
    ///
    /// Why: distinct from `pr_state_unknown`, which now means only "inspected,
    /// and the answer was genuinely indeterminate". A failed lookup is a fault
    /// with a fix; an indeterminate answer is not. Conflating them produced the
    /// live report this issue is about — 261 of 261 unknown, `0 reclaimable`,
    /// and no cause anywhere in the output.
    /// Test: `survey_counts_a_failed_lookup_apart_from_an_unknown_state`.
    pub lookup_failed: usize,
    /// The FIRST failure reason observed, verbatim from `gh` (#6561).
    ///
    /// One reason rather than a list: every candidate under a broken `gh`
    /// fails identically, so 261 copies of one line would bury the count.
    /// Test: `survey_counts_a_failed_lookup_apart_from_an_unknown_state`.
    pub lookup_failure: Option<String>,
    /// One line per candidate refused by an ordinary [`ReclaimVerdict::Blocked`],
    /// as `"<path>: blocked at <gate>: <reason>"` (#6507).
    ///
    /// Why: `agent_owned` names gate 4's refusals and nothing else, so an
    /// ordinary `Blocked` — every other gate — reached no log line and no field
    /// of the prune reply. On 2026-09-03 that left a worktree whose work had
    /// fully landed sitting unreclaimed with nothing anywhere saying which gate
    /// refused it, and three verification passes attributed it by elimination
    /// to the wrong gate. A refusal the operator cannot read is a refusal
    /// nobody can debug.
    ///
    /// This list and `agent_owned` PARTITION the blocked set: every
    /// [`ReclaimVerdict::BlockedByAgent`] belongs to `agent_owned` alone, so a
    /// candidate appears in exactly one of them and their lengths sum to
    /// `blocked`. The exclusion is by VARIANT, never by gate — `BlockedByAgent`
    /// is constructed at gate 1 (the harness agent lock) as well as at gate 4,
    /// so excluding [`ReclaimGate::AgentOwnership`] would leave the harness-lock
    /// candidate in both lists.
    /// Test: `survey_names_the_gate_that_blocked_each_candidate`,
    /// `survey_lists_an_agent_held_candidate_in_exactly_one_place`.
    pub blocked_reasons: Vec<String>,
}

impl ReclaimSurvey {
    /// Fold `candidates` into the derived totals.
    ///
    /// Why: computing the totals anywhere but here would let a caller publish
    /// a figure that disagrees with the list it came from.
    /// What: one pass; `bytes: None` contributes to `unmeasured` and to no sum.
    /// Test: `survey_past_its_measure_deadline_still_classifies`.
    pub(crate) fn from_candidates(candidates: Vec<ReclaimCandidate>) -> Self {
        let mut out = Self {
            total_bytes: 0,
            reclaimable_bytes: 0,
            reclaimable: 0,
            reclaimable_measured: 0,
            not_inspected: 0,
            blocked: 0,
            unmeasured: 0,
            pr_state_unknown: 0,
            lookup_failed: 0,
            lookup_failure: None,
            agent_owned: Vec::new(),
            blocked_reasons: Vec::new(),
            candidates: Vec::new(),
        };
        for c in &candidates {
            // #6507: folded here, with the totals, for the same reason
            // `agent_owned` is — a disclosed line that is derived anywhere else
            // can disagree with the candidate list it claims to describe.
            // Matched on the VARIANT so every `BlockedByAgent` — gate 1's
            // harness lock as well as gate 4's — is left to `agent_owned` and
            // no candidate is disclosed twice.
            if let ReclaimVerdict::Blocked { gate, reason } = &c.verdict {
                out.blocked_reasons.push(format!(
                    "{}: blocked at {}: {reason}",
                    c.path.display(),
                    gate.label()
                ));
            }
            match c.bytes {
                Some(b) => {
                    out.total_bytes = out.total_bytes.saturating_add(b);
                    if c.verdict.is_reclaimable() {
                        out.reclaimable_bytes = out.reclaimable_bytes.saturating_add(b);
                    }
                }
                None => out.unmeasured += 1,
            }
            if c.verdict.is_reclaimable() {
                out.reclaimable += 1;
                if c.bytes.is_some() {
                    out.reclaimable_measured += 1;
                }
            } else {
                out.blocked += 1;
            }
            // #6561: an inspected-but-indeterminate state and a BROKEN lookup
            // are counted apart, and the first failure keeps its reason.
            match &c.pr {
                BranchPrState::Unknown => out.pr_state_unknown += 1,
                BranchPrState::LookupFailed { reason } => {
                    out.lookup_failed += 1;
                    out.lookup_failure.get_or_insert_with(|| reason.clone());
                }
                _ => {}
            }
            // #5829: collected here, with the totals, so the disclosed list can
            // never disagree with the candidate list it is derived from.
            if let ReclaimVerdict::BlockedByAgent { reason, .. } = &c.verdict {
                out.agent_owned
                    .push(format!("{}: {reason}", c.path.display()));
            }
            // #6507: the deadline skip is now a gate of its own, so the count no
            // longer depends on matching the refusal's wording.
            if matches!(
                &c.verdict,
                ReclaimVerdict::Blocked {
                    gate: ReclaimGate::Deadline,
                    ..
                }
            ) {
                out.not_inspected += 1;
            }
        }
        out.candidates = candidates;
        out
    }
}

/// How many walk entries pass between deadline checks (#2919).
///
/// Why: reading the clock once per file over a terabyte-scale tree is pure
/// overhead, but checking too rarely lets a single worktree blow the whole
/// survey's budget. 4096 entries is roughly a tenth of a second of walking.
/// Test: `measure_bytes_stops_at_an_expired_deadline`.
const DEADLINE_CHECK_INTERVAL: usize = 4096;

/// Bytes held by `path` and everything beneath it, bounded by `deadline`
/// (#2919).
///
/// Why (the measurement): each Rust worktree carries its own `target/` (3–63 GB
/// observed), so worktree COUNT is a useless proxy for disk pressure — bytes
/// are the number the 1.1 TiB post-mortem is actually about.
///
/// Why (the deadline): `tokio::time::timeout` cannot cancel a `spawn_blocking`
/// task. Wrapping the survey in an outer timeout returns a verdict on schedule
/// but leaves the walk running, and tokio's runtime shutdown then waits for
/// that thread. Measured while building this change: two doctor tests each left
/// a walk running over the operator's real ~1 TiB worktree store and the test
/// binary hung for over twenty minutes at shutdown. A blocking task must
/// therefore bound ITSELF; an outer timeout is not a bound, it is a report of
/// one.
///
/// What: a non-following walk summing regular-file lengths, polling the clock
/// every [`DEADLINE_CHECK_INTERVAL`] entries. Symlinks are never followed, so a
/// link out of the worktree cannot inflate the figure or loop. Returns `None`
/// when `path` is not a directory OR the deadline passed — both mean "could not
/// be measured", which the caller counts in `unmeasured` and the doctor check
/// discloses as an UNDERCOUNT. A `None` deadline never expires. Entries that
/// error mid-walk are skipped, which can undercount; that is acceptable because
/// this value is reported, never gated on — no deletion decision reads it.
/// Test: `measure_bytes_counts_file_contents`,
/// `measure_bytes_of_missing_path_is_none`,
/// `measure_bytes_stops_at_an_expired_deadline`.
pub(crate) fn measure_bytes_until(path: &Path, deadline: Option<Instant>) -> Option<u64> {
    if !path.is_dir() {
        return None;
    }
    let expired = |d: Option<Instant>| d.is_some_and(|d| Instant::now() >= d);
    if expired(deadline) {
        return None;
    }
    let mut total: u64 = 0;
    for (seen, entry) in walkdir::WalkDir::new(path)
        .follow_links(false)
        .into_iter()
        .filter_map(Result::ok)
        .enumerate()
    {
        if seen % DEADLINE_CHECK_INTERVAL == 0 && seen > 0 && expired(deadline) {
            return None;
        }
        if entry.file_type().is_file()
            && let Ok(md) = entry.metadata()
        {
            total = total.saturating_add(md.len());
        }
    }
    Some(total)
}

/// Does any session still claim `path` (#2919)?
///
/// Why: `git worktree list` cannot see a live session, and a session record's
/// persisted state is bookkeeping rather than a liveness signal (#4288). The
/// only defensible test is containment in BOTH directions: a claimed path
/// inside the candidate means the candidate is a live session's parent, and a
/// candidate inside a claimed path means it is a live session's subdirectory.
/// What: compares canonical AND raw spellings of every claimed path — if
/// canonicalization fails on either side the raw form still protects, so a
/// failed observation can only ever spare a worktree, never expose one.
/// Test: `live_check_matches_exact_ancestor_and_descendant_paths`.
pub(crate) fn is_live(path: &Path, in_use: &[PathBuf]) -> bool {
    let candidates = [
        std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf()),
        path.to_path_buf(),
    ];
    in_use.iter().any(|claimed| {
        let claimed_forms = [
            std::fs::canonicalize(claimed).unwrap_or_else(|_| claimed.clone()),
            claimed.clone(),
        ];
        claimed_forms.iter().any(|c| {
            candidates
                .iter()
                .any(|p| c == p || c.starts_with(p) || p.starts_with(c))
        })
    })
}

/// Whether a reclamation run may delete anything (#2919).
///
/// Why: the owner's directive asks for merge-triggered cleanup, but every
/// automatic caller uses [`Report`](Self::Report). Deletion is reachable only
/// from an operator action that opts in explicitly, so no timer, hook, or
/// daemon sweep can remove a worktree on evidence this module gathered.
/// What: `Report` surveys and returns; `Remove` additionally deletes the
/// worktrees that pass BOTH the survey and a fresh per-candidate re-check.
/// Test: `reclaim_report_mode_removes_nothing`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum ReclaimMode {
    /// Survey only — the default, and the only mode anything automatic uses.
    #[default]
    Report,
    /// Explicit operator opt-in: delete what the survey and re-check approve.
    Remove,
}

/// What a reclamation run surveyed and (when permitted) removed (#2919).
#[derive(Debug, Clone, Default, Serialize)]
pub(crate) struct ReclaimOutcome {
    /// The full survey, identical in both modes.
    pub survey: ReclaimSurvey,
    /// Worktrees actually deleted (always empty in `Report` mode).
    pub removed: Vec<PathBuf>,
    /// Bytes the removed worktrees held, per the survey's measurement.
    pub removed_bytes: u64,
    /// Candidates the survey approved but the FRESH re-check refused.
    pub refused_at_recheck: Vec<String>,
    /// Candidates the re-check approved but which are still on disk, each as
    /// `"<path>: <reason>"` (#4732 — the reason used to be dropped, and a
    /// deliberate `git worktree lock` refusal read as a transient error).
    pub removal_failed: Vec<String>,
}

#[cfg(test)]
#[path = "worktree_reclaim_tests.rs"]
mod worktree_reclaim_tests;
