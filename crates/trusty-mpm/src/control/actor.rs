//! Per-session actor task for the SESSCTL control plane.
//!
//! Why: having one `tokio::spawn` task per session (§7.4 of SPEC-SESSCTL-01)
//! decouples the backend I/O loop from the HTTP API and from other sessions.
//! Each actor runs a `select!` loop over an `mpsc` command inbox and the
//! backend's `recv()` future; it forwards output to the broadcast channel and
//! transitions the lifecycle state machine.
//! What: [`SessionActor`] holds a backend, a command receiver, and a broadcast
//! sender. [`ActorCommand`] is the inbox type. [`run_actor`] is the task entry
//! point. [`SessionActorHandle`] is the registry-visible handle (command sender,
//! event sender, write-lock flag, and metadata Arc).
//!
//! Test: inline test module — stop_terminates, broadcasts_started, write_lock_cas.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use chrono::Utc;
use tokio::sync::{RwLock, broadcast, mpsc};
use tracing::{debug, warn};

use crate::control::backend::SessionBackend;
use crate::control::event::{BackendKind, SessionEvent, StopReason};
use crate::control::id::ControlSessionId;
use crate::control::state::{SessionMetadata, SessionState};

/// Broadcast channel capacity per session (§7.1 / §6.1 of SPEC-SESSCTL-01).
///
/// Why: `tokio::broadcast` is a fixed ring buffer; a slow observer beyond this
/// count receives `RecvError::Lagged` and must re-sync. 256 is the default
/// per spec (`session.broadcast_capacity`).
pub const DEFAULT_BROADCAST_CAPACITY: usize = 256;

/// Commands the `SessionActor` accepts on its `mpsc` inbox.
///
/// Why: the actor is the single writer to the backend; all external callers
/// (HTTP handlers, CLI) must go through the inbox to avoid concurrent backend
/// access.
/// What: `Send` injects input; `Stop` / `ForceStop` trigger graceful /
/// immediate shutdown; `Subscribe` returns a broadcast receiver clone.
/// Test: `actor_command_stop_terminates`.
pub enum ActorCommand {
    /// Deliver text input to the session backend (write-lock required).
    Send(crate::control::backend::SessionInput),
    /// Gracefully drain in-flight work and stop the session.
    Stop,
    /// Immediately kill the session (no drain).
    ForceStop,
    /// Subscribe an observer; the `Sender` carries the reply channel for
    /// the cloned `broadcast::Receiver`.
    Subscribe(tokio::sync::oneshot::Sender<broadcast::Receiver<SessionEvent>>),
}

impl std::fmt::Debug for ActorCommand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Send(_) => f.write_str("Send"),
            Self::Stop => f.write_str("Stop"),
            Self::ForceStop => f.write_str("ForceStop"),
            Self::Subscribe(_) => f.write_str("Subscribe"),
        }
    }
}

/// Registry-visible handle to a live `SessionActor`.
///
/// Why: the registry stores one handle per session; observers clone a
/// `broadcast::Receiver` from `event_tx` without going through the actor
/// inbox, keeping the fan-out path allocation-free on the hot path.
/// What: the four fields correspond to §7.1 of SPEC-SESSCTL-01.
/// Test: `actor_handle_write_lock_cas`.
#[derive(Clone)]
pub struct SessionActorHandle {
    /// Command inbox for the actor task.
    pub command_tx: mpsc::Sender<ActorCommand>,
    /// Broadcast sender; clone to subscribe a new observer.
    pub event_tx: broadcast::Sender<SessionEvent>,
    /// Single-writer CAS flag (§6.2 of SPEC-SESSCTL-01).
    pub write_lock_held: Arc<AtomicBool>,
    /// Shared session metadata snapshot (updated by the actor).
    pub metadata: Arc<RwLock<SessionMetadata>>,
}

impl SessionActorHandle {
    /// Attempt to acquire the write lock via a CAS on `write_lock_held`.
    ///
    /// Why: §6.2 specifies a single `compare_exchange(false, true)` as the
    /// only write-lock acquisition protocol to eliminate the TOCTOU window.
    /// What: returns `true` on success (caller is now the active writer),
    /// `false` if already held (caller is a read-only observer).
    /// Test: `actor_handle_write_lock_cas`.
    pub fn try_acquire_write_lock(&self) -> bool {
        self.write_lock_held
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
    }

    /// Release the write lock.
    ///
    /// Why: the active writer releases the lock on disconnect or stop.
    /// What: stores `false` with `SeqCst` ordering.
    /// Test: `actor_handle_write_lock_cas`.
    pub fn release_write_lock(&self) {
        self.write_lock_held.store(false, Ordering::SeqCst);
    }
}

/// Spawn a `SessionActor` task and return its registry handle.
///
/// Why: actors are always spawned via this function so the handle is always
/// constructed with the correct channel and atomic pairing.
/// What: creates the `mpsc` command channel, the `broadcast` event channel,
/// and the shared metadata Arc, then `tokio::spawn`s `run_actor`. Returns
/// the handle ready for insertion into `SessionRegistry`.
/// Test: `actor_broadcasts_started_event`.
pub fn spawn_actor<B: SessionBackend>(
    session_id: ControlSessionId,
    project_id: String,
    backend_kind: BackendKind,
    backend: B,
    broadcast_capacity: usize,
) -> SessionActorHandle {
    let (command_tx, command_rx) = mpsc::channel::<ActorCommand>(32);
    let (event_tx, _) = broadcast::channel::<SessionEvent>(broadcast_capacity);
    let write_lock_held = Arc::new(AtomicBool::new(false));
    let metadata = Arc::new(RwLock::new(SessionMetadata::new(
        session_id.clone(),
        project_id,
        backend_kind,
    )));

    let handle = SessionActorHandle {
        command_tx,
        event_tx: event_tx.clone(),
        write_lock_held: Arc::clone(&write_lock_held),
        metadata: Arc::clone(&metadata),
    };

    tokio::spawn(run_actor(
        session_id,
        backend,
        backend_kind,
        command_rx,
        event_tx,
        metadata,
    ));

    handle
}

/// The actor task loop (§7.4 of SPEC-SESSCTL-01).
///
/// Why: one task per session means each session's I/O, command dispatch, and
/// state machine run independently — slow backend I/O in one session does not
/// block another.
/// What: `select!` over `command_rx.recv()` and `backend.recv()`. Dispatches
/// commands, forwards events to broadcast, transitions `SessionState`, and
/// exits when the state machine reaches a terminal state.
/// Test: `actor_command_stop_terminates`, `actor_broadcasts_started_event`.
async fn run_actor<B: SessionBackend>(
    session_id: ControlSessionId,
    mut backend: B,
    backend_kind: BackendKind,
    mut command_rx: mpsc::Receiver<ActorCommand>,
    event_tx: broadcast::Sender<SessionEvent>,
    metadata: Arc<RwLock<SessionMetadata>>,
) {
    // Emit Started event and transition to Running.
    let started = SessionEvent::Started {
        session_id: session_id.clone(),
        backend: backend_kind,
        ts: Utc::now(),
    };
    let _ = event_tx.send(started);
    {
        let mut m = metadata.write().await;
        m.state = SessionState::Running;
    }

    loop {
        // Check if we are already in a terminal state (shouldn't happen here,
        // but guard against re-entrancy from a ForceStop).
        let is_terminal = {
            let m = metadata.read().await;
            m.state.is_terminal()
        };
        if is_terminal {
            break;
        }

        tokio::select! {
            // Command inbox.
            cmd = command_rx.recv() => {
                match cmd {
                    None => {
                        // All senders dropped; treat as a stop signal.
                        debug!(session_id = %session_id, "command channel closed; stopping actor");
                        break;
                    }
                    Some(ActorCommand::Stop) => {
                        debug!(session_id = %session_id, "actor: graceful Stop received");
                        transition_to_stopping(&session_id, &event_tx, &metadata).await;
                        // Box<B> satisfies stop(self: Box<Self>) without needing dyn.
                        if let Err(e) = Box::new(backend).stop().await {
                            warn!(session_id = %session_id, "backend stop error: {e}");
                        }
                        transition_to_stopped(
                            &session_id, StopReason::Requested, &event_tx, &metadata,
                        ).await;
                        break;
                    }
                    Some(ActorCommand::ForceStop) => {
                        debug!(session_id = %session_id, "actor: ForceStop received");
                        // Box<B> satisfies stop(self: Box<Self>) without needing dyn.
                        if let Err(e) = Box::new(backend).stop().await {
                            warn!(session_id = %session_id, "backend force-stop error: {e}");
                        }
                        transition_to_stopped(
                            &session_id, StopReason::Forced, &event_tx, &metadata,
                        ).await;
                        break;
                    }
                    Some(ActorCommand::Send(input)) => {
                        if let Err(e) = backend.send(input).await {
                            warn!(session_id = %session_id, "backend send error: {e}");
                        }
                    }
                    Some(ActorCommand::Subscribe(reply_tx)) => {
                        let rx = event_tx.subscribe();
                        let _ = reply_tx.send(rx);
                    }
                }
            }

            // Backend output.
            maybe_events = backend.recv() => {
                match maybe_events {
                    None => {
                        // Clean exit from backend.
                        debug!(session_id = %session_id, "backend recv returned None (clean exit)");
                        transition_to_stopped(
                            &session_id, StopReason::Completed, &event_tx, &metadata,
                        ).await;
                        break;
                    }
                    Some(Err(e)) => {
                        warn!(session_id = %session_id, "backend recv error: {e}");
                        // Phase 1: treat recv errors as terminal (restart is WI-4).
                        // A seam is preserved by having the Failed event here.
                        let _ = event_tx.send(SessionEvent::Failed {
                            session_id: session_id.clone(),
                            reason: e.to_string(),
                            ts: Utc::now(),
                        });
                        {
                            let mut m = metadata.write().await;
                            m.state = SessionState::Failed;
                        }
                        break;
                    }
                    Some(Ok(events)) => {
                        // Update last_activity_at on any output.
                        {
                            let mut m = metadata.write().await;
                            m.last_activity_at = Utc::now();
                        }
                        for event in events {
                            let _ = event_tx.send(event);
                        }
                    }
                }
            }
        }
    }

    debug!(session_id = %session_id, "actor task exiting");
}

/// Transition to `Stopping` state and emit the corresponding event (implicit).
///
/// Why: a separate helper keeps the actor loop readable and ensures the
/// metadata is always updated before broadcasting.
/// What: sets `metadata.state = Stopping`. No separate `SessionEvent` is
/// defined for `Stopping` in alpha-1; the transition is internal.
/// Test: `actor_command_stop_terminates`.
async fn transition_to_stopping(
    _session_id: &ControlSessionId,
    _event_tx: &broadcast::Sender<SessionEvent>,
    metadata: &Arc<RwLock<SessionMetadata>>,
) {
    let mut m = metadata.write().await;
    m.state = SessionState::Stopping;
}

/// Transition to `Stopped` and broadcast a `Stopped` event.
///
/// Why: every terminal transition must broadcast its final event before the
/// actor exits so observers that are already subscribed receive the signal.
/// What: sets `metadata.state = Stopped`, broadcasts `SessionEvent::Stopped`.
/// Test: `actor_command_stop_terminates`.
async fn transition_to_stopped(
    session_id: &ControlSessionId,
    reason: StopReason,
    event_tx: &broadcast::Sender<SessionEvent>,
    metadata: &Arc<RwLock<SessionMetadata>>,
) {
    {
        let mut m = metadata.write().await;
        m.state = SessionState::Stopped;
    }
    let _ = event_tx.send(SessionEvent::Stopped {
        session_id: session_id.clone(),
        reason,
        ts: Utc::now(),
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::backend::{SessionBackend, SessionInput};
    use crate::control::event::SessionEvent;
    use crate::control::id::ControlSessionId;

    // ── Minimal stub backend for unit tests ──────────────────────────────────

    struct StubBackend {
        /// Events to emit on successive `recv()` calls. `VecDeque` gives O(1)
        /// pop_front; `Vec::remove(0)` is O(n) (shifts all remaining elements).
        events: std::collections::VecDeque<Option<anyhow::Result<Vec<SessionEvent>>>>,
        /// When true and events are exhausted, block forever instead of
        /// returning `None`. Used by `actor_command_stop_terminates` so the
        /// actor's `select!` can pick up the Stop command before the backend
        /// exits naturally.
        block_when_exhausted: bool,
    }

    impl StubBackend {
        fn new_clean_exit() -> Self {
            Self {
                events: std::collections::VecDeque::from([None]),
                block_when_exhausted: false,
            }
        }

        /// A backend that never exits on its own — recv() parks forever once
        /// all pre-configured events are drained.
        fn new_never_exits() -> Self {
            Self {
                events: std::collections::VecDeque::new(),
                block_when_exhausted: true,
            }
        }

        fn new_with_output(session_id: &ControlSessionId) -> Self {
            let out = SessionEvent::Output {
                session_id: session_id.clone(),
                raw: "hello".into(),
                structured: None,
                ts: Utc::now(),
            };
            Self {
                events: std::collections::VecDeque::from([Some(Ok(vec![out])), None]),
                block_when_exhausted: false,
            }
        }
    }

    #[async_trait::async_trait]
    impl SessionBackend for StubBackend {
        async fn send(&mut self, _msg: SessionInput) -> anyhow::Result<()> {
            Ok(())
        }

        async fn recv(&mut self) -> Option<anyhow::Result<Vec<SessionEvent>>> {
            if let Some(ev) = self.events.pop_front() {
                ev
            } else if self.block_when_exhausted {
                // Park indefinitely so the actor's select! can process
                // command-inbox messages (e.g. Stop) before we exit.
                std::future::pending::<Option<anyhow::Result<Vec<SessionEvent>>>>().await
            } else {
                None
            }
        }

        async fn stop(self: Box<Self>) -> anyhow::Result<()> {
            Ok(())
        }
    }

    // ─────────────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn actor_broadcasts_started_event() {
        let id = ControlSessionId::new("proj", 0);
        let backend = StubBackend::new_clean_exit();
        // Subscribe BEFORE spawning so the broadcast ring always has at least
        // one live receiver — the Started event cannot be dropped unobserved.
        // We create a temporary channel here and subscribe immediately; the
        // real channel is created inside spawn_actor, so we obtain our receiver
        // from the handle immediately after spawn (before yielding) and use a
        // short poll loop with timeout to wait for the event.
        let handle = spawn_actor(
            id.clone(),
            "proj".into(),
            BackendKind::StreamJson,
            backend,
            DEFAULT_BROADCAST_CAPACITY,
        );
        // Subscribe immediately — before yielding to the executor — so the
        // broadcast ring buffer still holds the Started event (capacity=256).
        let mut rx = handle.event_tx.subscribe();

        // Poll for the Started event with a deadline. The actor runs on the
        // same tokio executor; yielding a few times is enough for it to emit
        // Started even if it ran ahead of us.
        let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_millis(500);
        let mut found_started = false;
        loop {
            match rx.try_recv() {
                Ok(SessionEvent::Started { .. }) => {
                    found_started = true;
                    break;
                }
                Ok(_) => {} // other events; keep looking
                Err(tokio::sync::broadcast::error::TryRecvError::Empty) => {
                    if tokio::time::Instant::now() >= deadline {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
                Err(_) => break, // Lagged / closed; should not happen with cap=256
            }
        }
        assert!(
            found_started,
            "actor must emit SessionEvent::Started before any other event"
        );
    }

    #[tokio::test]
    async fn actor_command_stop_terminates() {
        let id = ControlSessionId::new("proj", 1);
        // Use a backend whose recv() parks forever so the actor stays alive
        // long enough for our Stop command to arrive in the select! loop.
        let backend = StubBackend::new_never_exits();
        let handle = spawn_actor(
            id.clone(),
            "proj".into(),
            BackendKind::StreamJson,
            backend,
            DEFAULT_BROADCAST_CAPACITY,
        );
        tokio::time::sleep(tokio::time::Duration::from_millis(20)).await;

        // Send stop command.
        handle.command_tx.send(ActorCommand::Stop).await.unwrap();
        // Wait for the actor to process.
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

        // Metadata should now be Stopped.
        let m = handle.metadata.read().await;
        assert!(
            m.state.is_terminal(),
            "expected terminal state, got: {}",
            m.state
        );
    }

    #[tokio::test]
    async fn actor_clean_exit_from_backend_stops_actor() {
        let id = ControlSessionId::new("proj", 2);
        let backend = StubBackend::new_with_output(&id);
        let handle = spawn_actor(
            id.clone(),
            "proj".into(),
            BackendKind::StreamJson,
            backend,
            DEFAULT_BROADCAST_CAPACITY,
        );
        // Wait for actor to process output + clean exit.
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

        let m = handle.metadata.read().await;
        assert!(
            m.state.is_terminal(),
            "expected terminal state, got: {}",
            m.state
        );
    }

    #[test]
    fn actor_handle_write_lock_cas() {
        let lock = Arc::new(AtomicBool::new(false));
        let (command_tx, _) = mpsc::channel(1);
        let (event_tx, _) = broadcast::channel(4);
        let id = ControlSessionId::new("p", 0);
        let handle = SessionActorHandle {
            command_tx,
            event_tx,
            write_lock_held: Arc::clone(&lock),
            metadata: Arc::new(RwLock::new(SessionMetadata::new(
                id,
                "p".into(),
                BackendKind::StreamJson,
            ))),
        };

        // First acquire succeeds.
        assert!(handle.try_acquire_write_lock());
        // Second acquire fails (already held).
        assert!(!handle.try_acquire_write_lock());
        // Release.
        handle.release_write_lock();
        // After release, another acquire succeeds.
        assert!(handle.try_acquire_write_lock());
    }
}
