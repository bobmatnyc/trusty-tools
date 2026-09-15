//! Gate 4 of the merged-PR reclaim: WHO owns a candidate worktree, and is that
//! owner provably finished (#5661, #7652).
//!
//! Why: split out of `worktree_reclaim` so that file stays under the 500-SLOC
//! production cap, and because gate 4 answers a question the other gates do
//! not ask. Gates 1-3 and 5-6 read git, a claim set and the working tree; gate 4
//! reads the `.trusty-mpm-worktree` OWNERSHIP SENTINEL and asks whether the party
//! it names is still working. The sentinel names one of two kinds of owner, so
//! the gate has two halves: [`agent_ownership_blocks`] for a dispatched agent
//! and [`session_ownership_blocks`] for a managed session. A third check,
//! [`unattributed_nested_blocks`], covers the sentinel that names nobody.
//!
//! # Fail direction
//!
//! Every check permits only on POSITIVE evidence that the owner has ended.
//! Silence, an unread store and an unobservable probe all refuse (ADR-0045).
//! Test: `worktree_reclaim_tests`, `worktree_reclaim_owner_liveness_tests`.

use std::collections::HashMap;
use std::path::Path;

use super::worktree_ownership::{
    AgentDelegationState, AgentWorktreeOwner, SentinelOwner, is_harness_agent_worktree,
    read_sentinel_owner,
};
use super::worktree_reclaim_claim::{ClaimLiveness, ClaimState};
use super::worktree_registry::{HarnessLockState, harness_lock_state};

/// Resolves the delegation registry's answer for the agent a sentinel names.
///
/// Why: named so the four call sites that pass one around agree on the shape,
/// and so a caller that has no registry to consult has to say so explicitly by
/// returning [`AgentDelegationState::Unknown`] rather than by passing an empty
/// list that reads as "nobody claims it".
pub(crate) type AgentStateProbe<'a> = &'a dyn Fn(&AgentWorktreeOwner) -> AgentDelegationState;

/// Why a DISPATCHED AGENT's ownership forbids reclaiming `path`, or `None` when
/// it does not (#5661).
///
/// Why: `SentinelOwner::Agent` exists to stop an agent-owned worktree being
/// reclaimed by a sweep that cannot tell whether the agent is still working.
/// The merged-PR reclaim path never read the sentinel, so it deleted three live
/// agents' worktrees on 2026-08-15/16, two of them holding unpushed commits.
///
/// What: reads the sentinel through [`read_sentinel_owner`], the same tolerant
/// parse the orphan path uses, and refuses on two answers.
///
/// 1. [`SentinelOwner::Agent`] whose agent the registry calls
///    [`Live`](AgentDelegationState::Live). An
///    [`Unknown`](AgentDelegationState::Unknown) registry answer (the map is
///    rebuilt empty at every daemon boot) consults git's durable harness lock
///    instead (#6561): `Released` permits; `Held` and `Undeterminable` refuse.
/// 2. [`SentinelOwner::Unknown`] for ANY path inside the harness agent store
///    ([`is_harness_agent_worktree`]), whether the sentinel is unreadable or
///    absent. Nothing attributes that tree, so the question is undeterminable,
///    not absent (#6561 critic round).
///
/// A worktree OUTSIDE the agent store with an absent or unparsable sentinel is
/// left to the later gates; widening the refusal there would make the
/// `.worktrees/` population permanently unreclaimable.
/// Test: `classify_blocks_a_live_agents_worktree`,
/// `classify_blocks_an_agent_the_harness_still_holds_after_a_restart`,
/// `classify_allows_a_finished_agents_merged_worktree`,
/// `classify_blocks_an_agent_store_worktree_with_an_unreadable_sentinel`,
/// `an_unattributed_agent_store_worktree_is_never_reclaimable`,
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

/// Every stored session record's liveness, keyed by session id (#7652).
///
/// Why: gate 4's session half has to answer "is the session this sentinel names
/// finished?" from the RECORD that names it, not from gate 2's claim list. The
/// claim list holds only records that carry a `workspace_path`, so a live owner
/// whose record has none was indistinguishable there from an owner nobody
/// recorded. This map is built from every record, with or without a workspace.
/// What: `None` until a store read fills it, so a caller that never read the
/// store (the `Default`, and the survey's fallback when the claim probe returned
/// nothing) refuses rather than permits. Each entry reuses the claim set's
/// [`ClaimLiveness`]: `SessionGone` only when the tmux probe answered and did
/// not list the record's managed tmux name; `Live` for a listed name, an
/// unmanaged name, and a probe that errored.
/// Test: `worktree_7652_owners_include_a_record_with_no_workspace_path`,
/// `worktree_7652_an_unread_owner_map_refuses`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct SessionOwners {
    /// Session id (as `ManagedSessionId` displays it) → that record's liveness.
    by_id: Option<HashMap<String, ClaimLiveness>>,
}

impl SessionOwners {
    /// An owner map read from a store, one `(id, liveness)` pair per record.
    ///
    /// A duplicated id keeps `Live` if any entry says so: the fail-closed
    /// direction, since one live record must not be outvoted by a stale one.
    pub(crate) fn observed(records: impl IntoIterator<Item = (String, ClaimLiveness)>) -> Self {
        let mut by_id: HashMap<String, ClaimLiveness> = HashMap::new();
        for (id, liveness) in records {
            let entry = by_id.entry(id).or_insert(liveness);
            if liveness == ClaimLiveness::Live {
                *entry = ClaimLiveness::Live;
            }
        }
        Self { by_id: Some(by_id) }
    }
}

/// Why a MANAGED SESSION's ownership forbids reclaiming `path`, or `None` when
/// it does not (#7652).
///
/// Why: #7652 narrowed gate 2 so a live foreign session's PROJECT-ROOT claim no
/// longer vetoes the worktrees nested under it. Gate 4 then answered `None` for
/// every [`SentinelOwner::Known`] whatever that session's state, so a worktree a
/// live session created directly, merged and clean, was deleted under it.
///
/// What: for a `Known` sentinel, permits ONLY when the owner is provably gone.
/// The evidence is exactly the one #7232 accepts for discarding a claim: the
/// store holds a record with this id, its tmux name is managed-shaped, and a
/// tmux probe that ANSWERED does not list it. That evidence does not depend on
/// the record carrying a `workspace_path`, so a record without one is judged the
/// same way. Every other answer refuses:
///
/// - the tree is in the harness agent store and git reports the harness's
///   agent-lifetime lock held, or cannot be asked (#7652 critic round). An
///   agent-store sentinel can still name the parent session (#7958), so that
///   session ending says nothing about the agent working in the tree;
/// - the owner map was never read (an unreadable registry);
/// - no stored record names the owner. Records are tombstoned, never dropped, so
///   an owner with no record has no evidence of its end at all, and the
///   sentinel stores no pid or pane that could supply one. The cost is that a
///   tree whose owning record was lost stays unreclaimable until an operator
///   removes it;
/// - the record's tmux session is listed, its name is unmanaged, or the tmux
///   probe errored. The map already reports all three as `Live`.
///
/// Agent and unattributed sentinels are [`agent_ownership_blocks`]'s question.
/// Test: `worktree_7652_a_live_owners_nested_worktree_is_refused`,
/// `worktree_7652_a_dead_owners_nested_worktree_is_reclaimed`,
/// `worktree_7652_an_owner_with_no_record_is_refused`,
/// `worktree_7652_an_errored_tmux_probe_is_refused`,
/// `worktree_7652_an_unanswerable_claim_probe_reclaims_nothing`,
/// `worktree_7652_an_unread_owner_map_refuses`,
/// `worktree_7652_a_held_harness_lock_outranks_an_ended_sentinel_session`.
pub(crate) fn session_ownership_blocks(path: &Path, owners: &SessionOwners) -> Option<String> {
    let SentinelOwner::Known(owner, _) = read_sentinel_owner(path) else {
        return None;
    };
    // #7652 critic round: in the agent store the harness lock outranks the
    // sentinel's session — see this function's doc, first refusal.
    if is_harness_agent_worktree(path) {
        match harness_lock_state(path) {
            HarnessLockState::Released => {}
            HarnessLockState::Held => {
                return Some(format!(
                    "sentinel names session {owner}, but this tree is in the harness agent store \
                     and git still reports the harness's agent-lifetime lock on it — an agent is \
                     still working here, whatever that session's state (#7652, #6561)"
                ));
            }
            HarnessLockState::Undeterminable => {
                return Some(format!(
                    "sentinel names session {owner}, this tree is in the harness agent store, and \
                     git could not be asked whether the harness still holds it — undeterminable, \
                     not absent (#7652, #6561, ADR-0045)"
                ));
            }
        }
    }
    let Some(by_id) = &owners.by_id else {
        return Some(format!(
            "owned by session {owner}, and the session store was not read, so nothing can show \
             that session has ended (#7652, ADR-0045)"
        ));
    };
    match by_id.get(&owner.to_string()) {
        Some(ClaimLiveness::SessionGone) => None,
        Some(ClaimLiveness::Live) => Some(format!(
            "owned by session {owner}, which tmux still lists, or whose liveness the tmux probe \
             could not establish (#7652)"
        )),
        None => Some(format!(
            "owned by session {owner}, and no stored session record names it, so nothing can \
             show that session has ended — unrecorded is not dead (#7652, ADR-0045)"
        )),
    }
}

/// Why an UNATTRIBUTED tree nested under a live foreign session's claim may not
/// be reclaimed, or `None` when this check does not apply (#7652 critic round).
///
/// Why: gate 2 permits a live foreign project-root claim over a nested tree
/// ([`ClaimState::ForeignNested`]) because the sentinel attributes the tree
/// instead. A sentinel that is absent, empty, malformed or unreadable
/// attributes nothing, so the live claim is the only ownership evidence left.
/// Before #7652, gate 2 refused this tree.
/// What: refuses exactly `ForeignNested` × [`SentinelOwner::Unknown`]. Every
/// other claim state, and every sentinel that names an owner, is left to the
/// other gates. The survey's `classify` and the delete loop's two re-checks all
/// call it.
/// Test: `worktree_7652_an_unattributed_tree_under_a_live_foreign_claim_is_refused`.
pub(crate) fn unattributed_nested_blocks(path: &Path, claim: &ClaimState) -> Option<String> {
    let ClaimState::ForeignNested { session, .. } = claim else {
        return None;
    };
    match read_sentinel_owner(path) {
        SentinelOwner::Unknown => Some(format!(
            "session {session} claims a workspace containing this worktree, and the ownership \
             sentinel is absent, empty, malformed or unreadable, so nothing attributes the tree \
             to anyone else — the live claim stands (#7652, ADR-0045)"
        )),
        SentinelOwner::Known(..) | SentinelOwner::Agent(..) => None,
    }
}
