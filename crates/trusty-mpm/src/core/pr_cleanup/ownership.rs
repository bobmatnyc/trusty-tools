//! Which trees and claims one `tm pr cleanup` run may take (#8301, #7771).
//!
//! Why: [`super::remove::remove_one`] ended every session's claim on a tree and then
//! removed it, so session X's `tm pr merge` took session Y's idle, resumable
//! agent tree. It now applies the prune sweep's own rule first.
//! What: [`gate`] refuses a tree any claim holder other than the caller or a
//! provably ended session holds, then asks [`ClaimOwnership::tree_gate`]. The
//! trait is crate-private, so the public `ClaimEnder` is unchanged: the public
//! entry points run [`CallerOwnership`], which knows only this process's own
//! session ids, and the supervisor passes its session store instead.
//!
//! Condition (b), a live delegation naming the agent, is not reachable from
//! either cleanup process: the delegation registry lives in the daemon's
//! memory. (a) the harness lock and (d) the owner file's session cover the
//! agent the registry would name.
//! [`regate`] asks both questions again immediately before the removal, and
//! [`ClaimOwnership::release_stale_lock`] judges the lock a third time at the
//! moment it would be released.
//! Test: `cleanup_keeps_a_tree_another_live_session_claims`,
//! `cleanup_8301_two_sessions_each_keep_their_own_tree`,
//! `cleanup_keeps_a_tree_the_ownership_gate_refuses`,
//! `cleanup_default_ownership_gate_fails_closed`,
//! `cli_tree_gate_keeps_another_sessions_agent_tree`.

use std::path::Path;

use super::driver::{ClaimEnder, UnavailableClaims};
use crate::session_manager::worktree_owner_gate::{
    OwnerGate, SessionEnd, owner_refusal, release_stale_lock,
};
use crate::session_manager::worktree_ownership::{AgentDelegationState, AgentWorktreeOwner};

/// Whether one claim holder may lose its claim (#8301).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum HolderState {
    /// The session running this cleanup.
    Caller,
    /// A session the #7652 evidence proves ended.
    Ended,
    /// A session that is live, or whose liveness probe errored.
    Live,
    /// Nothing proves the session ended — carrying why.
    Unknown(String),
}

/// The ownership answers a cleanup run needs (#8301).
///
/// Why: crate-private and separate from `ClaimEnder`, so the public trait
/// keeps its exact signature. What: every method defaults to refusing
/// (ADR-0045).
#[async_trait::async_trait]
pub(crate) trait ClaimOwnership: Sync {
    /// Whether session `id`'s claim may be ended.
    async fn holder_state(&self, id: &str) -> HolderState {
        HolderState::Unknown(format!("this claim store cannot prove session {id} ended"))
    }

    /// The #7771 rule for `path`: `Ok(())` permits.
    async fn tree_gate(&self, path: &Path) -> Result<(), String> {
        Err(format!(
            "this claim store cannot run the ownership gate for {}",
            path.display()
        ))
    }

    /// Judge `path`'s lock now and release it only when it is a stale harness
    /// lock (#7771). `Ok(())` leaves the tree unlocked; `Err(why)` keeps it.
    ///
    /// Test: `cleanup_8301_a_lock_judged_live_at_release_keeps_the_tree_and_its_claim`,
    /// `cleanup_default_lock_release_fails_closed`.
    async fn release_stale_lock(&self, path: &Path) -> Result<(), String> {
        Err(format!(
            "this claim store cannot judge the lock on {}",
            path.display()
        ))
    }
}

impl ClaimOwnership for UnavailableClaims {}

/// The ownership a process can prove about itself alone: its own session ids.
///
/// Why: the CLI reaches the daemon over HTTP and cannot observe tmux, so it
/// proves no foreign session ended; only its managed id and Claude Code
/// session id are the caller.
/// Test: `cli_tree_gate_keeps_another_sessions_agent_tree`.
pub(crate) struct CallerOwnership {
    /// Every id naming the calling session.
    callers: Vec<String>,
}

impl CallerOwnership {
    /// The caller ids this process's environment names (#7771).
    pub(crate) fn from_env() -> Self {
        Self::new(
            ["TM_MANAGED_SESSION_ID", "CLAUDE_CODE_SESSION_ID"]
                .into_iter()
                .filter_map(|k| std::env::var(k).ok())
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
                .collect(),
        )
    }

    /// Ownership naming exactly `callers` as the calling session.
    pub(crate) fn new(callers: Vec<String>) -> Self {
        Self { callers }
    }

    /// Condition (d) as this process can answer it.
    fn session_end(&self, id: &str) -> SessionEnd {
        if self.callers.iter().any(|c| c == id) {
            return SessionEnd::Caller;
        }
        SessionEnd::Undeterminable(
            "this process cannot observe tmux to prove it; `tm session prune-worktrees \
             --merged-prs` reclaims the tree once it has ended"
                .into(),
        )
    }
}

#[async_trait::async_trait]
impl ClaimOwnership for CallerOwnership {
    async fn holder_state(&self, id: &str) -> HolderState {
        holder_state_of(self.session_end(id))
    }

    async fn tree_gate(&self, path: &Path) -> Result<(), String> {
        host_tree_gate(path, &|id: &str| self.session_end(id))
    }

    async fn release_stale_lock(&self, path: &Path) -> Result<(), String> {
        release_stale_lock(path)
    }
}

/// The ownership verdict for one target, before any claim on it is ended.
///
/// What: every holder must be the caller or provably ended; then
/// [`ClaimOwnership::tree_gate`]. `Ok(())` permits.
/// Test: `cleanup_keeps_a_tree_another_live_session_claims`,
/// `cleanup_keeps_a_tree_the_ownership_gate_refuses`.
pub(super) async fn gate(
    ownership: &dyn ClaimOwnership,
    path: &Path,
    holders: &[String],
) -> Result<(), String> {
    for id in holders {
        match ownership.holder_state(id).await {
            HolderState::Caller | HolderState::Ended => {}
            HolderState::Live => {
                return Err(format!(
                    "session {id} claims it, is live, and is not the session running this \
                     cleanup — its claim is not this run's to end (#8301)"
                ));
            }
            HolderState::Unknown(why) => {
                return Err(format!(
                    "session {id} claims it and nothing proves that session ended: {why} (#8301)"
                ));
            }
        }
    }
    ownership.tree_gate(path).await
}

/// [`gate`] again, from fresh claims, immediately before the removal (#8301).
///
/// Why: the first answer is several `gh` and `git` round trips old by the time
/// the tree is removed; a session that claimed the tree in that window would
/// lose it.
/// What: re-reads the claim holders and re-runs [`gate`]. `Ok(holders)` are
/// the claims the removal ends — the fresh ones, not the first read's;
/// `Err(why)` keeps the tree, as does a claim store that cannot be read
/// (ADR-0045).
/// Test: `cleanup_8301_rejudges_ownership_before_removing`,
/// `cleanup_8301_a_claim_taken_during_cleanup_keeps_the_tree`,
/// `cleanup_8301_unreadable_claims_at_the_recheck_keep_the_tree`.
pub(super) async fn regate<C: ClaimEnder>(
    claims: &C,
    ownership: &dyn ClaimOwnership,
    path: &Path,
) -> Result<Vec<String>, String> {
    let holders = claims
        .claims_on(path)
        .await
        .map_err(|e| format!("its claims could not be re-read before removal: {e:#}"))?;
    gate(ownership, path, &holders).await?;
    Ok(holders)
}

/// The #7771 rule for `path`, with `session_end` answering condition (d).
///
/// What: [`owner_refusal`] — (a) the lock, (b)/(d) the owner file, (c) a
/// process in the tree. A stale lock is not released here; that waits for
/// [`ClaimOwnership::release_stale_lock`], immediately before the removal.
/// Test: `host_tree_gate_permits_an_unowned_unlocked_tree`,
/// `cli_tree_gate_keeps_another_sessions_agent_tree`.
pub(crate) fn host_tree_gate(
    path: &Path,
    session_end: &dyn Fn(&str) -> SessionEnd,
) -> Result<(), String> {
    // See the module doc: (b) is unreachable from a cleanup process.
    let agent_state = |_: &AgentWorktreeOwner| AgentDelegationState::Unknown;
    match owner_refusal(path, &OwnerGate::host(&agent_state, session_end)) {
        Some(why) => Err(why),
        None => Ok(()),
    }
}

/// [`HolderState`] from a [`SessionEnd`] (#8301).
pub(crate) fn holder_state_of(end: SessionEnd) -> HolderState {
    match end {
        SessionEnd::Caller => HolderState::Caller,
        SessionEnd::Ended => HolderState::Ended,
        SessionEnd::Live => HolderState::Live,
        SessionEnd::Undeterminable(why) => HolderState::Unknown(why),
    }
}
