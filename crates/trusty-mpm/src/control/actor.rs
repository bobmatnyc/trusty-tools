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
//! Test: actor_tests.rs — stop_terminates, broadcasts_started, write_lock_cas,
//! activity_parsed_on_output, critical_observer_lag, no_false_positive_lag.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use chrono::Utc;
use tokio::sync::mpsc::error::TrySendError;
// TrySendError has two variants: Full (channel saturated) and Closed (all
// Receivers dropped). We treat them differently — only Full means lag.
use tokio::sync::{RwLock, broadcast, mpsc};
use tracing::{debug, info, warn};

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

/// Per-critical-observer mpsc channel capacity (§6.1 of SPEC-SESSCTL-01).
///
/// Why: the actor detects a critical observer's lag via `try_send` on a bounded
/// `mpsc::Sender`. When the channel is full the consumer has fallen behind and
/// the actor must surface `ObserverCriticalLag`. This capacity matches the
/// broadcast capacity so an idle consumer can buffer the same number of events
/// before triggering intervention.
pub const CRITICAL_OBSERVER_CAPACITY: usize = DEFAULT_BROADCAST_CAPACITY;

/// Commands the `SessionActor` accepts on its `mpsc` inbox.
///
/// Why: the actor is the single writer to the backend; all external callers
/// (HTTP handlers, CLI) must go through the inbox to avoid concurrent backend
/// access.
/// What: `Send` injects input; `Stop` / `ForceStop` trigger graceful /
/// immediate shutdown; `Subscribe` returns a broadcast receiver clone;
/// `SubscribeCritical` registers the caller as a critical observer (§6.1) via
/// a dedicated bounded mpsc so the actor can detect genuine backpressure.
/// Test: `actor_command_stop_terminates`, `critical_observer_lag`.
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
    /// Unlike `Subscribe`, events are delivered via a dedicated bounded
    /// `mpsc::Receiver` (capacity `CRITICAL_OBSERVER_CAPACITY`). The actor
    /// uses `try_send` to forward each event; a full channel means the real
    /// consumer has fallen behind → `ObserverCriticalLag` is emitted and the
    /// session transitions to `AwaitingIntervention`.
    SubscribeCritical(tokio::sync::oneshot::Sender<mpsc::Receiver<SessionEvent>>),
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
    /// resubscribed after a lag — the first full-channel condition transitions
    /// the session to `AwaitingIntervention` and surfaces `ObserverCriticalLag`.
    /// What: sends `ActorCommand::SubscribeCritical` over the command inbox
    /// and awaits a oneshot that carries an `mpsc::Receiver`. The actor holds
    /// the `Sender` and uses `try_send` to detect genuine backpressure from the
    /// real consumer. Returns `None` if the actor inbox is closed.
    /// Test: `critical_observer_lag`, `critical_observer_no_false_positive`.
    pub async fn subscribe_critical(&self) -> Option<mpsc::Receiver<SessionEvent>> {
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
/// commands, forwards events to broadcast AND the critical-observer mpsc,
/// transitions `SessionState`, and exits when the state machine reaches a
/// terminal state.
/// WI-3: after forwarding each `Output` event, runs the §8.2 regex/NLP parse
/// layer and emits `ActivityParsed` / `PendingDecision` if a match is found,
/// updating `SessionMetadata` accordingly. Critical-observer lag is detected
/// via `try_send` on the per-observer bounded mpsc (§6.1).
/// Test: `actor_command_stop_terminates`, `actor_broadcasts_started_event`,
/// `actor_activity_parsed_on_output`, `critical_observer_lag`.
pub(crate) async fn run_actor<B: SessionBackend>(
    session_id: ControlSessionId,
    mut backend: B,
    backend_kind: BackendKind,
    mut command_rx: mpsc::Receiver<ActorCommand>,
    event_tx: broadcast::Sender<SessionEvent>,
    metadata: Arc<RwLock<SessionMetadata>>,
) {
    // Emit Started event and transition to Running.
    let _ = event_tx.send(SessionEvent::Started {
        session_id: session_id.clone(),
        backend: backend_kind,
        ts: Utc::now(),
    });
    {
        let mut m = metadata.write().await;
        m.state = SessionState::Running;
    }

    // Critical observer (§6.1): a bounded mpsc Sender. The actor `try_send`s
    // each event here; `Err(Full)` means the REAL consumer is behind → trigger
    // AwaitingIntervention. Using mpsc rather than a broadcast clone means we
    // measure the actual consumer's queue depth, not a self-drained proxy.
    let mut critical_tx: Option<mpsc::Sender<SessionEvent>> = None;

    loop {
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
                        debug!(session_id = %session_id, "command channel closed; stopping actor");
                        break;
                    }
                    Some(ActorCommand::Stop) => {
                        debug!(session_id = %session_id, "actor: graceful Stop received");
                        transition_to_stopping(&metadata).await;
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
                        let _ = reply_tx.send(event_tx.subscribe());
                    }
                    Some(ActorCommand::SubscribeCritical(reply_tx)) => {
                        // §6.1 — enforce single critical observer: if an existing
                        // Sender is still connected (not all Receivers dropped),
                        // reject the new subscription to avoid silently evicting
                        // the first observer.
                        if let Some(ref existing_tx) = critical_tx {
                            if !existing_tx.is_closed() {
                                warn!(
                                    session_id = %session_id,
                                    "SubscribeCritical rejected: \
                                     a connected critical observer already exists (§6.1). \
                                     Only one critical observer is supported at a time."
                                );
                                // Drop reply_tx without sending — caller gets
                                // None from rx.await.ok() in subscribe_critical().
                                drop(reply_tx);
                            } else {
                                // Previous observer disconnected; safe to replace.
                                let (tx, rx) =
                                    mpsc::channel::<SessionEvent>(CRITICAL_OBSERVER_CAPACITY);
                                let _ = reply_tx.send(rx);
                                critical_tx = Some(tx);
                                debug!(
                                    session_id = %session_id,
                                    "critical observer replaced (prior was disconnected, §6.1)"
                                );
                            }
                        } else {
                            // No existing observer; register fresh.
                            let (tx, rx) =
                                mpsc::channel::<SessionEvent>(CRITICAL_OBSERVER_CAPACITY);
                            let _ = reply_tx.send(rx);
                            critical_tx = Some(tx);
                            debug!(session_id = %session_id, "critical observer subscribed (§6.1)");
                        }
                    }
                }
            }

            // Backend output.
            maybe_events = backend.recv() => {
                match maybe_events {
                    None => {
                        debug!(session_id = %session_id, "backend recv returned None (clean exit)");
                        transition_to_stopped(
                            &session_id, StopReason::Completed, &event_tx, &metadata,
                        ).await;
                        break;
                    }
                    Some(Err(e)) => {
                        warn!(session_id = %session_id, "backend recv error: {e}");
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

                        // Forward events to broadcast channel AND to the critical
                        // observer mpsc. Detect lag via try_send (§6.1):
                        //   Err(Full)         → genuine sustained backpressure →
                        //                        AwaitingIntervention + teardown.
                        //   Err(Disconnected) → observer dropped its Receiver →
                        //                        clear critical_tx, keep running.
                        let mut critical_lagged = false;
                        let mut dropped_count: u64 = 0;
                        let mut raw_text = String::new();

                        for event in &events {
                            if let SessionEvent::Output { raw, .. } = event {
                                raw_text.push_str(raw);
                                raw_text.push('\n');
                            }
                            let _ = event_tx.send(event.clone());

                            if let Some(ref tx) = critical_tx {
                                match tx.try_send(event.clone()) {
                                    Ok(()) => {}
                                    Err(TrySendError::Full(_)) => {
                                        // Channel of capacity N is full → N events
                                        // have backed up without being consumed.
                                        // Count each failed send so ObserverCriticalLag
                                        // reports the real number of dropped events.
                                        dropped_count += 1;
                                        critical_lagged = true;
                                    }
                                    Err(TrySendError::Closed(_)) => {
                                        // Critical observer dropped its Receiver and
                                        // went away cleanly. Stop tracking it so we
                                        // do NOT kill an otherwise-healthy session.
                                        info!(
                                            session_id = %session_id,
                                            "critical observer disconnected (Closed) — \
                                             clearing critical_tx, session continues (§6.1)"
                                        );
                                        critical_tx = None;
                                        // Stop iterating over remaining events for
                                        // the critical path; broadcast already sent.
                                        break;
                                    }
                                }
                            }
                        }

                        // Update last_activity_at.
                        {
                            let mut m = metadata.write().await;
                            m.last_activity_at = now;
                        }

                        // §6.1: critical observer lag → emit event, transition to
                        // AwaitingIntervention, stop the backend, and exit the loop.
                        if critical_lagged {
                            warn!(
                                session_id = %session_id,
                                "critical observer lagged (§6.1) — \
                                 transitioning to AwaitingIntervention"
                            );
                            let _ = event_tx.send(SessionEvent::ObserverCriticalLag {
                                session_id: session_id.clone(),
                                // Count of events that failed try_send(Full) in
                                // this batch — the real number of undelivered
                                // critical-observer events.
                                dropped: dropped_count,
                            });
                            {
                                let mut m = metadata.write().await;
                                m.state = SessionState::AwaitingIntervention;
                            }
                            // Stop the backend to release resources and avoid
                            // a process/tmux-pane leak (fixes review bug #2).
                            if let Err(e) = Box::new(backend).stop().await {
                                warn!(session_id = %session_id, "backend stop on lag: {e}");
                            }
                            // Emit a terminal Stopped event so observers can drain.
                            let _ = event_tx.send(SessionEvent::Stopped {
                                session_id: session_id.clone(),
                                reason: StopReason::Forced,
                                ts: Utc::now(),
                            });
                            break;
                        }

                        // §8.2 Regex/NLP parse layer.
                        if !raw_text.is_empty()
                            && let Some(parsed) = ActivityParser::parse_output(&raw_text)
                        {
                            // Auth prompt: emit ActivityParsed, then handle
                            // stream-json (terminal) or tmux (AwaitingAuth).
                            // Never emit PendingDecision for AuthPrompt —
                            // the decision surface is wrong and the state
                            // write must be coherent (fixes review bug #3).
                            if parsed.kind == ActivityKind::AuthPrompt {
                                let _ = event_tx.send(SessionEvent::ActivityParsed {
                                    session_id: session_id.clone(),
                                    kind: parsed.kind.clone(),
                                    summary: parsed.summary.clone(),
                                    ts: now,
                                });
                                if backend_kind == BackendKind::StreamJson {
                                    {
                                        let mut m = metadata.write().await;
                                        m.last_summary = Some(parsed.summary.clone());
                                        m.state = SessionState::AuthFailed;
                                    }
                                    let _ = event_tx.send(SessionEvent::AuthFailed {
                                        session_id: session_id.clone(),
                                        backend: backend_kind,
                                        reason: "auth prompt detected on stream-json; \
                                                 pre-authenticate ~/.claude and re-launch"
                                            .into(),
                                        ts: now,
                                    });
                                    break;
                                } else if backend_kind == BackendKind::Tmux {
                                    let mut m = metadata.write().await;
                                    m.last_summary = Some(parsed.summary.clone());
                                    m.state = SessionState::AwaitingAuth;
                                }
                                // Note: `continue` here re-enters the select! loop without
                                // falling through to the PendingDecision path below.
                                // The StreamJson arm is the only one that moved `backend`
                                // (via Box::new(backend).stop()); the Tmux arm only wrote
                                // metadata and we continue without calling backend.stop().
                                continue;
                            }

                            // Non-auth path: update metadata, emit ActivityParsed,
                            // optionally emit PendingDecision, clear stale decision
                            // when this output is not itself a decision prompt
                            // (fixes review bug #6).
                            {
                                let mut m = metadata.write().await;
                                m.last_summary = Some(parsed.summary.clone());
                                if parsed.pending_decision.is_some() {
                                    m.pending_decision = parsed.pending_decision.clone();
                                    m.proposed_default = parsed.proposed_default.clone();
                                } else {
                                    // Clear stale pending_decision when the new
                                    // output is not itself a decision prompt.
                                    m.pending_decision = None;
                                    m.proposed_default = None;
                                }
                            }

                            let _ = event_tx.send(SessionEvent::ActivityParsed {
                                session_id: session_id.clone(),
                                kind: parsed.kind.clone(),
                                summary: parsed.summary.clone(),
                                ts: now,
                            });

                            if let Some(ref prompt) = parsed.pending_decision {
                                let _ = event_tx.send(SessionEvent::PendingDecision {
                                    session_id: session_id.clone(),
                                    prompt: prompt.clone(),
                                    proposed_default: parsed.proposed_default.clone(),
                                    ts: now,
                                });
                            }
                        }
                    }
                }
            }
        }
    }

    debug!(session_id = %session_id, "actor task exiting");
}

/// Transition to `Stopping` state (internal; no event emitted in alpha-1).
///
/// Why: a separate helper keeps the actor loop readable.
/// What: sets `metadata.state = Stopping`.
/// Test: `actor_command_stop_terminates`.
async fn transition_to_stopping(metadata: &Arc<RwLock<SessionMetadata>>) {
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
