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
//! rather than reimplemented) fails toward DIRTY, and removal is git-mediated
//! with no `remove_dir_all` fallback — a `git worktree remove` this module
//! cannot complete leaves the tree alone.
//!
//! Test: the `#[cfg(test)]` suite in `agent_worktree_reap_tests.rs`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::Value;

use crate::core::hook::HookEvent;
use crate::core::session::SessionId;
use crate::daemon::state::DaemonState;
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
/// 6. `git worktree remove --force` — force because a clean tree can still hold
///    gitignored build output that plain `remove` refuses, and because gate 5
///    has already established there is nothing to lose. A non-zero exit
///    (a git-`locked` worktree exits 128) refuses.
///
/// The branch is NOT deleted. It may carry the pushed commits an open PR is
/// built on, and removing the worktree is what this reap is for.
/// Test: `reap_removes_a_clean_pushed_worktree`, `reap_refuses_a_dirty_worktree`,
/// `reap_refuses_an_unpushed_commit`, `reap_refuses_a_path_a_live_session_holds`,
/// `reap_refuses_a_worktree_outside_the_harness_base`,
/// `reap_reports_already_gone_when_the_harness_reclaimed_it`.
pub(crate) fn reap_worktree(path: &Path, in_use: &[PathBuf]) -> ReapOutcome {
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

/// Every path a reap must leave alone because something live is in it.
///
/// Why: gate 3 of [`reap_worktree`] needs both halves of "still in use", and
/// both are reads of live state that must happen close to the removal rather
/// than at dispatch time.
/// What: every managed session's `workspace_path` — deliberately UNFILTERED by
/// record state, for the reason `prune.rs` gives (a record's state is
/// bookkeeping, not a liveness signal) — plus the registered worktree of every
/// delegation in this session that is not terminal, excluding `self_id`'s own.
/// Test: `reap_refuses_a_path_a_live_session_holds`.
async fn paths_in_use(
    state: &Arc<DaemonState>,
    session: SessionId,
    self_agent_id: &str,
) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = state
        .session_manager()
        .await
        .list()
        .await
        .into_iter()
        .filter_map(|r| r.workspace_path)
        .collect();
    for d in state.delegations_for(session) {
        if d.status.is_terminal() || d.agent_id.as_deref() == Some(self_agent_id) {
            continue;
        }
        if let Some(p) = d.worktree_path {
            out.push(p);
        }
    }
    out
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
    let Some((id, path)) = state
        .delegations_for(session)
        .into_iter()
        .find(|d| d.agent_id.as_deref() == Some(agent_id.as_str()))
        .and_then(|d| d.worktree_path.clone().map(|p| (d.id, p)))
    else {
        return;
    };

    let state = Arc::clone(state);
    tokio::spawn(async move {
        let in_use = paths_in_use(&state, session, &agent_id).await;
        let removal_path = path.clone();
        let outcome = tokio::task::spawn_blocking(move || reap_worktree(&removal_path, &in_use))
            .await
            .unwrap_or_else(|e| ReapOutcome::Refused(format!("reap task panicked: {e}")));
        match &outcome {
            ReapOutcome::Removed | ReapOutcome::AlreadyGone => {
                state.mutate_delegation(id, |d| d.worktree_path = None);
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
