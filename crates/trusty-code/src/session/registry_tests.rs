//! Unit tests for `SessionRegistry` (#2054, #2055). Split out of
//! `registry.rs` per the crate's `_tests.rs` sibling-file convention (see
//! `intent::classifier_tests` for precedent) to keep the production file
//! under the 500-SLOC cap.

use super::*;
use tokio::sync::broadcast;
use tokio::sync::mpsc;
use tokio::time::{Duration, timeout};

/// Build a throwaway `(NotifySender, receiver)` pair for attach tests.
fn notify_channel() -> (NotifySender, mpsc::UnboundedReceiver<serde_json::Value>) {
    mpsc::unbounded_channel()
}

/// Read from the process-global event bus until an envelope matching
/// `session_id` arrives, ignoring anything else.
///
/// Why: `crate::events::bus()` is a single process-wide singleton shared by
/// every test in this binary; cargo runs tests concurrently, so a raw
/// `events.recv().await` can observe another test's unrelated envelope
/// interleaved on the same subscription. Filtering by `session_id` (unique
/// per test via a fresh UUID) makes these tests robust to that interleaving
/// instead of assuming "the very next envelope is mine". Bounded by a 2s
/// timeout so a genuine bug (event never published) still fails fast
/// instead of hanging.
async fn next_event_for(
    rx: &mut broadcast::Receiver<SessionEventEnvelope>,
    session_id: &str,
) -> SessionEventEnvelope {
    timeout(Duration::from_secs(2), async {
        loop {
            let envelope = rx.recv().await.expect("event bus closed unexpectedly");
            if envelope.session_id == session_id {
                return envelope;
            }
        }
    })
    .await
    .expect("timed out waiting for an event on this session")
}

/// `create` must publish `SessionStarted` (seq 1) then `SessionStatusChanged`
/// (seq 2, `status: "running"`), and the returned snapshot must already be
/// `Running`.
#[tokio::test]
async fn create_publishes_started_and_status_events() {
    let registry = SessionRegistry::new();
    let mut events = crate::events::subscribe();

    let session = registry.create("do the thing".to_string(), None, Some("proj".to_string()));
    assert_eq!(session.status, SessionStatus::Running);

    let first = next_event_for(&mut events, &session.id).await;
    assert_eq!(first.seq, 1);
    assert_eq!(first.kind, "session_started");
    assert!(matches!(first.event, Event::SessionStarted { project, .. } if project == "proj"));

    let second = next_event_for(&mut events, &session.id).await;
    assert_eq!(second.seq, 2);
    assert!(
        matches!(second.event, Event::SessionStatusChanged { status, .. } if status == "running")
    );
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

    let envelope = next_event_for(&mut events, &session.id).await;
    assert_eq!(envelope.seq, 3); // after SessionStarted(1), StatusChanged(2)
    assert!(matches!(envelope.event, Event::SessionInput { input, .. } if input == "hello"));
}

/// `send` on an unknown session must error, not panic.
#[tokio::test]
async fn send_unknown_session_errors() {
    let registry = SessionRegistry::new();
    let err = registry.send("nope", "hi").unwrap_err();
    assert_eq!(err.code, -32007);
}

/// `cancel` must transition to `Cancelled` and publish the terminal events
/// (`status_changed` -> `session_cancelled` -> `session_done`), each with a
/// strictly increasing `seq`.
#[tokio::test]
async fn cancel_transitions_to_cancelled_and_publishes() {
    let registry = SessionRegistry::new();
    let session = registry.create("t".to_string(), None, None);
    let mut events = crate::events::subscribe();

    let cancelled = registry.cancel(&session.id).unwrap();
    assert_eq!(cancelled.status, SessionStatus::Cancelled);

    let status_changed = next_event_for(&mut events, &session.id).await;
    assert!(
        matches!(status_changed.event, Event::SessionStatusChanged { status, .. } if status == "cancelled")
    );

    let cancelled_event = next_event_for(&mut events, &session.id).await;
    assert!(matches!(
        cancelled_event.event,
        Event::SessionCancelled { .. }
    ));
    assert!(cancelled_event.seq > status_changed.seq);

    let done = next_event_for(&mut events, &session.id).await;
    assert!(matches!(done.event, Event::SessionDone { status, .. } if status == "cancelled"));
    assert!(done.seq > cancelled_event.seq);

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

/// `attach` must replay the ring buffer accumulated so far, in order, with
/// `seq` starting at 1 and increasing by 1 per entry.
#[tokio::test]
async fn attach_returns_ring_buffer_replay() {
    let registry = SessionRegistry::new();
    let session = registry.create("t".to_string(), None, None);
    registry.send(&session.id, "one").unwrap();

    let (tx, _rx) = notify_channel();
    let replay = registry.attach(&session.id, Uuid::new_v4(), tx).unwrap();

    // SessionStarted(1), SessionStatusChanged(2, running), SessionInput(3).
    assert_eq!(replay.len(), 3);
    let seqs: Vec<u64> = replay.iter().map(|e| e.seq).collect();
    assert_eq!(seqs, vec![1, 2, 3]);
    assert!(
        matches!(&replay.last().unwrap().event, Event::SessionInput { input, .. } if input == "one")
    );
}

/// After `attach`, a live event published on the session must be forwarded
/// to the connection's notify channel as a JSON-RPC notification whose
/// `params` is the full envelope.
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
    assert_eq!(notification["params"]["seq"], 3);
    assert_eq!(notification["params"]["kind"], "session_input");
    assert_eq!(notification["params"]["event"]["type"], "session_input");
    assert_eq!(notification["params"]["event"]["input"], "live");
    assert!(notification["params"]["at"].is_string());

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

/// Replay and live events must share ONE gap-free, strictly increasing
/// `seq` — the correctness property the whole envelope design exists for.
#[tokio::test]
async fn attach_replay_then_live_seq_is_contiguous() {
    let registry = SessionRegistry::new();
    let session = registry.create("t".to_string(), None, None);
    registry.send(&session.id, "before-attach").unwrap();

    let (tx, mut rx) = notify_channel();
    let replay = registry.attach(&session.id, Uuid::new_v4(), tx).unwrap();
    let last_replay_seq = replay.last().unwrap().seq;

    registry.send(&session.id, "after-attach").unwrap();
    let notification = timeout(Duration::from_secs(2), rx.recv())
        .await
        .expect("forwarder must deliver the live event")
        .expect("channel must still be open");
    let live_seq = notification["params"]["seq"].as_u64().unwrap();

    assert_eq!(
        live_seq,
        last_replay_seq + 1,
        "no gap and no duplicate between replay and live"
    );

    // The whole replay sequence itself must be gap-free from 1.
    for (i, envelope) in replay.iter().enumerate() {
        assert_eq!(envelope.seq, (i as u64) + 1);
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
        replay.first().unwrap().event,
        Event::SessionStatusChanged { .. }
    ));
    assert!(
        matches!(&replay.last().unwrap().event, Event::SessionInput { input, .. } if input == "evicts-started")
    );
    // Eviction must not reset the seq counter — the surviving entries keep
    // their original sequence numbers (2, 3), not renumbered to (1, 2).
    let seqs: Vec<u64> = replay.iter().map(|e| e.seq).collect();
    assert_eq!(seqs, vec![2, 3]);
}

/// `record_tool_started` must publish a `ToolStarted` event with a
/// truncated `args_preview`.
#[tokio::test]
async fn record_tool_started_publishes_event() {
    let registry = SessionRegistry::new();
    let session = registry.create("t".to_string(), None, None);
    let mut events = crate::events::subscribe();

    registry
        .record_tool_started(&session.id, "bash", "call-1", "ls -la")
        .unwrap();

    let envelope = next_event_for(&mut events, &session.id).await;
    assert_eq!(envelope.kind, "tool_started");
    assert!(matches!(
        envelope.event,
        Event::ToolStarted { tool, call_id, args_preview, .. }
            if tool == "bash" && call_id == "call-1" && args_preview == "ls -la"
    ));
}

/// `record_tool_finished` must publish a `ToolFinished` event carrying
/// `success` and a truncated `result_preview`.
#[tokio::test]
async fn record_tool_finished_publishes_event() {
    let registry = SessionRegistry::new();
    let session = registry.create("t".to_string(), None, None);
    let mut events = crate::events::subscribe();

    registry
        .record_tool_finished(&session.id, "bash", "call-1", true, "done")
        .unwrap();

    let envelope = next_event_for(&mut events, &session.id).await;
    assert_eq!(envelope.kind, "tool_finished");
    assert!(matches!(
        envelope.event,
        Event::ToolFinished { success, result_preview, .. } if success && result_preview == "done"
    ));
}

/// `record_tool_error` must publish a `ToolError` event.
#[tokio::test]
async fn record_tool_error_publishes_event() {
    let registry = SessionRegistry::new();
    let session = registry.create("t".to_string(), None, None);
    let mut events = crate::events::subscribe();

    registry
        .record_tool_error(&session.id, "bash", "call-1", "timed out")
        .unwrap();

    let envelope = next_event_for(&mut events, &session.id).await;
    assert_eq!(envelope.kind, "tool_error");
    assert!(matches!(envelope.event, Event::ToolError { error, .. } if error == "timed out"));
}

/// `record_log` must publish a `Log` event.
#[tokio::test]
async fn record_log_publishes_event() {
    let registry = SessionRegistry::new();
    let session = registry.create("t".to_string(), None, None);
    let mut events = crate::events::subscribe();

    registry
        .record_log(&session.id, "warn", "disk almost full")
        .unwrap();

    let envelope = next_event_for(&mut events, &session.id).await;
    assert_eq!(envelope.kind, "log");
    assert!(
        matches!(envelope.event, Event::Log { level, message, .. } if level == "warn" && message == "disk almost full")
    );
}

/// `record_progress` must publish a `Progress` event.
#[tokio::test]
async fn record_progress_publishes_event() {
    let registry = SessionRegistry::new();
    let session = registry.create("t".to_string(), None, None);
    let mut events = crate::events::subscribe();

    registry
        .record_progress(&session.id, "indexing", Some(0.5))
        .unwrap();

    let envelope = next_event_for(&mut events, &session.id).await;
    assert_eq!(envelope.kind, "progress");
    assert!(matches!(envelope.event, Event::Progress { percent: Some(p), .. } if p == 0.5));
}

/// `record_message` must publish a `Message` event.
#[tokio::test]
async fn record_message_publishes_event() {
    let registry = SessionRegistry::new();
    let session = registry.create("t".to_string(), None, None);
    let mut events = crate::events::subscribe();

    registry.record_message(&session.id, "hello world").unwrap();

    let envelope = next_event_for(&mut events, &session.id).await;
    assert_eq!(envelope.kind, "message");
    assert!(matches!(envelope.event, Event::Message { text, .. } if text == "hello world"));
}

/// Every `record_*` emission-plumbing method must reject an unknown
/// session id with `session_not_found` rather than panicking.
#[tokio::test]
async fn record_plumbing_methods_reject_unknown_session() {
    let registry = SessionRegistry::new();
    assert_eq!(
        registry
            .record_tool_started("nope", "t", "c", "a")
            .unwrap_err()
            .code,
        -32007
    );
    assert_eq!(
        registry
            .record_tool_finished("nope", "t", "c", true, "r")
            .unwrap_err()
            .code,
        -32007
    );
    assert_eq!(
        registry
            .record_tool_error("nope", "t", "c", "e")
            .unwrap_err()
            .code,
        -32007
    );
    assert_eq!(
        registry.record_log("nope", "info", "m").unwrap_err().code,
        -32007
    );
    assert_eq!(
        registry
            .record_progress("nope", "m", None)
            .unwrap_err()
            .code,
        -32007
    );
    assert_eq!(
        registry.record_message("nope", "m").unwrap_err().code,
        -32007
    );
}

// ── #2056: execution-lifecycle tracking ─────────────────────────────────────────

/// `begin_execution` on an unknown session must error.
#[tokio::test]
async fn begin_execution_unknown_session_errors() {
    let registry = SessionRegistry::new();
    let err = registry.begin_execution("nope").unwrap_err();
    assert_eq!(err.code, -32007);
}

/// `begin_execution` on an already-terminal session must be rejected.
#[tokio::test]
async fn begin_execution_rejects_terminal_session() {
    let registry = SessionRegistry::new();
    let session = registry.create("t".to_string(), None, None);
    registry.cancel(&session.id).unwrap();

    let err = registry.begin_execution(&session.id).unwrap_err();
    assert_eq!(
        err.code, -32003,
        "must be invalid_argument, not session_not_found"
    );
}

/// A second `begin_execution` while one is already in flight must be
/// rejected; after `finish_execution`, a new one must succeed.
#[tokio::test]
async fn begin_execution_rejects_second_overlapping_run() {
    let registry = SessionRegistry::new();
    let session = registry.create("t".to_string(), None, None);

    let _first = registry.begin_execution(&session.id).unwrap();
    let err = registry.begin_execution(&session.id).unwrap_err();
    assert_eq!(err.code, -32003);

    registry.finish_execution(&session.id);
    assert!(
        registry.begin_execution(&session.id).is_ok(),
        "a new execution must be startable once the prior one finished"
    );
}

/// `request_cancel` on a session with no in-flight execution must return
/// `Ok(false)` (the caller falls back to the immediate-transition `cancel`).
#[tokio::test]
async fn request_cancel_returns_false_when_idle() {
    let registry = SessionRegistry::new();
    let session = registry.create("t".to_string(), None, None);
    assert!(!registry.request_cancel(&session.id).unwrap());
}

/// `request_cancel` on an executing session must set the flag and return
/// `Ok(true)`; `is_executing` must reflect the in-flight state throughout.
#[tokio::test]
async fn request_cancel_sets_flag_when_executing() {
    let registry = SessionRegistry::new();
    let session = registry.create("t".to_string(), None, None);
    assert!(!registry.is_executing(&session.id));

    let cancel = registry.begin_execution(&session.id).unwrap();
    assert!(registry.is_executing(&session.id));
    assert!(!cancel.load(std::sync::atomic::Ordering::Relaxed));

    assert!(registry.request_cancel(&session.id).unwrap());
    assert!(
        cancel.load(std::sync::atomic::Ordering::Relaxed),
        "the SAME flag must observe the request"
    );
}

/// `request_cancel` on an unknown session must error.
#[tokio::test]
async fn request_cancel_unknown_session_errors() {
    let registry = SessionRegistry::new();
    assert_eq!(registry.request_cancel("nope").unwrap_err().code, -32007);
}

/// `set_run_outcome` must store the transcript/usage/cost verbatim, and must
/// not panic when the session no longer exists.
#[tokio::test]
async fn set_run_outcome_stores_transcript_and_usage() {
    let registry = SessionRegistry::new();
    let session = registry.create("t".to_string(), None, None);

    let turns = vec![crate::run_task::TurnRecord {
        role: "pm".to_string(),
        model: "openai/gpt-4o-mini".to_string(),
        text: "done".to_string(),
        tool_calls: vec![],
        ran_test_command: false,
        usage: crate::perf::TokenUsage::new(10, 5, 0, 0),
    }];
    registry.set_run_outcome(
        &session.id,
        turns.clone(),
        crate::perf::TokenUsage::new(10, 5, 0, 0),
        Some(0.01),
    );

    let stored = registry.get_transcript(&session.id).unwrap();
    assert_eq!(stored.turns, turns);
    assert_eq!(stored.usage, crate::perf::TokenUsage::new(10, 5, 0, 0));
    assert_eq!(stored.cost_usd, Some(0.01));

    // A no-op on a missing session, not a panic.
    registry.set_run_outcome("nope", turns, crate::perf::TokenUsage::default(), None);
}

/// `get_transcript` on a session that has never run a task must return an
/// empty, valid `TranscriptRecord` — not an error (#2058).
#[tokio::test]
async fn get_transcript_on_never_run_session_is_empty() {
    let registry = SessionRegistry::new();
    let session = registry.create("t".to_string(), None, None);

    let transcript = registry.get_transcript(&session.id).unwrap();
    assert_eq!(transcript.session_id, session.id);
    assert!(transcript.turns.is_empty());
    assert_eq!(transcript.usage, crate::perf::TokenUsage::default());
    assert_eq!(transcript.cost_usd, None);
    assert_eq!(transcript.mode, None);
}

/// `get_transcript` on an unknown session must error `session_not_found`
/// (#2058).
#[tokio::test]
async fn get_transcript_unknown_session_errors() {
    let registry = SessionRegistry::new();
    let err = registry.get_transcript("nope").unwrap_err();
    assert_eq!(err.code, -32007);
}

/// `set_mode` must store the mode on the session, queryable both via
/// `status`/`list` (`Session.mode`) and `get_transcript`
/// (`TranscriptRecord.mode`, the SAME field) (#2059).
#[tokio::test]
async fn set_mode_stores_on_session() {
    let registry = SessionRegistry::new();
    let session = registry.create("t".to_string(), None, None);
    assert_eq!(session.mode, None, "a fresh session has no mode yet");

    registry
        .set_mode(&session.id, crate::mode::HarnessMode::Parity)
        .unwrap();

    let status = registry.status(&session.id).unwrap();
    assert_eq!(status.mode, Some(crate::mode::HarnessMode::Parity));

    let transcript = registry.get_transcript(&session.id).unwrap();
    assert_eq!(transcript.mode, Some(crate::mode::HarnessMode::Parity));
}

/// `set_mode` on an unknown session must error `session_not_found` (#2059).
#[tokio::test]
async fn set_mode_unknown_session_errors() {
    let registry = SessionRegistry::new();
    let err = registry
        .set_mode("nope", crate::mode::HarnessMode::DailyDriver)
        .unwrap_err();
    assert_eq!(err.code, -32007);
}

/// `shutdown_executions` must flip every tracked execution's cancel flag and
/// await its handle within the grace period.
#[tokio::test]
async fn shutdown_executions_awaits_cancelled_tasks() {
    let registry = SessionRegistry::new();
    let session = registry.create("t".to_string(), None, None);
    let cancel = registry.begin_execution(&session.id).unwrap();

    let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let done_clone = done.clone();
    let handle = tokio::spawn(async move {
        // A well-behaved execution loop: poll the flag, exit promptly once set.
        while !cancel.load(std::sync::atomic::Ordering::Relaxed) {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        done_clone.store(true, std::sync::atomic::Ordering::Relaxed);
    });
    registry.attach_execution_handle(&session.id, handle);

    registry.shutdown_executions(Duration::from_secs(2)).await;
    assert!(
        done.load(std::sync::atomic::Ordering::Relaxed),
        "the task must have observed cancellation and finished"
    );
}

/// `finish` must transition the session and publish `SessionDone` with the
/// given status.
#[tokio::test]
async fn finish_transitions_and_publishes_session_done() {
    let registry = SessionRegistry::new();
    let session = registry.create("t".to_string(), None, None);
    let mut events = crate::events::subscribe();

    let finished = registry
        .finish(&session.id, SessionStatus::Finished)
        .unwrap();
    assert_eq!(finished.status, SessionStatus::Finished);

    // `finish` publishes `SessionStatusChanged` (via `transition`) THEN
    // `SessionDone` — consume the former before asserting on the latter.
    let status_changed = next_event_for(&mut events, &session.id).await;
    assert!(
        matches!(status_changed.event, Event::SessionStatusChanged { status, .. } if status == "finished")
    );

    let done = next_event_for(&mut events, &session.id).await;
    assert!(matches!(done.event, Event::SessionDone { status, .. } if status == "finished"));
    assert!(done.seq > status_changed.seq);
}

/// `finish` on an already-terminal session must be a no-op, not a double
/// transition.
#[tokio::test]
async fn finish_is_idempotent_on_terminal_session() {
    let registry = SessionRegistry::new();
    let session = registry.create("t".to_string(), None, None);
    registry
        .finish(&session.id, SessionStatus::Finished)
        .unwrap();

    let again = registry.finish(&session.id, SessionStatus::Failed).unwrap();
    assert_eq!(
        again.status,
        SessionStatus::Finished,
        "must not overwrite an existing terminal status"
    );
}

/// `finish` on an unknown session must error.
#[tokio::test]
async fn finish_unknown_session_errors() {
    let registry = SessionRegistry::new();
    let err = registry
        .finish("nope", SessionStatus::Finished)
        .unwrap_err();
    assert_eq!(err.code, -32007);
}
