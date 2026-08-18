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
//! could not be determined ([`BranchPrState::Unknown`]) blocks, and so does an
//! index that may have been TRUNCATED (an absent branch is only `NoPr` when
//! the index is known complete).
//!
//! Test: `worktree_reclaim_tests` — one refusal test per gate, each of which
//! fails if its gate is deleted.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use super::worktree_ownership::{
    AgentDelegationState, AgentWorktreeOwner, SentinelOwner, is_harness_agent_worktree,
    read_sentinel_owner,
};
use super::worktree_registry::Admission;
use super::worktree_safety::DirtyWorktree;

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

/// Environment variables that would point `gh`/`git` at a DIFFERENT repository.
///
/// Why: the same hazard `worktree_safety::GIT_REDIRECTING_ENV` documents —
/// `gh` resolves the repository through git, and an inherited `GIT_DIR` names
/// a repository the worktree under inspection has nothing to do with. A
/// work-destroying gate must not be steerable by ambient environment.
/// What: removed from the child environment so the repository is resolved from
/// `-C <registry_root>` alone.
/// Test: `gh_command_strips_repository_redirecting_env`.
const GH_STRIPPED_ENV: &[&str] = &[
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_COMMON_DIR",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GH_REPO",
];

/// The pull-request state of the branch a worktree has checked out (#2919).
///
/// Why: only ONE of these five states is evidence that the worktree's work has
/// landed and the directory is disposable. Modelling the other four explicitly
/// — rather than as `Option<bool>` — is what keeps "we could not find out"
/// from collapsing into "there is nothing here", which is the fail-open shape
/// this repository keeps re-encountering.
/// What: `Merged` carries the PR number that proves the branch landed. `Open`
/// and `ClosedUnmerged` are active/abandoned-but-unlanded branches. `NoPr` is
/// asserted only against a KNOWN-COMPLETE index. `Unknown` covers every
/// unanswerable case: `gh` missing, unauthenticated, erroring, a truncated
/// index, or a detached HEAD with no branch to attribute a PR to.
/// Test: `classify_blocks_open_pr`, `classify_blocks_closed_unmerged_pr`,
/// `classify_blocks_no_pr`, `classify_blocks_unknown_pr_state`.
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
}

impl BranchPrState {
    /// How strongly this state BLOCKS reclamation — higher wins a collision.
    ///
    /// Why: a branch can carry several pull requests over its life (merge PR
    /// #1, push again, open PR #2). Reclaiming on the merged one while a newer
    /// one is open would delete live work, so when one branch maps to several
    /// rows the most blocking state must win, not the newest or the first.
    /// What: `Open` (3) > `ClosedUnmerged` (2) > `Merged` (1); `NoPr`/`Unknown`
    /// never come from a row so they rank 0.
    /// Test: `pr_index_open_pr_beats_a_merged_one_on_the_same_branch`.
    fn block_rank(&self) -> u8 {
        match self {
            Self::Open { .. } => 3,
            Self::ClosedUnmerged { .. } => 2,
            Self::Merged { .. } => 1,
            Self::NoPr | Self::Unknown => 0,
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
    /// call, yielding [`PrIndex::unavailable`].
    #[serde(default, rename = "isCrossRepository")]
    is_cross_repository: bool,
}

/// Branch → pull-request state for one repository (#2919).
///
/// Why: one `gh` call per repository instead of one per worktree, and — more
/// importantly — one place that knows whether the answer set is trustworthy.
/// An index that failed to build, or that may have been truncated, must not
/// let an absent branch read as "no PR", because "no PR" would otherwise be
/// indistinguishable from a merged PR that fell off the end of the page.
/// What: the branch map plus a `complete` flag. [`state_for`](Self::state_for)
/// returns `NoPr` for an absent branch ONLY when `complete` holds; otherwise
/// `Unknown`.
/// Test: `pr_index_absent_branch_is_no_pr_when_complete`,
/// `pr_index_truncated_reply_makes_absent_branches_unknown`,
/// `pr_index_unavailable_makes_every_branch_unknown`.
#[derive(Debug, Default)]
pub(crate) struct PrIndex {
    /// Branch name → the most blocking state observed for it.
    by_branch: BTreeMap<String, BranchPrState>,
    /// True only when the reply is known to hold every pull request.
    complete: bool,
}

impl PrIndex {
    /// An index that answers nothing — every branch reads `Unknown`.
    ///
    /// Why: the single value every failure path returns, so a `gh` that is
    /// absent, unauthenticated, rate-limited, or erroring produces refusals
    /// rather than a silently empty "no PRs anywhere" index.
    /// What: empty map, `complete: false`.
    /// Test: `pr_index_unavailable_makes_every_branch_unknown`.
    pub(crate) fn unavailable() -> Self {
        Self {
            by_branch: BTreeMap::new(),
            complete: false,
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
            return Self::unavailable();
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
    /// environment stripped. Any failure to spawn, a timeout, a non-zero exit,
    /// or unparsable output all yield [`unavailable`](Self::unavailable) —
    /// never an empty-but-complete index.
    /// Test: `gh_command_passes_no_dash_c_flag`,
    /// `gh_command_runs_in_the_requested_directory`,
    /// `gh_command_strips_repository_redirecting_env` (argv/env shape);
    /// `pr_index_from_gh_reads_this_repository` (a real successful call);
    /// `pr_index_malformed_json_is_unavailable` (failure-to-unavailable).
    pub(crate) fn from_gh(registry_root: &Path) -> Self {
        let mut cmd = gh_command(registry_root);
        cmd.args(["pr", "list", "--state", "all", "--limit"])
            .arg(PR_INDEX_LIMIT.to_string())
            .args(["--json", PR_JSON_FIELDS]);
        match run_with_timeout(cmd, GH_TIMEOUT) {
            Some((true, stdout)) => {
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
            _ => {
                tracing::debug!(
                    root = %registry_root.display(),
                    "worktree-reclaim: `gh pr list` failed, timed out, or could not be \
                     run — every branch will read PR-state-unknown and block (#2919)"
                );
                Self::unavailable()
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
    /// when the index is complete and `Unknown` otherwise.
    /// Test: `pr_index_detached_worktree_is_unknown`,
    /// `pr_index_absent_branch_is_no_pr_when_complete`.
    pub(crate) fn state_for(&self, branch: Option<&str>) -> BranchPrState {
        let Some(branch) = branch else {
            return BranchPrState::Unknown;
        };
        match self.by_branch.get(branch) {
            Some(state) => state.clone(),
            None if self.complete => BranchPrState::NoPr,
            None => BranchPrState::Unknown,
        }
    }
}

/// A `gh` invocation rooted at `dir` with a sanitised environment (#2919).
///
/// Why: `gh` resolves the repository from its WORKING DIRECTORY. It has no
/// global `-C` flag — that is a `git` habit, and carrying it across made every
/// call in this module fail at flag parsing with
/// `unknown shorthand flag: 'C' in -C` (verified against gh 2.96.0;
/// `gh --help` mentions no `-C`). The failure was silent by design: a failed
/// call yields [`PrIndex::unavailable`], every branch reads
/// [`BranchPrState::Unknown`], `classify` gate 4 blocks every candidate, and
/// the delete loop never executes — so `--merged-prs` reclaimed zero bytes
/// ALWAYS, and `tm doctor` blamed `gh auth status` for what was an argv bug.
/// Nothing in the test suite noticed, because no test ever ran a SUCCESSFUL
/// `gh` call. Hence `current_dir`, and hence
/// `gh_command_passes_no_dash_c_flag`.
///
/// The pager and prompt suppression are not cosmetic either — an interactive
/// prompt in a daemon-run probe hangs it forever. See [`GH_STRIPPED_ENV`] for
/// the environment scrub.
/// What: `gh` with its working directory set to `dir`, the repository-
/// redirecting variables removed, and pager/prompt/colour disabled.
/// Test: `gh_command_passes_no_dash_c_flag`,
/// `gh_command_runs_in_the_requested_directory`,
/// `gh_command_strips_repository_redirecting_env`.
fn gh_command(dir: &Path) -> Command {
    // #5475: argv/binary/env come from trusty-common's single `gh` entry
    // point; this module keeps its own kill-on-timeout runner, which is why it
    // takes the unspawned `std::process::Command` rather than a runner method.
    // #2919: `current_dir`, NEVER `-C` — `gh` has no such flag.
    let mut cmd = trusty_common::gh::GhCommand::bare().cwd(dir);
    for key in GH_STRIPPED_ENV {
        cmd = cmd.env_remove(key);
    }
    cmd.env("GH_PAGER", "")
        .env("GH_PROMPT_DISABLED", "1")
        .env("GH_NO_UPDATE_NOTIFIER", "1")
        .env("NO_COLOR", "1")
        .to_std_command()
}

/// The `--json` field set every `gh pr list` call in this module requests.
///
/// Why: named once so the bulk index and the per-branch fallback cannot drift
/// into asking for different facts. `isCrossRepository` is not optional — see
/// [`PrRow::is_cross_repository`]; a `gh` that does not understand the field
/// makes the whole call fail, which yields [`PrIndex::unavailable`] and blocks,
/// rather than silently returning rows with fork PRs indistinguishable from
/// local ones.
/// Test: `pr_index_skips_fork_pull_requests`.
const PR_JSON_FIELDS: &str = "number,headRefName,state,isCrossRepository";

/// Wall-clock ceiling for any single `gh` invocation (#2919).
///
/// Why: `gh pr list` is a NETWORK call. `std::process::Command::output()` has
/// no timeout, so a wedged or throttled request hangs the calling thread
/// forever — and because this runs inside `spawn_blocking`, which cannot be
/// cancelled, that hang propagates to runtime shutdown. This is the same
/// failure shape already fixed for the byte walk; a subprocess needs its own
/// bound for the same reason.
/// What: 10 seconds, after which the child is killed and the call reports
/// failure — which resolves to [`PrIndex::unavailable`] and therefore blocks.
/// Test: `run_with_timeout_kills_a_hung_child`.
const GH_TIMEOUT: Duration = Duration::from_secs(10);

/// Run `cmd`, killing it if it outlives `budget` (#2919).
///
/// Why: see [`GH_TIMEOUT`]. `Command::output()` waits indefinitely.
/// What: spawns the child with piped stdio and DRAINS both pipes on their own
/// threads — a child that fills a pipe buffer blocks forever if nobody reads
/// it, which would defeat the timeout it is meant to enforce (a 400-PR JSON
/// reply comfortably exceeds a 64 KiB pipe buffer). Polls `try_wait` until the
/// budget expires, then kills and reaps the child. Returns `None` on spawn
/// failure, timeout, or a wait error — every failure is indistinguishable to
/// the caller, and every one of them blocks reclamation.
/// Test: `run_with_timeout_captures_output`, `run_with_timeout_kills_a_hung_child`.
fn run_with_timeout(mut cmd: Command, budget: Duration) -> Option<(bool, String)> {
    let mut child = cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    let mut stderr = child.stderr.take()?;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = String::new();
        let _ = stdout.read_to_string(&mut buf);
        let _ = tx.send(buf);
    });
    std::thread::spawn(move || {
        let mut sink = Vec::new();
        let _ = stderr.read_to_end(&mut sink);
    });
    let deadline = Instant::now() + budget;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let stdout = rx.recv_timeout(Duration::from_secs(2)).unwrap_or_default();
                return Some((status.success(), stdout));
            }
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(25)),
            Err(_) => return None,
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
/// [`PrIndex::from_json`] exactly as in the bulk path. Any failure or timeout
/// yields [`BranchPrState::Unknown`], which blocks.
/// Test: `pr_state_for_branch_maps_a_failed_call_to_unknown`.
pub(crate) fn pr_state_for_branch(registry_root: &Path, branch: &str) -> BranchPrState {
    const PER_BRANCH_LIMIT: usize = 50;
    let mut cmd = gh_command(registry_root);
    cmd.args(["pr", "list", "--head", branch, "--state", "all", "--limit"])
        .arg(PER_BRANCH_LIMIT.to_string())
        .args(["--json", PR_JSON_FIELDS]);
    match run_with_timeout(cmd, GH_TIMEOUT) {
        Some((true, stdout)) => {
            PrIndex::from_json(&stdout, PER_BRANCH_LIMIT).state_for(Some(branch))
        }
        _ => BranchPrState::Unknown,
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
/// What: mirrors `remove_session_worktree`'s two-tier ownership test — the
/// `.trusty-mpm-worktree` sentinel, or the `.worktrees/<name>` convention.
/// Test: `classify_blocks_a_worktree_trusty_mpm_does_not_own`,
/// `tm_provisioned_matches_the_removers_own_predicate`.
pub(crate) fn tm_provisioned(path: &Path) -> bool {
    path.join(super::decommission::WORKTREE_SENTINEL_FILE)
        .exists()
        || super::decommission::is_session_worktree(path)
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
///    [`Live`](AgentDelegationState::Live) or
///    [`Unknown`](AgentDelegationState::Unknown). `Unknown` refuses because the
///    delegation map is rebuilt empty at every daemon boot: after a restart it
///    reports nothing for an agent that is still working, and an unanswerable
///    liveness question must never resolve to "free" (ADR-0045).
/// 2. [`SentinelOwner::Unknown`] for a path inside the harness agent store
///    ([`is_harness_agent_worktree`]). Reaching this gate means gate 3 already
///    found a sentinel FILE there — `tm_provisioned`'s other tier,
///    `.worktrees/<name>`, cannot match an agent-store path — so the file exists
///    and its content does not name an owner. Empty, truncated, garbage or
///    unreadable are indistinguishable to the tolerant parse, and any of them
///    could be hiding an agent claim. Undeterminable, not absent.
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
/// `classify_blocks_an_agent_the_registry_never_heard_of`,
/// `classify_allows_a_finished_agents_merged_worktree`,
/// `classify_blocks_an_agent_store_worktree_with_an_unreadable_sentinel`,
/// `classify_leaves_a_session_owned_worktree_alone`.
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
            AgentDelegationState::Unknown => Some(format!(
                "owned by dispatched agent {} and the delegation registry holds no record of \
                 that agent — a registry rebuilt empty at daemon boot cannot tell a finished \
                 agent from a working one, so its silence is undeterminable, not absent \
                 (#5661, ADR-0045)",
                owner.agent_id
            )),
            AgentDelegationState::Ended => None,
        },
        SentinelOwner::Unknown if is_harness_agent_worktree(path) => Some(
            "carries an ownership sentinel that names no owner (empty, malformed or unreadable) \
             inside the harness agent-worktree store — it could be an agent claim this cannot \
             read, and an unreadable claim on a destructive path is undeterminable, not absent \
             (#5661, ADR-0045)"
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

/// Whether a worktree may be reclaimed, or the first reason it may not (#2919).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ReclaimVerdict {
    /// Every gate passed; the named pull request is the landing evidence.
    Reclaimable {
        /// The merged pull request that proves the branch's work landed.
        pr: u64,
    },
    /// Refused — `reason` names the FIRST gate that said no.
    Blocked {
        /// Operator-facing explanation of the refusal.
        reason: String,
    },
    /// Refused by gate 4 because a DISPATCHED AGENT owns the worktree (#5829).
    ///
    /// Why a separate variant rather than a `Blocked` carrying agent wording:
    /// the operator has to be TOLD about this one. Every other refusal leaves
    /// a directory the operator can see and re-run against; this one spares a
    /// tree an agent is working in right now, and a sweep that spared it
    /// silently is indistinguishable from a sweep that found nothing to do —
    /// which is what `--merged-prs --force` printed while the gate was working
    /// correctly. Matching on the reason STRING to recover the distinction
    /// would re-couple the operator surface to the wording of a refusal
    /// message, so the survey records the kind instead.
    /// Test: `survey_discloses_a_live_agents_spared_worktree`,
    /// `classify_blocks_a_live_agents_worktree`.
    BlockedByAgent {
        /// Operator-facing explanation naming the agent being protected.
        reason: String,
    },
}

impl ReclaimVerdict {
    /// Shorthand for a refusal.
    pub(crate) fn blocked(reason: impl Into<String>) -> Self {
        Self::Blocked {
            reason: reason.into(),
        }
    }

    /// Shorthand for gate 4's agent-ownership refusal (#5829).
    pub(crate) fn blocked_by_agent(reason: impl Into<String>) -> Self {
        Self::BlockedByAgent {
            reason: reason.into(),
        }
    }

    /// True when this verdict permits deletion.
    pub(crate) fn is_reclaimable(&self) -> bool {
        matches!(self, Self::Reclaimable { .. })
    }
}

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
/// `classify_blocks_an_agent_the_registry_never_heard_of`,
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
    if admission != Admission::Admitted {
        return ReclaimVerdict::blocked(admission.reason());
    }
    // Gate 2 (#2919): a live session can occupy a directory whose record reads
    // terminal — measured on this repo 2026-07-28. Never trust the state field.
    if live {
        return ReclaimVerdict::blocked("a session still claims this workspace");
    }
    // Gate 3 (#2919): only worktrees trusty-mpm provisioned can actually be
    // removed. Classifying one it cannot remove as `Reclaimable` made
    // `tm doctor` advertise a command that then failed and left the directory
    // on disk — see `tm_provisioned`.
    if !tm_provisioned(path) {
        return ReclaimVerdict::blocked(
            "not a trusty-mpm-provisioned worktree — no ownership sentinel and not under \
             `.worktrees/`; the harness `.claude/worktrees/` store is out of scope (ADR-0020) \
             and `prune-worktrees` cannot remove it",
        );
    }
    // Gate 4 (#5661): a dispatched agent has no session record, so gate 2 is
    // blind to it, and gate 3 reads only whether a sentinel FILE exists — never
    // whose claim it carries. See `agent_ownership_blocks`.
    // #5829: recorded as its OWN verdict kind, because sparing a live agent's
    // tree is the one refusal the operator must be told about by name — see
    // `ReclaimVerdict::BlockedByAgent`.
    if let Some(reason) = agent_ownership_blocks(path, agent_state) {
        return ReclaimVerdict::blocked_by_agent(reason);
    }
    // Gate 5 (#2919): the merged PR is the landing evidence DOC-52 §3.4 makes
    // the reclamation trigger. Everything else — including "we could not find
    // out" — refuses.
    let merged_pr = match pr {
        BranchPrState::Merged { pr } => *pr,
        BranchPrState::Open { pr } => {
            return ReclaimVerdict::blocked(format!("PR #{pr} is still open"));
        }
        BranchPrState::ClosedUnmerged { pr } => {
            return ReclaimVerdict::blocked(format!("PR #{pr} was closed without merging"));
        }
        BranchPrState::NoPr => {
            return ReclaimVerdict::blocked("no pull request found for this branch");
        }
        BranchPrState::Unknown => {
            return ReclaimVerdict::blocked(
                "pull-request state could not be determined (is `gh` installed \
                 and authenticated?)",
            );
        }
    };
    // Gate 6 (#2919): a merged PR does NOT prove the directory holds nothing
    // novel — the 2026-07-21 salvage found merged-PR worktrees carrying real
    // unpushed source. This is the last gate and it fails toward dirty.
    if let Some(dirt) = probe_dirt(path) {
        return ReclaimVerdict::blocked(format!("holds unsaved work: {}", dirt.reason));
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
            agent_owned: Vec::new(),
            candidates: Vec::new(),
        };
        for c in &candidates {
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
            if c.pr == BranchPrState::Unknown {
                out.pr_state_unknown += 1;
            }
            // #5829: collected here, with the totals, so the disclosed list can
            // never disagree with the candidate list it is derived from.
            if let ReclaimVerdict::BlockedByAgent { reason } = &c.verdict {
                out.agent_owned
                    .push(format!("{}: {reason}", c.path.display()));
            }
            if matches!(&c.verdict, ReclaimVerdict::Blocked { reason } if reason == NOT_INSPECTED_REASON)
            {
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
