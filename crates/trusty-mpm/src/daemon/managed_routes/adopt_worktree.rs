//! POST /api/v1/sessions/managed/adopt-worktree — hand a dead owner's worktree
//! to a live session (#6497).
//!
//! Why: the policy in
//! [`crate::session_manager::worktree_adopt`] needs two liveness answers that
//! live in two different registries — a dispatched agent's in the delegation
//! map, a managed session's in the session-record store — and only the daemon
//! holds both. Answering them here keeps the policy a pure function and keeps
//! the CLI from having to re-derive liveness over several round trips and guess
//! when one of them is silent.
//!
//! What: one POST carrying the worktree `path` and the `as_session` taking it
//! over. The daemon reads the ownership sentinel, resolves the CURRENT owner's
//! liveness through whichever registry owns it, collects the agents any live
//! delegation still has working inside the tree, and runs
//! [`evaluate_adoption`](crate::session_manager::worktree_adopt::evaluate_adoption).
//! A refusal is a 409 carrying the policy's own reason; nothing is written.
//!
//! It changes ONE thing on disk: the sentinel. It moves no files, commits
//! nothing, and creates and deletes no worktree.
//! Test: `adopt_worktree_route_refuses_a_live_owner`,
//! `adopt_worktree_route_transfers_a_dead_owners_tree`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::{Json, extract::State, response::IntoResponse};
use serde::Deserialize;

use crate::daemon::rpc::managed::outcome::RouteOutcome;
use crate::daemon::services::agent_worktree_reap::delegation_state_for_agent;
use crate::daemon::state::DaemonState;
use crate::session_manager::record::ManagedSessionId;
use crate::session_manager::worktree_adopt::{
    AdoptionVerdict, OwnerLiveness, adopt_worktree, evaluate_adoption,
};
use crate::session_manager::worktree_ownership::{SentinelOwner, read_sentinel_owner};

/// The request body: which tree, and which session takes it (#6497).
#[derive(Debug, Deserialize)]
pub struct AdoptWorktreeRequest {
    /// The worktree whose ownership sentinel is being rewritten.
    pub path: PathBuf,
    /// The managed session that becomes the new owner.
    pub as_session: ManagedSessionId,
}

/// POST /api/v1/sessions/managed/adopt-worktree (#6497).
///
/// Test: `adopt_worktree_route_refuses_a_live_owner`,
/// `adopt_worktree_route_transfers_a_dead_owners_tree`.
pub async fn adopt_worktree_route(
    State(state): State<Arc<DaemonState>>,
    Json(req): Json<AdoptWorktreeRequest>,
) -> impl IntoResponse {
    adopt_worktree_core(&state, req).await
}

/// The transport-neutral body of `POST .../managed/adopt-worktree` (#6497).
///
/// Why: mirrors [`super::reconcile::reconcile_worktrees_core`] so a socket
/// transport can serve the same verb without a second copy of the policy call.
/// What: resolves the owner's liveness, runs the pure gate, and writes the
/// sentinel only on [`AdoptionVerdict::Adopt`]. A refusal is a 409 whose body
/// is the gate's own reason, so the operator reads exactly what the guard read.
///
/// **The claimant set is read TWICE, and the second read is what the write
/// stands on.** `owner_liveness` awaits the session-manager store, so the first
/// read can be milliseconds old by the time the sentinel is written, and a
/// dispatch landing in that window records a `cwd` under the tree this call is
/// about to hand away. Re-asking immediately before the write is the two-pass
/// shape [`dirt_blocks_removal`](crate::session_manager::worktree_safety::dirt_blocks_removal)
/// already uses on the reclaim path.
///
/// It NARROWS the window rather than closing it: nothing holds a lock across
/// the check and the write, so a delegation recorded between the second read and
/// the `std::fs::write` is still missed. Closing it needs a per-path lock the
/// daemon does not have today, and the residual costs an adoption racing a
/// dispatch into the same tree — an operator-typed verb racing a dispatch, not
/// two automatic sweeps.
/// Test: `adopt_worktree_route_refuses_a_live_owner`,
/// `adopt_worktree_route_transfers_a_dead_owners_tree`,
/// `adopt_worktree_route_refuses_a_claimant_under_a_symlinked_spelling`.
pub(crate) async fn adopt_worktree_core(
    state: &Arc<DaemonState>,
    req: AdoptWorktreeRequest,
) -> RouteOutcome {
    let owner = read_sentinel_owner(&req.path);
    let liveness = owner_liveness(state, &owner).await;
    let claimants = live_claimants_in(state, &req.path);
    match evaluate_adoption(&req.path, &owner, liveness, &claimants) {
        AdoptionVerdict::Refuse(reason) => {
            tracing::info!(path = %req.path.display(), %reason, "adopt-worktree: refused (#6497)");
            RouteOutcome::text(409, reason)
        }
        // #6497: re-ask who is in the tree, now, immediately before the write.
        AdoptionVerdict::Adopt => match evaluate_adoption(
            &req.path,
            &owner,
            liveness,
            &live_claimants_in(state, &req.path),
        ) {
            AdoptionVerdict::Refuse(reason) => {
                tracing::warn!(
                    path = %req.path.display(), %reason,
                    "adopt-worktree: re-check refused a claim that passed the first pass (#6497)"
                );
                RouteOutcome::text(409, reason)
            }
            AdoptionVerdict::Adopt => match adopt_worktree(&req.path, req.as_session) {
                Ok(()) => {
                    tracing::info!(
                        path = %req.path.display(),
                        owner = %req.as_session,
                        "adopt-worktree: ownership transferred (#6497)"
                    );
                    RouteOutcome::ok(&serde_json::json!({
                        "adopted": true,
                        "path": req.path,
                        "owner": req.as_session.to_string(),
                    }))
                }
                Err(e) => RouteOutcome::text(500, format!("sentinel rewrite failed: {e}")),
            },
        },
    }
}

/// What the registry owning `owner` says about its liveness (#6497).
///
/// Why: the two owner shapes resolve through different registries, and only one
/// of them can distinguish "ended" from "never heard of" — so the mapping is
/// per-shape rather than one lookup with a fallback.
/// What: a dispatched agent goes through the delegation map, which
/// [`OwnerLiveness::from_agent_state`] reads with #5661's absent-vs-
/// undeterminable split; a managed session goes through the session-record
/// store's own terminal-state check, whose grace window already covers a
/// sentinel written before its record was persisted. An unreadable sentinel is
/// undeterminable and gate 1 refuses it before this answer matters.
/// Test: `adopt_worktree_route_refuses_a_live_owner`.
async fn owner_liveness(state: &Arc<DaemonState>, owner: &SentinelOwner) -> OwnerLiveness {
    match owner {
        SentinelOwner::Agent(agent, _) => {
            OwnerLiveness::from_agent_state(delegation_state_for_agent(state, &agent.agent_id))
        }
        SentinelOwner::Known(session, created_at) => {
            let mgr = state.session_manager().await;
            if mgr
                .resolve_ownerless_with_grace(*session, *created_at)
                .await
            {
                OwnerLiveness::Dead
            } else {
                OwnerLiveness::Alive
            }
        }
        SentinelOwner::Unknown => OwnerLiveness::Undeterminable,
    }
}

/// The agents any LIVE delegation still has working inside `path` (#6497).
///
/// Why: the sentinel names who PROVISIONED the tree, never everyone who is in
/// it. A successor dispatched into an adopted tree, or a sibling agent that was
/// pointed at it, is a claim the sentinel cannot see — and taking the tree out
/// from under one is the same harm as taking it from its owner.
/// What: every live delegation whose recorded `cwd` is the tree or sits beneath
/// it, by agent name. A delegation with no recorded `cwd` claims nothing here.
///
/// **Both sides are compared in BOTH spellings.** A raw `starts_with` is
/// component-wise, so `/a/bc` never matches `/a/b` — but it is also purely
/// lexical, and a delegation's `cwd` and the request's `path` routinely reach
/// this function through different symlinks. On macOS every `TempDir` is under
/// `/var`, which IS `/private/var`, so one side canonicalized and the other not
/// is the ordinary case rather than the exotic one. A missed match here means
/// adoption proceeds against a tree an agent is still writing in. So each side
/// contributes its canonical form AND its raw form, and any pairing that matches
/// refuses — the same both-spellings pattern
/// [`is_live`](crate::session_manager::worktree_reclaim::is_live) uses, for the
/// same reason: a failed canonicalization may only ever spare a tree.
///
/// The containment stays one-directional on purpose. `is_live` also counts a
/// claimed path that CONTAINS the candidate, because a session standing in a
/// parent still owns it. Here that would count every delegation recorded in the
/// enclosing checkout — which is exactly the population #6497 is about — and
/// refuse every adoption in the reported scenario.
/// Test: `adopt_worktree_route_refuses_a_claimant_under_a_symlinked_spelling`,
/// `adopt_worktree_route_refuses_a_claimant_under_a_trailing_slash_spelling`,
/// `adopt_worktree_route_refuses_a_live_owner` covers the empty case.
fn live_claimants_in(state: &Arc<DaemonState>, path: &Path) -> Vec<String> {
    let tree_forms = both_spellings(path);
    state
        .all_delegations()
        .into_iter()
        .filter(|d| d.status.is_live())
        .filter(|d| {
            d.cwd.as_deref().is_some_and(|cwd| {
                both_spellings(cwd)
                    .iter()
                    .any(|c| tree_forms.iter().any(|t| c.starts_with(t)))
            })
        })
        .map(|d| d.agent)
        .collect()
}

/// A path's canonical form and its raw form (#6497).
///
/// Why: canonicalization resolves symlinks and `..`, and fails outright for a
/// path that no longer exists. Keeping the raw form alongside it means a failed
/// resolution degrades to today's lexical comparison instead of dropping the
/// path from the comparison entirely.
/// What: `[canonical, raw]`, which may be the same path twice — the caller only
/// ever asks whether ANY pairing matches, so a duplicate costs one comparison.
fn both_spellings(path: &Path) -> [PathBuf; 2] {
    [
        std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf()),
        path.to_path_buf(),
    ]
}

#[cfg(test)]
#[path = "adopt_worktree_tests.rs"]
mod adopt_worktree_tests;
