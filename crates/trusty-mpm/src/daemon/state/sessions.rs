//! Session, project, and delegation methods on [`DaemonState`].
//!
//! Why: the session-registry operations are a cohesive group used by many
//! handlers but unrelated to resource-management (breakers, memory, overseer);
//! splitting them keeps each file focused and under the SLOC cap.
//! What: session CRUD, project CRUD, delegation upsert/query, and the
//! dead-session reaper.
//! Test: see `super::tests`.

use std::path::PathBuf;

use crate::core::agent::{Delegation, DelegationId, DelegationSource, DelegationStatus};
use crate::core::project::ProjectInfo;
use crate::core::session::{Session, SessionId};

use super::core::PAIR_CODE_TTL;
use super::core::{DaemonState, ReapResult};
use crate::daemon::tmux::TmuxDriver;

/// How long a live delegation may go without a terminal signal before tracking
/// gives up on it and marks it [`DelegationStatus::Stale`] (#2864 review).
///
/// Why this exists at all: `SubagentStop` is the only signal that closes a
/// `Running` delegation, and it is not guaranteed to arrive — `tm hook` POSTs
/// fail open on a 2 s budget, an interrupted subagent emits no stop at all, and
/// a dispatch that never learned an `agent_id` can never be resolved by one.
/// Without a bound, one such delegation suppresses its session's idle nudge for
/// the daemon's entire lifetime, which is the very bug #2864 set out to fix.
///
/// Why six hours and not minutes: agents in this workspace legitimately run for
/// hours (a foreground `gh pr checks --watch` on a slow CI leg, a multi-crate
/// release chain). A short TTL would manufacture the false negative it is meant
/// to prevent. Six hours is well past the longest observed real run (~2 h) while
/// still bounding the damage to one working day rather than forever.
///
/// Why being wrong is survivable: the sweep writes `Stale`, never `Completed`.
/// A delegation that outlives the budget is reported as "tracking lost", not as
/// finished, and a late `SubagentStop` still resolves it to the truth because
/// `Stale` is not terminal.
/// Test: `stale_running_delegation_stops_suppressing_the_nudge`.
pub(crate) const RUNNING_STALE_AFTER_SECS: i64 = 6 * 60 * 60;

/// How long a `Queued` `McpDeclared` delegation may sit undispatched before it
/// is marked [`DelegationStatus::Stale`] (#2864 review).
///
/// Why so much shorter than [`RUNNING_STALE_AFTER_SECS`]: a `Queued`
/// `McpDeclared` record is a *declaration of intent* — `agent_delegate`
/// explicitly does not execute anything (#1942) — so it is not evidence of a
/// running agent at all. Nothing but a matching hook observation ever advances
/// it, and the dedup that would consume it gives up after
/// `delegation_tracker::DEDUP_WINDOW_SECS`. Past that window an undispatched
/// declaration is stale by definition; keeping it "live" only suppresses the
/// idle nudge for a subagent that was never spawned.
/// Test: `declared_but_never_dispatched_goes_stale_quickly`.
pub(crate) const DECLARED_STALE_AFTER_SECS: i64 = 15 * 60;

/// How long a **terminal** delegation is retained after `ended_at` (#2864
/// review).
///
/// Why: without eviction the delegation map grows monotonically for the
/// daemon's lifetime, and every dispatch pays two O(N) scans over it. An hour
/// is far longer than any consumer needs — `session_status` and
/// `/tm-session-pause` both ask "what is in flight *now*" — and it bounds N to
/// roughly one hour of fleet dispatch volume instead of one uptime's worth.
/// Nothing can ever resolve a terminal record, so evicting it loses nothing.
/// [`DelegationStatus::Stale`] is emphatically NOT terminal and does not use
/// this window — see [`STALE_RETENTION_SECS`].
/// Test: `terminal_delegations_are_evicted_after_retention`,
/// `live_delegations_are_never_evicted`.
pub(crate) const DELEGATION_RETENTION_SECS: i64 = 60 * 60;

/// How long a [`DelegationStatus::Stale`] delegation is retained, measured from
/// its own start (#2864 re-review).
///
/// Why this is separate from — and 24x — [`DELEGATION_RETENTION_SECS`]: the
/// entire justification for `Stale` being a distinct, *non-terminal* status is
/// that a late `SubagentStop` can still resolve it to the truth. Reusing the
/// terminal retention window silently cancelled that guarantee: a record whose
/// agent may still be alive was dropped one hour after tracking gave up on it
/// (about 7 h total), after which a stop resolved nothing because there was no
/// record left to find. A guarantee that expires unannounced is worse than no
/// guarantee.
///
/// Measuring from `started_at`/`created_at` rather than from a separate
/// "when we gave up" stamp keeps [`Delegation::ended_at`] meaning exactly one
/// thing — *reached a terminal status* — which matters because S3 will persist
/// it. With the current constants a `Running` delegation goes `Stale` at 6 h and
/// remains resolvable until 24 h: an **18-hour** recovery window, and a hard
/// bound so the map still cannot grow without limit.
/// Test: `stale_delegation_stays_resolvable_far_past_the_terminal_window`,
/// `a_stale_delegation_is_eventually_evicted`.
pub(crate) const STALE_RETENTION_SECS: i64 = 24 * 60 * 60;

/// What one [`DaemonState::sweep_delegations`] pass did.
///
/// Why: the sweep runs on a timer with no caller to inspect its effects, so it
/// returns a count for the log line and gives tests a precise assertion target
/// instead of forcing them to re-derive the outcome from the map.
/// Test: every `*_goes_stale_*` / `*_evicted_*` test asserts on this.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct DelegationSweep {
    /// Live delegations marked [`DelegationStatus::Stale`] this pass.
    pub staled: usize,
    /// Non-live delegations removed from the map this pass.
    pub evicted: usize,
}

impl DaemonState {
    // ---- bot pairing ----------------------------------------------------

    /// Generate and store a one-time pairing code.
    ///
    /// Why: `tm pair` asks the daemon for a short code the operator types into
    /// the Telegram bot; the daemon must remember it (and its issue time) so a
    /// later `/pair` confirm can validate it within the TTL window. The code is
    /// ALSO persisted to a canonical file under the framework root so the confirm
    /// surface validates against a single shared source of truth — without it a
    /// code minted on one daemon instance was rejected as "invalid" when confirmed
    /// against another instance's empty in-memory store (#1500).
    /// What: derives a six-character uppercase alphanumeric code from a fresh
    /// UUID, stores it both in memory (`pair_code`) and on disk
    /// (`<framework_root>/pending_pair.json`), and returns the code. A failed disk
    /// write is logged, not fatal — the in-memory copy still works for the
    /// same-process case.
    /// Test: `pairing_round_trip`, `pairing_code_persists_to_disk`.
    pub fn generate_pair_code(&self) -> String {
        let code: String = uuid::Uuid::new_v4()
            .simple()
            .to_string()
            .chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .take(6)
            .collect::<String>()
            .to_uppercase();
        *self.pair_code.lock() = Some((code.clone(), std::time::Instant::now()));
        let pending = crate::daemon::pairing_store::PendingPairCode::new(code.clone());
        if let Err(e) = crate::daemon::pairing_store::save_pending(&self.framework_root, &pending) {
            tracing::warn!("failed to persist pending Telegram pairing code: {e}");
        }
        code
    }

    /// Confirm a pairing code and register `chat_id` on success.
    ///
    /// Why: the bot's `/pair <code>` flow validates the operator's code and, on
    /// success, binds the chat so push alerts have a destination — and the
    /// binding must survive a daemon restart. The code is validated against the
    /// SHARED on-disk store (falling back to the in-memory copy) so a code minted
    /// by any surface rooted at the same framework root is accepted, eliminating
    /// the cross-instance mismatch that forced the `pairing.json` pre-seed
    /// workaround (#1500).
    /// What: returns `true` and stores `chat_id` (in memory *and* persisted to
    /// `~/.trusty-mpm/pairing.json`) when `code` matches the outstanding code
    /// (on disk OR in memory) and it is within [`PAIR_CODE_TTL`]; clears the
    /// pending code from BOTH stores either way (a used or expired code never
    /// validates twice). A failed disk write is logged, not fatal.
    /// Test: `pairing_round_trip`, `pairing_persists_to_disk`,
    /// `pairing_confirms_shared_disk_code`.
    pub fn confirm_pair_code(&self, code: &str, chat_id: i64) -> bool {
        use crate::daemon::pairing_store;
        let mut guard = self.pair_code.lock();
        // Prefer the shared on-disk pending code (the cross-instance source of
        // truth); fall back to the in-memory copy for the same-process case when
        // no disk write has happened (or the file is unreadable).
        let disk_valid = pairing_store::claim_pending(&self.framework_root)
            .is_some_and(|p| p.code == code && p.is_fresh(PAIR_CODE_TTL));
        let mem_valid = matches!(
            guard.as_ref(),
            Some((stored, issued))
                if stored == code && issued.elapsed() < PAIR_CODE_TTL
        );
        let valid = disk_valid || mem_valid;
        // A confirm attempt always consumes the outstanding code from both
        // stores so neither a used nor an expired code can validate twice.
        *guard = None;
        if let Err(e) = pairing_store::clear_pending(&self.framework_root) {
            tracing::warn!("failed to clear pending Telegram pairing code: {e}");
        }
        if valid {
            *self.paired_chat_id.lock() = Some(chat_id);
            let record = pairing_store::PairingRecord::new(chat_id);
            if let Err(e) = pairing_store::save(&self.framework_root, &record) {
                tracing::warn!("failed to persist Telegram pairing: {e}");
            }
        }
        valid
    }

    /// Clear the Telegram pairing, in memory and on disk.
    ///
    /// Why: `POST /pair/reset` (or any explicit unpair) must drop the binding so
    /// a restart does not resurrect it from `pairing.json`. Any outstanding
    /// pending CODE is dropped too so a reset truly returns the daemon to an
    /// unpaired, no-code state.
    /// What: sets `paired_chat_id` to `None`, clears the in-memory `pair_code`,
    /// and deletes both the persisted record and the shared pending code; a
    /// failed delete is logged, not fatal.
    /// Test: `pairing_reset_clears_disk`.
    pub fn clear_pairing(&self) {
        *self.paired_chat_id.lock() = None;
        *self.pair_code.lock() = None;
        if let Err(e) = crate::daemon::pairing_store::clear(&self.framework_root) {
            tracing::warn!("failed to delete persisted Telegram pairing: {e}");
        }
        if let Err(e) = crate::daemon::pairing_store::clear_pending(&self.framework_root) {
            tracing::warn!("failed to delete pending Telegram pairing code: {e}");
        }
    }

    /// The chat id currently paired with this daemon, if any.
    ///
    /// Why: `GET /pair/status` and the alert loop need the paired destination.
    /// What: returns the stored chat id, or `None` when unpaired.
    /// Test: `pairing_round_trip`.
    pub fn paired_chat_id(&self) -> Option<i64> {
        *self.paired_chat_id.lock()
    }

    // ---- sessions -------------------------------------------------------

    /// Register (or replace) a managed session.
    pub fn register_session(&self, session: Session) {
        self.sessions.insert(session.id, session);
    }

    /// Record the OS-level `claude` process PID on a registered session.
    ///
    /// Why: the CLI and the daemon discover the real `claude` PID inside a tmux
    /// pane *after* launch; reporting it back lets the reaper check process
    /// liveness rather than relying on the tmux session alone.
    /// What: sets `session.pid = Some(pid)` under a write guard; returns `true`
    /// when the session existed, `false` for an unknown id. On success it ALSO
    /// records the PID in the on-disk PID-file registry (§10.3) so a future
    /// orphan-GC sweep can find and reap this `claude` process even after its
    /// tmux pane (and this in-memory entry) are gone. A registry write failure is
    /// logged but never fails the call — PID tracking is best-effort hardening.
    /// Test: `set_session_pid_updates_field`, `set_session_pid_writes_pidfile`.
    pub fn set_session_pid(&self, id: SessionId, pid: u32) -> bool {
        let updated = self.update_session(&id, |s| s.pid = Some(pid));
        if updated && let Err(e) = self.pid_registry().register(&id.0.to_string(), pid) {
            tracing::warn!(session_id = %id.0, pid, "pid-registry: register failed: {e}");
        }
        updated
    }

    /// Remove a session and its associated memory snapshot.
    ///
    /// Also unregisters the session's PID-file (§10.3) so a cleanly-removed
    /// session is never treated as an orphan by a later sweep. A missing PID file
    /// is a no-op; an unexpected removal error is logged, not propagated.
    pub fn remove_session(&self, id: SessionId) -> Option<Session> {
        self.memory.remove(&id);
        if let Err(e) = self.pid_registry().unregister(&id.0.to_string()) {
            tracing::warn!(session_id = %id.0, "pid-registry: unregister failed: {e}");
        }
        self.sessions.remove(&id).map(|(_, s)| s)
    }

    /// Snapshot all managed sessions.
    pub fn list_sessions(&self) -> Vec<Session> {
        self.sessions.iter().map(|e| e.value().clone()).collect()
    }

    /// Look up one session by id.
    pub fn session(&self, id: SessionId) -> Option<Session> {
        self.sessions.get(&id).map(|e| e.value().clone())
    }

    /// Mutate an existing session in place under a write lock.
    ///
    /// Why: the pause/resume handlers must change a session's `status`,
    /// `paused_at`, and `pause_summary` atomically without the read-modify-write
    /// race of `session()` + `register_session()`.
    /// What: takes a write guard on the session entry and calls `f` if the
    /// session exists; returns `true` when it ran, `false` for an unknown id.
    /// Test: `update_session_mutates_existing`, `update_session_missing_is_false`.
    pub fn update_session<F>(&self, id: &SessionId, f: F) -> bool
    where
        F: FnOnce(&mut Session),
    {
        match self.sessions.get_mut(id) {
            Some(mut entry) => {
                f(entry.value_mut());
                true
            }
            None => false,
        }
    }

    /// Snapshot the sessions belonging to one project.
    ///
    /// Why: `GET /sessions?project=<path>` and `trusty-mpm session list`
    /// scope the listing to the caller's project.
    /// What: returns every session whose `project_path` equals `path`.
    /// Test: `list_sessions_for_project_filters`.
    pub fn list_sessions_for_project(&self, path: &std::path::Path) -> Vec<Session> {
        self.sessions
            .iter()
            .filter(|e| e.value().project_path.as_deref() == Some(path))
            .map(|e| e.value().clone())
            .collect()
    }

    /// Look up one session by id or by friendly tmux name.
    ///
    /// Why: the `session stop` / `session info` subcommands accept either a
    /// UUID or the friendly `tm-<adj>-<noun>` name the daemon prints on
    /// start; resolving both keeps the CLI ergonomic.
    /// What: tries to parse `key` as a UUID first; on failure scans the
    /// registry for a session whose `tmux_name` matches.
    /// Test: `find_session_by_id_or_name`.
    pub fn find_session(&self, key: &str) -> Option<Session> {
        if let Ok(uuid) = uuid::Uuid::parse_str(key) {
            return self.session(SessionId(uuid));
        }
        self.sessions
            .iter()
            .find(|e| e.value().tmux_name == key)
            .map(|e| e.value().clone())
    }

    /// Drop dead tmux sessions and mark Stopped ones whose process has exited.
    ///
    /// Why: sessions accumulate forever otherwise — a dead tmux session leaves a
    /// stale registry entry behind. Additionally a tmux session can outlive the
    /// `claude` process inside it (the pane drops to a bare shell); such a
    /// session should be visibly `Stopped`, not silently "active". The daemon's
    /// housekeeping loop calls this periodically, and `DELETE /sessions/dead`
    /// calls it on demand.
    /// What: discovers the live tmux session names via `driver.list_sessions()`,
    /// then delegates to [`reap_against`](Self::reap_against). A failed tmux
    /// listing reaps nothing (returns a zeroed [`ReapResult`]) rather than
    /// wrongly deleting every session.
    /// Test: `reap_dead_sessions`, `reap_marks_stopped_when_pid_dead`.
    pub fn reap_dead_sessions(&self, driver: &TmuxDriver) -> ReapResult {
        let live: std::collections::HashSet<String> = match driver.list_sessions() {
            Ok(sessions) => sessions.into_iter().map(|s| s.name).collect(),
            Err(e) => {
                tracing::warn!("reap skipped — tmux list-sessions failed: {e}");
                return ReapResult::default();
            }
        };
        self.reap_against(&live)
    }

    /// Remove dead tmux sessions and mark Stopped ones with a dead process.
    ///
    /// Why: separating the set-difference logic from the tmux call makes the
    /// reaping rule unit-testable without spawning a tmux process. Native
    /// (`SessionHost::Native`) sessions have no tmux session, so the tmux
    /// liveness check must skip them — otherwise every discovered Terminal.app
    /// process would be reaped the instant after it was discovered.
    /// What: for tmux-origin sessions —
    /// - if the `tmux_name` is absent from `live`, the entry is removed;
    /// - if the `tmux_name` is alive but the session has a tracked `pid` whose
    ///   `claude` process has exited, the session is marked
    ///   [`SessionStatus::Stopped`] in place (kept so the operator can see it).
    ///
    /// Returns the [`ReapResult`] with both counts. Native sessions are left
    /// untouched.
    /// Test: `reap_dead_sessions`, `reap_keeps_native_sessions`,
    /// `reap_marks_stopped_when_pid_dead`.
    pub(super) fn reap_against(&self, live: &std::collections::HashSet<String>) -> ReapResult {
        use crate::core::session::{SessionHost, SessionStatus};

        let mut dead: Vec<SessionId> = Vec::new();
        let mut stopped_ids: Vec<SessionId> = Vec::new();
        for entry in self.sessions.iter() {
            let session = entry.value();
            if session.origin != SessionHost::Tmux {
                continue;
            }
            if !live.contains(&session.tmux_name) {
                dead.push(*entry.key());
            } else if session.status != SessionStatus::Stopped
                && let Some(pid) = session.pid
                && !crate::core::process::is_process_alive(pid)
            {
                stopped_ids.push(*entry.key());
            }
        }
        for id in &dead {
            self.remove_session(*id);
        }
        for id in &stopped_ids {
            self.update_session(id, |s| s.status = SessionStatus::Stopped);
        }
        ReapResult {
            reaped: dead.len(),
            stopped: stopped_ids.len(),
        }
    }

    /// Gather the tmux names tracked by BOTH session registries.
    ///
    /// Why: the orphan-GC's safety hinges on "absent from BOTH registries". The
    /// old-style in-memory `DaemonState` registry and the new-style
    /// `SessionManager` store each track sessions the daemon owns; the GC must
    /// union them so a session tracked by *either* is protected from reaping.
    /// What: collects every `tmux_name` from `self.sessions` into the `legacy`
    /// set and every store-known name (via
    /// [`SessionManager::known_tmux_names`](crate::session_manager::SessionManager::known_tmux_names))
    /// into the `managed` set, returning the combined
    /// [`crate::daemon::orphan_gc::TrackedNames`]. If the store read FAILS, the
    /// `managed` set cannot be trusted to be complete, so the snapshot is marked
    /// `degraded` (and `managed` left empty): the orphan-GC then skips its reap
    /// phase entirely rather than fail OPEN by treating a store-tracked session as
    /// an untracked orphan. This mirrors the GC's "tmux list error → reap nothing"
    /// safety stance for the registry-read error case.
    /// Test: `gather_tracked_names_unions_both` (happy path) and
    /// `gather_tracked_names_degraded_on_store_error`; the `orphan_gc` pure-logic
    /// tests cover the consuming `classify_session` / `run_sweep`.
    pub async fn gather_tracked_names(&self) -> crate::daemon::orphan_gc::TrackedNames {
        let legacy: std::collections::HashSet<String> = self
            .sessions
            .iter()
            .map(|e| e.value().tmux_name.clone())
            .collect();
        match self.session_manager().await.known_tmux_names().await {
            Ok(managed) => crate::daemon::orphan_gc::TrackedNames {
                legacy,
                managed,
                degraded: false,
            },
            Err(e) => {
                tracing::warn!(
                    "orphan-GC: store read for known names failed: {e}; \
                     marking tracked-names snapshot degraded (sweep will not reap)"
                );
                crate::daemon::orphan_gc::TrackedNames {
                    legacy,
                    managed: std::collections::HashSet::new(),
                    degraded: true,
                }
            }
        }
    }

    /// Gather the set of live session-id strings across BOTH registries.
    ///
    /// Why: the PID-file orphan-GC (§10.3) reaps any recorded PID whose session
    /// is no longer tracked. The PID files are keyed by session-id (a UUID
    /// string), so the sweep needs the union of every live id — from the legacy
    /// `DaemonState` registry (the launch path's `set_session_pid` records PIDs
    /// under these ids) and from the `SessionManager` store — to know which PID
    /// files are still attached to a live session and must be spared.
    /// What: collects `self.sessions` keys and every `SessionManager` record id
    /// (`mgr.list()`) into one `HashSet<String>` of lowercase-hyphenated UUIDs.
    /// The union is conservative for the PID sweep: a session tracked by EITHER
    /// registry has its PID file spared, so a still-live `claude` is never reaped.
    /// Test: `gather_live_session_ids_unions_both` in `state/tests.rs`.
    pub async fn gather_live_session_ids(&self) -> std::collections::HashSet<String> {
        let mut ids: std::collections::HashSet<String> = self
            .sessions
            .iter()
            .map(|e| e.key().0.to_string())
            .collect();
        let mgr = self.session_manager().await;
        for record in mgr.list().await {
            ids.insert(record.id.0.to_string());
        }
        ids
    }

    // ---- projects -------------------------------------------------------

    /// Register a project by its working-directory path.
    ///
    /// Why: `trusty-mpm project init` and `POST /projects` need to record a
    /// directory as a managed project so sessions can be associated with it.
    /// What: builds a [`ProjectInfo`] from `path`, inserting (or replacing) it
    /// in the registry keyed by the path; returns the stored info.
    /// Test: `register_and_list_projects`.
    pub fn register_project(&self, path: PathBuf) -> ProjectInfo {
        let info = ProjectInfo::new(path.clone());
        self.projects.write().insert(path, info.clone());
        info
    }

    /// Snapshot every registered project.
    ///
    /// Why: `trusty-mpm project list` and `GET /projects` need the full set.
    /// What: clones each [`ProjectInfo`] out from under a short read lock.
    /// Test: `register_and_list_projects`.
    pub fn list_projects(&self) -> Vec<ProjectInfo> {
        self.projects.read().values().cloned().collect()
    }

    /// Look up one registered project by its path.
    ///
    /// Why: `GET /projects/current` resolves the project for the caller's cwd.
    /// What: returns a clone of the stored [`ProjectInfo`], or `None` if the
    /// path is not registered.
    /// Test: `project_lookup_by_path`.
    pub fn project(&self, path: &std::path::Path) -> Option<ProjectInfo> {
        self.projects.read().get(path).cloned()
    }

    // ---- delegations ----------------------------------------------------

    /// Record a new (or updated) delegation.
    pub fn upsert_delegation(&self, delegation: Delegation) {
        self.delegations.insert(delegation.id.0, delegation);
    }

    /// Hold the dispatch-record lock for the duration of one find-then-insert
    /// (#5769).
    ///
    /// Why: see the `dispatch_record` field's own doc. Two writers describe one
    /// dispatch — the tracker's `matcher: "*"` hook and the guard's grant POST —
    /// and both resolve by `tool_use_id` before inserting, which a `DashMap`
    /// cannot make atomic. Exposed as a guard rather than as a closure-taking
    /// method because both takers already own their record logic and only need
    /// it serialised.
    /// What: blocks until the lock is free. `pub(crate)`, not `pub`: this is an
    /// internal invariant between two modules of this crate, not an API a
    /// consumer may hold.
    ///
    /// It is ALWAYS taken inside [`Self::claim_shared_tree_dispatch`]'s lock,
    /// never around it. Take them in the other order and the two deadlock.
    /// Test: `a_grant_and_the_tracker_converge_in_either_order`.
    pub(crate) fn dispatch_record_guard(&self) -> parking_lot::MutexGuard<'_, ()> {
        self.dispatch_record.lock()
    }

    /// Stops still waiting for the `agent_id` that names them (#4142).
    ///
    /// Why: `crate::daemon::services::delegation_tracker` is the ledger's only
    /// caller — it records on an unresolvable `SubagentStop` and consults it on
    /// the `PostToolUse` that finally teaches the id. Exposed as a borrow rather
    /// than wrapped in per-operation methods so the ledger's own invariants
    /// (bound, TTL, peek-before-clear) stay in one file.
    /// What: `pub(crate)` — internal correlation state, never a consumer API.
    /// Test: `out_of_order_subagent_stop_resolves_when_its_post_tool_use_lands`.
    pub(crate) fn pending_stops(&self) -> &super::pending_stops::PendingStops {
        &self.pending_stops
    }

    /// All delegations belonging to one session.
    pub fn delegations_for(&self, session: SessionId) -> Vec<Delegation> {
        self.delegations
            .iter()
            .filter(|e| e.value().session == session)
            .map(|e| e.value().clone())
            .collect()
    }

    /// Every tracked delegation, across every session (#4311).
    ///
    /// Why: `agent_worktree_reap::paths_in_use` asks "is any agent still
    /// working in this directory", and a session boundary is not an answer to
    /// that — an agent dispatched from another session is exactly as live, and
    /// its worktree exactly as un-deletable. Filtering by session there let a
    /// reap approve a path a sibling still held.
    /// Test: `reap_spares_a_worktree_another_sessions_agent_still_holds`.
    pub fn all_delegations(&self) -> Vec<Delegation> {
        self.delegations.iter().map(|e| e.value().clone()).collect()
    }

    /// Agents of every live delegation writing into `cwd` without a working
    /// tree of their own (#4480, widened across sessions by ADR-0048).
    ///
    /// Why: this is the exact population a new file-mutating dispatch would be
    /// joining. Answering it here — over the tracker's own records, which are
    /// the only place a delegation's liveness is resolved from real
    /// `SubagentStop` signals — is what keeps the guard from having to
    /// re-derive liveness from a timer and guess.
    ///
    /// **It deliberately does not filter by session.** It used to, and that
    /// made it blind to the only shape that has actually caused harm: three
    /// sessions standing in ONE checkout, each seeing an empty answer because
    /// the other two writers belonged to a different session id. The hazard is
    /// a shared git HEAD, and a HEAD is a property of the DIRECTORY — two
    /// agents in one directory collide whether or not the same PM dispatched
    /// them, and two agents in different directories never collide however
    /// closely related their sessions are. The session was never the right
    /// key; the daemon holds every session's delegations and this is the one
    /// place that fact is usable.
    /// What: every delegation that is [`DelegationStatus::is_live`], whose
    /// `cwd` equals `cwd`, whose agent
    /// [`shares_the_callers_tree`](crate::core::dispatch_isolation::shares_the_callers_tree),
    /// and whose `tool_use_id` is not `exclude_tool_use_id`; returned as agent
    /// names for the deny message.
    ///
    /// `exclude_tool_use_id` is load-bearing, not a convenience. The daemon's
    /// `matcher: "*"` `PreToolUse` hook and `tm hook --pm-guard` fire on the
    /// SAME dispatch and race; if the tracker records the dispatch first, the
    /// caller would find ITSELF here and deny the very first dispatch of a
    /// session. Excluding the caller's own `tool_use_id` makes the answer
    /// independent of that ordering.
    ///
    /// A delegation with no recorded `cwd` is skipped rather than assumed to
    /// share one: it is indeterminate, and this whole guard fails toward ALLOW.
    /// `Stale` is deliberately not live — a record tracking has given up on
    /// must not block a dispatch for the remaining hours of its retention.
    /// Test: `shared_tree_dispatch_route_reports_live_unisolated_writers` and
    /// siblings in [`crate::daemon::delegation_routes`], which drive every
    /// filter here through the route that is this method's only caller.
    pub fn live_shared_tree_writers(
        &self,
        cwd: &std::path::Path,
        exclude_tool_use_id: Option<&str>,
    ) -> Vec<String> {
        self.delegations
            .iter()
            .filter(|e| {
                let d = e.value();
                d.status.is_live()
                    && d.cwd.as_deref() == Some(cwd)
                    && !(exclude_tool_use_id.is_some()
                        && d.tool_use_id.as_deref() == exclude_tool_use_id)
                    && crate::core::dispatch_isolation::shares_the_callers_tree(
                        &d.agent,
                        d.isolation.as_deref(),
                    )
            })
            .map(|e| e.value().agent.clone())
            .collect()
    }

    /// Answer [`Self::live_shared_tree_writers`] and claim the tree in one step
    /// (#5324).
    ///
    /// Why: [`Self::live_shared_tree_writers`] alone is a question, and
    /// `tm hook --pm-guard` used to act on the answer without anything having
    /// claimed the directory in between. Two dispatches issued in one PM turn
    /// could both ask before either was recorded, both see an empty set, and
    /// both be admitted. Asking and claiming have to be indivisible, and a
    /// `DashMap` makes each entry atomic but never a scan-then-insert pair — so
    /// [`shared_tree_claim`](Self::shared_tree_claim) is held across both here.
    ///
    /// What: under that mutex, computes the live-writer answer and, when it is
    /// EMPTY and `eligible` says this dispatch would itself share the tree, runs
    /// `record` before releasing. Returns the answer the caller decides on plus
    /// whether the claim was taken. A second caller arriving concurrently blocks
    /// until the first has recorded, so it sees a non-empty answer and is denied
    /// — exactly one of two simultaneous dispatches is admitted.
    ///
    /// `record` is a closure rather than an inlined write so this method keeps
    /// no opinion about what a delegation record looks like: the only caller
    /// passes the delegation tracker's own `PreToolUse` observer, so the claim
    /// IS the record that tracker would have written milliseconds later, with
    /// the same lifecycle, the same liveness, and the same staleness sweep. No
    /// second kind of state and no new expiry are introduced.
    ///
    /// `record` must not take THIS lock again — it is not reentrant — and must
    /// not await. It MAY take [`Self::dispatch_record_guard`], and both of the
    /// closures passed today do: that is the documented lock order, and taking
    /// the two in the other order is what would deadlock. It must not block on
    /// anything else.
    ///
    /// It takes no session (ADR-0048). [`Self::live_shared_tree_writers`] spans
    /// every session, so a guard in one session now sees a writer another
    /// session put in the same directory, and the caller's own `record` closure
    /// carries whichever session the claim is written under. One mutex still
    /// serialises every caller, because the directory it protects is a property
    /// of this state rather than of a session.
    /// Test: `shared_tree_dispatch_route_denies_the_second_claim`,
    /// `shared_tree_writers_span_sessions_in_one_checkout`,
    /// `shared_tree_dispatch_route_reserves_the_tree_on_an_empty_answer`,
    /// `shared_tree_dispatch_route_does_not_reserve_when_it_denies`.
    pub fn claim_shared_tree_dispatch<F: FnOnce(&Self)>(
        &self,
        cwd: &std::path::Path,
        exclude_tool_use_id: Option<&str>,
        eligible: bool,
        record: F,
    ) -> (Vec<String>, bool) {
        let _claim = self.shared_tree_claim.lock();
        let live = self.live_shared_tree_writers(cwd, exclude_tool_use_id);
        let claimed = eligible && live.is_empty();
        if claimed {
            record(self);
        }
        (live, claimed)
    }

    /// Find one session's delegation matching `pred`, returning its id (#2864).
    ///
    /// Why: the hook delegation tracker resolves a record by correlation key
    /// (`tool_use_id` or `agent_id`) and then mutates it. Returning the id
    /// rather than the record keeps the scan's read guards from overlapping the
    /// subsequent write, which would deadlock a `DashMap` shard.
    /// What: scans this session's delegations and returns the first match's
    /// [`DelegationId`], or `None`. The session filter is what stops one
    /// session's `SubagentStop` from resolving another's child.
    /// Test: `daemon::services::delegation_tracker` suite, in particular
    /// `delegations_are_scoped_per_session`.
    pub fn find_delegation(
        &self,
        session: SessionId,
        pred: impl Fn(&Delegation) -> bool,
    ) -> Option<DelegationId> {
        self.delegations
            .iter()
            .find(|e| e.value().session == session && pred(e.value()))
            .map(|e| e.value().id)
    }

    /// The most recently created delegation of one session matching `pred`.
    ///
    /// Why: [`Self::delegations_for`] clones every matching record — including
    /// its `String`s and `PathBuf`s — which is pure waste when the caller only
    /// wants an id to mutate. The delegation-tracker dedup runs this on the
    /// dispatch path, so the allocations were per-dispatch and proportional to
    /// the whole map. Returning a `Copy` id also keeps the scan's read guards
    /// from overlapping the subsequent write, exactly as [`Self::find_delegation`]
    /// does.
    /// What: scans this session's delegations, keeps those satisfying `pred`,
    /// and returns the id of the one with the greatest `created_at`.
    /// Test: `dedups_declaration_and_observation`, `dedup_window_expires`.
    pub fn latest_delegation_matching(
        &self,
        session: SessionId,
        pred: impl Fn(&Delegation) -> bool,
    ) -> Option<DelegationId> {
        self.delegations
            .iter()
            .filter(|e| e.value().session == session && pred(e.value()))
            .max_by_key(|e| e.value().created_at)
            .map(|e| e.value().id)
    }

    /// Bound delegation liveness and delegation-map growth (#2864 review).
    ///
    /// Why: `SubagentStop` is the only signal that closes a `Running`
    /// delegation and it is not guaranteed to arrive (dropped hook POST,
    /// interrupted subagent, a dispatch that never learned an `agent_id`).
    /// Before this sweep such a record was immortal: it counted as live in
    /// [`crate::daemon::idle_nudge::has_live_children`] forever, suppressing
    /// that session's idle nudge for the daemon's lifetime, and it was never
    /// removed from the map, which therefore grew without bound while every
    /// dispatch paid an O(N) scan over it. Both are the same root cause — a
    /// delegation with no route out — so one sweep closes both.
    ///
    /// What: one pass over the map, off the `PreToolUse` hot path (it is driven
    /// by the 60 s reap loop, never by a hook). A record is *aged* from
    /// `started_at` when known, else `created_at`. Exactly three dispositions,
    /// and eviction is stated as a guarantee the code actually keeps:
    ///
    /// | status | staling | eviction |
    /// |---|---|---|
    /// | `Queued`/`Running` (live) | past [`DECLARED_STALE_AFTER_SECS`] for an undispatched `McpDeclared` declaration, else [`RUNNING_STALE_AFTER_SECS`] → [`DelegationStatus::Stale`] | **never, at any age** |
    /// | `Stale` | — | aged past [`STALE_RETENTION_SECS`] |
    /// | terminal | — | [`DELEGATION_RETENTION_SECS`] after `ended_at` |
    ///
    /// The sweep writes `Stale` and **never** `Completed`: it records that
    /// tracking gave up, not that the agent finished. It deliberately does NOT
    /// stamp `ended_at`, which keeps that field meaning exactly *"reached a
    /// terminal status"* and keeps `Stale` off the terminal retention clock.
    ///
    /// So the precise safety property — the one a reader will rely on — is:
    /// **a record is evicted only once nothing can still resolve it.** A live
    /// record is never evicted at any age. A `Stale` record stays present, and
    /// stays resolvable by a late `SubagentStop` (which matches on
    /// `!is_terminal()`), for `STALE_RETENTION_SECS` minus its staling budget —
    /// **18 hours** with the current constants — after tracking gave up. A
    /// terminal record can be resolved by nothing, so its shorter window costs
    /// nothing. Past 24 h a `Stale` record IS dropped and a later stop finds
    /// nothing: the recovery window is long, not infinite, because the map must
    /// stay bounded. That bound is asserted, not incidental.
    /// It also ages the #4142 deferred-stop ledger on the same pass, for the
    /// same reason: both are bounded off the hook path, never on it.
    /// Test: `stale_running_delegation_stops_suppressing_the_nudge`,
    /// `declared_but_never_dispatched_goes_stale_quickly`,
    /// `terminal_delegations_are_evicted_after_retention`,
    /// `live_delegations_are_never_evicted`,
    /// `stale_delegation_stays_resolvable_far_past_the_terminal_window`,
    /// `a_stale_delegation_is_eventually_evicted`,
    /// `the_delegation_sweep_ages_the_deferred_stop_ledger`.
    pub fn sweep_delegations(&self) -> DelegationSweep {
        self.sweep_delegations_at(chrono::Utc::now())
    }

    /// [`Self::sweep_delegations`] with an injected clock.
    ///
    /// Why: the budgets are hours long, so the tests must be able to name "now"
    /// rather than sleep or backdate every field of every fixture.
    /// Test: as [`Self::sweep_delegations`].
    pub(crate) fn sweep_delegations_at(
        &self,
        now: chrono::DateTime<chrono::Utc>,
    ) -> DelegationSweep {
        let mut sweep = DelegationSweep::default();
        // #4142: the deferred-stop ledger ages on the same loop, and off the
        // hook path for the same reason. An expired entry costs only the
        // recovery — the delegation it would have closed is staled below.
        let expired = self.pending_stops.prune_at(now);
        if expired > 0 {
            tracing::debug!(expired, "delegation: pruned expired deferred stops (#4142)");
        }
        self.delegations.retain(|_, d| {
            let age_from = d.started_at.unwrap_or(d.created_at);
            if d.status.is_live() {
                let budget = if d.status == DelegationStatus::Queued
                    && d.source == DelegationSource::McpDeclared
                {
                    DECLARED_STALE_AFTER_SECS
                } else {
                    RUNNING_STALE_AFTER_SECS
                };
                if now - age_from > chrono::Duration::seconds(budget) {
                    // Status only. Stamping `ended_at` here would both overload
                    // that field's meaning and put a still-possibly-live record
                    // on the terminal retention clock, silently expiring the
                    // recovery guarantee `Stale` exists to provide.
                    d.status = DelegationStatus::Stale;
                    sweep.staled += 1;
                }
                // A live record is never evicted, at any age.
                return true;
            }
            let keep = if d.status == DelegationStatus::Stale {
                // May still be resolvable by a late `SubagentStop`: held far
                // longer, on its own age clock.
                now - age_from <= chrono::Duration::seconds(STALE_RETENTION_SECS)
            } else {
                // Terminal: nothing can resolve it, so nothing is lost.
                now - d.ended_at.unwrap_or(d.created_at)
                    <= chrono::Duration::seconds(DELEGATION_RETENTION_SECS)
            };
            if !keep {
                sweep.evicted += 1;
            }
            keep
        });
        sweep
    }

    /// Apply `f` to one delegation in place (#2864).
    ///
    /// Why: hook correlation updates a few fields of an existing record
    /// (status, `agent_id`, tier, timestamps); a read-clone-reinsert cycle would
    /// race a concurrent update from another hook event.
    /// What: takes the entry's write guard and runs `f`. Returns `false` when no
    /// such delegation exists. `f` must not touch the delegation store — it runs
    /// under a shard lock.
    /// Test: `daemon::services::delegation_tracker` suite.
    pub fn mutate_delegation(&self, id: DelegationId, f: impl FnOnce(&mut Delegation)) -> bool {
        match self.delegations.get_mut(&id.0) {
            Some(mut entry) => {
                f(entry.value_mut());
                true
            }
            None => false,
        }
    }

    /// Move one delegation to a terminal status, stamping `ended_at` (#2864).
    ///
    /// Why: before #2864 nothing ever left `Queued`, so
    /// [`has_live_children`](crate::daemon::idle_nudge::has_live_children)
    /// reported every delegation as live forever and permanently suppressed the
    /// idle nudge. This is the mutator that closes a delegation out.
    /// What: sets `status` and stamps `ended_at` with the current time. Returns
    /// `false` when no such delegation exists. Callers are responsible for only
    /// passing a terminal [`DelegationStatus`].
    /// Test: `subagent_stop_completes_matching_delegation`,
    /// `concurrent_delegations_terminalize_independently`.
    pub fn terminate_delegation(&self, id: DelegationId, status: DelegationStatus) -> bool {
        self.mutate_delegation(id, |d| {
            d.status = status;
            d.ended_at = Some(chrono::Utc::now());
        })
    }

    /// Reap managed sessions whose tmux session has disappeared.
    ///
    /// Why (#1744): the 60-second reap loop only handled legacy in-memory
    /// sessions. Managed sessions that exit ungracefully (terminal kill, tmux pane
    /// close) would stay `Active` in the store for up to 60 seconds. Running this
    /// in the reap loop transitions them to `Stopped` as soon as their tmux session
    /// disappears (detected via `driver.list_sessions()`). The `SessionEnd` hook
    /// handles the common case immediately; this is the safety net for the rare
    /// cases where the hook did not fire (daemon restart race, hook misconfiguration).
    /// What: calls `driver.list_sessions()` to build a live-name set, then
    /// delegates to [`Self::reap_managed_against`]. On driver error logs a warning
    /// and returns without touching the store (fail-safe: better to leave a stale
    /// Active record than to incorrectly mark a running session Stopped).
    /// Test: `reap_dead_managed_sessions_marks_stopped` in `super::tests` via
    /// `reap_managed_against`.
    pub async fn reap_dead_managed_sessions(&self, driver: &TmuxDriver) {
        let live: std::collections::HashSet<String> = match driver.list_sessions() {
            Ok(s) => s.into_iter().map(|s| s.name).collect(),
            Err(e) => {
                tracing::warn!("managed reap skipped — tmux list-sessions failed: {e}");
                return;
            }
        };
        self.reap_managed_against(&live).await;
    }

    /// Core of the managed-session reaper: stop every Active session not in `live`.
    ///
    /// Why: extracted from [`Self::reap_dead_managed_sessions`] so tests can
    /// exercise the stop logic with a pre-built live set without requiring a real
    /// `TmuxDriver` or tmux binary. The outer function handles the tmux call;
    /// this method is the deterministic, testable heart.
    /// What: for every Active managed session whose `tmux_name` is absent from
    /// `live`, calls `SessionManager::stop_with_cause` (best-effort kill + mark
    /// Stopped). Failures are logged and swallowed so the caller's reap loop
    /// continues.
    ///
    /// #6194 — WHICH cause, and why the empty set is its own case. An EMPTY
    /// `live` means no tmux session of any kind exists on this host, because
    /// `TmuxDriver::list_sessions` maps "no server running" to `Ok(vec![])`.
    /// That is a whole-server loss — `tmux kill-server`, a crash, an upgrade, a
    /// logout — and it is not attributable to anyone's decision about any
    /// individual session, so those records are stopped as
    /// [`StopCause::Unexpected`] and stay auto-resumable, which is how the
    /// supervisor recovered such a fleet before #6194. A NON-EMPTY `live` means
    /// the tmux server is up and other sessions are running, so a record whose
    /// own name is missing was killed on purpose:
    /// [`StopCause::Deliberate`], and no automatic path revives it. The
    /// one-session host is genuinely ambiguous — tmux exits its server when the
    /// last session dies, so killing the only session and killing the server
    /// produce the identical observation — and it is resolved toward restoring
    /// the fleet, the more recoverable of the two mistakes (`tm session stop`
    /// still records Deliberate on that host and still sticks).
    ///
    /// Test: `reap_dead_managed_sessions_marks_stopped`,
    /// `reap_marks_a_targeted_kill_deliberate`,
    /// `reap_leaves_a_whole_server_loss_auto_resumable` in `super::tests`.
    pub(crate) async fn reap_managed_against(&self, live: &std::collections::HashSet<String>) {
        let mgr = self.session_manager().await;
        let records = mgr.list().await;
        // #6194: an empty live set is a lost tmux SERVER, not N killed sessions.
        let cause = if live.is_empty() {
            crate::session_manager::StopCause::Unexpected
        } else {
            crate::session_manager::StopCause::Deliberate
        };
        for r in records {
            if matches!(r.state, crate::session_manager::ManagedSessionState::Active)
                && !live.contains(&r.tmux_name)
            {
                match mgr.stop_with_cause(&r.id, cause).await {
                    Ok(_) => tracing::info!(
                        id = %r.id,
                        name = %r.tmux_name,
                        cause = ?cause,
                        "reaper: marked managed session Stopped (tmux gone, #1744)"
                    ),
                    Err(e) => tracing::warn!(
                        id = %r.id,
                        name = %r.tmux_name,
                        "reaper: failed to mark managed session Stopped: {e}"
                    ),
                }
            }
        }
    }
}
