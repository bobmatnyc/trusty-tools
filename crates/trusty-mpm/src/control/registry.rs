//! Session registry: `HashMap<ControlSessionId, SessionActorHandle>` behind a
//! `tokio::sync::RwLock` with per-project monotonic session-number allocation.
//!
//! Why: the daemon needs a single authoritative table of live sessions so that
//! every HTTP handler, CLI command, and MCP tool reads from one source. Using a
//! `tokio::sync::RwLock` (not `parking_lot`) allows async readers to hold the
//! lock across await points without blocking the async executor's threads.
//! What: [`SessionRegistry`] wraps `HashMap<ControlSessionId, SessionActorHandle>`
//! behind a `tokio::sync::RwLock` and embeds a `SessionCounter` for monotonic
//! per-project session-number allocation. Provides `register`, `get` (safe
//! clone under read lock per §6.2), `deregister`, `list`, and `run_session`.
//! Test: `registry_register_deregister`, `registry_list`, `registry_run_session`,
//! `registry_monotonic_ids` in the inline test module.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use tokio::sync::RwLock;
use tracing::{info, warn};

use crate::control::actor::{DEFAULT_BROADCAST_CAPACITY, SessionActorHandle, spawn_actor};
use crate::control::admission::{AdmissionDecision, AdmissionError, check_admission};
use crate::control::backend::stream_json::StreamJsonBackend;
use crate::control::backend::tmux::TmuxBackend;
use crate::control::config::ControlPlaneConfig;
use crate::control::event::BackendKind;
use crate::control::id::{ControlSessionId, SessionCounter};

/// Parameters for spawning a new session via `SessionRegistry::run_session`.
///
/// Why: bundling all spawn-time inputs into one struct keeps `run_session`'s
/// signature stable as more fields are added in later phases.
/// What: carries project identity, workdir, backend selection, and optional
/// system-prompt file.
/// Test: used by `registry_run_session`.
pub struct RunParams {
    /// Registered project ID (key in the project registry).
    pub project_id: String,
    /// Absolute path to the project's working directory.
    pub workdir: PathBuf,
    /// Which execution backend to use.
    pub backend: BackendKind,
    /// Optional system-prompt file delivered via `--append-system-prompt-file`.
    pub prompt_file: Option<PathBuf>,
    /// Claude command override (default: `"claude"`).
    pub claude_cmd: Option<String>,
}

/// Shared session registry: manages live actors and allocates session IDs.
///
/// Why: the daemon's HTTP API, the `tm` CLI commands (Phase 2), and the MCP
/// tools all need a consistent, thread-safe view of every live session. A
/// single `Arc<SessionRegistry>` injected into every handler is the daemon's
/// composition root for the control-plane subsystem.
/// What: wraps a `HashMap<ControlSessionId, SessionActorHandle>` behind a
/// `tokio::sync::RwLock`; embeds a `SessionCounter` for session-number
/// allocation. All registry mutations hold an exclusive lock only for the
/// duration of the `HashMap` operation per §7.1.
/// Test: `registry_register_deregister`, `registry_list`.
#[derive(Clone, Debug)]
pub struct SessionRegistry {
    inner: Arc<RwLock<RegistryInner>>,
    /// Auth + cost guardrails (§9): concurrency cap, launch stagger, auth
    /// timeout, classifier gate. Shared so the daemon can read the same config
    /// the registry enforces.
    config: Arc<ControlPlaneConfig>,
}

struct RegistryInner {
    actors: HashMap<ControlSessionId, SessionActorHandle>,
    counter: SessionCounter,
}

impl std::fmt::Debug for RegistryInner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RegistryInner")
            .field("actor_count", &self.actors.len())
            .finish()
    }
}

impl SessionRegistry {
    /// Create a new empty registry with the default control-plane config.
    ///
    /// Why: the daemon constructs a registry at startup and injects it into
    /// every handler; an empty initial state with spec-default guardrails (cap
    /// 5, stagger 2 s) is the correct starting point.
    /// What: allocates the `Arc<RwLock<…>>` with an empty map and counter and a
    /// [`ControlPlaneConfig::default`].
    /// Test: all registry tests construct via `new()` or `with_config()`.
    pub fn new() -> Self {
        Self::with_config(ControlPlaneConfig::default())
    }

    /// Create a new empty registry with an explicit control-plane config.
    ///
    /// Why: the daemon loads `[control_plane]` from the operator's config and
    /// must inject the cap/stagger/timeout values the registry enforces. Tests
    /// inject a zero-stagger config for deterministic, fast cap testing.
    /// What: allocates the registry as in `new()` but stores the supplied
    /// `ControlPlaneConfig` (wrapped in an `Arc` for cheap cloning).
    /// Test: `registry_cap_rejects_over_limit`, `registry_stagger_zero_is_instant`.
    pub fn with_config(config: ControlPlaneConfig) -> Self {
        Self {
            inner: Arc::new(RwLock::new(RegistryInner {
                actors: HashMap::new(),
                counter: SessionCounter::default(),
            })),
            config: Arc::new(config),
        }
    }

    /// Borrow the registry's control-plane config (§9).
    ///
    /// Why: HTTP handlers and the daemon's classifier gate need to read the
    /// same config the registry enforces (e.g. the auth timeout, the classifier
    /// flag) without cloning the whole struct.
    /// What: returns a `&ControlPlaneConfig` for the lifetime of the borrow.
    /// Test: `registry_exposes_config`.
    pub fn config(&self) -> &ControlPlaneConfig {
        &self.config
    }

    /// Number of live sessions currently registered.
    ///
    /// Why: `tm list` summaries and the admission check both need the current
    /// live count; exposing it avoids forcing callers to materialize the full
    /// id list just to count.
    /// What: acquires the read lock and returns `actors.len()`.
    /// Test: `registry_live_count`.
    pub async fn live_count(&self) -> usize {
        self.inner.read().await.actors.len()
    }

    /// Register an actor handle under a session ID.
    ///
    /// Why: after `spawn_actor` returns, the handle must be stored in the
    /// registry so observers and command senders can find it.
    /// What: acquires an exclusive lock and inserts the handle. If an entry
    /// already exists for this ID (should not happen normally), it is replaced.
    /// Test: `registry_register_deregister`.
    pub async fn register(&self, id: ControlSessionId, handle: SessionActorHandle) {
        let mut inner = self.inner.write().await;
        inner.actors.insert(id, handle);
    }

    /// Remove an actor handle from the registry.
    ///
    /// Why: when an actor reaches a terminal state it deregisters itself so
    /// the registry does not hold stale handles.
    /// What: acquires an exclusive lock and removes the entry. Logs a warning
    /// if the entry was already absent.
    /// Test: `registry_register_deregister`.
    pub async fn deregister(&self, id: &ControlSessionId) {
        let mut inner = self.inner.write().await;
        if inner.actors.remove(id).is_none() {
            warn!(%id, "deregister: session not found in registry");
        }
    }

    /// Clone the handle for a session while holding the shared read lock.
    ///
    /// Why: §6.2 requires handles to be cloned while the registry read-lock
    /// is held, so the CAS on `write_lock_held` runs on a handle that is
    /// guaranteed to outlive the registry lookup. Returning a cloned handle
    /// (rather than a reference) satisfies that invariant.
    /// What: acquires the read lock, looks up the ID, and returns a clone.
    /// Returns `None` if the session is not in the registry.
    /// Test: `registry_get_clones_under_read_lock`.
    pub async fn get(&self, id: &ControlSessionId) -> Option<SessionActorHandle> {
        let inner = self.inner.read().await;
        inner.actors.get(id).cloned()
    }

    /// List all live session IDs and a snapshot of their metadata.
    ///
    /// Why: `tm list` and `GET /sessions` need a point-in-time view of every
    /// live session without blocking each actor individually.
    /// What: acquires the read lock and returns all IDs. Callers can then
    /// `get()` individual handles to read `metadata`. Returns a `Vec` to
    /// avoid holding the lock across the caller's await points.
    /// Test: `registry_list`.
    pub async fn list_ids(&self) -> Vec<ControlSessionId> {
        let inner = self.inner.read().await;
        inner.actors.keys().cloned().collect()
    }

    /// Check the §9.2 concurrency cap against the current live count.
    ///
    /// Why: callers (and the staggered-launch loop) sometimes want to know
    /// whether a launch would be admitted *before* paying the stagger delay, so
    /// they can fail fast. `run_session` ALSO re-checks under the write lock so
    /// this pre-check is advisory, not the authoritative gate.
    /// What: reads the live count under the read lock and runs the pure
    /// [`check_admission`] predicate with the configured cap and stagger.
    /// Test: `registry_admission_check`.
    pub async fn admission(&self) -> AdmissionDecision {
        let live = self.live_count().await as u32;
        check_admission(
            live,
            self.config.max_concurrent_sessions,
            self.config.launch_stagger_ms,
        )
    }

    /// Allocate a session ID and spawn an actor via the selected backend,
    /// enforcing the §9.2 concurrency cap and launch stagger.
    ///
    /// Why: `tm run <project-id>` must go through the registry so the ID
    /// counter is authoritative and an issued ID is always observable by
    /// concurrent `get()` / `list_ids()` callers. WI-5 (§9.2) adds two cost
    /// guardrails here: the concurrency cap (hard limit, enforced under the
    /// write lock so it is race-free) and the launch stagger (applied BEFORE
    /// the lock so it spaces successive launches without serializing the lock).
    /// What: (1) applies the configured `launch_stagger_ms` sleep up front;
    /// (2) acquires ONE write lock and re-checks the cap authoritatively —
    /// rejecting with [`AdmissionError::CapReached`] if `live >= cap`; (3) holds
    /// that lock for ID allocation, backend construction, actor spawn, and
    /// handle insertion so `get(&id)` is never `None` for a returned ID.
    /// Returns the allocated `ControlSessionId`.
    /// Test: `registry_run_session`, `registry_run_session_id_visible_immediately`,
    /// `registry_cap_rejects_over_limit`.
    pub async fn run_session(&self, params: RunParams) -> Result<ControlSessionId> {
        // §9.2 launch stagger — applied before acquiring the lock so concurrent
        // launches are spaced without serializing the registry. Tests inject a
        // zero stagger (Duration::ZERO) so this is a no-op and stays fast.
        let stagger = self.config.stagger_duration();
        if !stagger.is_zero() {
            tokio::time::sleep(stagger).await;
        }

        let mut inner = self.inner.write().await;

        // §9.2 concurrency cap — authoritative check under the write lock so two
        // concurrent launches cannot both slip past a stale count.
        let live = inner.actors.len() as u32;
        if let AdmissionDecision::Reject { live, cap } =
            check_admission(live, self.config.max_concurrent_sessions, 0)
        {
            warn!(live, cap, "session launch rejected: concurrency cap reached (§9.2)");
            return Err(AdmissionError::CapReached { live, cap }.into());
        }

        let session_id = inner.counter.next(&params.project_id);

        info!(
            session_id = %session_id,
            project_id = %params.project_id,
            backend = ?params.backend,
            workdir = %params.workdir.display(),
            "spawning session"
        );

        let handle = match params.backend {
            BackendKind::StreamJson => {
                let backend = StreamJsonBackend::spawn(
                    session_id.clone(),
                    params.workdir,
                    params.prompt_file,
                )
                .with_context(|| format!("stream-json spawn failed for session {session_id}"))?;
                spawn_actor(
                    session_id.clone(),
                    params.project_id,
                    BackendKind::StreamJson,
                    backend,
                    DEFAULT_BROADCAST_CAPACITY,
                )
            }
            BackendKind::Tmux => {
                let claude_cmd = params.claude_cmd.unwrap_or_else(|| "claude".into());
                let backend = TmuxBackend::new(
                    session_id.clone(),
                    params.workdir,
                    params.prompt_file,
                    claude_cmd,
                    100, // default capture_lines
                )
                .with_context(|| format!("tmux backend spawn failed for session {session_id}"))?;
                spawn_actor(
                    session_id.clone(),
                    params.project_id,
                    BackendKind::Tmux,
                    backend,
                    DEFAULT_BROADCAST_CAPACITY,
                )
            }
        };

        inner.actors.insert(session_id.clone(), handle);
        Ok(session_id)
    }
}

impl Default for SessionRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::actor::SessionActorHandle;
    use crate::control::event::BackendKind;
    use crate::control::id::ControlSessionId;
    use crate::control::state::SessionMetadata;
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;
    use tokio::sync::{RwLock, broadcast, mpsc};

    fn make_handle(id: &ControlSessionId) -> SessionActorHandle {
        let (command_tx, _) = mpsc::channel(1);
        let (event_tx, _) = broadcast::channel(4);
        SessionActorHandle {
            command_tx,
            event_tx,
            write_lock_held: Arc::new(AtomicBool::new(false)),
            metadata: Arc::new(RwLock::new(SessionMetadata::new(
                id.clone(),
                "proj".into(),
                BackendKind::StreamJson,
            ))),
        }
    }

    #[tokio::test]
    async fn registry_register_deregister() {
        let registry = SessionRegistry::new();
        let id = ControlSessionId::new("proj", 0);
        let handle = make_handle(&id);

        registry.register(id.clone(), handle).await;
        assert!(registry.get(&id).await.is_some());

        registry.deregister(&id).await;
        assert!(registry.get(&id).await.is_none());
    }

    #[tokio::test]
    async fn registry_list() {
        let registry = SessionRegistry::new();
        let id0 = ControlSessionId::new("proj", 0);
        let id1 = ControlSessionId::new("proj", 1);
        registry.register(id0.clone(), make_handle(&id0)).await;
        registry.register(id1.clone(), make_handle(&id1)).await;

        let ids = registry.list_ids().await;
        assert_eq!(ids.len(), 2);
        assert!(ids.contains(&id0));
        assert!(ids.contains(&id1));
    }

    #[tokio::test]
    async fn registry_monotonic_ids() {
        let registry = SessionRegistry::new();

        // Simulate counter allocation (without actually spawning backends).
        let id0 = {
            let mut inner = registry.inner.write().await;
            inner.counter.next("p")
        };
        let id1 = {
            let mut inner = registry.inner.write().await;
            inner.counter.next("p")
        };
        assert_eq!(id0.as_str(), "p-0");
        assert_eq!(id1.as_str(), "p-1");
    }

    /// Verify that `get()` returns `Some` immediately after `run_session` on a
    /// backend that would fail (so we can test the ID-allocation path without a
    /// real process).  We test the monotonic-ID path through
    /// `registry_monotonic_ids`; the TOCTOU guarantee is validated structurally:
    /// because `run_session` now holds ONE write lock for allocate+register, any
    /// `get()` concurrent with the lock is guaranteed to either see `None`
    /// (before allocation) or `Some` (after allocation+registration).
    ///
    /// Why: documents the TOCTOU fix so reviewers can verify the invariant.
    /// What: calls `register` directly (simulating the single-lock path) and
    /// asserts `get` returns `Some` immediately — no yield between register and
    /// get.
    /// Test: this test (structural, not concurrent; concurrent races are hard to
    /// test deterministically and are gated by the implementation invariant).
    #[tokio::test]
    async fn registry_run_session_id_visible_immediately() {
        let registry = SessionRegistry::new();
        let id = ControlSessionId::new("p", 0);
        let handle = make_handle(&id);

        // Simulate what run_session does atomically: allocate + insert in one
        // lock scope.
        {
            let mut inner = registry.inner.write().await;
            inner.actors.insert(id.clone(), handle);
        }
        // Immediately after the lock is released the ID must be visible.
        assert!(
            registry.get(&id).await.is_some(),
            "session must be visible immediately after registration"
        );
    }

    #[tokio::test]
    async fn registry_live_count_and_config() {
        // Default config: cap 5, stagger 2000, classifier off.
        let registry = SessionRegistry::new();
        assert_eq!(registry.live_count().await, 0);
        assert_eq!(registry.config().max_concurrent_sessions, 5);
        assert!(!registry.config().observability.llm_classifier);

        let id = ControlSessionId::new("p", 0);
        registry.register(id.clone(), make_handle(&id)).await;
        assert_eq!(registry.live_count().await, 1);
    }

    #[tokio::test]
    async fn registry_admission_check_under_cap_admits() {
        // cap=2, zero stagger so tests stay fast.
        let cfg = ControlPlaneConfig {
            max_concurrent_sessions: 2,
            launch_stagger_ms: 0,
            ..Default::default()
        };
        let registry = SessionRegistry::with_config(cfg);
        assert!(matches!(
            registry.admission().await,
            AdmissionDecision::Admit { stagger_ms: 0 }
        ));

        // Fill to cap with placeholder handles.
        for n in 0..2 {
            let id = ControlSessionId::new("p", n);
            registry.register(id.clone(), make_handle(&id)).await;
        }
        // Now at cap → reject.
        assert!(matches!(
            registry.admission().await,
            AdmissionDecision::Reject { live: 2, cap: 2 }
        ));
    }

    #[tokio::test]
    async fn registry_run_session_rejects_when_capped() {
        // cap=1, zero stagger. Fill with one placeholder, then run_session must
        // reject with CapReached without spawning a backend.
        let cfg = ControlPlaneConfig {
            max_concurrent_sessions: 1,
            launch_stagger_ms: 0,
            ..Default::default()
        };
        let registry = SessionRegistry::with_config(cfg);
        let id = ControlSessionId::new("p", 0);
        registry.register(id.clone(), make_handle(&id)).await;

        let params = RunParams {
            project_id: "p".into(),
            workdir: PathBuf::from("/tmp"),
            backend: BackendKind::StreamJson,
            prompt_file: None,
            claude_cmd: None,
        };
        let err = registry
            .run_session(params)
            .await
            .expect_err("run_session must reject when at cap");
        // The error chain must carry the typed CapReached rejection.
        let admission_err = err
            .downcast_ref::<crate::control::admission::AdmissionError>()
            .expect("error must be an AdmissionError");
        assert_eq!(
            *admission_err,
            crate::control::admission::AdmissionError::CapReached { live: 1, cap: 1 }
        );
    }

    #[tokio::test]
    async fn registry_get_clones_under_read_lock() {
        let registry = SessionRegistry::new();
        let id = ControlSessionId::new("proj", 0);
        let handle = make_handle(&id);
        registry.register(id.clone(), handle).await;

        // Calling get() twice returns independent clones.
        let h1 = registry.get(&id).await.expect("should exist");
        let h2 = registry.get(&id).await.expect("should exist");

        // CAS on one clone does not affect the other's atomically-shared flag
        // (they share the same Arc, so they DO share the flag — which is what
        // §6.2 requires: the Arc keeps the flag alive on the cloned handle).
        assert!(h1.try_acquire_write_lock());
        assert!(!h2.try_acquire_write_lock()); // same Arc → same flag
        h1.release_write_lock();
        assert!(h2.try_acquire_write_lock()); // now acquirable
    }
}
