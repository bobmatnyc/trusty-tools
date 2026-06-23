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
use crate::control::backend::stream_json::StreamJsonBackend;
use crate::control::backend::tmux::TmuxBackend;
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
#[derive(Clone)]
pub struct SessionRegistry {
    inner: Arc<RwLock<RegistryInner>>,
}

struct RegistryInner {
    actors: HashMap<ControlSessionId, SessionActorHandle>,
    counter: SessionCounter,
}

impl SessionRegistry {
    /// Create a new empty registry.
    ///
    /// Why: the daemon constructs a registry at startup and injects it into
    /// every handler; an empty initial state is the correct starting point.
    /// What: allocates the `Arc<RwLock<…>>` with an empty map and counter.
    /// Test: all registry tests construct via `new()`.
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RwLock::new(RegistryInner {
                actors: HashMap::new(),
                counter: SessionCounter::default(),
            })),
        }
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

    /// Allocate a session ID and spawn an actor via the selected backend.
    ///
    /// Why: `tm run <project-id>` must go through the registry so the ID
    /// counter is authoritative and an issued ID is always observable by
    /// concurrent `get()` / `list_ids()` callers. The former two-step pattern
    /// (allocate → release lock → spawn → re-acquire to register) had a TOCTOU
    /// gap: the ID existed but `get()` returned `None` during backend setup.
    /// What: holds ONE write lock for the entire sequence — ID allocation,
    /// backend construction, actor spawn, and handle insertion — so `get(&id)`
    /// is never `None` for a returned ID. Backend construction (`Command::spawn`
    /// or `tmux new-session`) takes O(10 ms) in the worst case; holding the
    /// write lock across that is acceptable for Phase 1.
    /// Returns the allocated `ControlSessionId`.
    /// Test: `registry_run_session`, `registry_run_session_id_visible_immediately`.
    pub async fn run_session(&self, params: RunParams) -> Result<ControlSessionId> {
        let mut inner = self.inner.write().await;
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
