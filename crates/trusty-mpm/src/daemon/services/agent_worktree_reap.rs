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
//! # Two triggers, because one was not enough (#4311 follow-up)
//!
//! [`spawn_on_stop`] was the only caller of [`reap_worktree`], so the reap ran
//! exactly once per agent and only when a `SubagentStop` actually arrived. Every
//! other exit route — the harness session exiting or restarting under an
//! in-flight agent, an interrupt, a `tm hook` POST that lost its 2 s budget —
//! reached it never, and `prune_orphaned_worktrees` skips a `SentinelOwner::Agent`
//! tree by design, so nothing else could reclaim it either. [`spawn_on_session_end`]
//! is the second trigger: a session's end is proof its agents exited, and it
//! runs the same gates against the same store.
//!
//! Stated gap: an agent interrupted inside a session that keeps running still
//! reaches neither trigger, and its tree waits for that session to end.
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
    AgentWorktreeOwner, SentinelOwner, find_agent_worktree, read_sentinel_owner,
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
/// Why: this is the only shape this module will delete, and it is deliberately
/// the STRICT form. `worktree_reconcile::categorize` carries a looser
/// "somewhere under `.claude/worktrees`" test, but that one is documented as
/// report text that is "never an input to [`ReconcileState`]" — promoting a
/// descriptive label to a deletion gate is precisely what that doc forbids, so
/// this is a separate predicate answering a separate question.
///
/// A path the agent reported from anywhere else — a `/private/tmp` scratchpad,
/// a `/private/var/folders` tree, a checkout outside the project — is still
/// RECORDED against the delegation, and is refused here. Recording it is what
/// makes it visible; refusing it is what keeps this module from deleting a
/// directory whose provenance it cannot establish.
/// What: the immediate parent's name is `worktrees` and its parent's is
/// `.claude`.
/// Test: `reap_refuses_a_worktree_outside_the_harness_base`.
pub(crate) fn is_harness_agent_worktree(path: &Path) -> bool {
    let Some(parent) = path.parent() else {
        return false;
    };
    parent.file_name().is_some_and(|n| n == "worktrees")
        && parent
            .parent()
            .and_then(Path::file_name)
            .is_some_and(|n| n == ".claude")
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
        reap_and_record(&state, &agent_id, path, id).await;
    });
}

/// Run the reap for one agent's worktree and record what happened (#4311).
///
/// Why a shared body: [`spawn_on_stop`] and [`spawn_on_session_end`] are two
/// TRIGGERS for one decision. Two copies of the gate call, the registry
/// clear and the refusal log would eventually disagree about what a reap does,
/// and the trigger is the only thing that legitimately differs between them.
/// What: resolves this agent's `in_use` set, runs [`reap_worktree`] on a
/// blocking thread (it shells out to git and `lsof`), clears the delegation's
/// `worktree_path` on a removal so nothing reads a path that no longer exists,
/// and reports a refusal.
///
/// `id` is `None` when the path came from disk rather than the registry — there
/// is no in-memory record to clear.
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
    id: Option<DelegationId>,
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
            if let Some(id) = id {
                state.mutate_delegation(id, |d| d.worktree_path = None);
            }
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
/// Test: `session_end_spares_another_sessions_agent`,
/// `session_end_reaps_every_agent_of_the_ending_session`.
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
        .collect()
}

/// Reap the worktrees of every agent the session that just ended dispatched
/// (#4311 follow-up).
///
/// Why: [`spawn_on_stop`] was the reap's ONLY trigger, and it fires only on a
/// `SubagentStop` the daemon actually receives. An agent that ends any other way
/// — the harness session it runs inside exits or restarts under it, a user
/// interrupt, a `tm hook` POST that loses its 2 s budget — emits no stop, so the
/// reap never ran and nothing recorded that it had not. `prune_orphaned_worktrees`
/// skips `SentinelOwner::Agent` by design, so nothing else would ever reclaim
/// the tree: the leak was permanent and, for this class, entirely silent. In
/// this machine's daemon log, 54 registered agent worktrees produced 30
/// removals; 17 of the 24 survivors carry no reap decision of any kind.
///
/// A leaked worktree is not only disk. The harness reassigns a stale worktree to
/// a later agent along with the branch the previous one left checked out, which
/// is how an unrelated commit reached an open PR's branch on 2026-08-17. That
/// makes prompt reclamation a correctness property, not housekeeping.
/// What: on `SessionEnd`, resolves the harness worktree store from the payload's
/// `cwd` exactly as [`rebuild_from_disk`] does, then runs the UNCHANGED
/// [`reap_worktree`] gate stack against every directory whose sentinel names
/// this session as its parent, and reports the split.
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
/// Test: `session_end_reaps_a_worktree_whose_agent_never_stopped`,
/// `session_end_reaps_every_agent_of_the_ending_session`,
/// `session_end_keeps_a_worktree_that_holds_unsaved_work`,
/// `session_end_spares_another_sessions_agent`,
/// `session_end_without_a_cwd_reaps_nothing`.
pub fn spawn_on_session_end(
    state: &Arc<DaemonState>,
    session: SessionId,
    event: HookEvent,
    payload: &Value,
) {
    if event != HookEvent::SessionEnd {
        return;
    }
    let Some(cwd) = payload.get("cwd").and_then(Value::as_str) else {
        return;
    };
    let Some(root) = crate::core::harness_root::harness_root_for(Path::new(cwd)) else {
        return;
    };
    let candidates = agent_worktrees_of(&root.join(".claude").join("worktrees"), session);
    if candidates.is_empty() {
        return;
    }

    let state = Arc::clone(state);
    tokio::spawn(async move {
        let (mut reclaimed, mut kept) = (0usize, 0usize);
        for (path, owner) in candidates {
            // Each candidate resolves its OWN `in_use` set, which excludes its
            // own delegation. These agents never reported a stop, so their
            // records are still live and a set computed once would name every
            // candidate as in use — the sweep would then reclaim nothing.
            match reap_and_record(&state, &owner.agent_id, path, Some(owner.delegation_id)).await {
                ReapOutcome::Refused(_) => kept += 1,
                _ => reclaimed += 1,
            }
        }
        tracing::info!(
            session = %session.0,
            reclaimed,
            kept,
            "agent-worktree reap: this session ended — reclaimed {reclaimed} of its agents' \
             worktrees and kept {kept} (#4311)"
        );
    });
}

#[cfg(test)]
#[path = "agent_worktree_reap_tests.rs"]
mod tests;
