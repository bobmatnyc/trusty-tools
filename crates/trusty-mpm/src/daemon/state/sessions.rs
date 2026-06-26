//! Session, project, and delegation methods on [`DaemonState`].
//!
//! Why: the session-registry operations are a cohesive group used by many
//! handlers but unrelated to resource-management (breakers, memory, overseer);
//! splitting them keeps each file focused and under the SLOC cap.
//! What: session CRUD, project CRUD, delegation upsert/query, and the
//! dead-session reaper.
//! Test: see `super::tests`.

use std::path::PathBuf;

use crate::core::agent::Delegation;
use crate::core::project::ProjectInfo;
use crate::core::session::{Session, SessionId};

use super::core::PAIR_CODE_TTL;
use super::core::{DaemonState, ReapResult};
use crate::daemon::tmux::TmuxDriver;

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
    /// UUID or the friendly `tmpm-<adj>-<noun>` name the daemon prints on
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

    /// All delegations belonging to one session.
    pub fn delegations_for(&self, session: SessionId) -> Vec<Delegation> {
        self.delegations
            .iter()
            .filter(|e| e.value().session == session)
            .map(|e| e.value().clone())
            .collect()
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
    /// What: lists live tmux session names via `driver`; for every Active managed
    /// session whose `tmux_name` is absent from the live set, calls
    /// `SessionManager::stop` (best-effort kill + mark Stopped). Failures are
    /// logged and silently ignored so the caller's reap loop continues unaffected.
    /// Test: `reap_dead_managed_sessions_marks_stopped` in `super::tests`.
    pub async fn reap_dead_managed_sessions(&self, driver: &TmuxDriver) {
        let live: std::collections::HashSet<String> = match driver.list_sessions() {
            Ok(s) => s.into_iter().map(|s| s.name).collect(),
            Err(e) => {
                tracing::warn!("managed reap skipped — tmux list-sessions failed: {e}");
                return;
            }
        };
        let mgr = self.session_manager().await;
        let records = mgr.list().await;
        for r in records {
            if matches!(r.state, crate::session_manager::ManagedSessionState::Active)
                && !live.contains(&r.tmux_name)
            {
                match mgr.stop(&r.id).await {
                    Ok(_) => tracing::info!(
                        id = %r.id,
                        name = %r.tmux_name,
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
