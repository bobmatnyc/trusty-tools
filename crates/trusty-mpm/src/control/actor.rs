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
//! event sender, write-lock flag, and metadata Arc). WI-3 adds critical-observer
//! subscription via [`ActorCommand::SubscribeCritical`] and wires the §8.2
//! regex/NLP parse layer into the output path.
//!
//! Test: inline test module — stop_terminates, broadcasts_started, write_lock_cas,
//! activity_parsed_on_output, critical_observer_lag_on_first_lag.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use chrono::Utc;
use tokio::sync::{RwLock, broadcast, mpsc};
use tracing::{debug, warn};

use crate::control::activity::ActivityParser;
use crate::control::backend::SessionBackend;
use crate::control::event::{ActivityKind, BackendKind, SessionEvent, StopReason};
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
/// immediate shutdown; `Subscribe` returns a broadcast receiver clone;
/// `SubscribeCritical` returns a receiver AND registers the caller as a
/// critical observer (§6.1) so the first lag yields `ObserverCriticalLag`
/// instead of a silent resubscription.
/// Test: `actor_command_stop_terminates`, `critical_observer_lag_on_first_lag`.
pub enum ActorCommand {
    /// Deliver text input to the session backend (write-lock required).
    Send(crate::control::backend::SessionInput),
    /// Gracefully drain in-flight work and stop the session.
    Stop,
    /// Immediately kill the session (no drain).
    ForceStop,
    /// Subscribe a best-effort observer; the `Sender` carries the reply
    /// channel for the cloned `broadcast::Receiver`.
    Subscribe(tokio::sync::oneshot::Sender<broadcast::Receiver<SessionEvent>>),
    /// Subscribe a **critical** observer (e.g. the SM agent) per §6.1.
    ///
    /// Unlike `Subscribe`, the first `Lagged` error on this receiver causes
    /// the actor to emit `ObserverCriticalLag` and transition the session to
    /// `AwaitingIntervention` instead of silently resubscribing.
    SubscribeCritical(tokio::sync::oneshot::Sender<broadcast::Receiver<SessionEvent>>),
}

impl std::fmt::Debug for ActorCommand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Send(_) => f.write_str("Send"),
            Self::Stop => f.write_str("Stop"),
            Self::ForceStop => f.write_str("ForceStop"),
            Self::Subscribe(_) => f.write_str("Subscribe"),
            Self::SubscribeCritical(_) => f.write_str("SubscribeCritical"),
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

    /// Subscribe a **critical** observer (§6.1 of SPEC-SESSCTL-01).
    ///
    /// Why: critical observers (e.g. the SM agent) must never be silently
    /// resubscribed after a lag — the first `Lagged` error transitions the
    /// session to `AwaitingIntervention` and surfaces `ObserverCriticalLag`.
    /// What: sends `ActorCommand::SubscribeCritical` over the command inbox
    /// and waits on a oneshot for the `broadcast::Receiver`. Returns `None`
    /// if the actor inbox is closed (actor already stopped).
    /// Test: `critical_observer_lag_on_first_lag`.
    pub async fn subscribe_critical(&self) -> Option<broadcast::Receiver<SessionEvent>> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.command_tx
            .send(ActorCommand::SubscribeCritical(tx))
            .await
            .ok()?;
        rx.await.ok()
    }
}

/// Spawn a `SessionActor` task and return its registry handle.
///
/// Why: actors are always spawned via this function so the handle is always
/// constructed with the correct channel and atomic pairing.
/// What: creates the `mpsc` command channel, the `broadcast` event channel,
/// and the shared metadata Arc, then `tokio::spawn`s `run_actor`. Returns
/// the handle ready for insertion into `SessionRegistry`. WI-3: also
/// initialises the `critical_observer_active` flag that tracks whether a
/// critical observer is currently subscribed.
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
/// WI-3: after forwarding each `Output` event, runs the §8.2 regex/NLP parse
/// layer and emits `ActivityParsed` / `PendingDecision` if a match is found,
/// updating `SessionMetadata` accordingly. Also handles `SubscribeCritical`
/// to track critical observers per §6.1.
/// Test: `actor_command_stop_terminates`, `actor_broadcasts_started_event`,
/// `actor_activity_parsed_on_output`, `critical_observer_lag_yields_event`.
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

    // Track whether a critical observer is currently registered (§6.1).
    // We store the receiver so we can poll it for lag detection.
    let mut critical_rx: Option<broadcast::Receiver<SessionEvent>> = None;

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
                    Some(ActorCommand::SubscribeCritical(reply_tx)) => {
                        // Replace any prior critical receiver with a fresh one.
                        let rx = event_tx.subscribe();
                        // Keep a second clone for lag detection inside the actor.
                        let lag_rx = event_tx.subscribe();
                        let _ = reply_tx.send(rx);
                        critical_rx = Some(lag_rx);
                        debug!(
                            session_id = %session_id,
                            "critical observer subscribed (§6.1)"
                        );
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
                        let now = Utc::now();

                        // Check critical observer for lag BEFORE forwarding new
                        // events. `try_recv` on a critical receiver that has lagged
                        // returns `Err(Lagged)` — the receiver is still valid for
                        // future events but we must surface the lag immediately.
                        let mut critical_lagged = false;
                        if let Some(ref mut crit_rx) = critical_rx {
                            // Drain what we can; if we hit a Lagged error, the
                            // critical observer missed events.
                            loop {
                                match crit_rx.try_recv() {
                                    Ok(_) => {} // successfully drained an event
                                    Err(broadcast::error::TryRecvError::Lagged(n)) => {
                                        warn!(
                                            session_id = %session_id,
                                            dropped = n,
                                            "critical observer lagged (§6.1) — \
                                             transitioning to AwaitingIntervention"
                                        );
                                        let _ = event_tx.send(
                                            SessionEvent::ObserverCriticalLag {
                                                session_id: session_id.clone(),
                                                dropped: n,
                                            },
                                        );
                                        {
                                            let mut m = metadata.write().await;
                                            m.state = SessionState::AwaitingIntervention;
                                        }
                                        // Clear the critical receiver — recovery
                                        // requires an explicit re-attach.
                                        critical_rx = None;
                                        critical_lagged = true;
                                        break;
                                    }
                                    Err(broadcast::error::TryRecvError::Empty) => break,
                                    Err(broadcast::error::TryRecvError::Closed) => {
                                        critical_rx = None;
                                        break;
                                    }
                                }
                            }
                        }
                        // §6.1: AwaitingIntervention is a halt state — stop
                        // processing further events and exit the actor loop.
                        if critical_lagged {
                            break;
                        }

                        // Forward all raw output events and collect raw text.
                        let mut raw_text = String::new();
                        for event in &events {
                            if let SessionEvent::Output { raw, .. } = event {
                                raw_text.push_str(raw);
                                raw_text.push('\n');
                            }
                            let _ = event_tx.send(event.clone());
                        }

                        // Update last_activity_at after forwarding.
                        {
                            let mut m = metadata.write().await;
                            m.last_activity_at = now;
                        }

                        // §8.2 Regex/NLP parse layer — runs only when there is
                        // raw text in the output batch.
                        if !raw_text.is_empty()
                            && let Some(parsed) = ActivityParser::parse_output(&raw_text)
                        {
                            // Update metadata with the parse result.
                            {
                                let mut m = metadata.write().await;
                                m.last_summary = Some(parsed.summary.clone());
                                if parsed.pending_decision.is_some() {
                                    m.pending_decision = parsed.pending_decision.clone();
                                    m.proposed_default = parsed.proposed_default.clone();
                                }
                                // Auth prompt on stream-JSON is a terminal error.
                                if parsed.kind == ActivityKind::AuthPrompt
                                    && backend_kind == BackendKind::StreamJson
                                {
                                    m.state = SessionState::AuthFailed;
                                }
                            }

                            let _ = event_tx.send(SessionEvent::ActivityParsed {
                                session_id: session_id.clone(),
                                kind: parsed.kind.clone(),
                                summary: parsed.summary.clone(),
                                ts: now,
                            });

                            // Emit PendingDecision if decision prompt detected.
                            if let Some(ref prompt) = parsed.pending_decision {
                                let _ = event_tx.send(SessionEvent::PendingDecision {
                                    session_id: session_id.clone(),
                                    prompt: prompt.clone(),
                                    proposed_default: parsed.proposed_default.clone(),
                                    ts: now,
                                });
                            }

                            // Auth prompt on stream-JSON → transition and exit.
                            if parsed.kind == ActivityKind::AuthPrompt
                                && backend_kind == BackendKind::StreamJson
                            {
                                let _ = event_tx.send(SessionEvent::AuthFailed {
                                    session_id: session_id.clone(),
                                    backend: backend_kind,
                                    reason: "auth prompt detected on stream-json backend; \
                                             pre-authenticate ~/.claude and re-launch"
                                        .into(),
                                    ts: now,
                                });
                                break;
                            }

                            // Auth prompt on tmux → AwaitingAuth state.
                            if parsed.kind == ActivityKind::AuthPrompt
                                && backend_kind == BackendKind::Tmux
                            {
                                let mut m = metadata.write().await;
                                m.state = SessionState::AwaitingAuth;
                            }
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
#[path = "actor_tests.rs"]
mod tests;
