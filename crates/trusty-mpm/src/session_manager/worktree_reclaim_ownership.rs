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

use super::worktree_owner_gate::{OwnerGate, SessionEnd, owner_refusal};
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
/// What: a tree inside the harness agent store ([`is_harness_agent_worktree`])
/// is judged by [`owner_refusal`], the #7771 rule, with `owners` answering
/// condition (d). Before #7771 an agent-store tree whose sentinel named nobody
/// was refused forever, which kept every hand-made tree.
///
/// Outside the store, the sentinel is read through [`read_sentinel_owner`] and
/// [`SentinelOwner::Agent`] whose agent the registry calls
/// [`Live`](AgentDelegationState::Live) refuses. An
/// [`Unknown`](AgentDelegationState::Unknown) registry answer consults git's
/// durable harness lock instead (#6561): `Released` permits; `Held` and
/// `Undeterminable` refuse. An absent or unparsable sentinel there is left to
/// the later gates.
/// Test: `classify_blocks_a_live_agents_worktree`,
/// `classify_blocks_an_agent_the_harness_still_holds_after_a_restart`,
/// `classify_allows_a_finished_agents_merged_worktree`,
/// `worktree_7771_a_hand_made_tree_is_reclaimed`,
/// `classify_allows_a_merged_agent_tree_the_harness_released`,
/// `classify_blocks_an_agent_tree_git_cannot_be_asked_about`.
pub(crate) fn agent_ownership_blocks(
    path: &Path,
    agent_state: AgentStateProbe<'_>,
    owners: &SessionOwners,
) -> Option<String> {
    // #7771: an agent-store tree, with or without an owner file, is judged by
    // the session-safe rule — a live lock holder, a live delegation, a live
    // foreign owning session, or a process standing in it keeps it.
    if is_harness_agent_worktree(path) {
        let session_end = |id: &str| owners.session_end(id);
        return owner_refusal(path, &OwnerGate::host(agent_state, &session_end));
    }
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
    /// #7771: the session running the reclaim, which may take its own trees.
    caller: Option<String>,
    /// #7771: Claude Code session id → every managed session id recording it.
    /// An agent owner file names the dispatching CLAUDE session, not the
    /// managed record, and a resumed session can be recorded more than once.
    aliases: HashMap<String, Vec<String>>,
}

impl SessionOwners {
    /// This map, naming the calling session (#7771).
    pub(crate) fn with_caller(mut self, caller: Option<String>) -> Self {
        self.caller = caller;
        self
    }

    /// This map, resolving Claude session ids to managed ids (#7771).
    pub(crate) fn with_aliases(
        mut self,
        pairs: impl IntoIterator<Item = (String, String)>,
    ) -> Self {
        for (claude, managed) in pairs {
            self.aliases.entry(claude).or_default().push(managed);
        }
        self
    }

    /// Whether session `id` (managed or Claude) is the caller or provably
    /// ended (#7771 condition d).
    ///
    /// What: [`SessionEnd::Caller`] when `id` is the caller. A Claude id is
    /// judged by EVERY managed record aliasing it, strictest first: any `Live`
    /// record is `Live`, then any record nothing can judge is
    /// `Undeterminable`, then a caller record is `Caller`; `Ended` needs every
    /// record gone. One record is judged by [`Self::record_end`].
    /// Test: `session_end_resolves_a_claude_id_through_its_alias`,
    /// `session_end_keeps_a_claude_id_any_live_record_holds`,
    /// `session_end_ranks_a_live_alias_above_the_caller_alias`,
    /// `session_end_refuses_an_unread_map`.
    pub(crate) fn session_end(&self, id: &str) -> SessionEnd {
        if self.caller.as_deref() == Some(id) {
            return SessionEnd::Caller;
        }
        // #7771 critic: a Claude session resumed into two records is not
        // judged by whichever pair was recorded last.
        self.aliases
            .get(id)
            .and_then(|records| {
                records
                    .iter()
                    .map(|m| self.record_end(m))
                    .max_by_key(strictness)
            })
            .unwrap_or_else(|| self.record_end(id))
    }

    /// One managed record's answer: the caller is `Caller`, then the #7652
    /// evidence — `SessionGone` is `Ended`, `Live` is `Live`, and an unread map
    /// or an unrecorded id is `Undeterminable`.
    fn record_end(&self, managed: &str) -> SessionEnd {
        if self.caller.as_deref() == Some(managed) {
            return SessionEnd::Caller;
        }
        let Some(by_id) = &self.by_id else {
            return SessionEnd::Undeterminable("the session store was not read".into());
        };
        match by_id.get(managed) {
            Some(ClaimLiveness::SessionGone) => SessionEnd::Ended,
            Some(ClaimLiveness::Live) => SessionEnd::Live,
            None => SessionEnd::Undeterminable("no stored session record names it".into()),
        }
    }

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
        Self {
            by_id: Some(by_id),
            ..Self::default()
        }
    }
}

/// How strongly one record's [`SessionEnd`] keeps a tree; the strictest wins.
fn strictness(end: &SessionEnd) -> u8 {
    match end {
        SessionEnd::Ended => 0,
        SessionEnd::Caller => 1,
        SessionEnd::Undeterminable(_) => 2,
        SessionEnd::Live => 3,
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
/// same way. The calling session is permitted too (#7771). A tree in the
/// harness agent store is gate 4's, judged by [`owner_refusal`] with the
/// harness lock first (#7771). Every other answer refuses:
///
/// - the owner map was never read (an unreadable registry);
/// - no stored record names the owner. The sentinel stores no pid or pane, so a
///   record is the only thing that could evidence that session's end, and its
///   absence evidences nothing. That absence is not always temporary:
///   `prune_managed` COMPACTS `Decommissioned` and `Deleted` tombstones out of
///   the store (`prune.rs`, `is_tombstone`), so after `tm sessions prune` a tree
///   whose sentinel names a compacted session is permanently unreclaimable by
///   this path. The escape is an operator's own `git worktree remove`, which the
///   refusal names (#7652 critic round 2);
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
    // #7771: gate 4 already judged an agent-store tree by the full rule, the
    // harness lock included.
    if is_harness_agent_worktree(path) {
        return None;
    }
    let SentinelOwner::Known(owner, _) = read_sentinel_owner(path) else {
        return None;
    };
    // #7771: the calling session may take a tree its own sentinel names.
    if owners.session_end(&owner.to_string()) == SessionEnd::Caller {
        return None;
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
        // #7652 critic round 2: `tm sessions prune` compacts tombstones, so this
        // can be permanent — name the escape rather than implying it will clear.
        None => Some(format!(
            "owned by session {owner}, and no stored session record names it, so nothing can \
             show that session has ended — unrecorded is not dead. If that session's record was \
             compacted by `tm sessions prune`, no later run will recover it: confirm nobody is \
             working in this tree and remove it yourself with `git worktree remove` (#7652, \
             ADR-0045)"
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
///
/// Reading only `ForeignNested` is why gate 2 reports that state ahead of
/// `CallerNested` (#7652 critic round 2): when the invoking session claims the
/// enclosing checkout too, as two sessions sharing one project do, reporting the
/// caller's claim first left this check nothing to fire on.
/// Test: `worktree_7652_an_unattributed_tree_under_a_live_foreign_claim_is_refused`,
/// `worktree_7652_gate_4c_survives_a_caller_claim_on_the_same_checkout`.
pub(crate) fn unattributed_nested_blocks(path: &Path, claim: &ClaimState) -> Option<String> {
    let ClaimState::ForeignNested { session, .. } = claim else {
        return None;
    };
    // #7771: an agent-store tree nests under the shared checkout every session
    // claims; gate 4 judged it by the full rule instead.
    if is_harness_agent_worktree(path) {
        return None;
    }
    match read_sentinel_owner(path) {
        SentinelOwner::Unknown => Some(format!(
            "session {session} claims a workspace containing this worktree, and the ownership \
             sentinel is absent, empty, malformed or unreadable, so nothing attributes the tree \
             to anyone else — the live claim stands (#7652, ADR-0045)"
        )),
        SentinelOwner::Known(..) | SentinelOwner::Agent(..) => None,
    }
}
