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
//! [`SessionManager::resolve_ownerless`] / [`SessionManager::set_worktree_owner`]
//! (the store-backed "is this owner reclaimable?" check and the registry
//! setter, respectively).
//! Test: `sentinel_owner_*` (parse matrix) and `resolve_ownerless_*` (terminal
//! vs. live vs. absent owner) below.

use std::path::Path;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::decommission::WORKTREE_SENTINEL_FILE;
use super::manager::{ManagedError, SessionManager};
use super::record::{ManagedSessionId, SessionRecord};

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
    pub owner_session_id: ManagedSessionId,
    pub created_at: DateTime<Utc>,
}

impl WorktreeSentinel {
    /// Build a fresh sentinel payload for `owner`, timestamped `now`.
    ///
    /// Why: both sentinel WRITE sites (`workspace.rs`, `inproject.rs`) need
    /// the identical payload shape; centralising construction here keeps them
    /// from drifting.
    /// What: `Self { owner_session_id: owner, created_at: Utc::now() }`.
    /// Test: `sentinel_owner_round_trips_valid_payload`.
    pub(crate) fn new(owner: ManagedSessionId) -> Self {
        Self {
            owner_session_id: owner,
            created_at: Utc::now(),
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
/// What: [`Known`](Self::Known) wraps the resolved [`ManagedSessionId`];
/// [`Unknown`](Self::Unknown) covers every other case (absent file, empty
/// file, or a file whose content does not parse as [`WorktreeSentinel`]).
/// Test: `sentinel_owner_absent_file_is_unknown`,
/// `sentinel_owner_empty_file_is_unknown`,
/// `sentinel_owner_garbage_file_is_unknown`,
/// `sentinel_owner_round_trips_valid_payload`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SentinelOwner {
    /// The sentinel parsed and named an owning session.
    Known(ManagedSessionId),
    /// Absent, empty, or unparsable — legacy or corrupted; owner unknown.
    Unknown,
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
    match serde_json::from_slice::<WorktreeSentinel>(&bytes) {
        Ok(payload) => SentinelOwner::Known(payload.owner_session_id),
        Err(_) => SentinelOwner::Unknown,
    }
}

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
    /// Test: `decommission_owner_gate_refuses_foreign_caller`,
    /// `decommission_owner_gate_falls_back_to_sentinel` in
    /// `super::decommission_worktree_tests` / this module.
    pub(crate) fn known_owner_of(&self, record: &SessionRecord) -> Option<ManagedSessionId> {
        if let Some(owner) = record.worktree_owner {
            return Some(owner);
        }
        let ws = record.workspace_path.as_deref()?;
        match read_sentinel_owner(ws) {
            SentinelOwner::Known(owner) => Some(owner),
            SentinelOwner::Unknown => None,
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
        std::fs::write(
            dir.path().join(WORKTREE_SENTINEL_FILE),
            sentinel_payload_bytes(owner),
        )
        .expect("write sentinel");
        assert_eq!(read_sentinel_owner(dir.path()), SentinelOwner::Known(owner));
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
}
