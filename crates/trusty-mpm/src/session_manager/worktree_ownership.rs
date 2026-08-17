//! Worktree ownership sentinel payload + owner-resolution primitives (#3649).
//!
//! Why: issue #3649 found three independent on-disk worktree stores with no
//! record of WHO is entitled to reclaim a given worktree. The zero-byte
//! `.trusty-mpm-worktree` sentinel (`super::decommission::WORKTREE_SENTINEL_FILE`)
//! only ever answered "is this an SM-created worktree?", never "whose is it?".
//! This module adds a small JSON payload to that same sentinel file plus the
//! owner-resolution logic the orphan-GC sweep and the `decommission` owner
//! gate both need, so neither has to re-invent a tolerant parse or a
//! terminal-state lookup.
//! What: [`WorktreeSentinel`] (the JSON payload), [`sentinel_payload_bytes`]
//! (serialize for the two sentinel WRITE sites — `provisioner::workspace` and
//! `daemon::managed_routes::inproject`), [`SentinelOwner`] +
//! [`read_sentinel_owner`] (the TOLERANT parse: absent/empty/unparsable all
//! read as [`SentinelOwner::Unknown`], never an error), and
//! [`SessionManager::resolve_ownerless`] / [`SessionManager::resolve_ownerless_with_grace`]
//! / [`SessionManager::set_worktree_owner`] (the store-backed "is this owner
//! reclaimable?" checks and the registry setter, respectively).
//!
//! Creation-race hardening (#3649 review fix): both real sentinel WRITE sites
//! write the sentinel (naming the new session as owner) BEFORE that session's
//! [`super::record::SessionRecord`] is persisted to the store — real I/O
//! (git clone/worktree-add, `prepare_session`) happens in between. A
//! not-yet-persisted owner is therefore indistinguishable from a
//! genuinely-deleted one by a plain store lookup. [`OWNERLESS_GRACE`] plus
//! [`SessionManager::resolve_ownerless_with_grace`] close that window: an
//! absent-owner sentinel younger than the grace period is treated as
//! "not yet provably ownerless" (skip, never reclaim) rather than ownerless.
//!
//! # Agent worktrees (#4311, DOC-66 §1.3)
//!
//! The payload widens to carry an [`AgentWorktreeOwner`] instead of a
//! `ManagedSessionId`, for a worktree the HARNESS created for a dispatched
//! agent. That is a THIRD owner answer, not a shape of the second: an agent has
//! no session record, so routing it through
//! [`SessionManager::resolve_ownerless_with_grace`] would report every agent
//! worktree reclaimable once past [`OWNERLESS_GRACE`], and the orphan-GC's
//! chain holds no liveness check. [`write_agent_sentinel`] is the write, and
//! [`find_agent_worktree`] is the ADR-0023-point-4 rebuild the reap falls back
//! to when a daemon restart has emptied the delegation map.
//! Test: `sentinel_owner_*` (parse matrix), `agent_sentinel_*` (#4311), and
//! `resolve_ownerless_*` (terminal vs. live vs. absent-but-young vs.
//! absent-and-aged owner) below.

use std::path::Path;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::decommission::WORKTREE_SENTINEL_FILE;
use super::manager::{ManagedError, SessionManager};
use super::record::{ManagedSessionId, SessionRecord};
use crate::core::agent::DelegationId;
use crate::core::session::SessionId;

/// JSON payload written into every SM-created worktree's ownership sentinel
/// (#3649), replacing the pre-#3649 zero-byte convention.
///
/// Why: a zero-byte file can only assert "an SM created this worktree", never
/// "which session owns it". Recording the owning session id (and, for
/// observability, when the worktree was created) lets the orphan-GC and the
/// `decommission` owner gate answer "who owns this?" from the sentinel alone,
/// without consulting the (possibly stale, possibly absent) session-record
/// store.
/// What: `owner_session_id` — the [`ManagedSessionId`] that provisioned this
/// worktree; `created_at` — when the sentinel was written.
/// Test: `sentinel_owner_round_trips_valid_payload`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct WorktreeSentinel {
    /// The managed session that provisioned this worktree.
    ///
    /// `Option` since #4311: an AGENT worktree is owned by a dispatched agent,
    /// and no managed session record exists for it. Every sentinel written
    /// before #4311 carries this field, so widening it changes no existing
    /// parse — and a payload naming neither owner reads as
    /// [`SentinelOwner::Unknown`], the safe default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_session_id: Option<ManagedSessionId>,
    pub created_at: DateTime<Utc>,
    /// The dispatched agent that owns this worktree (#4311), when one does.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<AgentWorktreeOwner>,
}

/// Who owns a worktree the HARNESS created for a dispatched agent (#4311).
///
/// Why: DOC-66 §1.3 requires the sentinel to carry enough to rebuild the
/// ownership record from disk alone (ADR-0023 point 4), and §5 defines an agent
/// worktree's owner as "a recorded parent id and nothing more". Until #4311 the
/// only record of that parentage was
/// [`Delegation::worktree_path`](crate::core::agent::Delegation::worktree_path)
/// in the daemon's in-memory `DashMap`, which a restart drops with no recovery
/// path — so a restart silently returned every agent worktree to owner-unknown.
/// What: `agent_id` — Claude Code's subagent id, the exact key `SubagentStop`
/// quotes back and therefore the one the reap resolves on; `delegation_id` and
/// `parent_session_id` — the dispatching delegation and session, which are the
/// parentage §5 asks for.
///
/// # Substitution for DOC-66's `workstream_id`
///
/// §1.3 names `workstream_id` and `parent_workstream_id`. Neither exists:
/// §1.2's workstream record is unimplemented, and DOC-66 states so itself. The
/// fields here are the same parentage in the identity space that DOES exist
/// today. When workstream identity lands, `parent_session_id` is what it
/// replaces; a sentinel written now stays readable because both fields are
/// optional by construction.
/// Test: `agent_sentinel_round_trips`, `agent_sentinel_survives_a_lost_registry`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct AgentWorktreeOwner {
    /// Claude Code's subagent id — the reap's exact correlation key.
    pub agent_id: String,
    /// The delegation this worktree was granted to.
    pub delegation_id: DelegationId,
    /// The session that dispatched that delegation — DOC-66 §5's parent id.
    pub parent_session_id: SessionId,
}

impl WorktreeSentinel {
    /// Build a fresh sentinel payload for `owner`, timestamped `now`.
    ///
    /// Why: both sentinel WRITE sites (`workspace.rs`, `inproject.rs`) need
    /// the identical payload shape; centralising construction here keeps them
    /// from drifting.
    /// What: `Self { owner_session_id: Some(owner), created_at: Utc::now() }`.
    /// Test: `sentinel_owner_round_trips_valid_payload`.
    pub(crate) fn new(owner: ManagedSessionId) -> Self {
        Self {
            owner_session_id: Some(owner),
            created_at: Utc::now(),
            agent: None,
        }
    }

    /// Build a fresh AGENT sentinel payload (#4311).
    /// Test: `agent_sentinel_round_trips`.
    pub(crate) fn for_agent(owner: AgentWorktreeOwner) -> Self {
        Self {
            owner_session_id: None,
            created_at: Utc::now(),
            agent: Some(owner),
        }
    }
}

/// Serialize a fresh [`WorktreeSentinel`] for `owner` to JSON bytes, ready to
/// `std::fs::write` at `<worktree>/.trusty-mpm-worktree`.
///
/// Why: `serde_json::to_vec` can only fail on a writer error, which a `Vec`
/// buffer never produces for this payload shape — falling back to an empty
/// (legacy-shaped) byte string on the theoretical error path is safer than
/// panicking during workspace provisioning, and an empty file already parses
/// as [`SentinelOwner::Unknown`] (tolerant-parse, never a hard failure).
/// What: `serde_json::to_vec(&WorktreeSentinel::new(owner))`, defaulting to
/// an empty `Vec` on the (unreachable in practice) serialize error.
/// Test: `sentinel_owner_round_trips_valid_payload`.
pub(crate) fn sentinel_payload_bytes(owner: ManagedSessionId) -> Vec<u8> {
    serde_json::to_vec(&WorktreeSentinel::new(owner)).unwrap_or_default()
}

/// The result of reading a worktree's ownership sentinel — TOLERANT by
/// construction (#3649): absent, empty, or unparsable content is always
/// [`Unknown`](Self::Unknown), never an error.
///
/// Why: the sentinel file predates this JSON payload (pre-#3649 worktrees
/// carry a zero-byte file) and a corrupted/partially-written file must never
/// crash a GC sweep or a decommission call. "Owner unknown" is also the
/// SAFE default: an unknown owner is never auto-deleted and never blocks a
/// `caller`-gated decommission (see `super::decommission`'s owner gate).
/// What: [`Known`](Self::Known) wraps the resolved [`ManagedSessionId`] AND
/// the sentinel's `created_at` (needed by [`SessionManager::resolve_ownerless_with_grace`]
/// to distinguish a not-yet-persisted owner from a genuinely-deleted one —
/// see the module doc); [`Unknown`](Self::Unknown) covers every other case
/// (absent file, empty file, or a file whose content does not parse as
/// [`WorktreeSentinel`] — since `WorktreeSentinel` has no optional fields, a
/// `Known` result ALWAYS carries a valid timestamp too, by construction).
/// Test: `sentinel_owner_absent_file_is_unknown`,
/// `sentinel_owner_empty_file_is_unknown`,
/// `sentinel_owner_garbage_file_is_unknown`,
/// `sentinel_owner_round_trips_valid_payload`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SentinelOwner {
    /// The sentinel parsed and named an owning session, created at this time.
    Known(ManagedSessionId, DateTime<Utc>),
    /// The sentinel named a dispatched AGENT, not a managed session (#4311).
    ///
    /// Why this is a third answer and not a shape of `Known`: `Known`'s owner
    /// is resolved through
    /// [`SessionManager::resolve_ownerless_with_grace`], a SESSION-STORE
    /// lookup. An agent has no session record, so that lookup finds nothing
    /// and — past [`OWNERLESS_GRACE`] — reports the worktree reclaimable. The
    /// orphan-GC would then delete a live agent's tree on a 60-second cadence
    /// with no liveness check anywhere in its chain. Answering `Agent` keeps
    /// the reclamation authority where it belongs: the agent's own exit
    /// (`daemon::services::agent_worktree_reap`), which resolves on the exact
    /// `agent_id` a `SubagentStop` quotes.
    ///
    /// # Which paths actually read this (#5661)
    ///
    /// Three do, and they do not agree on the answer, so state each one rather
    /// than a single claim. `session_manager::prune::prune_orphaned_worktrees`
    /// and `worktree_reconcile::classify` skip an `Agent` tree
    /// UNCONDITIONALLY. `worktree_reclaim::classify` — the merged-PR reclaim
    /// reached by `tm session prune-worktrees --merged-prs` — did not read this
    /// enum at all until #5661, which is how it deleted three live agents'
    /// worktrees on 2026-08-15/16; it now consults it and permits only the one
    /// case the delegation registry can positively call finished (see
    /// [`AgentDelegationState`]).
    Agent(AgentWorktreeOwner, DateTime<Utc>),
    /// Absent, empty, or unparsable — legacy or corrupted; owner unknown.
    Unknown,
}

/// What the delegation registry can say about the agent named by an
/// [`SentinelOwner::Agent`] sentinel (#5661).
///
/// Why: "no non-terminal delegation claims this agent" and "this registry has
/// never heard of this agent" are the same empty answer read two ways, and only
/// the first one is evidence. `DaemonState::delegations` is a `DashMap` built
/// empty at every boot with no load path, so after a restart the registry
/// answers "nothing claims it" for an agent that is still working — which on a
/// destructive path is [ADR-0045](../../../../docs/adr/0045-distinguish-absent-from-undeterminable-on-destructive-paths.md)'s
/// absent-vs-undeterminable confusion, and is how the merged-PR reclaim deleted
/// live agents' worktrees. Splitting the two lets the reclaim gate permit only
/// the answer that carries information.
/// What: [`Live`](Self::Live) — a delegation naming this agent has not reached a
/// terminal status; [`Ended`](Self::Ended) — the registry holds at least one
/// delegation naming this agent and every one of them is terminal;
/// [`Unknown`](Self::Unknown) — the registry holds no delegation naming this
/// agent, so its silence proves nothing.
/// Test: `classify_blocks_a_live_agents_worktree`,
/// `classify_blocks_an_agent_the_registry_never_heard_of`,
/// `classify_allows_a_finished_agents_merged_worktree`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AgentDelegationState {
    /// A delegation naming this agent has not ended — the agent is still working.
    Live,
    /// Every delegation the registry holds for this agent has ended.
    Ended,
    /// The registry holds no delegation for this agent at all.
    Unknown,
}

/// Is `path` a leaf of a harness agent-worktree store — `…/.claude/worktrees/<name>`?
///
/// Why: the STRICT form of "this directory belongs to the agent store", used by
/// every gate that must treat such a directory differently from a session
/// worktree. `worktree_reconcile::categorize` carries a looser "somewhere under
/// `.claude/worktrees`" test, but that one is documented as report text that is
/// "never an input to `ReconcileState`" — promoting a descriptive label to a
/// deletion gate is precisely what that doc forbids, so this is a separate
/// predicate answering a separate question.
///
/// It lives here rather than in `daemon::services::agent_worktree_reap` (its
/// first home, #4311) because two reclamation paths now ask it and this module
/// already owns the store's shape — [`find_agent_worktree`] enumerates the same
/// directory. One implementation, in the domain that owns it.
/// What: the immediate parent's name is `worktrees` and its parent's is
/// `.claude`.
/// Test: `reap_refuses_a_worktree_outside_the_harness_base`,
/// `classify_blocks_an_agent_store_worktree_with_an_unreadable_sentinel`.
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

/// Read and tolerantly parse the ownership sentinel under `worktree_path`.
///
/// Why: the single call site every consumer (orphan-GC, decommission's owner
/// gate) should use, so the tolerant-parse rule lives in exactly one place.
/// What: reads `<worktree_path>/.trusty-mpm-worktree`; a missing file, an
/// empty file, or a read/parse failure all resolve to
/// [`SentinelOwner::Unknown`]; a valid [`WorktreeSentinel`] resolves to
/// [`SentinelOwner::Known`].
/// Test: `sentinel_owner_absent_file_is_unknown`,
/// `sentinel_owner_empty_file_is_unknown`,
/// `sentinel_owner_garbage_file_is_unknown`,
/// `sentinel_owner_round_trips_valid_payload`.
pub(crate) fn read_sentinel_owner(worktree_path: &Path) -> SentinelOwner {
    let sentinel_path = worktree_path.join(WORKTREE_SENTINEL_FILE);
    let Ok(bytes) = std::fs::read(&sentinel_path) else {
        return SentinelOwner::Unknown;
    };
    if bytes.is_empty() {
        return SentinelOwner::Unknown;
    }
    let Ok(payload) = serde_json::from_slice::<WorktreeSentinel>(&bytes) else {
        return SentinelOwner::Unknown;
    };
    // #4311: agent first. Both fields present is a payload no writer produces,
    // and `Agent` is the answer that removes the LEAST authority — the
    // orphan-GC never deletes it — so a malformed sentinel resolves toward
    // keeping the directory.
    if let Some(agent) = payload.agent {
        return SentinelOwner::Agent(agent, payload.created_at);
    }
    match payload.owner_session_id {
        Some(owner) => SentinelOwner::Known(owner, payload.created_at),
        None => SentinelOwner::Unknown,
    }
}

/// Write the AGENT ownership sentinel into `worktree_path` (#4311).
///
/// Why: this is the durable half of the registration. The in-memory delegation
/// record grants the reaper authority to delete a directory; this file is the
/// only evidence of that authority that survives a daemon restart, which is
/// what ADR-0023 point 4 requires of the ownership record.
/// What: serialises a [`WorktreeSentinel::for_agent`] payload and writes it to
/// `<worktree_path>/.trusty-mpm-worktree`. Both failure modes are propagated
/// rather than swallowed — the caller declines to register when this fails, so
/// a silent success here would be the fail-open branch the write exists to
/// close.
///
/// It is NOT atomic (no temp-file-and-rename). A torn write leaves content that
/// does not parse, which [`read_sentinel_owner`] resolves to
/// [`SentinelOwner::Unknown`] — the safe default that is never auto-deleted —
/// and the next subagent tool call rewrites it. Atomicity would buy a stricter
/// guarantee than the tolerant parse needs.
/// Test: `agent_sentinel_round_trips`,
/// `agent_sentinel_write_fails_when_the_path_is_not_writable`.
pub(crate) fn write_agent_sentinel(
    worktree_path: &Path,
    owner: AgentWorktreeOwner,
) -> std::io::Result<()> {
    let bytes = serde_json::to_vec(&WorktreeSentinel::for_agent(owner))
        .map_err(|e| std::io::Error::other(format!("serialize agent sentinel: {e}")))?;
    std::fs::write(worktree_path.join(WORKTREE_SENTINEL_FILE), bytes)
}

/// Rebuild one agent worktree's ownership from disk alone (#4311).
///
/// Why: ADR-0023 point 4 — the ownership record must be reconstructable from
/// on-disk sentinels plus `git worktree list --porcelain`, with no other
/// durable input. The daemon's delegation map has neither persistence nor a
/// load path (`daemon::state::core` initialises it empty), so after a restart
/// this scan is the ONLY thing that can still answer "which directory did agent
/// `X` have?" — without it a restart returns every agent worktree to
/// owner-unknown and the reap never fires again for it.
/// What: reads each immediate child of `store` (`…/.claude/worktrees/`), parses
/// its sentinel, and returns the directory whose recorded `agent_id` equals
/// `agent_id` — but ONLY when exactly one does. An exact key, never a
/// heuristic: the same discipline `delegation_tracker::on_subagent_stop`
/// applies, and for the same reason, except that here the named directory gets
/// deleted.
///
/// # Two matches is undeterminable, not a permit
///
/// Nothing deletes a stale sentinel, and registration re-fires on every
/// `PreToolUse`, so one `agent_id` CAN come to name several directories — an
/// agent that moved between trees leaves the first one stamped. Returning the
/// first `read_dir` hit would make the deletion target depend on filesystem
/// enumeration order, which is not an ownership answer. Ambiguity resolves to
/// `None` per ADR-0045, and the reap ends there, keeping both directories.
/// Test: `agent_sentinel_survives_a_lost_registry`,
/// `agent_sentinel_lookup_ignores_a_different_agent`,
/// `agent_sentinel_lookup_refuses_an_ambiguous_match`.
pub(crate) fn find_agent_worktree(store: &Path, agent_id: &str) -> Option<std::path::PathBuf> {
    let entries = std::fs::read_dir(store).ok()?;
    let mut matches: Vec<std::path::PathBuf> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if let SentinelOwner::Agent(owner, _) = read_sentinel_owner(&path)
            && owner.agent_id == agent_id
        {
            matches.push(path);
        }
    }
    match matches.len() {
        1 => matches.pop(),
        0 => None,
        n => {
            tracing::warn!(
                agent_id,
                store = %store.display(),
                "worktree ownership: {n} directories carry a sentinel naming this agent — \
                 which one it owns is undeterminable, so none is reclaimed (#4311, ADR-0045)"
            );
            None
        }
    }
}

/// Grace window (#3649 review fix): how young an ABSENT-owner sentinel must
/// be before the orphan-GC is willing to treat it as provably ownerless.
///
/// Why: both real sentinel WRITE sites write the sentinel BEFORE the owning
/// session's `SessionRecord` is persisted — real I/O (git clone/worktree-add,
/// `prepare_session`) runs in between (`daemon::managed_routes::lifecycle::
/// spawn_managed_cloned` and `spawn_managed_inproject`). An orphan-GC sweep
/// landing in that window would see "owner not found" and, absent this
/// grace window, wrongly conclude the brand-new worktree is ownerless and
/// reclaim it out from under its own provisioning. 10 minutes is a fixed,
/// self-contained constant (session_manager does not depend on the daemon's
/// configurable `ORPHAN_GC_INTERVAL_SECS`, which defaults to 60s and can be
/// overridden via `TRUSTY_MPM_ORPHAN_GC_INTERVAL_SECS`) chosen to comfortably
/// exceed both that default sweep cadence AND the realistic worst-case
/// provisioning duration (clone + prepare_session), by roughly an order of
/// magnitude — a deliberately generous margin rather than a tight multiple
/// of a value this module cannot see.
/// What: 10 minutes. Applies ONLY to the "owner not found in the store"
/// case; a FOUND owner in a terminal state is provably ownerless regardless
/// of the sentinel's age (its existence is direct evidence the session did
/// exist and has since finished, never a mid-creation race).
/// Test: `resolve_ownerless_with_grace_spares_recent_absent_owner`,
/// `resolve_ownerless_with_grace_reclaims_aged_absent_owner`,
/// `resolve_ownerless_with_grace_true_for_terminal_owner_regardless_of_age`.
pub(crate) const OWNERLESS_GRACE: chrono::Duration = chrono::Duration::minutes(10);

impl SessionManager {
    /// Mark `id`'s record as OWNING its own worktree (#3649).
    ///
    /// Why: the registry field ([`super::record::SessionRecord::worktree_owner`])
    /// is set post-creation (mirroring the existing `set_workspace_owned`
    /// precedent) rather than threaded through the many `create_with_id`/
    /// `create_with_reserved_name` call sites, so only the two real
    /// provisioning call sites (`spawn_managed_cloned`, `spawn_managed_inproject`
    /// in `daemon::managed_routes::lifecycle`) need to call it. Every other
    /// creation path (local-path spawn, adopt, tests) leaves the field at its
    /// `#[serde(default)]` `None` — legacy/owner-unknown, the safe default.
    /// What: looks up the record and sets `worktree_owner = Some(owner)`
    /// (normally `owner == id`, since a session owns its own worktree),
    /// persists, and returns.
    /// Test: `set_worktree_owner_round_trips` in this module's tests.
    pub async fn set_worktree_owner(
        &self,
        id: &ManagedSessionId,
        owner: ManagedSessionId,
    ) -> Result<(), ManagedError> {
        let mut record = self.get(id).await?;
        record.worktree_owner = Some(owner);
        self.store.write().await.upsert(record).await?;
        Ok(())
    }

    /// Best-effort [`Self::set_worktree_owner`] for the two real provisioning
    /// call sites (#3649): logs and swallows a failure rather than propagating,
    /// since a failed set only leaves the record at its safe owner-unknown
    /// default and must never block the spawn it is called from.
    /// Test: covered transitively by `set_worktree_owner_round_trips`.
    pub async fn set_worktree_owner_best_effort(
        &self,
        id: &ManagedSessionId,
        owner: ManagedSessionId,
    ) {
        if let Err(e) = self.set_worktree_owner(id, owner).await {
            tracing::warn!(id = %id, "set_worktree_owner failed (non-fatal): {e}");
        }
    }

    /// Resolve whether `owner` is PROVABLY OWNERLESS (#3649): its worktree may
    /// be safely reclaimed by someone other than itself.
    ///
    /// Why: both the orphan-GC sweep (reading a sentinel's `owner_session_id`)
    /// and the `decommission` owner gate (reading a target record's
    /// `worktree_owner`) need the SAME answer to "is this owner still using
    /// its workspace?" — centralising it here means the GC and the gate can
    /// never disagree on what "ownerless" means.
    /// What: `true` when `owner`'s record does not resolve in the store at
    /// all (deleted/never-existed — the record itself is the strongest
    /// evidence available, and its absence means nothing can contest the
    /// reclaim), OR when it resolves to a record in a TERMINAL state
    /// (`Decommissioned`/`Deleted` — [`ManagedSessionState::is_terminal`]).
    /// `false` for every live/resumable state (`Provisioning`/`Active`/
    /// `Stopped`/`Errored`) — a live or stopped-but-resumable owner's
    /// worktree is NEVER ownerless, matching the #3649 safe-default rule.
    /// Test: `resolve_ownerless_true_for_absent_owner`,
    /// `resolve_ownerless_true_for_terminal_owner`,
    /// `resolve_ownerless_false_for_live_owner`.
    pub(crate) async fn resolve_ownerless(&self, owner: ManagedSessionId) -> bool {
        match self.get(&owner).await {
            Ok(record) => record.state.is_terminal(),
            Err(_) => true,
        }
    }

    /// [`Self::resolve_ownerless`], hardened with the [`OWNERLESS_GRACE`]
    /// window for the sentinel-driven orphan-GC path (#3649 review fix).
    ///
    /// Why: unlike the `decommission` owner gate (which only ever calls
    /// `resolve_ownerless` for an owner id it already fetched a live record
    /// for — see `known_owner_of`'s doc), the orphan-GC sweep resolves
    /// ownership PURELY from an on-disk sentinel it has no other proof about.
    /// A `get()`-not-found result there is genuinely ambiguous: it could mean
    /// the owner was deleted (safe to reclaim), OR that the owner's
    /// `SessionRecord` simply has not been persisted YET because the
    /// sentinel-write and the record-persist are not atomic (see the module
    /// doc). Gating the not-found case on the sentinel's own age resolves
    /// that ambiguity without needing to fix the creation ordering itself.
    /// What: identical to [`Self::resolve_ownerless`] for a FOUND owner
    /// (terminal → `true`, live/resumable → `false`, unconditionally on age).
    /// For a NOT-found owner: `true` (ownerless, reclaim) only when
    /// `sentinel_created_at` is OLDER than [`OWNERLESS_GRACE`]; `false`
    /// (not yet provably ownerless, skip) while still within the window.
    /// Test: `resolve_ownerless_with_grace_spares_recent_absent_owner`,
    /// `resolve_ownerless_with_grace_reclaims_aged_absent_owner`,
    /// `resolve_ownerless_with_grace_true_for_terminal_owner_regardless_of_age`.
    pub(crate) async fn resolve_ownerless_with_grace(
        &self,
        owner: ManagedSessionId,
        sentinel_created_at: DateTime<Utc>,
    ) -> bool {
        match self.get(&owner).await {
            Ok(record) => record.state.is_terminal(),
            Err(_) => Utc::now() - sentinel_created_at > OWNERLESS_GRACE,
        }
    }

    /// Resolve the KNOWN owner of `record`'s worktree, if any (#3649).
    ///
    /// Why: the `decommission` owner gate needs a single "does this target
    /// have a known owner?" answer that checks BOTH sources of truth — the
    /// registry field is the fast path (no disk I/O) for every record created
    /// after #3649, and the on-disk sentinel is a fallback for a worktree
    /// whose registry field was never set (e.g. the post-creation
    /// `set_worktree_owner` call raced with a decommission, or a manual
    /// registry edit) but whose sentinel was still written at provision time.
    /// What: returns `record.worktree_owner` if `Some`; otherwise, if
    /// `record.workspace_path` is set, reads that path's ownership sentinel
    /// via [`read_sentinel_owner`] and returns
    /// [`SentinelOwner::Known`](SentinelOwner::Known)'s inner id; otherwise
    /// `None` (owner unknown — the gate never fires for this target).
    /// Test: `decommission_owner_gate_refuses_foreign_caller` in
    /// `super::decommission::tests`; `known_owner_of_falls_back_to_sentinel`
    /// in this module.
    pub(crate) fn known_owner_of(&self, record: &SessionRecord) -> Option<ManagedSessionId> {
        if let Some(owner) = record.worktree_owner {
            return Some(owner);
        }
        let ws = record.workspace_path.as_deref()?;
        match read_sentinel_owner(ws) {
            SentinelOwner::Known(owner, _created_at) => Some(owner),
            // #4311: an agent worktree has no managed-session owner, so the
            // decommission owner gate has nothing to gate on and never fires.
            SentinelOwner::Agent(..) | SentinelOwner::Unknown => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session_manager::record::{ManagedSessionState, SessionRecord};
    use crate::session_manager::tests::FakeTmuxDriver;

    // ── sentinel parse matrix (#3649) ───────────────────────────────────────

    #[test]
    fn sentinel_owner_absent_file_is_unknown() {
        let dir = tempfile::tempdir().expect("tempdir");
        // No sentinel file written at all.
        assert_eq!(read_sentinel_owner(dir.path()), SentinelOwner::Unknown);
    }

    #[test]
    fn sentinel_owner_empty_file_is_unknown() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join(WORKTREE_SENTINEL_FILE), b"").expect("write empty");
        assert_eq!(read_sentinel_owner(dir.path()), SentinelOwner::Unknown);
    }

    #[test]
    fn sentinel_owner_garbage_file_is_unknown() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join(WORKTREE_SENTINEL_FILE),
            b"not json { garbage",
        )
        .expect("write garbage");
        assert_eq!(read_sentinel_owner(dir.path()), SentinelOwner::Unknown);
    }

    #[test]
    fn sentinel_owner_round_trips_valid_payload() {
        let dir = tempfile::tempdir().expect("tempdir");
        let owner = ManagedSessionId::new();
        let before = Utc::now();
        std::fs::write(
            dir.path().join(WORKTREE_SENTINEL_FILE),
            sentinel_payload_bytes(owner),
        )
        .expect("write sentinel");
        match read_sentinel_owner(dir.path()) {
            SentinelOwner::Known(got_owner, created_at) => {
                assert_eq!(got_owner, owner);
                assert!(
                    created_at >= before && created_at <= Utc::now(),
                    "created_at must be freshly stamped at write time"
                );
            }
            other => panic!("expected Known, got {other:?}"),
        }
    }

    // ── agent sentinel (#4311, DOC-66 §1.3) ─────────────────────────────────

    fn an_agent(agent_id: &str) -> AgentWorktreeOwner {
        AgentWorktreeOwner {
            agent_id: agent_id.to_string(),
            delegation_id: DelegationId(uuid::Uuid::new_v4()),
            parent_session_id: SessionId::new(),
        }
    }

    /// An agent sentinel round-trips, and resolves as `Agent` — never `Known`.
    ///
    /// Why the distinction is the point: `Known` routes the owner through
    /// `resolve_ownerless_with_grace`, a session-store lookup an agent has no
    /// record in. Past the grace window that lookup reports "no owner" and the
    /// orphan-GC deletes the tree on its 60-second cadence, with no liveness
    /// check anywhere in its chain. If this assertion is ever relaxed to
    /// `Known`, that is the failure it lets through.
    #[test]
    fn agent_sentinel_round_trips() {
        let dir = tempfile::tempdir().expect("tempdir");
        let owner = an_agent("a403cdbc078b5c474");
        let before = Utc::now();

        write_agent_sentinel(dir.path(), owner.clone()).expect("write agent sentinel");

        match read_sentinel_owner(dir.path()) {
            SentinelOwner::Agent(got, created_at) => {
                assert_eq!(
                    got, owner,
                    "every recorded field must survive the round trip"
                );
                assert!(created_at >= before && created_at <= Utc::now());
            }
            other => panic!("an agent sentinel must resolve as Agent, got {other:?}"),
        }
    }

    /// A pre-#4311 sentinel still reads as `Known` — widening the payload
    /// migrates nothing and re-reads every sentinel already on disk.
    #[test]
    fn a_pre_agent_sentinel_still_resolves_to_its_session_owner() {
        let dir = tempfile::tempdir().expect("tempdir");
        let owner = ManagedSessionId::new();
        // The exact bytes the pre-#4311 writer produced: no `agent` key at all.
        let legacy = format!(
            r#"{{"owner_session_id":"{owner}","created_at":"{}"}}"#,
            Utc::now().to_rfc3339()
        );
        std::fs::write(dir.path().join(WORKTREE_SENTINEL_FILE), legacy).expect("write legacy");

        assert!(
            matches!(read_sentinel_owner(dir.path()), SentinelOwner::Known(got, _) if got == owner),
            "a payload written before #4311 must keep resolving to its session owner"
        );
    }

    /// A payload naming NEITHER owner is owner-unknown, not a half-answer.
    #[test]
    fn a_sentinel_naming_no_owner_at_all_is_unknown() {
        let dir = tempfile::tempdir().expect("tempdir");
        let bytes = format!(r#"{{"created_at":"{}"}}"#, Utc::now().to_rfc3339());
        std::fs::write(dir.path().join(WORKTREE_SENTINEL_FILE), bytes).expect("write");

        assert_eq!(read_sentinel_owner(dir.path()), SentinelOwner::Unknown);
    }

    /// #4311 REGRESSION: the write's ERROR arm is reported, never swallowed.
    ///
    /// Why this test and not a happy-path one: the caller declines to register
    /// the worktree when this fails, and that decision is only reachable if the
    /// failure actually propagates. An implementation that logged and returned
    /// `Ok(())` — or that fell back to empty bytes the way
    /// `sentinel_payload_bytes` does — would leave the tree registered in
    /// memory with nothing on disk backing it, which is the fail-open branch
    /// the whole write exists to close.
    ///
    /// The injection is a DIRECTORY at the sentinel's path: `fs::write` cannot
    /// truncate one, so the write fails for a real filesystem reason with no
    /// mocking and no test-only production code.
    #[test]
    fn agent_sentinel_write_fails_when_the_path_is_not_writable() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir(dir.path().join(WORKTREE_SENTINEL_FILE)).expect("occupy the path");

        let err = write_agent_sentinel(dir.path(), an_agent("a403"))
            .expect_err("writing over a directory must fail, not silently succeed");

        assert!(
            !err.to_string().is_empty(),
            "the reason must reach the caller"
        );
        assert_eq!(
            read_sentinel_owner(dir.path()),
            SentinelOwner::Unknown,
            "nothing readable was written, so the tree stays owner-unknown"
        );
    }

    /// #4311 REGRESSION: ownership is rebuildable from disk alone (ADR-0023
    /// point 4) — no daemon state, no session store, no delegation map.
    ///
    /// Why: the delegation map is rebuilt empty at every daemon boot with no
    /// load path, so this scan is the only thing that can still answer "which
    /// directory did agent X have?" after a restart. A test exercising the
    /// in-memory path would pass against an implementation that loses
    /// everything on restart.
    #[test]
    fn agent_sentinel_survives_a_lost_registry() {
        let store = tempfile::tempdir().expect("tempdir");
        for name in ["agent-one", "agent-two", "agent-three"] {
            let wt = store.path().join(name);
            std::fs::create_dir(&wt).expect("create worktree dir");
            write_agent_sentinel(&wt, an_agent(name)).expect("write sentinel");
        }

        assert_eq!(
            find_agent_worktree(store.path(), "agent-two"),
            Some(store.path().join("agent-two")),
            "the sentinel alone must name the directory, with no registry consulted"
        );
    }

    /// The lookup is an EXACT key match, never a nearest guess.
    ///
    /// Why: `on_subagent_stop` forbids a "most recent" fallback because under
    /// concurrency it closes the wrong agent. Here the consequence is worse —
    /// the wrong DIRECTORY gets deleted — so an unmatched id must end the
    /// path rather than return a neighbour.
    #[test]
    fn agent_sentinel_lookup_ignores_a_different_agent() {
        let store = tempfile::tempdir().expect("tempdir");
        let wt = store.path().join("agent-one");
        std::fs::create_dir(&wt).expect("create worktree dir");
        write_agent_sentinel(&wt, an_agent("a403")).expect("write sentinel");
        // A sibling with no sentinel at all must not be offered up either.
        std::fs::create_dir(store.path().join("agent-bare")).expect("create bare dir");

        assert_eq!(find_agent_worktree(store.path(), "a404"), None);
        assert_eq!(find_agent_worktree(store.path(), ""), None);
    }

    /// #4311 REGRESSION: one `agent_id` in TWO directories resolves to neither.
    ///
    /// Why: nothing deletes a stale sentinel and registration re-fires on every
    /// `PreToolUse`, so an agent that moved between trees leaves both stamped.
    /// Returning the first `read_dir` hit would make the deletion target depend
    /// on filesystem enumeration order — the reap would remove whichever
    /// directory the OS happened to list first. Ambiguity is undeterminable
    /// (ADR-0045), not a licence to pick one.
    #[test]
    fn agent_sentinel_lookup_refuses_an_ambiguous_match() {
        let store = tempfile::tempdir().expect("tempdir");
        for name in ["agent-first", "agent-second"] {
            let wt = store.path().join(name);
            std::fs::create_dir(&wt).expect("create worktree dir");
            write_agent_sentinel(&wt, an_agent("a-moved-around")).expect("write sentinel");
        }

        assert_eq!(
            find_agent_worktree(store.path(), "a-moved-around"),
            None,
            "two directories claiming one agent must reclaim neither"
        );
    }

    /// Two agents registering concurrently keep separate directories.
    ///
    /// Why: a sentinel is written per worktree, so the interleaving that
    /// matters is two writers racing against one STORE. An implementation
    /// keyed on anything but the exact `agent_id` — a scan order, a "latest
    /// sentinel wins" rule — resolves both stops to one directory and deletes
    /// a live agent's tree.
    #[test]
    fn concurrent_agent_registrations_stay_separate() {
        let store = tempfile::tempdir().expect("tempdir");
        let paths: Vec<_> = (0..8)
            .map(|i| {
                let wt = store.path().join(format!("agent-{i}"));
                std::fs::create_dir(&wt).expect("create worktree dir");
                wt
            })
            .collect();

        std::thread::scope(|s| {
            for (i, wt) in paths.iter().enumerate() {
                s.spawn(move || {
                    write_agent_sentinel(wt, an_agent(&format!("id-{i}"))).expect("write");
                });
            }
        });

        for (i, wt) in paths.iter().enumerate() {
            assert_eq!(
                find_agent_worktree(store.path(), &format!("id-{i}")).as_ref(),
                Some(wt),
                "each agent must resolve to its OWN directory, never a sibling's"
            );
        }
    }

    // ── resolve_ownerless (#3649) ────────────────────────────────────────────

    /// Returns the [`tempfile::TempDir`] alongside the manager/id — the caller
    /// MUST keep it bound (e.g. `let (mgr, id, _dir) = ...`) for the test's
    /// full lifetime. Dropping it early deletes the backing `sessions.json`
    /// directory; a subsequent `SessionManager::get` then reloads against an
    /// ABSENT file, which `SessionStore::read_file` treats as "starting
    /// fresh" (empty store) rather than an error — silently losing the
    /// upserted record instead of surfacing a failure.
    async fn manager_with_record(
        state: ManagedSessionState,
    ) -> (SessionManager, ManagedSessionId, tempfile::TempDir) {
        let dir = crate::test_support::hermetic_temp_dir();
        let tmux = FakeTmuxDriver::new();
        let mgr = SessionManager::new(dir.path(), tmux)
            .await
            .expect("SessionManager::new");
        let id = ManagedSessionId::new();
        let record = SessionRecord {
            id,
            tmux_name: "tm-ownerless-test".into(),
            cwd: std::path::PathBuf::from("/tmp"),
            task: "task".into(),
            state,
            created_at: Utc::now(),
            last_activity_at: None,
            workspace_path: None,
            repo_url: None,
            branch: None,
            pending_decision: None,
            proposed_default: None,
            correlation: Default::default(),
            runtime: Default::default(),
            ephemeral: false,
            workspace_owned: false,
            source_id: None,
            claude_session_id: None,
            scrollback_path: None,
            last_cwd: None,
            deliverable_id: None,
            pane_id: None,
            injection_status: Default::default(),
            worktree_owner: Some(id),
            terminal_at: None,
        };
        mgr.store
            .write()
            .await
            .upsert(record)
            .await
            .expect("upsert");
        (mgr, id, dir)
    }

    #[tokio::test]
    async fn resolve_ownerless_true_for_absent_owner() {
        let dir = crate::test_support::hermetic_temp_dir();
        let mgr = SessionManager::new(dir.path(), FakeTmuxDriver::new())
            .await
            .expect("SessionManager::new");
        let never_existed = ManagedSessionId::new();
        assert!(
            mgr.resolve_ownerless(never_existed).await,
            "an owner with no record at all must be provably ownerless"
        );
    }

    #[tokio::test]
    async fn resolve_ownerless_true_for_terminal_owner() {
        let (mgr, id, _dir) = manager_with_record(ManagedSessionState::Decommissioned).await;
        assert!(
            mgr.resolve_ownerless(id).await,
            "a terminal (Decommissioned) owner must be provably ownerless"
        );
    }

    #[tokio::test]
    async fn resolve_ownerless_false_for_live_owner() {
        let (mgr, id, _dir) = manager_with_record(ManagedSessionState::Active).await;
        assert!(
            !mgr.resolve_ownerless(id).await,
            "a live (Active) owner must NEVER be treated as ownerless"
        );
    }

    #[tokio::test]
    async fn resolve_ownerless_false_for_stopped_owner() {
        let (mgr, id, _dir) = manager_with_record(ManagedSessionState::Stopped).await;
        assert!(
            !mgr.resolve_ownerless(id).await,
            "a Stopped (resumable) owner must NEVER be treated as ownerless"
        );
    }

    // ── resolve_ownerless_with_grace (#3649 review fix) ──────────────────────

    /// A sentinel naming an owner with NO record at all, but stamped RECENTLY
    /// (well within [`OWNERLESS_GRACE`]), must NOT be treated as ownerless —
    /// this is the exact creation-race window the sentinel-before-record
    /// ordering opens up.
    #[tokio::test]
    async fn resolve_ownerless_with_grace_spares_recent_absent_owner() {
        let dir = crate::test_support::hermetic_temp_dir();
        let mgr = SessionManager::new(dir.path(), FakeTmuxDriver::new())
            .await
            .expect("SessionManager::new");
        let never_existed = ManagedSessionId::new();
        assert!(
            !mgr.resolve_ownerless_with_grace(never_existed, Utc::now())
                .await,
            "a freshly-stamped absent owner must NOT be treated as ownerless \
             (mid-creation race, #3649)"
        );
    }

    /// A sentinel naming an owner with no record, stamped OLDER than
    /// [`OWNERLESS_GRACE`], IS provably ownerless — this preserves the
    /// legitimate "owner was purged" cleanup path.
    #[tokio::test]
    async fn resolve_ownerless_with_grace_reclaims_aged_absent_owner() {
        let dir = crate::test_support::hermetic_temp_dir();
        let mgr = SessionManager::new(dir.path(), FakeTmuxDriver::new())
            .await
            .expect("SessionManager::new");
        let never_existed = ManagedSessionId::new();
        let aged = Utc::now() - OWNERLESS_GRACE - chrono::Duration::minutes(1);
        assert!(
            mgr.resolve_ownerless_with_grace(never_existed, aged).await,
            "an absent owner older than the grace window must be reclaimable"
        );
    }

    /// A FOUND terminal-state owner is ownerless regardless of the
    /// sentinel's age — the grace window only ever gates the not-found case.
    #[tokio::test]
    async fn resolve_ownerless_with_grace_true_for_terminal_owner_regardless_of_age() {
        let (mgr, id, _dir) = manager_with_record(ManagedSessionState::Decommissioned).await;
        assert!(
            mgr.resolve_ownerless_with_grace(id, Utc::now()).await,
            "a terminal owner must be ownerless even with a freshly-stamped sentinel"
        );
    }

    // ── set_worktree_owner (#3649) ───────────────────────────────────────────

    #[tokio::test]
    async fn set_worktree_owner_round_trips() {
        let (mgr, id, _dir) = manager_with_record(ManagedSessionState::Active).await;
        mgr.set_worktree_owner(&id, id)
            .await
            .expect("set_worktree_owner");
        let record = mgr.get(&id).await.expect("get");
        assert_eq!(record.worktree_owner, Some(id));
    }

    // ── known_owner_of (#3649) ───────────────────────────────────────────────

    /// `known_owner_of` falls back to the on-disk sentinel when the
    /// registry field itself is `None` (e.g. `set_worktree_owner` never ran
    /// or raced with a decommission), so the owner gate still resolves the
    /// owner from whichever source recorded it.
    #[tokio::test]
    async fn known_owner_of_falls_back_to_sentinel() {
        let (mgr, _id, _dir) = manager_with_record(ManagedSessionState::Active).await;
        let ws = tempfile::tempdir().expect("tempdir");
        let sentinel_owner = ManagedSessionId::new();
        std::fs::write(
            ws.path().join(WORKTREE_SENTINEL_FILE),
            sentinel_payload_bytes(sentinel_owner),
        )
        .expect("write sentinel");

        let record = SessionRecord {
            id: ManagedSessionId::new(),
            tmux_name: "tm-fallback-test".into(),
            cwd: ws.path().to_path_buf(),
            task: "task".into(),
            state: ManagedSessionState::Active,
            created_at: Utc::now(),
            last_activity_at: None,
            workspace_path: Some(ws.path().to_path_buf()),
            repo_url: None,
            branch: None,
            pending_decision: None,
            proposed_default: None,
            correlation: Default::default(),
            runtime: Default::default(),
            ephemeral: false,
            workspace_owned: false,
            source_id: None,
            claude_session_id: None,
            scrollback_path: None,
            last_cwd: None,
            deliverable_id: None,
            pane_id: None,
            injection_status: Default::default(),
            worktree_owner: None, // registry field unset — must fall back
            terminal_at: None,
        };

        assert_eq!(mgr.known_owner_of(&record), Some(sentinel_owner));
    }
}
