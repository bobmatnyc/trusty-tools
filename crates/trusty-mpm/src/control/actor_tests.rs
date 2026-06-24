//! Unit tests for `control::actor` (WI-1 through WI-3).
//!
//! Why: keeping tests in a sibling file lets actor.rs stay within the 500-SLOC
//! production cap while giving tests access to private helpers.
//! What: covers lifecycle (Started / Stopped events), command dispatch (Stop),
//! write-lock CAS, §8.2 activity parse (ActivityParsed / PendingDecision),
//! critical observer lag detection per §6.1 (real backpressure via mpsc, NOT a
//! self-drained proxy), and stale pending_decision clearing.
//! Test: these tests; run with `cargo test -p trusty-mpm control::actor_tests`.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use chrono::Utc;
use tokio::sync::{RwLock, broadcast, mpsc};

use crate::control::actor::{
    ActorCommand, CRITICAL_OBSERVER_CAPACITY, DEFAULT_BROADCAST_CAPACITY, SessionActorConfig,
    SessionActorHandle, run_actor, spawn_actor,
};
use crate::control::backend::{SessionBackend, SessionInput};
use crate::control::event::{BackendKind, SessionEvent};
use crate::control::id::ControlSessionId;
use crate::control::state::{SessionMetadata, SessionState};

// ── Channel-fed backend ───────────────────────────────────────────────────────
//
// Used by critical-lag tests so the test controls exactly WHEN each batch of
// events arrives at the actor.

struct ChanBackend {
    rx: mpsc::Receiver<Option<anyhow::Result<Vec<SessionEvent>>>>,
    /// Optional notify: set to true when `stop()` is called.
    stop_flag: Option<Arc<std::sync::atomic::AtomicBool>>,
}

#[async_trait::async_trait]
impl SessionBackend for ChanBackend {
    async fn send(&mut self, _: SessionInput) -> anyhow::Result<()> {
        Ok(())
    }
    async fn recv(&mut self) -> Option<anyhow::Result<Vec<SessionEvent>>> {
        self.rx.recv().await.unwrap_or_default()
    }
    async fn stop(self) -> anyhow::Result<()> {
        if let Some(flag) = self.stop_flag {
            flag.store(true, std::sync::atomic::Ordering::SeqCst);
        }
        Ok(())
    }
}

// ── Minimal VecDeque stub backend ────────────────────────────────────────────

struct StubBackend {
    events: std::collections::VecDeque<Option<anyhow::Result<Vec<SessionEvent>>>>,
    block_when_exhausted: bool,
}

impl StubBackend {
    fn new_clean_exit() -> Self {
        Self {
            events: std::collections::VecDeque::from([None]),
            block_when_exhausted: false,
        }
    }

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
            std::future::pending::<Option<anyhow::Result<Vec<SessionEvent>>>>().await
        } else {
            None
        }
    }

    async fn stop(self) -> anyhow::Result<()> {
        Ok(())
    }
}

// ── Helper: build a SessionActorHandle with explicit channels ─────────────────

fn make_handle(
    _id: &ControlSessionId,
    _project: &str,
    command_tx: mpsc::Sender<ActorCommand>,
    event_tx: broadcast::Sender<SessionEvent>,
    metadata: Arc<RwLock<SessionMetadata>>,
) -> SessionActorHandle {
    SessionActorHandle {
        command_tx,
        event_tx,
        write_lock_held: Arc::new(AtomicBool::new(false)),
        metadata,
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Lifecycle tests
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn actor_broadcasts_started_event() {
    let id = ControlSessionId::new("proj", 0);
    let handle = spawn_actor(
        id.clone(),
        "proj".into(),
        BackendKind::StreamJson,
        StubBackend::new_clean_exit(),
        DEFAULT_BROADCAST_CAPACITY,
        SessionActorConfig::default(),
    );
    let mut rx = handle.event_tx.subscribe();

    let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_millis(500);
    let mut found_started = false;
    loop {
        match rx.try_recv() {
            Ok(SessionEvent::Started { .. }) => {
                found_started = true;
                break;
            }
            Ok(_) => {}
            Err(tokio::sync::broadcast::error::TryRecvError::Empty) => {
                if tokio::time::Instant::now() >= deadline {
                    break;
                }
                tokio::task::yield_now().await;
            }
            Err(_) => break,
        }
    }
    assert!(found_started, "actor must emit SessionEvent::Started");
}

#[tokio::test]
async fn actor_command_stop_terminates() {
    let id = ControlSessionId::new("proj", 1);
    let handle = spawn_actor(
        id.clone(),
        "proj".into(),
        BackendKind::StreamJson,
        StubBackend::new_never_exits(),
        DEFAULT_BROADCAST_CAPACITY,
        SessionActorConfig::default(),
    );
    tokio::time::sleep(tokio::time::Duration::from_millis(20)).await;
    handle.command_tx.send(ActorCommand::Stop).await.unwrap();
    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

    let m = handle.metadata.read().await;
    assert!(m.state.is_terminal(), "expected terminal, got: {}", m.state);
}

#[tokio::test]
async fn actor_clean_exit_from_backend_stops_actor() {
    let id = ControlSessionId::new("proj", 2);
    let handle = spawn_actor(
        id.clone(),
        "proj".into(),
        BackendKind::StreamJson,
        StubBackend::new_with_output(&id),
        DEFAULT_BROADCAST_CAPACITY,
        SessionActorConfig::default(),
    );
    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

    let m = handle.metadata.read().await;
    assert!(m.state.is_terminal(), "expected terminal, got: {}", m.state);
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

    assert!(handle.try_acquire_write_lock());
    assert!(!handle.try_acquire_write_lock());
    handle.release_write_lock();
    assert!(handle.try_acquire_write_lock());
}

// ─────────────────────────────────────────────────────────────────────────────
// WI-3 activity parse tests
// ─────────────────────────────────────────────────────────────────────────────

/// Verify that `Output` with a tool-use line causes the actor to update
/// `last_summary` in metadata.
///
/// Why: §8.2 requires the regex parse layer to produce `ActivityParsed` after
/// every `Output` event that matches a known pattern.
/// What: spawns an actor with a backend that produces one tool-use output,
/// waits, then checks metadata.last_summary.
/// Test: this test.
#[tokio::test]
async fn actor_activity_parsed_on_output() {
    let id = ControlSessionId::new("parse-proj", 0);
    let output_event = SessionEvent::Output {
        session_id: id.clone(),
        raw: "Tool use: Bash".into(),
        structured: None,
        ts: Utc::now(),
    };
    let backend = StubBackend {
        events: std::collections::VecDeque::from([Some(Ok(vec![output_event])), None]),
        block_when_exhausted: false,
    };
    let handle = spawn_actor(
        id.clone(),
        "parse-proj".into(),
        BackendKind::StreamJson,
        backend,
        DEFAULT_BROADCAST_CAPACITY,
        SessionActorConfig::default(),
    );
    tokio::time::sleep(tokio::time::Duration::from_millis(150)).await;

    let m = handle.metadata.read().await;
    assert!(
        m.last_summary.is_some(),
        "last_summary should be set after tool-use output"
    );
    assert!(
        m.last_summary.as_deref().unwrap_or("").contains("Bash"),
        "last_summary should mention 'Bash'"
    );
}

/// Verify that a `[Y/n]` prompt populates `pending_decision` and
/// `proposed_default` in metadata.
///
/// Why: §8.4 requires pending-decision extraction from decision prompts.
/// What: sends a `[Y/n]`-style output; asserts metadata fields are set.
/// Test: this test.
#[tokio::test]
async fn actor_pending_decision_extracted() {
    let id = ControlSessionId::new("decision-proj", 0);
    let output_event = SessionEvent::Output {
        session_id: id.clone(),
        raw: "Proceed with deletion? [Y/n]:".into(),
        structured: None,
        ts: Utc::now(),
    };
    let backend = StubBackend {
        events: std::collections::VecDeque::from([Some(Ok(vec![output_event])), None]),
        block_when_exhausted: false,
    };
    let handle = spawn_actor(
        id.clone(),
        "decision-proj".into(),
        BackendKind::StreamJson,
        backend,
        DEFAULT_BROADCAST_CAPACITY,
        SessionActorConfig::default(),
    );
    tokio::time::sleep(tokio::time::Duration::from_millis(150)).await;

    let m = handle.metadata.read().await;
    assert!(
        m.pending_decision.is_some(),
        "pending_decision should be set for a [Y/n] prompt"
    );
    assert!(
        m.proposed_default.is_some(),
        "proposed_default should be set when [Y/n] default is present"
    );
}

/// Verify that stale `pending_decision` is cleared when the next output is not
/// a decision prompt (review bug #6).
///
/// Why: without clearing, `sessctl list` shows a stale decision indefinitely.
/// What: set a pending decision via a `[Y/n]` output, then send a plain output
/// and assert that pending_decision is cleared.
/// Test: this test.
#[tokio::test]
async fn actor_pending_decision_cleared_on_next_output() {
    let id = ControlSessionId::new("clear-proj", 0);

    // First output: decision prompt
    let decision_event = SessionEvent::Output {
        session_id: id.clone(),
        raw: "Confirm? [Y/n]:".into(),
        structured: None,
        ts: Utc::now(),
    };
    // Second output: normal tool-use (not a decision prompt)
    let normal_event = SessionEvent::Output {
        session_id: id.clone(),
        raw: "Tool use: Bash".into(),
        structured: None,
        ts: Utc::now(),
    };
    let backend = StubBackend {
        events: std::collections::VecDeque::from([
            Some(Ok(vec![decision_event])),
            Some(Ok(vec![normal_event])),
            None,
        ]),
        block_when_exhausted: false,
    };
    let handle = spawn_actor(
        id.clone(),
        "clear-proj".into(),
        BackendKind::StreamJson,
        backend,
        DEFAULT_BROADCAST_CAPACITY,
        SessionActorConfig::default(),
    );
    tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;

    let m = handle.metadata.read().await;
    assert!(
        m.pending_decision.is_none(),
        "pending_decision must be cleared after a non-decision output, \
         got: {:?}",
        m.pending_decision
    );
    assert!(
        m.proposed_default.is_none(),
        "proposed_default must be cleared after a non-decision output"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Auth-prompt batch forwarding test
// ─────────────────────────────────────────────────────────────────────────────

/// Verify that events AFTER an AuthPrompt in the same batch are still
/// forwarded to the broadcast channel.
///
/// Why: if the forwarding loop breaks early (e.g. on critical Closed) while
/// an AuthPrompt is present, trailing events in the batch would be dropped
/// from broadcast. §7.4 requires all events in a batch to be forwarded before
/// the actor acts on any state transition.
/// What: sends a batch of [Output("normal"), Output("auth"), Output("trailing")]
/// where "auth" triggers AuthPrompt detection. Subscribes a broadcast receiver
/// and asserts all three Output events arrive.
/// Test: this test.
#[tokio::test]
async fn actor_auth_prompt_batch_trailing_event_forwarded() {
    let id = ControlSessionId::new("auth-batch-proj", 0);

    // Build a batch where the second event contains an auth prompt phrase and
    // the third event is a normal output that must NOT be dropped.
    let batch = vec![
        SessionEvent::Output {
            session_id: id.clone(),
            raw: "Normal output before auth".into(),
            structured: None,
            ts: Utc::now(),
        },
        SessionEvent::Output {
            session_id: id.clone(),
            raw: "Please log in to claude.ai".into(), // triggers AuthPrompt
            structured: None,
            ts: Utc::now(),
        },
        SessionEvent::Output {
            session_id: id.clone(),
            raw: "trailing-sentinel-event".into(), // must reach broadcast
            structured: None,
            ts: Utc::now(),
        },
    ];
    let backend = StubBackend {
        events: std::collections::VecDeque::from([Some(Ok(batch)), None]),
        block_when_exhausted: false,
    };

    let handle = spawn_actor(
        id.clone(),
        "auth-batch-proj".into(),
        BackendKind::Tmux, // Tmux: auth → AwaitingAuth but does NOT break the loop
        backend,
        DEFAULT_BROADCAST_CAPACITY,
        SessionActorConfig::default(),
    );
    let mut rx = handle.event_tx.subscribe();

    // Collect all Output events from the broadcast for up to 500ms.
    let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_millis(500);
    let mut raw_outputs: Vec<String> = Vec::new();
    loop {
        match rx.try_recv() {
            Ok(SessionEvent::Output { raw, .. }) => {
                raw_outputs.push(raw);
            }
            Ok(_) => {}
            Err(tokio::sync::broadcast::error::TryRecvError::Empty) => {
                if tokio::time::Instant::now() >= deadline {
                    break;
                }
                tokio::task::yield_now().await;
            }
            Err(_) => break,
        }
    }

    assert!(
        raw_outputs
            .iter()
            .any(|r| r.contains("trailing-sentinel-event")),
        "trailing event after AuthPrompt in the same batch must be broadcast; \
         received outputs: {raw_outputs:?}"
    );
    assert!(
        raw_outputs
            .iter()
            .any(|r| r.contains("Normal output before auth")),
        "first event before AuthPrompt must also be broadcast; \
         received outputs: {raw_outputs:?}"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// §6.1 critical observer lag tests (mpsc backpressure design)
// ─────────────────────────────────────────────────────────────────────────────

/// Verify that a CRITICAL observer whose mpsc channel fills triggers
/// `ObserverCriticalLag`, `AwaitingIntervention`, and backend teardown.
///
/// Why: §6.1 mandates that a lagging critical observer causes `AwaitingIntervention`
/// and the actor must NOT silently skip or resubscribe the observer. The mpsc
/// design gives real backpressure visibility: `try_send` returning `Full` means
/// the ACTUAL consumer has fallen behind (not a proxy the actor drains itself).
/// What: uses a channel-fed backend so the test controls when events arrive.
/// Registers a critical observer but never drains its mpsc Receiver. Sends
/// enough events to fill the mpsc (capacity = CRITICAL_OBSERVER_CAPACITY + 1).
/// Waits (event-driven via broadcast + timeout) for `ObserverCriticalLag` and
/// then asserts AwaitingIntervention + backend stop() called.
/// Test: this test; would fail if lag detection used a self-drained broadcast
/// clone because that proxy would never report Full.
#[tokio::test]
async fn critical_observer_lag_yields_awaiting_intervention() {
    let id = ControlSessionId::new("crit-proj", 0);

    let stop_called = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stop_flag_clone = Arc::clone(&stop_called);

    let (backend_tx, backend_rx) = mpsc::channel::<Option<anyhow::Result<Vec<SessionEvent>>>>(32);
    let backend = ChanBackend {
        rx: backend_rx,
        stop_flag: Some(stop_flag_clone),
    };

    let (command_tx, command_rx) = mpsc::channel::<ActorCommand>(32);
    // Use 4× the overflow batch size so the broadcast ring never wraps.
    // The lag test sends CRITICAL_OBSERVER_CAPACITY + 2 events; if the broadcast
    // ring is only DEFAULT_BROADCAST_CAPACITY (256) wide the receiver gets
    // RecvError::Lagged and can miss the ObserverCriticalLag sentinel. A ring
    // of 4× ensures every event — including ObserverCriticalLag — is visible.
    let bcast_cap = (CRITICAL_OBSERVER_CAPACITY + 16) * 4;
    let (event_tx, _) = broadcast::channel::<SessionEvent>(bcast_cap);
    let metadata = Arc::new(RwLock::new(SessionMetadata::new(
        id.clone(),
        "crit-proj".into(),
        BackendKind::StreamJson,
    )));
    let handle = make_handle(
        &id,
        "crit-proj",
        command_tx,
        event_tx.clone(),
        Arc::clone(&metadata),
    );

    // Subscribe to the broadcast BEFORE spawning so we don't miss events.
    let mut bcast_rx = event_tx.subscribe();

    // Spawn actor (blocks on backend.recv() immediately).
    tokio::spawn(run_actor(
        id.clone(),
        backend,
        BackendKind::StreamJson,
        command_rx,
        event_tx.clone(),
        Arc::clone(&metadata),
        SessionActorConfig::default(),
    ));

    // Register critical observer and wait for the mpsc Receiver.
    // The actor is blocked in backend.recv() so the command arm wins.
    let (sub_tx, sub_rx) = tokio::sync::oneshot::channel();
    handle
        .command_tx
        .send(ActorCommand::SubscribeCritical(sub_tx))
        .await
        .unwrap();
    // Hold the Receiver but never poll it — the channel will fill up when
    // the actor try_sends events, causing Err(Full) on the (capacity+1)th event.
    let _critical_rx = sub_rx.await.expect("actor must reply to SubscribeCritical");

    // Send CRITICAL_OBSERVER_CAPACITY + 2 events to overflow the mpsc.
    // Each try_send on a Full channel increments the dropped counter.
    let overflow_count = CRITICAL_OBSERVER_CAPACITY + 2;
    let big_batch: Vec<SessionEvent> = (0..overflow_count as u32)
        .map(|i| SessionEvent::Output {
            session_id: id.clone(),
            raw: format!("overflow-{i}"),
            structured: None,
            ts: Utc::now(),
        })
        .collect();
    backend_tx
        .send(Some(Ok(big_batch)))
        .await
        .expect("backend channel open");

    // Wait (event-driven) for ObserverCriticalLag on the broadcast, with a
    // 2-second timeout. This is immune to scheduler jitter: the assertion only
    // fires after the actor has actually processed the batch and emitted the
    // event, not after an arbitrary sleep.
    //
    // Note: we overflow_count > DEFAULT_BROADCAST_CAPACITY events through the
    // broadcast, so the receiver may get RecvError::Lagged (ring overflowed).
    // Lagged is not fatal — we re-enter recv() and will catch ObserverCriticalLag
    // or Stopped if they're still in the ring. RecvError::Closed is terminal.
    let lag_seen = tokio::time::timeout(tokio::time::Duration::from_secs(2), async {
        loop {
            match bcast_rx.recv().await {
                Ok(SessionEvent::ObserverCriticalLag { .. }) => break true,
                Ok(_) => {} // keep draining other events
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    // Ring overflowed — some events were dropped from the broadcast.
                    // Continue polling; ObserverCriticalLag may still be in the ring
                    // or we'll see the Stopped event that follows it.
                }
                Err(broadcast::error::RecvError::Closed) => break false,
            }
        }
    })
    .await
    .unwrap_or(false);

    assert!(
        lag_seen,
        "ObserverCriticalLag event must be emitted when critical mpsc is full"
    );

    let m = handle.metadata.read().await;
    assert_eq!(
        m.state,
        SessionState::AwaitingIntervention,
        "critical lag must transition to AwaitingIntervention, got: {}",
        m.state
    );

    assert!(
        stop_called.load(std::sync::atomic::Ordering::SeqCst),
        "backend stop() must be called on critical-lag path (no resource leak)"
    );
}

/// Verify that a critical observer that drops its Receiver does NOT kill the
/// session (Disconnected ≠ Full).
///
/// Why: `Err(TrySendError::Disconnected)` just means the observer went away
/// cleanly — the session is healthy and must continue running. Only
/// `Err(TrySendError::Full)` (genuine backpressure) triggers AwaitingIntervention.
/// What: registers a critical observer, drops its Receiver, then sends more
/// output. Waits for the session to reach a terminal state (clean exit) and
/// asserts it is NOT AwaitingIntervention.
/// Test: this test.
#[tokio::test]
async fn critical_observer_disconnected_keeps_session_running() {
    let id = ControlSessionId::new("disc-proj", 0);

    let (backend_tx, backend_rx) = mpsc::channel::<Option<anyhow::Result<Vec<SessionEvent>>>>(32);
    let backend = ChanBackend {
        rx: backend_rx,
        stop_flag: None,
    };

    let (command_tx, command_rx) = mpsc::channel::<ActorCommand>(32);
    let (event_tx, _) = broadcast::channel::<SessionEvent>(DEFAULT_BROADCAST_CAPACITY);
    let metadata = Arc::new(RwLock::new(SessionMetadata::new(
        id.clone(),
        "disc-proj".into(),
        BackendKind::StreamJson,
    )));
    let handle = make_handle(
        &id,
        "disc-proj",
        command_tx,
        event_tx.clone(),
        Arc::clone(&metadata),
    );

    let mut bcast_rx = event_tx.subscribe();

    tokio::spawn(run_actor(
        id.clone(),
        backend,
        BackendKind::StreamJson,
        command_rx,
        event_tx.clone(),
        Arc::clone(&metadata),
        SessionActorConfig::default(),
    ));

    // Subscribe, receive the Receiver, then immediately drop it (simulates the
    // critical observer going away before any events arrive).
    let (sub_tx, sub_rx) = tokio::sync::oneshot::channel();
    handle
        .command_tx
        .send(ActorCommand::SubscribeCritical(sub_tx))
        .await
        .unwrap();
    let critical_rx = sub_rx.await.expect("actor must reply to SubscribeCritical");
    // Drop the Receiver — actor's next try_send will get Disconnected.
    drop(critical_rx);

    // Send some events — actor should detect Disconnected, clear critical_tx,
    // and keep running without entering AwaitingIntervention.
    let events: Vec<SessionEvent> = (0..5u32)
        .map(|i| SessionEvent::Output {
            session_id: id.clone(),
            raw: format!("after-disconnect-{i}"),
            structured: None,
            ts: Utc::now(),
        })
        .collect();
    backend_tx.send(Some(Ok(events))).await.unwrap();
    // Signal clean exit.
    backend_tx.send(None).await.unwrap();

    // Wait (event-driven) for Stopped event to confirm the actor exited cleanly.
    let stopped = tokio::time::timeout(tokio::time::Duration::from_secs(2), async {
        loop {
            match bcast_rx.recv().await {
                Ok(SessionEvent::Stopped { .. }) => break true,
                Ok(_) => {}
                Err(_) => break false,
            }
        }
    })
    .await
    .unwrap_or(false);

    assert!(
        stopped,
        "session should reach Stopped after clean backend exit"
    );

    let m = handle.metadata.read().await;
    assert_ne!(
        m.state,
        SessionState::AwaitingIntervention,
        "Disconnected critical observer must NOT trigger AwaitingIntervention; \
         got: {}",
        m.state
    );
}

/// Verify that a second `SubscribeCritical` while the first observer is still
/// connected is rejected, not silently evicting the first observer.
///
/// Why: §6.1 intends exactly one critical observer per session. Silently
/// replacing the first would cause it to lose all subsequent events without
/// knowing, defeating the purpose of the critical-observer contract.
/// What: registers a critical observer, holds its Receiver, then issues a second
/// SubscribeCritical. Asserts that subscribe_critical() returns None (rejected)
/// and that the first Receiver is still functional (actor can still send to it).
/// Test: this test.
#[tokio::test]
async fn critical_observer_double_subscribe_rejects_second() {
    let id = ControlSessionId::new("dbl-proj", 0);

    let (_backend_tx, backend_rx) = mpsc::channel::<Option<anyhow::Result<Vec<SessionEvent>>>>(32);
    let backend = ChanBackend {
        rx: backend_rx,
        stop_flag: None,
    };

    let (command_tx, command_rx) = mpsc::channel::<ActorCommand>(32);
    let (event_tx, _) = broadcast::channel::<SessionEvent>(DEFAULT_BROADCAST_CAPACITY);
    let metadata = Arc::new(RwLock::new(SessionMetadata::new(
        id.clone(),
        "dbl-proj".into(),
        BackendKind::StreamJson,
    )));
    // handle only needed to keep metadata Arc alive; command_tx used directly.
    let _handle = make_handle(
        &id,
        "dbl-proj",
        command_tx.clone(),
        event_tx.clone(),
        Arc::clone(&metadata),
    );

    tokio::spawn(run_actor(
        id.clone(),
        backend,
        BackendKind::StreamJson,
        command_rx,
        event_tx.clone(),
        Arc::clone(&metadata),
        SessionActorConfig::default(),
    ));

    // First subscription — should succeed.
    let (sub_tx1, sub_rx1) = tokio::sync::oneshot::channel();
    command_tx
        .send(ActorCommand::SubscribeCritical(sub_tx1))
        .await
        .unwrap();
    let first_rx = sub_rx1.await.expect("first SubscribeCritical must succeed");

    // Second subscription while first is still alive — must be rejected.
    // The actor drops the reply_tx, so rx2.await returns Err(RecvError).
    let (sub_tx2, sub_rx2) = tokio::sync::oneshot::channel();
    command_tx
        .send(ActorCommand::SubscribeCritical(sub_tx2))
        .await
        .unwrap();
    // Give actor a moment to process the command.
    tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

    let second_result = sub_rx2.await;
    assert!(
        second_result.is_err(),
        "second SubscribeCritical must be rejected (reply_tx dropped) \
         while first observer is connected"
    );

    // The first Receiver is NOT dropped — the actor's critical_tx still
    // points to the original pair. Verify by ensuring first_rx is still open
    // (not closed from the actor side).
    assert!(
        !first_rx.is_closed(),
        "first critical Receiver must remain open after a rejected second subscription"
    );
}

/// Companion test: verify that a critical observer that IS keeping up does NOT
/// trigger a false `ObserverCriticalLag`.
///
/// Why: the lag detection must be a real backpressure signal, not an accidental
/// one. If try_send fires spuriously the session would enter AwaitingIntervention
/// during normal operation.
/// What: registers a critical observer and actively drains it in a background
/// task. Sends a burst of events (less than CRITICAL_OBSERVER_CAPACITY). Asserts
/// that the session stays in Running or Stopped (clean exit), never
/// AwaitingIntervention.
/// Test: this test; would trivially pass with a self-drained proxy because a
/// self-drained receiver always drains synchronously — but with the mpsc design
/// this test exercises the real consumer path.
#[tokio::test]
async fn critical_observer_no_false_positive_when_keeping_up() {
    let id = ControlSessionId::new("nofp-proj", 0);

    let (backend_tx, backend_rx) = mpsc::channel::<Option<anyhow::Result<Vec<SessionEvent>>>>(32);
    let backend = ChanBackend {
        rx: backend_rx,
        stop_flag: None,
    };

    let (command_tx, command_rx) = mpsc::channel::<ActorCommand>(32);
    let (event_tx, _) = broadcast::channel::<SessionEvent>(DEFAULT_BROADCAST_CAPACITY);
    let metadata = Arc::new(RwLock::new(SessionMetadata::new(
        id.clone(),
        "nofp-proj".into(),
        BackendKind::StreamJson,
    )));
    let handle = make_handle(
        &id,
        "nofp-proj",
        command_tx,
        event_tx.clone(),
        Arc::clone(&metadata),
    );

    tokio::spawn(run_actor(
        id.clone(),
        backend,
        BackendKind::StreamJson,
        command_rx,
        event_tx.clone(),
        Arc::clone(&metadata),
        SessionActorConfig::default(),
    ));

    // Register critical observer.
    let (sub_tx, sub_rx) = tokio::sync::oneshot::channel();
    handle
        .command_tx
        .send(ActorCommand::SubscribeCritical(sub_tx))
        .await
        .unwrap();
    let mut critical_rx = sub_rx.await.expect("actor must reply");

    // Drain the critical observer in a background task (keeping up).
    tokio::spawn(async move { while critical_rx.recv().await.is_some() {} });

    // Send a moderate burst (well under CRITICAL_OBSERVER_CAPACITY).
    let burst: Vec<SessionEvent> = (0..10u32)
        .map(|i| SessionEvent::Output {
            session_id: id.clone(),
            raw: format!("burst-{i}"),
            structured: None,
            ts: Utc::now(),
        })
        .collect();
    backend_tx.send(Some(Ok(burst))).await.unwrap();
    // Clean exit.
    drop(backend_tx);

    tokio::time::sleep(tokio::time::Duration::from_millis(300)).await;

    let m = handle.metadata.read().await;
    assert_ne!(
        m.state,
        SessionState::AwaitingIntervention,
        "no false positive: state should not be AwaitingIntervention when \
         consumer keeps up; got: {}",
        m.state
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// WI-5 #1649: auth-timeout auto-stop timer tests
// ─────────────────────────────────────────────────────────────────────────────

/// Helper: build the explicit-channel test rig and spawn the actor with a given
/// backend, backend kind, and actor config. Returns the handle, a pre-subscribed
/// broadcast receiver, and the backend command sender (for feeding events).
///
/// Why: the auth-timeout and classifier tests share the same channel-fed setup;
/// a helper keeps each test focused on its assertion.
/// What: wires command/event/metadata channels, subscribes a broadcast receiver
/// before spawn (so no event is missed), spawns `run_actor`, and returns the
/// handle + receiver + backend sender.
/// Test: used by `actor_auth_timeout_*` and `actor_classifier_*`.
/// Backend feed channel: the sender side a test uses to push event batches
/// (`Some(Ok(..))`), a clean exit (`None`), or an error (`Some(Err(..))`).
type BackendFeed = mpsc::Sender<Option<anyhow::Result<Vec<SessionEvent>>>>;

/// The rig returned by [`spawn_chan_actor`]: the actor handle, a pre-subscribed
/// broadcast receiver, and the backend feed sender.
type ChanActorRig = (
    SessionActorHandle,
    broadcast::Receiver<SessionEvent>,
    BackendFeed,
);

fn spawn_chan_actor(
    id: &ControlSessionId,
    backend_kind: BackendKind,
    actor_config: SessionActorConfig,
) -> ChanActorRig {
    let (backend_tx, backend_rx) = mpsc::channel::<Option<anyhow::Result<Vec<SessionEvent>>>>(32);
    let backend = ChanBackend {
        rx: backend_rx,
        stop_flag: None,
    };
    let (command_tx, command_rx) = mpsc::channel::<ActorCommand>(32);
    let (event_tx, _) = broadcast::channel::<SessionEvent>(DEFAULT_BROADCAST_CAPACITY);
    let metadata = Arc::new(RwLock::new(SessionMetadata::new(
        id.clone(),
        "auth-proj".into(),
        backend_kind,
    )));
    let handle = make_handle(
        id,
        "auth-proj",
        command_tx,
        event_tx.clone(),
        Arc::clone(&metadata),
    );
    let bcast_rx = event_tx.subscribe();
    tokio::spawn(run_actor(
        id.clone(),
        backend,
        backend_kind,
        command_rx,
        event_tx.clone(),
        metadata,
        actor_config,
    ));
    (handle, bcast_rx, backend_tx)
}

/// Await a terminal state on `handle.metadata`, polling up to `timeout`.
///
/// Why: condition-based waiting avoids brittle fixed sleeps; the test asserts on
/// the state the actor lands in, not on wall-clock timing.
/// What: loops reading metadata until `is_terminal()` or the timeout elapses,
/// returning the final observed `SessionState`.
/// Test: used by `actor_auth_timeout_*`.
async fn await_terminal(
    handle: &SessionActorHandle,
    timeout: tokio::time::Duration,
) -> SessionState {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let state = { handle.metadata.read().await.state.clone() };
        if state.is_terminal() || tokio::time::Instant::now() >= deadline {
            return state;
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(5)).await;
    }
}

/// A session stuck in `AwaitingAuth` past the (injected) auth timeout
/// auto-transitions to terminal `AuthFailed` and stops (#1649, §4.4).
///
/// Why: WI-5 added `auth_timeout_secs` but the auto-stop TIMER was not wired
/// into the actor loop. This test proves the timer fires.
/// What: uses `start_paused = true` (Tokio virtual clock) to advance time
/// deterministically — no real wall-clock sleep. Spawns a tmux actor with a
/// 5 s timeout, feeds an auth-prompt output (tmux → `AwaitingAuth`), then
/// advances virtual time by 10 s and asserts `AuthFailed`.
/// Test: this test.
#[tokio::test(start_paused = true)]
async fn actor_auth_timeout_fires_and_stops() {
    let id = ControlSessionId::new("auth-to", 0);
    let cfg = SessionActorConfig {
        auth_timeout: tokio::time::Duration::from_secs(5),
        classifier: None,
    };
    let (handle, _bcast, backend_tx) = spawn_chan_actor(&id, BackendKind::Tmux, cfg);

    // Feed an auth-prompt output: tmux backend → AwaitingAuth (not terminal).
    let auth_event = SessionEvent::Output {
        session_id: id.clone(),
        raw: "Please log in to claude.ai to continue".into(),
        structured: None,
        ts: Utc::now(),
    };
    backend_tx.send(Some(Ok(vec![auth_event]))).await.unwrap();
    // Yield so the actor task processes the output and enters AwaitingAuth.
    tokio::task::yield_now().await;

    // Advance virtual time past the timeout — no real wall-clock delay.
    tokio::time::advance(tokio::time::Duration::from_secs(10)).await;
    // Yield so the actor task wakes from sleep_until and handles the timeout.
    tokio::task::yield_now().await;

    // Allow up to a few more yields for state propagation (all virtual, no sleep).
    for _ in 0..20 {
        let state = handle.metadata.read().await.state.clone();
        if state.is_terminal() {
            break;
        }
        tokio::task::yield_now().await;
    }

    let state = handle.metadata.read().await.state.clone();
    assert_eq!(
        state,
        SessionState::AuthFailed,
        "session stuck in AwaitingAuth past the timeout must become AuthFailed; got: {state}"
    );
}

/// A session that does NOT enter `AwaitingAuth` is never auto-stopped by the
/// auth timer, even past its configured timeout (#1649).
///
/// Why: the timer must be armed ONLY in `AwaitingAuth`. A session in `Running`
/// with a configured timeout must not be spuriously killed.
/// What: uses `start_paused = true` (Tokio virtual clock). Spawns a tmux actor
/// with a 5 s timeout, feeds NORMAL output (no auth prompt), advances virtual
/// time well past the timeout, then signals clean exit and asserts `Stopped`.
/// Test: this test.
#[tokio::test(start_paused = true)]
async fn actor_auth_timeout_does_not_fire_without_awaiting_auth() {
    let id = ControlSessionId::new("auth-noto", 0);
    let cfg = SessionActorConfig {
        auth_timeout: tokio::time::Duration::from_secs(5),
        classifier: None,
    };
    let (handle, _bcast, backend_tx) = spawn_chan_actor(&id, BackendKind::Tmux, cfg);

    // Normal (non-auth) output: no auth-prompt regex match → stays in Running.
    let normal = SessionEvent::Output {
        session_id: id.clone(),
        raw: "Tool use: Bash".into(),
        structured: None,
        ts: Utc::now(),
    };
    backend_tx.send(Some(Ok(vec![normal]))).await.unwrap();
    // Yield so the actor processes the output.
    tokio::task::yield_now().await;

    // Advance virtual time well past the timeout — the timer must NOT fire
    // because the session is NOT in AwaitingAuth (the deadline is never armed).
    tokio::time::advance(tokio::time::Duration::from_secs(30)).await;
    tokio::task::yield_now().await;

    // Then signal a clean exit.
    backend_tx.send(None).await.unwrap();
    tokio::task::yield_now().await;

    // A few more yields for state propagation.
    for _ in 0..20 {
        let state = handle.metadata.read().await.state.clone();
        if state.is_terminal() {
            break;
        }
        tokio::task::yield_now().await;
    }

    let state = handle.metadata.read().await.state.clone();
    assert_eq!(
        state,
        SessionState::Stopped,
        "a session that never awaits auth must complete cleanly, never AuthFailed; got: {state}"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// WI-5 #1648: classifier fallback wiring tests
// ─────────────────────────────────────────────────────────────────────────────

use crate::control::classifier::ActivityClassifier;
use crate::control::event::ActivityKind;

/// A mock classifier that records whether it was called and returns a fixed
/// label (or an injected error).
///
/// Why: #1648 must prove the actor (a) makes ZERO classifier calls when the
/// gate is off (classifier `None`), and (b) calls the seam and surfaces the
/// label when the gate is on — all WITHOUT real network or an OpenRouter key.
/// What: `classify` flips `called` and returns `Ok(Some(result))` for `Ok`
/// variants, or `Err` for `Err` variants, matching the updated trait signature.
/// Test: `actor_classifier_*`.
struct MockClassifier {
    called: Arc<AtomicBool>,
    result: Result<ActivityKind, String>,
}

#[async_trait::async_trait]
impl ActivityClassifier for MockClassifier {
    async fn classify(&self, _raw_output: &str) -> Result<Option<ActivityKind>, String> {
        self.called.store(true, std::sync::atomic::Ordering::SeqCst);
        match &self.result {
            Ok(kind) => Ok(Some(kind.clone())),
            Err(e) => Err(e.clone()),
        }
    }
}

// NOTE: "gate-OFF makes no call" coverage is NOT an actor-level test — it is
// structurally impossible to make meaningful at the actor level. When
// `SessionActorConfig::classifier = None` there is no mock to wire, so any
// assertion about a separate mock staying false would be vacuous (the actor can
// never reach a mock it was never given). The gate DECISION tests live at the
// registry level where the gate is actually evaluated:
//   `registry_actor_config_gate_off_drops_classifier`  → gate closed  → None
//   `registry_actor_config_gate_on_attaches_classifier` → gate open → Some(_)
// See `crates/trusty-mpm/src/control/registry.rs` for those tests.

/// Gate-ON: the actor invokes the injected classifier on output the regex layer
/// could not classify and surfaces the returned label as `ActivityParsed` (#1648).
///
/// Why: WI-5 shipped only the gate; this proves the call is actually wired and
/// the label flows into the activity stream / metadata.
/// What: injects a `MockClassifier` returning `RateLimited`, feeds unmatched
/// output, and asserts (a) the classifier was called and (b) `last_summary`
/// reflects the classified label.
/// Test: this test.
#[tokio::test]
async fn actor_classifier_gate_on_invokes_and_surfaces_label() {
    let id = ControlSessionId::new("clf-on", 0);
    let called = Arc::new(AtomicBool::new(false));
    let mock = Arc::new(MockClassifier {
        called: Arc::clone(&called),
        result: Ok(ActivityKind::RateLimited),
    });
    let cfg = SessionActorConfig {
        auth_timeout: tokio::time::Duration::from_secs(30),
        classifier: Some(mock),
    };
    let (handle, _bcast, backend_tx) = spawn_chan_actor(&id, BackendKind::StreamJson, cfg);

    let gibberish = SessionEvent::Output {
        session_id: id.clone(),
        raw: "zzxq unmatched gibberish no pattern here".into(),
        structured: None,
        ts: Utc::now(),
    };
    backend_tx.send(Some(Ok(vec![gibberish]))).await.unwrap();
    backend_tx.send(None).await.unwrap();

    let _ = await_terminal(&handle, tokio::time::Duration::from_secs(2)).await;
    assert!(
        called.load(std::sync::atomic::Ordering::SeqCst),
        "gate-on must invoke the injected classifier on unmatched output"
    );
    let m = handle.metadata.read().await;
    assert!(
        m.last_summary
            .as_deref()
            .unwrap_or("")
            .contains("llm-classified"),
        "classified label must surface into last_summary; got: {:?}",
        m.last_summary
    );
}

/// A classifier FAILURE is fail-open: it logs and the session continues to a
/// clean terminal state, never breaking supervision (#1648).
///
/// Why: §8.3 mandates the classifier is best-effort; a failure must never abort
/// or stall the session.
/// What: injects a classifier that always errors, feeds unmatched output and a
/// clean exit, and asserts the session reaches `Stopped` (clean) — not a hung
/// or `Failed` state — proving the error was swallowed.
/// Test: this test.
#[tokio::test]
async fn actor_classifier_failure_is_fail_open() {
    let id = ControlSessionId::new("clf-fail", 0);
    let called = Arc::new(AtomicBool::new(false));
    let mock = Arc::new(MockClassifier {
        called: Arc::clone(&called),
        result: Err("simulated openrouter outage".to_string()),
    });
    let cfg = SessionActorConfig {
        auth_timeout: tokio::time::Duration::from_secs(30),
        classifier: Some(mock),
    };
    let (handle, _bcast, backend_tx) = spawn_chan_actor(&id, BackendKind::StreamJson, cfg);

    let gibberish = SessionEvent::Output {
        session_id: id.clone(),
        raw: "zzxq unmatched gibberish no pattern here".into(),
        structured: None,
        ts: Utc::now(),
    };
    backend_tx.send(Some(Ok(vec![gibberish]))).await.unwrap();
    backend_tx.send(None).await.unwrap();

    let state = await_terminal(&handle, tokio::time::Duration::from_secs(2)).await;
    assert!(
        called.load(std::sync::atomic::Ordering::SeqCst),
        "the classifier must have been invoked"
    );
    assert_eq!(
        state,
        SessionState::Stopped,
        "a classifier failure must be fail-open: session completes cleanly; got: {state}"
    );
}
