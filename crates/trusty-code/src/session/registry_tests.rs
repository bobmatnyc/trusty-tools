//! Unit tests for `SessionRegistry` (#2054). Split out of `registry.rs` per
//! the crate's `_tests.rs` sibling-file convention (see `intent::classifier_tests`
//! for precedent) to keep the production file under the 500-SLOC cap.

use super::*;
use tokio::sync::broadcast;
use tokio::sync::mpsc;
use tokio::time::{Duration, timeout};

/// Build a throwaway `(NotifySender, receiver)` pair for attach tests.
fn notify_channel() -> (NotifySender, mpsc::UnboundedReceiver<serde_json::Value>) {
    mpsc::unbounded_channel()
}

/// Read from the process-global event bus until an event matching
/// `session_id` arrives, ignoring anything else.
///
/// Why: `crate::events::bus()` is a single process-wide singleton shared by
/// every test in this binary; cargo runs tests concurrently, so a raw
/// `events.recv().await` can observe another test's unrelated event
/// interleaved on the same subscription. Filtering by `session_id` (unique
/// per test via a fresh UUID) makes these tests robust to that interleaving
/// instead of assuming "the very next event is mine". Bounded by a 2s
/// timeout so a genuine bug (event never published) still fails fast
/// instead of hanging.
async fn next_event_for(rx: &mut broadcast::Receiver<Event>, session_id: &str) -> Event {
    timeout(Duration::from_secs(2), async {
        loop {
            let ev = rx.recv().await.expect("event bus closed unexpectedly");
            if ev.session_id() == Some(session_id) {
                return ev;
            }
        }
    })
    .await
    .expect("timed out waiting for an event on this session")
}

/// `create` must publish `SessionStarted` then `SessionStatusChanged` (to
/// `"running"`), and the returned snapshot must already be `Running`.
#[tokio::test]
async fn create_publishes_started_and_status_events() {
    let registry = SessionRegistry::new();
    let mut events = crate::events::subscribe();

    let session = registry.create("do the thing".to_string(), None, Some("proj".to_string()));
    assert_eq!(session.status, SessionStatus::Running);

    let first = next_event_for(&mut events, &session.id).await;
    assert!(matches!(first, Event::SessionStarted { project, .. } if project == "proj"));

    let second = next_event_for(&mut events, &session.id).await;
    assert!(matches!(second, Event::SessionStatusChanged { status, .. } if status == "running"));
}

/// `list` must return every created session.
#[tokio::test]
async fn list_returns_every_created_session() {
    let registry = SessionRegistry::new();
    registry.create("a".to_string(), None, None);
    registry.create("b".to_string(), None, None);
    assert_eq!(registry.list().len(), 2);
}

/// `status` on an unknown id must return `session_not_found`.
#[tokio::test]
async fn status_returns_not_found_for_unknown_id() {
    let registry = SessionRegistry::new();
    let err = registry.status("does-not-exist").unwrap_err();
    assert_eq!(err.code, -32007);
}

/// `send` must publish `SessionInput` carrying the given text.
#[tokio::test]
async fn send_publishes_input_event() {
    let registry = SessionRegistry::new();
    let session = registry.create("t".to_string(), None, None);
    let mut events = crate::events::subscribe();

    registry.send(&session.id, "hello").unwrap();

    let ev = next_event_for(&mut events, &session.id).await;
    assert!(matches!(ev, Event::SessionInput { input, .. } if input == "hello"));
}

/// `send` on an unknown session must error, not panic.
#[tokio::test]
async fn send_unknown_session_errors() {
    let registry = SessionRegistry::new();
    let err = registry.send("nope", "hi").unwrap_err();
    assert_eq!(err.code, -32007);
}

/// `cancel` must transition to `Cancelled` and publish the terminal events.
#[tokio::test]
async fn cancel_transitions_to_cancelled_and_publishes() {
    let registry = SessionRegistry::new();
    let session = registry.create("t".to_string(), None, None);
    let mut events = crate::events::subscribe();

    let cancelled = registry.cancel(&session.id).unwrap();
    assert_eq!(cancelled.status, SessionStatus::Cancelled);

    let status_changed = next_event_for(&mut events, &session.id).await;
    assert!(
        matches!(status_changed, Event::SessionStatusChanged { status, .. } if status == "cancelled")
    );
    let done = next_event_for(&mut events, &session.id).await;
    assert!(matches!(done, Event::SessionDone { status, .. } if status == "cancelled"));

    // Idempotent: cancelling again returns the same terminal snapshot,
    // no error, no further events required.
    let again = registry.cancel(&session.id).unwrap();
    assert_eq!(again.status, SessionStatus::Cancelled);
}

/// `cancel` on an unknown session must error.
#[tokio::test]
async fn cancel_unknown_session_errors() {
    let registry = SessionRegistry::new();
    let err = registry.cancel("nope").unwrap_err();
    assert_eq!(err.code, -32007);
}

/// `attach` must replay the ring buffer accumulated so far.
#[tokio::test]
async fn attach_returns_ring_buffer_replay() {
    let registry = SessionRegistry::new();
    let session = registry.create("t".to_string(), None, None);
    registry.send(&session.id, "one").unwrap();

    let (tx, _rx) = notify_channel();
    let replay = registry.attach(&session.id, Uuid::new_v4(), tx).unwrap();

    // SessionStarted, SessionStatusChanged(running), SessionInput(one).
    assert_eq!(replay.len(), 3);
    assert!(matches!(replay.last().unwrap(), Event::SessionInput { input, .. } if input == "one"));
}

/// After `attach`, a live event published on the session must be forwarded
/// to the connection's notify channel as a JSON-RPC notification.
#[tokio::test]
async fn attach_forwards_live_events_until_detach() {
    let registry = SessionRegistry::new();
    let session = registry.create("t".to_string(), None, None);
    let connection_id = Uuid::new_v4();
    let (tx, mut rx) = notify_channel();

    registry.attach(&session.id, connection_id, tx).unwrap();

    registry.send(&session.id, "live").unwrap();

    let notification = timeout(Duration::from_secs(2), rx.recv())
        .await
        .expect("forwarder must deliver the live event")
        .expect("channel must still be open");
    assert_eq!(notification["method"], "session.event");
    assert_eq!(notification["params"]["session_id"], session.id);
    assert_eq!(notification["params"]["event"]["type"], "session_input");
    assert_eq!(notification["params"]["event"]["input"], "live");

    registry.detach(&session.id, connection_id).unwrap();
    // The forwarder task is cancelled by waking its `select!` on a
    // separate spawned task; give the scheduler a beat to actually run it
    // before publishing the next event, so this assertion isn't racing the
    // cancellation.
    tokio::time::sleep(Duration::from_millis(50)).await;
    registry.send(&session.id, "after-detach").unwrap();

    // Two outcomes both mean "no event arrived": the read times out (the
    // channel is still open but empty), or `recv` resolves to `None`
    // because the cancelled forwarder already dropped its `NotifySender`
    // (this is what actually happens here — the forwarder holds the only
    // sender, so cancelling it closes the channel almost immediately,
    // resolving `recv()` quickly rather than timing out). Only `Some(_)`
    // — an actual forwarded event — is a failure.
    match timeout(Duration::from_millis(300), rx.recv()).await {
        Err(_) => {}   // timed out waiting: nothing arrived, good.
        Ok(None) => {} // channel closed: nothing arrived, good.
        Ok(Some(v)) => panic!("no event should arrive after detach, got {v:?}"),
    }
}

/// `attach` on an unknown session must error.
#[tokio::test]
async fn attach_unknown_session_errors() {
    let registry = SessionRegistry::new();
    let (tx, _rx) = notify_channel();
    let err = registry.attach("nope", Uuid::new_v4(), tx).unwrap_err();
    assert_eq!(err.code, -32007);
}

/// `detach` without a prior `attach` for that connection must be a no-op
/// success, not an error.
#[tokio::test]
async fn detach_without_prior_attach_is_a_noop() {
    let registry = SessionRegistry::new();
    let session = registry.create("t".to_string(), None, None);
    registry.detach(&session.id, Uuid::new_v4()).unwrap();
}

/// `detach` on an unknown session must error.
#[tokio::test]
async fn detach_unknown_session_errors() {
    let registry = SessionRegistry::new();
    let err = registry.detach("nope", Uuid::new_v4()).unwrap_err();
    assert_eq!(err.code, -32007);
}

/// `replay` must return the ring buffer without registering an attachment
/// (no forwarder side effect).
#[tokio::test]
async fn replay_returns_ring_buffer_without_attaching() {
    let registry = SessionRegistry::new();
    let session = registry.create("t".to_string(), None, None);
    let replay = registry.replay(&session.id).unwrap();
    assert_eq!(replay.len(), 2); // SessionStarted + SessionStatusChanged(running)
}

/// The ring buffer must drop the oldest event once it reaches capacity.
#[tokio::test]
async fn ring_buffer_drops_oldest_when_full() {
    let registry = SessionRegistry::with_capacity(2);
    let session = registry.create("t".to_string(), None, None);
    // Ring already has 2 entries (SessionStarted, StatusChanged running) at
    // capacity 2; one more push must evict the oldest (SessionStarted).
    registry.send(&session.id, "evicts-started").unwrap();

    let replay = registry.replay(&session.id).unwrap();
    assert_eq!(replay.len(), 2);
    assert!(matches!(
        replay.first().unwrap(),
        Event::SessionStatusChanged { .. }
    ));
    assert!(
        matches!(replay.last().unwrap(), Event::SessionInput { input, .. } if input == "evicts-started")
    );
}
