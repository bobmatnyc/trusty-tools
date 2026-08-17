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
//! # The record survives a restart (#4311)
//!
//! `DaemonState::delegations` is a `DashMap` built empty at every boot with no
//! load path, so a restart used to end the reap permanently for any agent alive
//! across it — a `SubagentStop` arrives once. Registration now writes an
//! ownership sentinel into the worktree, and [`rebuild_from_disk`] reads it
//! back when the registry has nothing, which is the from-disk reconstruction
//! ADR-0023 point 4 requires of an ownership record.
//!
//! Test: the `#[cfg(test)]` suite in `agent_worktree_reap_tests.rs`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::Value;

use crate::core::hook::HookEvent;
use crate::core::session::SessionId;
use crate::daemon::state::DaemonState;
use crate::session_manager::worktree_liveness::process_holding;
use crate::session_manager::worktree_ownership::{
    AgentDelegationState, SentinelOwner, find_agent_worktree, is_harness_agent_worktree,
    read_sentinel_owner,
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

/// Is `path` a leaf of a harness agent-worktree store — `…/.claude/worktrees/<name>`?
///
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
///    session's `workspace_path` and every other non-terminal delegation's
///    registered worktree, so a tree whose owning session is still live, or
///    that a sibling agent is still writing in, survives.
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
/// non-terminal delegation in EVERY session, excluding `self_agent_id`'s own.
///
/// The delegation half was scoped to the stopping agent's own session until
/// #4311's review. That scoping was wrong for the same reason the record-state
/// filter above is: a sibling agent dispatched from a different session is
/// exactly as live, and its worktree is exactly as un-deletable. Sessions are
/// not an isolation boundary for "is something running in this directory".
/// Test: `reap_refuses_a_path_a_live_session_holds`,
/// `reap_spares_a_worktree_another_sessions_agent_still_holds`.
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
        if d.status.is_terminal() || d.agent_id.as_deref() == Some(self_agent_id) {
            continue;
        }
        if let Some(p) = d.worktree_path {
            out.push(p);
        }
    }
    out
}

/// Recover an agent's worktree path from on-disk sentinels alone (#4311).
///
/// Why: the delegation map is a `DashMap` built empty at every daemon boot with
/// no load path, so a restart drops every registration and, before this, ended
/// the reap for every agent alive across it — permanently, since a `SubagentStop`
/// arrives once. ADR-0023 point 4 requires the ownership record to be
/// rebuildable from on-disk sentinels plus git and nothing else; this is that
/// rebuild, applied to the one record the stop needs.
/// What: resolves the harness worktree store from the stopping payload's `cwd`
/// — [`harness_root_for`](crate::core::harness_root::harness_root_for) maps any
/// checkout back to the one that owns the project's state, and ADR-0036 puts
/// every worktree flat under `<that>/.claude/worktrees/` — then asks
/// [`find_agent_worktree`] for the directory whose sentinel names this exact
/// `agent_id`.
///
/// Returns `None` for a payload with no `cwd`, a `cwd` outside any repository,
/// or a store holding no sentinel for this agent. Every one of those ends the
/// reap, which is the pre-#4311 behaviour and keeps the directory.
/// Test: `stop_reaps_a_worktree_the_registry_lost_to_a_restart`,
/// `stop_ignores_a_sentinel_naming_a_different_agent`.
fn rebuild_from_disk(payload: &Value, agent_id: &str) -> Option<PathBuf> {
    let cwd = payload.get("cwd").and_then(Value::as_str)?;
    let root = crate::core::harness_root::harness_root_for(Path::new(cwd))?;
    let found = find_agent_worktree(&root.join(".claude").join("worktrees"), agent_id)?;
    tracing::info!(
        agent_id,
        worktree = %found.display(),
        "agent-worktree reap: the delegation registry did not know this agent — its \
         ownership sentinel did, so the reap proceeds against the rebuilt record (#4311)"
    );
    Some(found)
}

/// Reap the worktree of the agent a `SubagentStop` just ended (#4311).
///
/// Why: `SubagentStop` is the authoritative exit signal — it carries an exact
/// `agent_id`, so closing one of two concurrent agents cannot close the other
/// (see [`super::delegation_tracker`]'s correlation note). DOC-66 §5 makes the
/// agent's exit the reap trigger, and this is the only event that reports one.
/// What: no-ops for every event other than `SubagentStop`/`SubagentStopFailure`,
/// for a payload with no `agent_id`, and for a delegation carrying no registered
/// worktree. Otherwise spawns a detached task — the git calls are blocking and
/// this runs inside the hook's synchronous budget — which clears the record's
/// `worktree_path` on a removal so nothing reads a path that no longer exists.
///
/// A stale delegation is NOT reaped. `Stale` means tracking lost the agent, not
/// that it exited, and removing the tree of an agent that may still be running
/// is the one mistake this module must never make. Such a record keeps its
/// `worktree_path`, which is what makes the tree visible to `tm doctor` and the
/// reconcile report instead of invisible as it is today. Stated gap.
/// Test: `spawn_on_stop_ignores_a_non_stop_event`,
/// `spawn_on_stop_ignores_a_payload_without_an_agent_id`.
pub fn spawn_on_stop(
    state: &Arc<DaemonState>,
    session: SessionId,
    event: HookEvent,
    payload: &Value,
) {
    if !matches!(
        event,
        HookEvent::SubagentStop | HookEvent::SubagentStopFailure
    ) {
        return;
    }
    let Some(agent_id) = payload
        .get("agent_id")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
    else {
        return;
    };
    let registered = state
        .delegations_for(session)
        .into_iter()
        .find(|d| d.agent_id.as_deref() == Some(agent_id.as_str()))
        .and_then(|d| d.worktree_path.clone().map(|p| (Some(d.id), p)));
    let Some((id, path)) =
        registered.or_else(|| rebuild_from_disk(payload, &agent_id).map(|p| (None, p)))
    else {
        return;
    };

    let state = Arc::clone(state);
    tokio::spawn(async move {
        let in_use = paths_in_use(&state, &agent_id).await;
        let removal_path = path.clone();
        let reap_agent_id = agent_id.clone();
        let outcome = tokio::task::spawn_blocking(move || {
            reap_worktree(&removal_path, &reap_agent_id, &in_use)
        })
        .await
        .unwrap_or_else(|e| ReapOutcome::Refused(format!("reap task panicked: {e}")));
        match &outcome {
            ReapOutcome::Removed | ReapOutcome::AlreadyGone => {
                // `None` when the path came from disk rather than the registry
                // — there is no in-memory record left to clear.
                if let Some(id) = id {
                    state.mutate_delegation(id, |d| d.worktree_path = None);
                }
            }
            ReapOutcome::Refused(reason) => {
                tracing::info!(
                    path = %path.display(),
                    "agent-worktree reap: keeping this worktree — {reason} (#4311)"
                );
            }
        }
    });
}

#[cfg(test)]
#[path = "agent_worktree_reap_tests.rs"]
mod tests;
