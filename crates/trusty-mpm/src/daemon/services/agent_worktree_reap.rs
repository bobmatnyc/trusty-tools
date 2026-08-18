//! Reap the worktree an exiting agent was working in (#4311).
//!
//! Why: a measurement on 2026-07-29 found 119 worktree registrations across two
//! repos against a handful of real workstreams, 56 of them under
//! `.claude/worktrees` and every one of those unowned. An earlier sweep had
//! already reclaimed ~1.1 TiB by deleting 63 stale merged-PR worktrees, so
//! cleanup is not the fix — the trees accumulate because nothing owns them, and
//! ADR-0020's fail-closed rule correctly refuses to remove a worktree whose
//! owner it cannot name.
//!
//! # trusty-mpm still creates nothing
//!
//! [ADR-0044](../../../../../docs/adr/0044-main-checkout-write-boundary-and-agent-worktree-ownership.md)
//! decision 4 and [ADR-0048](../../../../../docs/adr/0048-dispatched-writers-get-a-worktree-and-the-write-boundary-is-enforced.md)
//! decision 2 assign worktree CREATION to the harness. This module does not
//! create one, does not choose a path, and does not ask for one. It reads a path
//! the agent itself reported and applies a reclamation authority to it. Creation
//! and reclamation are already separate authorities in this codebase —
//! [ADR-0023](../../../../../docs/adr/0023-worktree-authority-existence-vs-ownership.md)
//! splits them: git decides existence, a rebuildable record decides ownership.
//! The record here is the delegation. Registration is what turns an agent
//! worktree from owner-UNKNOWN into owner-KNOWN, so ADR-0020's fail-closed rule
//! stops applying to it by that rule's own terms rather than being weakened.
//! An ADR for this is deferred: the next ADR number is reserved by an unmerged
//! branch, and taking it would race that branch.
//!
//! # What actually survives to be reaped
//!
//! The harness reclaims a granted worktree when it is UNCHANGED, so the
//! population reaching this module has changes in it. Everything that holds
//! unsaved work is refused here, which leaves exactly one class: a tree whose
//! work is committed AND pushed. That is the merged-PR shape the 1.1 TiB sweep
//! deleted by hand.
//!
//! # Fail direction
//!
//! Toward KEEPING the directory. Every gate below refuses on uncertainty, the
//! dirt gate ([`crate::session_manager::worktree_safety::inspect_dirt`], reused
//! rather than reimplemented) fails toward DIRTY, the liveness gate
//! ([`crate::session_manager::worktree_liveness::process_holding`]) fails
//! toward IN USE, and removal is git-mediated with no `remove_dir_all`
//! fallback — a `git worktree remove` this module cannot complete leaves the
//! tree alone.
//!
//! # One trigger, because the other one had no proof (#5800)
//!
//! [`spawn_on_session_end`] is now the only trigger. A `SubagentStop` one was
//! removed: that event is a TURN boundary, not the agent's exit, and the agent
//! stays resumable after it. Four reaps on 2026-08-18 removed trees whose agents
//! the harness still considered resumable — one had finished and pushed its work
//! to an open PR — and the next `SendMessage` to each failed with "This agent
//! cannot be resumed: its worktree no longer exists". Two properties made that
//! reachable, and both were reachable ONLY from the stop trigger:
//!
//! - **A terminal delegation status was read as a release.** It is not. It says
//!   trusty-mpm watched the delegation finish, and says nothing about whether the
//!   harness still needs the directory to resume the agent.
//! - **An agent the registry had never heard of was reaped anyway.** The removed
//!   `rebuild_from_disk` reconstructed ownership from the on-disk sentinel when
//!   `DaemonState::delegations` — a `DashMap` rebuilt empty at every boot — held
//!   no record, and proceeded on that reconstruction. That is fail-OPEN on the
//!   `Unknown` state [ADR-0045](../../../../../docs/adr/0045-distinguish-absent-from-undeterminable-on-destructive-paths.md)
//!   requires be treated as undeterminable, and it is the inverse of what
//!   `worktree_reclaim::agent_ownership_blocks` does on the sibling reclaim path.
//!
//! A `SessionEnd` carries the proof a stop does not. A subagent runs inside its
//! parent Claude Code session's process and cannot outlive it, so the session's
//! end is Claude Code self-reporting that every agent it dispatched is gone and
//! unresumable. That is the positive release signal this module now requires, and
//! it is the one signal available today: the `.trusty-mpm-worktree` sentinel is
//! not yet authoritative over the harness store (ADR-0055 decision C, unimplemented
//! — #5997), so nothing on disk can answer "is this tree still resumable" instead.
//!
//! # What that costs, and why it is the right direction
//!
//! An agent whose session keeps running holds its tree until that session ends,
//! and a session killed hard enough to emit no `SessionEnd` holds it until
//! `tm session prune-worktrees --merged-prs` reclaims it behind
//! `worktree_reclaim`'s own fail-closed gates. Both are leaks, and a leak is
//! recoverable. The deletion this replaced was not: it destroyed ~11 minutes of
//! in-flight build once and broke agent resumption four times in one day.
//! Test: the `#[cfg(test)]` suite in `agent_worktree_reap_tests.rs`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::Value;

use crate::core::agent::DelegationId;
use crate::core::hook::HookEvent;
use crate::core::session::SessionId;
use crate::daemon::state::DaemonState;
use crate::session_manager::worktree_liveness::process_holding;
use crate::session_manager::worktree_ownership::{
    AgentDelegationState, AgentWorktreeOwner, SentinelOwner, find_agent_worktree,
    is_harness_agent_worktree, read_sentinel_owner,
};
use crate::session_manager::worktree_safety::{DirtyWorktreePolicy, dirt_blocks_removal};

/// What happened to one registered agent worktree.
///
/// Why: "kept" and "gone" are not the interesting distinction — WHY it was kept
/// is, because a refusal is the normal outcome for a tree holding work and an
/// operator reading the daemon log needs to tell that apart from a failure.
/// Test: `reap_refuses_a_dirty_worktree`, `reap_removes_a_clean_pushed_worktree`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReapOutcome {
    /// Git removed the worktree and its registry entry.
    Removed,
    /// Nothing was at the path — the harness reclaimed it first.
    AlreadyGone,
    /// Left in place, for the stated reason.
    Refused(String),
}

impl ReapOutcome {
    /// The refusal reason, when this outcome is one.
    pub fn refusal(&self) -> Option<&str> {
        match self {
            Self::Refused(r) => Some(r),
            _ => None,
        }
    }
}

/// What one `SessionEnd` sweep did, split by [`ReapOutcome`].
///
/// Why `already_gone` is its own counter: the summary used to fold it into
/// "reclaimed" via a `_ =>` arm, which reported a sweep that removed nothing as
/// having reclaimed every candidate. `AlreadyGone` means the harness got there
/// first, so counting it as work this sweep did overstates the only number an
/// operator reads to judge whether the reap is doing anything.
/// Test: `session_end_keeps_a_worktree_that_holds_unsaved_work`,
/// `session_end_spares_another_sessions_agent`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SweepSummary {
    /// `git worktree remove --force` succeeded.
    pub removed: usize,
    /// Nothing was at the path when the sweep reached it.
    pub already_gone: usize,
    /// A gate refused, and the directory is still there.
    pub kept: usize,
}

/// What the delegation registry can say about `agent_id` (#5661).
///
/// Why: the merged-PR reclaim sweep needs the same "is this agent still
/// working?" answer this module's gate 3 gets from [`paths_in_use`], but keyed
/// on the agent the SENTINEL names rather than on a path — the sweep starts from
/// a directory, not from a stop event. Resolving it here keeps one reading of
/// `DaemonState::delegations`; a second copy in `session_manager` would drift
/// from this one, and both authorise deletions.
/// What: scans every delegation in every session for one whose `agent_id`
/// matches. Any non-terminal match is [`AgentDelegationState::Live`]; matches
/// that are all terminal are [`AgentDelegationState::Ended`]; no match at all is
/// [`AgentDelegationState::Unknown`] — which is what a post-restart empty
/// `DashMap` yields, and is why the sweep treats it as undeterminable rather
/// than as "the agent is gone".
/// Test: `delegation_state_reports_live_ended_and_unknown`.
pub(crate) fn delegation_state_for_agent(
    state: &Arc<DaemonState>,
    agent_id: &str,
) -> AgentDelegationState {
    let mut seen = false;
    for d in state.all_delegations() {
        if d.agent_id.as_deref() != Some(agent_id) {
            continue;
        }
        if !d.status.is_terminal() {
            return AgentDelegationState::Live;
        }
        seen = true;
    }
    if seen {
        AgentDelegationState::Ended
    } else {
        AgentDelegationState::Unknown
    }
}

/// Reap one registered agent worktree, or say why not.
///
/// Why: the whole decision in one function so the refusal arms are testable
/// without a daemon, a session store, or a hook payload.
/// What, in order — every arm refuses toward keeping the directory:
///
/// 1. The path does not exist → [`ReapOutcome::AlreadyGone`]. The harness got
///    there first, which is the expected outcome for an unchanged tree.
/// 2. Not a `…/.claude/worktrees/<name>` leaf → refuse
///    ([`is_harness_agent_worktree`]).
/// 3. `in_use` names the path → refuse. The caller supplies every live managed
///    session's `workspace_path` and every other delegation's registered
///    worktree — terminal ones included since #5800, because a finished
///    delegation's tree can still be one the harness needs to resume its agent.
///    So a tree whose owning session is still live, that a sibling agent is
///    still writing in, or that another agent may yet be resumed into, survives.
/// 4. No git registry claims the path → refuse. There is deliberately no
///    `remove_dir_all` fallback here: `decommission::remove_session_worktree`
///    has one because it is removing a tree trusty-mpm created and can
///    recognise by its own sentinel, and this module can do neither.
/// 5. [`dirt_blocks_removal`] reports unsaved work → refuse, naming the counts.
///    This is #4091's check, reused — uncommitted files, untracked files, a
///    nested dirty checkout and unpushed commits are all one implementation, and
///    a second copy here would drift from it.
/// 6. [`process_holding`] finds a live process standing in the tree → refuse.
///    Gates 3 and 5 are both REGISTRY reads: gate 3 asks what trusty-mpm
///    recorded, gate 5 asks what git recorded. Neither can see a process
///    nobody told either of them about, and on 2026-08-15 a
///    `trusty-memory serve --foreground` started by hand inside an agent
///    worktree ran for a day in exactly that blind spot. This gate asks the
///    OS instead, and fails toward IN USE when it cannot get an answer.
/// 7. `git worktree remove --force` — force because a clean tree can still hold
///    gitignored build output that plain `remove` refuses, and because gates 5
///    and 6 have already established there is nothing to lose. A non-zero exit
///    (a git-`locked` worktree exits 128) refuses.
///
/// The branch is NOT deleted. It may carry the pushed commits an open PR is
/// built on, and removing the worktree is what this reap is for.
/// Test: `reap_removes_a_clean_pushed_worktree`, `reap_refuses_a_dirty_worktree`,
/// `reap_refuses_an_unpushed_commit`, `reap_refuses_a_path_a_live_session_holds`,
/// `reap_refuses_a_worktree_outside_the_harness_base`,
/// `reap_reports_already_gone_when_the_harness_reclaimed_it`,
/// `reap_refuses_a_worktree_a_live_process_is_standing_in`,
/// `reap_refuses_a_path_whose_sentinel_names_another_agent`,
/// `reap_refuses_a_path_with_no_sentinel`.
pub(crate) fn reap_worktree(path: &Path, agent_id: &str, in_use: &[PathBuf]) -> ReapOutcome {
    if !path.exists() {
        return ReapOutcome::AlreadyGone;
    }
    if !is_harness_agent_worktree(path) {
        return ReapOutcome::Refused(format!(
            "{} is not a `.claude/worktrees/<name>` leaf — trusty-mpm reaps only the store \
             ADR-0036 assigns to harness agent worktrees",
            path.display()
        ));
    }
    // #4311: confirm ownership against the DIRECTORY. Every other input to this
    // decision — `d.worktree_path` from the registry, or the path
    // `rebuild_from_disk` matched — was resolved somewhere else and could name
    // a directory this agent never owned. The sentinel sitting IN the target is
    // the only claim that cannot have been redirected on the way here, so it is
    // read rather than trusted from the caller. This gate holds independently
    // of the write-site claims in
    // `delegation_tracker::register_agent_worktree`; either one alone would
    // leave the removal reachable through the other path.
    //
    // Checked HERE so an unattributed path is refused before the expensive
    // gates below, and again immediately before the removal — #4118's
    // precedent, established when a per-candidate dirty verdict computed at
    // scan time went stale across a minutes-long sweep. The gates between the
    // two reads take real time (git subprocesses for the dirt check, ~1s of
    // `lsof` for the liveness probe), so the authoritative read is the one
    // adjacent to the deletion.
    if let Some(refusal) = ownership_unconfirmed(path, agent_id) {
        return ReapOutcome::Refused(refusal);
    }
    let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    if in_use
        .iter()
        .any(|p| std::fs::canonicalize(p).unwrap_or_else(|_| p.clone()) == canonical || p == path)
    {
        return ReapOutcome::Refused(format!(
            "{} is still in use — a live session records it as its workspace, or another \
             delegation that has not ended is registered against it",
            path.display()
        ));
    }
    let Some(registry_root) = crate::session_manager::worktree_registry::registry_root_for(path)
    else {
        return ReapOutcome::Refused(format!(
            "no git repository claims {} — refusing to remove a directory git does not \
             recognise as a worktree",
            path.display()
        ));
    };
    if let Some(dirt) = dirt_blocks_removal(path, DirtyWorktreePolicy::Skip, "agent-reap") {
        return ReapOutcome::Refused(format!(
            "{} holds unsaved work ({}): {} dirty file(s), {} unpushed commit(s)",
            path.display(),
            dirt.reason,
            dirt.dirty_files,
            dirt.unpushed_commits
        ));
    }
    // #4311: the only gate that asks the OS rather than a record trusty-mpm or
    // git wrote. Last, because it is the most expensive.
    if let Some(holder) = process_holding(path) {
        return ReapOutcome::Refused(format!("{} is still occupied — {holder}", path.display()));
    }
    // #4118 pattern: the authoritative ownership read is the one adjacent to
    // the removal. The only in-window transition is another agent claiming this
    // sentinel, which the write-site claims refuse while one is present — so
    // this is a cheap re-read that makes the guarantee structural rather than
    // dependent on that argument holding.
    if let Some(refusal) = ownership_unconfirmed(path, agent_id) {
        return ReapOutcome::Refused(refusal);
    }
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(&registry_root)
        .args(["worktree", "remove", "--force"])
        .arg(path)
        .output();
    match out {
        Ok(o) if o.status.success() => {
            tracing::info!(
                path = %path.display(),
                "agent-worktree reap: removed the worktree its agent has finished with (#4311)"
            );
            ReapOutcome::Removed
        }
        Ok(o) => ReapOutcome::Refused(format!(
            "`git worktree remove --force` exited {}: {}",
            o.status,
            String::from_utf8_lossy(&o.stderr).trim()
        )),
        Err(e) => ReapOutcome::Refused(format!("could not run git: {e}")),
    }
}

/// Why `path`'s on-disk sentinel does not confirm `agent_id` owns it, or `None`
/// when it does (#4311).
///
/// Why a function and not two inline matches: [`reap_worktree`] runs this twice
/// — once to fail fast before the expensive gates, once immediately before the
/// removal — and two copies of a refusal this load-bearing would eventually
/// disagree.
/// Test: `reap_refuses_a_path_whose_sentinel_names_another_agent`,
/// `reap_refuses_a_path_with_no_sentinel`.
fn ownership_unconfirmed(path: &Path, agent_id: &str) -> Option<String> {
    match read_sentinel_owner(path) {
        SentinelOwner::Agent(owner, _) if owner.agent_id == agent_id => None,
        other => Some(format!(
            "{} does not carry a sentinel naming agent {agent_id} (found {other:?}) — \
             refusing to remove a directory whose ownership it cannot confirm on disk",
            path.display()
        )),
    }
}

/// Every path a reap must leave alone because something live is in it.
///
/// Why: gate 3 of [`reap_worktree`] needs both halves of "still in use", and
/// both are reads of live state that must happen close to the removal rather
/// than at dispatch time.
/// What: every managed session's `workspace_path` — deliberately UNFILTERED by
/// record state, for the reason `prune.rs` gives (a record's state is
/// bookkeeping, not a liveness signal) — plus the registered worktree of every
/// delegation in EVERY session, excluding `self_agent_id`'s own.
///
/// The delegation half was scoped to the stopping agent's own session until
/// #4311's review. That scoping was wrong for the same reason the record-state
/// filter above is: a sibling agent dispatched from a different session is
/// exactly as live, and its worktree is exactly as un-deletable. Sessions are
/// not an isolation boundary for "is something running in this directory".
///
/// # A terminal delegation is not a released tree (#5800)
///
/// This dropped every terminal delegation from the set until #5800, which made
/// `is_terminal()` decide a question it cannot answer. Terminal means trusty-mpm
/// watched THIS delegation finish. The harness can still hold the same agent's
/// worktree open for a resume, and on 2026-08-18 it did: an agent that had
/// finished and pushed to an open PR had its tree removed and became permanently
/// unreachable. The record-state filter is dropped here for the same reason
/// `prune.rs` never had one — bookkeeping is not a liveness signal, in either
/// direction. Excluding `self_agent_id` is what still lets a candidate be
/// reclaimed; without that exclusion the sweep would find every tree in use and
/// reclaim none.
/// Test: `reap_refuses_a_path_a_live_session_holds`,
/// `reap_spares_a_worktree_another_sessions_agent_still_holds`,
/// `paths_in_use_protects_a_terminal_delegations_worktree`.
async fn paths_in_use(state: &Arc<DaemonState>, self_agent_id: &str) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = state
        .session_manager()
        .await
        .list()
        .await
        .into_iter()
        .filter_map(|r| r.workspace_path)
        .collect();
    for d in state.all_delegations() {
        if d.agent_id.as_deref() == Some(self_agent_id) {
            continue;
        }
        if let Some(p) = d.worktree_path {
            out.push(p);
        }
    }
    out
}

/// The harness agent-worktree store a hook payload's `cwd` names, or `None`.
///
/// Why a named function rather than three lines inlined twice: it is the ONLY
/// thing standing between a payload that carries no `cwd` and a sweep over a
/// store resolved from somewhere else. Both callers must refuse identically, and
/// a test can assert the refusal directly instead of inferring it from a
/// directory that happens to survive.
/// What: `cwd` → [`harness_root_for`](crate::core::harness_root::harness_root_for)
/// → `<root>/.claude/worktrees`, the flat store ADR-0036 defines. `None` for a
/// payload with no `cwd`, a non-string `cwd`, or a `cwd` outside any repository.
/// Test: `session_end_without_a_cwd_resolves_no_store`.
fn store_for(payload: &Value) -> Option<PathBuf> {
    let cwd = payload.get("cwd").and_then(Value::as_str)?;
    let root = crate::core::harness_root::harness_root_for(Path::new(cwd))?;
    Some(root.join(".claude").join("worktrees"))
}

/// Run the reap for one agent's worktree and record what happened (#4311).
///
/// Why still its own function with one caller: it is the whole decision minus
/// the trigger, and #5800 removed a second trigger rather than a second copy of
/// this body. Keeping the split is what made that removal a deletion of
/// `spawn_on_stop` alone, with no gate, log line or registry write to
/// re-derive.
/// What: resolves this agent's `in_use` set, runs [`reap_worktree`] on a
/// blocking thread (it shells out to git and `lsof`), clears the delegation's
/// `worktree_path` on a removal so nothing reads a path that no longer exists,
/// and reports a refusal.
///
/// `id` comes from the candidate's own sentinel, so it names a delegation the
/// registry may no longer hold; [`DaemonState::mutate_delegation`] no-ops on an
/// id it does not know.
///
/// The refusal is `warn!` and not `info!` (#4311 follow-up): a refusal is the
/// ONLY notice anyone gets that a tree was kept, and nothing retries it. On this
/// machine 7 worktrees were refused as holding unsaved work, logged once at
/// `info!`, and left on disk indefinitely with nothing surfacing them. A
/// refusal is a tree that now needs a human, so it is logged as one.
/// Test: `session_end_keeps_a_worktree_that_holds_unsaved_work`,
/// `reap_removes_a_clean_pushed_worktree`.
async fn reap_and_record(
    state: &Arc<DaemonState>,
    agent_id: &str,
    path: PathBuf,
    id: DelegationId,
) -> ReapOutcome {
    let in_use = paths_in_use(state, agent_id).await;
    let removal_path = path.clone();
    let reap_agent_id = agent_id.to_string();
    let outcome =
        tokio::task::spawn_blocking(move || reap_worktree(&removal_path, &reap_agent_id, &in_use))
            .await
            .unwrap_or_else(|e| ReapOutcome::Refused(format!("reap task panicked: {e}")));
    match &outcome {
        ReapOutcome::Removed | ReapOutcome::AlreadyGone => {
            state.mutate_delegation(id, |d| d.worktree_path = None);
        }
        ReapOutcome::Refused(reason) => {
            tracing::warn!(
                agent_id,
                path = %path.display(),
                "agent-worktree reap: keeping this worktree — {reason} (#4311)"
            );
        }
    }
    outcome
}

/// Every worktree in `store` whose sentinel names `session` as its parent.
///
/// Why the sentinel and not the delegation registry: the registry is a `DashMap`
/// built empty at every boot, so after a restart it knows none of the trees this
/// sweep exists to reclaim. The sentinel is the durable record ADR-0023 point 4
/// requires, and `parent_session_id` is the field DOC-66 §5 defines as an agent
/// worktree's parentage.
/// What: reads each immediate child of `store` and keeps those whose sentinel is
/// [`SentinelOwner::Agent`] with a matching `parent_session_id`. An unreadable
/// store yields nothing, which reclaims nothing.
///
/// # One agent must name one directory (#5800, ADR-0045)
///
/// A candidate is also dropped unless [`find_agent_worktree`] resolves its
/// agent back to this exact directory. That lookup returns `None` when two
/// directories carry a sentinel naming one agent, which is a state this store is
/// measurably in: on 2026-08-18, `agent-a23097670a51a0450`'s sentinel recorded
/// `agent_id: a5ed76bc80fd52e41`, cause undetermined. Nothing deletes a stale
/// sentinel and registration re-fires on every `PreToolUse`, so an agent that
/// moved between trees leaves the first one stamped. Which directory such an
/// agent owns is undeterminable, and an undeterminable owner on a destructive
/// path keeps every candidate rather than picking one.
/// Test: `session_end_spares_another_sessions_agent`,
/// `session_end_reaps_every_agent_of_the_ending_session`,
/// `session_end_keeps_a_worktree_whose_agent_names_two_directories`.
fn agent_worktrees_of(store: &Path, session: SessionId) -> Vec<(PathBuf, AgentWorktreeOwner)> {
    let Ok(entries) = std::fs::read_dir(store) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|e| {
            let path = e.path();
            match read_sentinel_owner(&path) {
                SentinelOwner::Agent(owner, _) if owner.parent_session_id == session => {
                    Some((path, owner))
                }
                _ => None,
            }
        })
        .filter(|(path, owner)| {
            if find_agent_worktree(store, &owner.agent_id).as_deref() == Some(path.as_path()) {
                return true;
            }
            tracing::warn!(
                agent_id = %owner.agent_id,
                path = %path.display(),
                "agent-worktree reap: keeping this worktree — its agent's sentinel does not \
                 resolve to this directory alone, so which tree it owns is undeterminable \
                 (#5800, ADR-0045)"
            );
            false
        })
        .collect()
}

/// Reap the worktrees of every agent the session that just ended dispatched
/// (#4311 follow-up).
///
/// Why: a `SubagentStop` trigger was the reap's only other one, and it fired on
/// an event that reports a turn boundary rather than an exit — #5800 removed it
/// for reaping trees the harness still needed. An agent that ends any way at all
/// — the harness session it runs inside exits or restarts under it, a user
/// interrupt, a `tm hook` POST that loses its 2 s budget — reaches this trigger
/// and no other. `prune_orphaned_worktrees` skips `SentinelOwner::Agent` by
/// design, so without it nothing else would ever reclaim the tree: the leak was
/// permanent and, for this class, entirely silent. In this machine's daemon log,
/// 54 registered agent worktrees produced 30 removals; 17 of the 24 survivors
/// carry no reap decision of any kind.
///
/// A leaked worktree is not only disk. The harness reassigns a stale worktree to
/// a later agent along with the branch the previous one left checked out, which
/// is how an unrelated commit reached an open PR's branch on 2026-08-17. That
/// makes prompt reclamation a correctness property, not housekeeping.
/// What: on `SessionEnd`, resolves the harness worktree store with [`store_for`],
/// then runs the [`reap_worktree`] gate stack against every directory whose
/// sentinel names this session as its parent, and reports the split as a
/// [`SweepSummary`].
///
/// # Why a `SessionEnd` is proof its agents exited
///
/// A subagent runs inside its parent Claude Code session's process and cannot
/// outlive it. `SessionEnd` is Claude Code self-reporting that this conversation
/// ended — the same signal #4337 already treats as unambiguous when it clears a
/// stale `claude_session_id`. That is the strength of proof `SubagentStop`
/// carries, applied to every child at once, so no gate is weakened to use it: a
/// candidate still has to pass the sentinel, in-use, git-registry, dirt and
/// process-liveness gates, and a tree holding unsaved work is still kept.
///
/// # The returned handle
///
/// `None` means no sweep ran: the event was not a `SessionEnd`, the payload
/// resolved no store ([`store_for`]), or no directory in that store names this
/// session. `Some` is the detached task, and awaiting it yields the
/// [`SweepSummary`] after EVERY candidate has been decided.
///
/// That await is the completion signal the tests use. A negative test — "this
/// tree must survive" — sampled the filesystem after a fixed 750 ms, and one
/// `lsof` alone costs 0.56-0.67 s on an idle machine before the dirt gate and
/// the removal even run. So the assertion fired while a real removal was still
/// in flight and passed against a gate stack that had been deleted. Awaiting the
/// handle removes the window rather than widening it.
/// Test: `session_end_reaps_a_worktree_whose_agent_never_stopped`,
/// `session_end_reaps_every_agent_of_the_ending_session`,
/// `session_end_keeps_a_worktree_that_holds_unsaved_work`,
/// `session_end_spares_another_sessions_agent`,
/// `session_end_without_a_cwd_reaps_nothing`,
/// `session_end_without_a_cwd_resolves_no_store`.
pub fn spawn_on_session_end(
    state: &Arc<DaemonState>,
    session: SessionId,
    event: HookEvent,
    payload: &Value,
) -> Option<tokio::task::JoinHandle<SweepSummary>> {
    if event != HookEvent::SessionEnd {
        return None;
    }
    let candidates = agent_worktrees_of(&store_for(payload)?, session);
    if candidates.is_empty() {
        return None;
    }

    let state = Arc::clone(state);
    Some(tokio::spawn(async move {
        let mut summary = SweepSummary::default();
        for (path, owner) in candidates {
            // Each candidate resolves its OWN `in_use` set, which excludes its
            // own delegation. These agents never reported a stop, so their
            // records are still live and a set computed once would name every
            // candidate as in use — the sweep would then reclaim nothing.
            match reap_and_record(&state, &owner.agent_id, path, owner.delegation_id).await {
                ReapOutcome::Removed => summary.removed += 1,
                ReapOutcome::AlreadyGone => summary.already_gone += 1,
                ReapOutcome::Refused(_) => summary.kept += 1,
            }
        }
        let SweepSummary {
            removed,
            already_gone,
            kept,
        } = summary;
        tracing::info!(
            session = %session.0,
            removed,
            already_gone,
            kept,
            "agent-worktree reap: this session ended — removed {removed} of its agents' \
             worktrees, found {already_gone} already gone and kept {kept} (#4311)"
        );
        summary
    }))
}

#[cfg(test)]
#[path = "agent_worktree_reap_tests.rs"]
mod tests;
