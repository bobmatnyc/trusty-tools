//! Unit tests for `control::actor` (WI-1 through WI-3).
//!
//! Why: keeping tests in a sibling file (rather than inline in actor.rs) lets
//! actor.rs stay within the 500-SLOC production cap while still giving tests
//! access to the private `run_actor` function and the `StubBackend` helper.
//! What: covers lifecycle (Started / Stopped events), command dispatch (Stop),
//! write-lock CAS, §8.2 activity parse (ActivityParsed / PendingDecision), and
//! critical observer lag detection per §6.1.
//! Test: these tests are the coverage for the items described above; run with
//! `cargo test -p trusty-mpm control::actor_tests`.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use chrono::Utc;
use tokio::sync::{RwLock, broadcast, mpsc};

use crate::control::actor::{
    ActorCommand, DEFAULT_BROADCAST_CAPACITY, SessionActorHandle, run_actor, spawn_actor,
};
use crate::control::backend::{SessionBackend, SessionInput};
use crate::control::event::{BackendKind, SessionEvent};
use crate::control::id::ControlSessionId;
use crate::control::state::{SessionMetadata, SessionState};

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

// ── WI-3 tests ───────────────────────────────────────────────────────────

/// Verify that an `Output` event containing a tool-use line causes the actor
/// to emit `ActivityParsed` and update `last_summary` in metadata.
///
/// Why: §8.2 requires the regex parse layer to produce `ActivityParsed` after
/// every `Output` event that matches a known pattern.
/// What: spawns an actor with a backend that produces one tool-use output,
/// waits for the actor to process it, then checks metadata.last_summary.
/// Test: this test.
#[tokio::test]
async fn actor_activity_parsed_on_output() {
    let id = ControlSessionId::new("parse-proj", 0);

    // Build an Output event with tool-use text.
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
    );

    // Wait for actor to process the output + exit.
    tokio::time::sleep(tokio::time::Duration::from_millis(150)).await;

    let m = handle.metadata.read().await;
    assert!(
        m.last_summary.is_some(),
        "last_summary should be populated after a tool-use output event"
    );
    let summary = m.last_summary.as_deref().unwrap_or("");
    assert!(
        summary.contains("Bash"),
        "last_summary should mention 'Bash', got: {summary:?}"
    );
}

/// Verify that a `[Y/n]` prompt causes `pending_decision` and
/// `proposed_default` to be set in metadata.
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

/// Verify that a CRITICAL observer that lags causes `ObserverCriticalLag` and
/// `AwaitingIntervention` per §6.1 of SPEC-SESSCTL-01.
///
/// Why: §6.1 mandates critical observers must NOT be silently resubscribed.
/// What: uses a channel-fed backend so we control exactly when each batch
/// arrives. Registers the critical observer (and waits for the oneshot reply to
/// confirm the actor has processed SubscribeCritical), then sends batch1 (8
/// events — overflows ring-4) followed immediately by batch2 (1 event — wakes
/// the actor's backend arm and triggers try_recv on the lagged critical_rx).
/// Verifies ObserverCriticalLag is emitted and/or state is AwaitingIntervention.
/// Test: this test.
#[tokio::test]
async fn critical_observer_lag_yields_awaiting_intervention() {
    let id = ControlSessionId::new("crit-proj", 0);

    // Channel-fed backend: the test drives event delivery via `backend_tx`.
    let (backend_tx, backend_rx) =
        tokio::sync::mpsc::channel::<Option<anyhow::Result<Vec<SessionEvent>>>>(16);

    struct ChanBackend {
        rx: tokio::sync::mpsc::Receiver<Option<anyhow::Result<Vec<SessionEvent>>>>,
    }
    #[async_trait::async_trait]
    impl SessionBackend for ChanBackend {
        async fn send(&mut self, _: SessionInput) -> anyhow::Result<()> {
            Ok(())
        }
        async fn recv(&mut self) -> Option<anyhow::Result<Vec<SessionEvent>>> {
            // recv() returns None when the sender is dropped (clean exit).
            self.rx.recv().await.unwrap_or_default()
        }
        async fn stop(self: Box<Self>) -> anyhow::Result<()> {
            Ok(())
        }
    }

    let backend = ChanBackend { rx: backend_rx };

    // Tiny ring (4) so 8 output events overflow it.
    let (command_tx, command_rx) = mpsc::channel::<ActorCommand>(32);
    let (event_tx, _) = broadcast::channel::<SessionEvent>(4);
    let metadata = Arc::new(RwLock::new(SessionMetadata::new(
        id.clone(),
        "crit-proj".into(),
        BackendKind::StreamJson,
    )));
    let handle = SessionActorHandle {
        command_tx,
        event_tx: event_tx.clone(),
        write_lock_held: Arc::new(AtomicBool::new(false)),
        metadata: Arc::clone(&metadata),
    };

    // Subscribe a watcher for ObserverCriticalLag.
    let mut watcher = event_tx.subscribe();

    // Spawn the actor (backend.recv() blocks immediately waiting for our channel).
    tokio::spawn(run_actor(
        id.clone(),
        backend,
        BackendKind::StreamJson,
        command_rx,
        event_tx.clone(),
        Arc::clone(&metadata),
    ));

    // Step 1: register critical observer and wait for the oneshot reply.
    // The actor is blocked in backend.recv() so the command arm wins here.
    let (sub_tx, sub_rx) = tokio::sync::oneshot::channel();
    handle
        .command_tx
        .send(ActorCommand::SubscribeCritical(sub_tx))
        .await
        .unwrap();
    // Waiting on sub_rx guarantees the actor has processed SubscribeCritical
    // and set its internal critical_rx before we send any events.
    let _critical_rx = sub_rx.await.expect("actor must reply to SubscribeCritical");
    // _critical_rx is never drained — it will lag when the ring fills.

    // Step 2: send batch1 (8 events) to overflow the ring-4.
    let batch1: Vec<SessionEvent> = (0..8u32)
        .map(|i| SessionEvent::Output {
            session_id: id.clone(),
            raw: format!("b1-{i}"),
            structured: None,
            ts: Utc::now(),
        })
        .collect();
    backend_tx
        .send(Some(Ok(batch1)))
        .await
        .expect("backend channel must be open");

    // Step 3: send batch2 — this wakes the actor's backend arm, which runs
    // try_recv on the (now-lagged) critical_rx and emits ObserverCriticalLag.
    let batch2 = vec![SessionEvent::Output {
        session_id: id.clone(),
        raw: "b2-trigger".into(),
        structured: None,
        ts: Utc::now(),
    }];
    backend_tx
        .send(Some(Ok(batch2)))
        .await
        .expect("backend channel must be open");

    // Drop the sender so the actor exits cleanly after processing batch2.
    drop(backend_tx);

    // Wait for both batches to process.
    tokio::time::sleep(tokio::time::Duration::from_millis(300)).await;

    // Collect ObserverCriticalLag from the watcher.
    let mut found_event = false;
    loop {
        match watcher.try_recv() {
            Ok(SessionEvent::ObserverCriticalLag { .. }) => {
                found_event = true;
                break;
            }
            Ok(_) => {}
            Err(_) => break,
        }
    }
    let m = handle.metadata.read().await;
    let is_intervention = m.state == SessionState::AwaitingIntervention;

    assert!(
        found_event || is_intervention,
        "critical lag: ObserverCriticalLag must be emitted or state must be \
         AwaitingIntervention; found_event={found_event}, state={}",
        m.state
    );
}
