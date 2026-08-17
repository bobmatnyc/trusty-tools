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

    let project_dir = tempfile::tempdir().expect("project tempdir");
    let binding = crate::binding::ProjectBinding::resolve(Some(project_dir.path().to_path_buf()))
        .expect("tempdir must bind");
    let expected_label = binding.label().expect("a bound project has a label");

    let session = registry.create("do the thing".to_string(), None, binding);
    assert_eq!(session.status, SessionStatus::Running);

    let first = next_event_for(&mut events, &session.id).await;
    assert_eq!(first.seq, 1);
    assert_eq!(first.kind, "session_started");
    // The event's `project` label is now DERIVED from the binding rather than
    // being an independently-supplied string, so it must equal the binding's
    // own label exactly — that equality IS the reconciliation (AC-16.2).
    assert!(
        matches!(first.event, Event::SessionStarted { ref project, .. } if *project == expected_label),
        "SessionStarted.project must be the binding-derived label {expected_label:?}, got {:?}",
        first.event
    );

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
    registry.create("a".to_string(), None, crate::binding::ProjectBinding::None);
    registry.create("b".to_string(), None, crate::binding::ProjectBinding::None);
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
    let session = registry.create("t".to_string(), None, crate::binding::ProjectBinding::None);
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
    let session = registry.create("t".to_string(), None, crate::binding::ProjectBinding::None);
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
    let session = registry.create("t".to_string(), None, crate::binding::ProjectBinding::None);
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
    let session = registry.create("t".to_string(), None, crate::binding::ProjectBinding::None);
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
    let session = registry.create("t".to_string(), None, crate::binding::ProjectBinding::None);
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
    let session = registry.create("t".to_string(), None, crate::binding::ProjectBinding::None);
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
    let session = registry.create("t".to_string(), None, crate::binding::ProjectBinding::None);
    let replay = registry.replay(&session.id).unwrap();
    assert_eq!(replay.len(), 2); // SessionStarted + SessionStatusChanged(running)
}

/// The ring buffer must drop the oldest event once it reaches capacity.
#[tokio::test]
async fn ring_buffer_drops_oldest_when_full() {
    let registry = SessionRegistry::with_capacity(2);
    let session = registry.create("t".to_string(), None, crate::binding::ProjectBinding::None);
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

/// Build a `SearchTelemetry` for the UI-Phase-1 record tests.
fn search_telemetry(lane: &str, hit_count: Option<usize>) -> crate::tools::SearchTelemetry {
    crate::tools::SearchTelemetry {
        lane: lane.to_string(),
        query: "where is auth".to_string(),
        hit_count,
        hits: vec![],
        latency_ms: 12,
    }
}

/// Build a `SearchTelemetry` carrying real `(path, score)` hits (DOC-39
/// Slice B), for the test that proves `hits` — not just `hit_count` —
/// survives `record_search_performed`.
fn search_telemetry_with_hits(hits: &[(&str, f64)]) -> crate::tools::SearchTelemetry {
    crate::tools::SearchTelemetry {
        lane: "semantic".to_string(),
        query: "where is auth".to_string(),
        hit_count: Some(hits.len()),
        hits: hits
            .iter()
            .map(|(path, score)| crate::events::SearchHit {
                path: (*path).to_string(),
                score: *score,
            })
            .collect(),
        latency_ms: 12,
    }
}

/// Build a `RecallTelemetry` from `(score, injected)` pairs.
fn recall_telemetry(results: &[(f64, bool)]) -> crate::tools::RecallTelemetry {
    crate::tools::RecallTelemetry {
        query: "pkce".to_string(),
        results: results
            .iter()
            .map(|(score, injected)| crate::events::RecalledMemory {
                score: *score,
                injected: *injected,
                text: String::new(),
                run_id: None,
            })
            .collect(),
    }
}

/// `record_search_performed` must publish a `SearchPerformed` event carrying
/// the real routed lane, hit count, latency, and agent attribution
/// (UI Phase 1).
///
/// Why: this is the structured search signal the UI joins against a change to
/// answer "what search drove this?" — it must survive the same
/// record -> sequence -> publish path as every other event.
#[tokio::test]
async fn record_search_performed_publishes_event() {
    let registry = SessionRegistry::new();
    let session = registry.create("t".to_string(), None, crate::binding::ProjectBinding::None);
    let mut events = crate::events::subscribe();

    registry
        .record_search_performed(
            &session.id,
            "python-engineer",
            "eng-1",
            &search_telemetry("grep", Some(7)),
        )
        .unwrap();

    let envelope = next_event_for(&mut events, &session.id).await;
    assert_eq!(envelope.kind, "search_performed");
    assert!(matches!(
        envelope.event,
        Event::SearchPerformed { agent, agent_id, lane, query, hit_count, latency_ms, .. }
            if agent == "python-engineer"
                && agent_id == "eng-1"
                && lane == "grep"
                && query == "where is auth"
                && hit_count == Some(7)
                && latency_ms == 12
    ));
}

/// `record_search_performed` must publish each hit's `path` AND `score` in
/// `hits`, not just `hit_count` (DOC-39 Slice B) — the search-audit UI's
/// whole point is knowing WHICH files a search touched.
///
/// Why: `hit_count` alone predates this ticket; a regression that keeps
/// threading `hit_count` but drops `hits` would pass every pre-existing
/// assertion while still failing the requirement this event exists to serve.
#[tokio::test]
async fn record_search_performed_publishes_hits_with_path_and_score() {
    let registry = SessionRegistry::new();
    let session = registry.create("t".to_string(), None, crate::binding::ProjectBinding::None);
    let mut events = crate::events::subscribe();

    registry
        .record_search_performed(
            &session.id,
            "python-engineer",
            "eng-1",
            &search_telemetry_with_hits(&[("src/auth.rs", 0.87), ("src/session/session.rs", 0.52)]),
        )
        .unwrap();

    let envelope = next_event_for(&mut events, &session.id).await;
    let Event::SearchPerformed {
        hits, hit_count, ..
    } = envelope.event
    else {
        panic!("expected SearchPerformed");
    };
    assert_eq!(hit_count, Some(2));
    assert_eq!(
        hits,
        vec![
            crate::events::SearchHit {
                path: "src/auth.rs".to_string(),
                score: 0.87
            },
            crate::events::SearchHit {
                path: "src/session/session.rs".to_string(),
                score: 0.52
            },
        ],
        "each hit's path AND score must survive record_search_performed"
    );
}

/// `record_memory_recalled` must publish a `MemoryRecalled` event preserving
/// each result's score AND its `injected` flag (UI Phase 1).
///
/// Why: `injected` is the differentiating surface — a UI renders held-back
/// memories beside injected ones. Losing the flag anywhere in the emission
/// path collapses that distinction.
#[tokio::test]
async fn record_memory_recalled_publishes_event() {
    let registry = SessionRegistry::new();
    let session = registry.create("t".to_string(), None, crate::binding::ProjectBinding::None);
    let mut events = crate::events::subscribe();

    registry
        .record_memory_recalled(
            &session.id,
            "pm",
            "pm-1",
            &recall_telemetry(&[(0.9, true), (0.41, false)]),
        )
        .unwrap();

    let envelope = next_event_for(&mut events, &session.id).await;
    assert_eq!(envelope.kind, "memory_recalled");
    let Event::MemoryRecalled {
        agent,
        agent_id,
        query,
        results,
        ..
    } = envelope.event
    else {
        panic!("expected MemoryRecalled");
    };
    assert_eq!(agent, "pm");
    assert_eq!(agent_id, "pm-1");
    assert_eq!(query, "pkce");
    assert_eq!(
        results,
        vec![
            crate::events::RecalledMemory {
                score: 0.9,
                injected: true,
                text: String::new(),
                run_id: None,
            },
            crate::events::RecalledMemory {
                score: 0.41,
                injected: false,
                text: String::new(),
                run_id: None,
            },
        ],
        "both the scores and the injected/held-back split must survive emission"
    );
}

/// (issue #3072) `record_search_performed` must ALSO append onto the
/// session's retained search/recall audit trail, not just publish the SSE
/// event — this is the wiring `session.get_search_audit` depends on.
#[tokio::test]
async fn record_search_performed_appends_to_search_audit() {
    let registry = SessionRegistry::new();
    let session = registry.create("t".to_string(), None, crate::binding::ProjectBinding::None);

    registry
        .record_search_performed(
            &session.id,
            "python-engineer",
            "eng-1",
            &search_telemetry("grep", Some(7)),
        )
        .unwrap();

    let audit = registry.get_search_audit(&session.id).unwrap();
    assert_eq!(audit.len(), 1);
    assert!(matches!(
        &audit[0],
        crate::events::SearchAuditRecord::Search {
            agent,
            agent_id,
            lane,
            query,
            hit_count,
            latency_ms,
            ..
        } if agent == "python-engineer"
            && agent_id == "eng-1"
            && lane == "grep"
            && query == "where is auth"
            && *hit_count == Some(7)
            && *latency_ms == 12
    ));
}

/// (issue #3072) `record_memory_recalled` must ALSO append onto the
/// session's retained search/recall audit trail, folding `results` into
/// `result_count`/`injected_count`.
#[tokio::test]
async fn record_memory_recalled_appends_to_search_audit() {
    let registry = SessionRegistry::new();
    let session = registry.create("t".to_string(), None, crate::binding::ProjectBinding::None);

    registry
        .record_memory_recalled(
            &session.id,
            "pm",
            "pm-1",
            &recall_telemetry(&[(0.9, true), (0.41, false)]),
        )
        .unwrap();

    let audit = registry.get_search_audit(&session.id).unwrap();
    assert_eq!(audit.len(), 1);
    assert!(matches!(
        &audit[0],
        crate::events::SearchAuditRecord::Recall {
            agent,
            agent_id,
            query,
            result_count,
            injected_count,
            ..
        } if agent == "pm"
            && agent_id == "pm-1"
            && query == "pkce"
            && *result_count == 2
            && *injected_count == 1
    ));
}

/// `record_tool_started` must publish a `ToolStarted` event with a
/// truncated `args_preview`.
#[tokio::test]
async fn record_tool_started_publishes_event() {
    let registry = SessionRegistry::new();
    let session = registry.create("t".to_string(), None, crate::binding::ProjectBinding::None);
    let mut events = crate::events::subscribe();

    registry
        .record_tool_started(&session.id, "pm", "pm-1", "bash", "call-1", "ls -la")
        .unwrap();

    let envelope = next_event_for(&mut events, &session.id).await;
    assert_eq!(envelope.kind, "tool_started");
    assert!(matches!(
        envelope.event,
        Event::ToolStarted { agent, agent_id, tool, call_id, args_preview, .. }
            if agent == "pm"
                && agent_id == "pm-1"
                && tool == "bash"
                && call_id == "call-1"
                && args_preview == "ls -la"
    ));
}

/// `record_tool_finished` must publish a `ToolFinished` event carrying
/// `success` and a truncated `result_preview`.
#[tokio::test]
async fn record_tool_finished_publishes_event() {
    let registry = SessionRegistry::new();
    let session = registry.create("t".to_string(), None, crate::binding::ProjectBinding::None);
    let mut events = crate::events::subscribe();

    registry
        .record_tool_finished(&session.id, "pm", "pm-1", "bash", "call-1", true, "done")
        .unwrap();

    let envelope = next_event_for(&mut events, &session.id).await;
    assert_eq!(envelope.kind, "tool_finished");
    assert!(matches!(
        envelope.event,
        Event::ToolFinished { agent, agent_id, success, result_preview, .. }
            if agent == "pm" && agent_id == "pm-1" && success && result_preview == "done"
    ));
}

/// `record_tool_error` must publish a `ToolError` event.
#[tokio::test]
async fn record_tool_error_publishes_event() {
    let registry = SessionRegistry::new();
    let session = registry.create("t".to_string(), None, crate::binding::ProjectBinding::None);
    let mut events = crate::events::subscribe();

    registry
        .record_tool_error(&session.id, "pm", "pm-1", "bash", "call-1", "timed out")
        .unwrap();

    let envelope = next_event_for(&mut events, &session.id).await;
    assert_eq!(envelope.kind, "tool_error");
    assert!(matches!(
        envelope.event,
        Event::ToolError { agent, agent_id, error, .. }
            if agent == "pm" && agent_id == "pm-1" && error == "timed out"
    ));
}

/// `record_log` must publish a `Log` event.
#[tokio::test]
async fn record_log_publishes_event() {
    let registry = SessionRegistry::new();
    let session = registry.create("t".to_string(), None, crate::binding::ProjectBinding::None);
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
    let session = registry.create("t".to_string(), None, crate::binding::ProjectBinding::None);
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
    let session = registry.create("t".to_string(), None, crate::binding::ProjectBinding::None);
    let mut events = crate::events::subscribe();

    registry.record_message(&session.id, "hello world").unwrap();

    let envelope = next_event_for(&mut events, &session.id).await;
    assert_eq!(envelope.kind, "message");
    assert!(matches!(envelope.event, Event::Message { text, .. } if text == "hello world"));
}

/// `record_agent_message_delta` must publish an `AgentMessageDelta` event
/// (tcode streaming epic #3696, Gap A, Slice 1).
#[tokio::test]
async fn record_agent_message_delta_publishes_event() {
    let registry = SessionRegistry::new();
    let session = registry.create("t".to_string(), None, crate::binding::ProjectBinding::None);
    let mut events = crate::events::subscribe();

    registry
        .record_agent_message_delta(&session.id, "pm", "pm-1", "turn-1", "hello", true)
        .unwrap();

    let envelope = next_event_for(&mut events, &session.id).await;
    assert_eq!(envelope.kind, "agent_message_delta");
    assert!(matches!(
        envelope.event,
        Event::AgentMessageDelta { agent, agent_id, turn_id, delta, done, .. }
            if agent == "pm" && agent_id == "pm-1" && turn_id == "turn-1"
                && delta == "hello" && done
    ));
}

/// Every `record_*` emission-plumbing method must reject an unknown
/// session id with `session_not_found` rather than panicking.
#[tokio::test]
async fn record_plumbing_methods_reject_unknown_session() {
    let registry = SessionRegistry::new();
    assert_eq!(
        registry
            .record_tool_started("nope", "pm", "pm-1", "t", "c", "a")
            .unwrap_err()
            .code,
        -32007
    );
    assert_eq!(
        registry
            .record_tool_finished("nope", "pm", "pm-1", "t", "c", true, "r")
            .unwrap_err()
            .code,
        -32007
    );
    assert_eq!(
        registry
            .record_tool_error("nope", "pm", "pm-1", "t", "c", "e")
            .unwrap_err()
            .code,
        -32007
    );
    assert_eq!(
        registry
            .record_search_performed("nope", "pm", "pm-1", &search_telemetry("semantic", Some(1)))
            .unwrap_err()
            .code,
        -32007
    );
    assert_eq!(
        registry
            .record_memory_recalled("nope", "pm", "pm-1", &recall_telemetry(&[(0.9, true)]))
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
    assert_eq!(
        registry
            .record_agent_message_delta("nope", "pm", "pm-1", "turn-1", "m", true)
            .unwrap_err()
            .code,
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
    let session = registry.create("t".to_string(), None, crate::binding::ProjectBinding::None);
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
    let session = registry.create("t".to_string(), None, crate::binding::ProjectBinding::None);

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
    let session = registry.create("t".to_string(), None, crate::binding::ProjectBinding::None);
    assert!(!registry.request_cancel(&session.id).unwrap());
}

/// `request_cancel` on an executing session must set the flag and return
/// `Ok(true)`; `is_executing` must reflect the in-flight state throughout.
#[tokio::test]
async fn request_cancel_sets_flag_when_executing() {
    let registry = SessionRegistry::new();
    let session = registry.create("t".to_string(), None, crate::binding::ProjectBinding::None);
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

/// (#2344) A `Finished` session is resumable: `begin_execution` must
/// succeed on it (rather than treating `Finished` as a dead end like
/// `Cancelled`/`Failed`/`DeadlineExceeded`), transitioning the session back
/// to `Running` and publishing the same `SessionStatusChanged` event every
/// other transition in this module publishes.
///
/// Why: this is the change that makes "two sequential `task.run` calls on
/// one session" (#2344's acceptance criterion) possible at all — without it,
/// the second call would be rejected here before ever reaching
/// `begin_pm_transcript`.
#[tokio::test]
async fn begin_execution_resumes_a_finished_session() {
    let registry = SessionRegistry::new();
    let session = registry.create("t".to_string(), None, crate::binding::ProjectBinding::None);

    let _first = registry.begin_execution(&session.id).unwrap();
    registry.finish_execution(&session.id);
    registry
        .finish(&session.id, SessionStatus::Finished)
        .unwrap();
    assert_eq!(
        registry.status(&session.id).unwrap().status,
        SessionStatus::Finished
    );

    let mut events = crate::events::subscribe();
    assert!(
        registry.begin_execution(&session.id).is_ok(),
        "a Finished session must be resumable for its next task.run"
    );
    assert_eq!(
        registry.status(&session.id).unwrap().status,
        SessionStatus::Running,
        "resuming must transition the session back to Running"
    );

    let envelope = next_event_for(&mut events, &session.id).await;
    assert!(
        matches!(envelope.event, Event::SessionStatusChanged { status, .. } if status == "running"),
        "resuming must publish the SAME SessionStatusChanged event other transitions do"
    );
}

/// (#3888) A session whose call landed in `TurnCapExceeded` (this call's
/// `AgentLoopConfig::max_turns` budget ran out, not a real failure) must be
/// resumable via `begin_execution` exactly like a `Finished` session —
/// mirrors `begin_execution_resumes_a_finished_session` above, proving the
/// regression reported in #3888 ("TurnCapExceeded permanently un-resumes a
/// session") is fixed: before this fix, `TurnCapExceeded` sessions fell
/// through `task::executor::run_and_record`'s catch-all into
/// `SessionStatus::Failed`, which `begin_execution` rejects outright.
///
/// Why: this is the concrete acceptance proof for #3888 — without the
/// `SessionStatus::TurnCapExceeded` variant and `begin_execution`'s updated
/// resume condition, this session would stay permanently unresumable,
/// directly regressing epic #2343's infinite-sessions goal.
#[tokio::test]
async fn begin_execution_resumes_a_turn_cap_exceeded_session() {
    let registry = SessionRegistry::new();
    let session = registry.create("t".to_string(), None, crate::binding::ProjectBinding::None);

    let _first = registry.begin_execution(&session.id).unwrap();
    registry.finish_execution(&session.id);
    registry
        .finish(&session.id, SessionStatus::TurnCapExceeded)
        .unwrap();
    assert_eq!(
        registry.status(&session.id).unwrap().status,
        SessionStatus::TurnCapExceeded,
        "a session whose call exhausted its turn budget must land in TurnCapExceeded, not Failed"
    );

    let mut events = crate::events::subscribe();
    assert!(
        registry.begin_execution(&session.id).is_ok(),
        "a TurnCapExceeded session must be resumable for its next task.run (#3888)"
    );
    assert_eq!(
        registry.status(&session.id).unwrap().status,
        SessionStatus::Running,
        "resuming must transition the session back to Running"
    );

    let envelope = next_event_for(&mut events, &session.id).await;
    assert!(
        matches!(envelope.event, Event::SessionStatusChanged { status, .. } if status == "running"),
        "resuming must publish the SAME SessionStatusChanged event other transitions do"
    );
}

/// `begin_execution` must still reject `Cancelled`/`Failed`/
/// `DeadlineExceeded` sessions even after the #2344 `Finished`-resumption
/// change — only a successful finish is resumable.
#[tokio::test]
async fn begin_execution_still_rejects_cancelled_failed_and_deadline_exceeded() {
    let registry = SessionRegistry::new();

    let cancelled = registry.create("t".to_string(), None, crate::binding::ProjectBinding::None);
    registry.cancel(&cancelled.id).unwrap();
    assert_eq!(
        registry.begin_execution(&cancelled.id).unwrap_err().code,
        -32003
    );

    let failed = registry.create("t".to_string(), None, crate::binding::ProjectBinding::None);
    registry.finish(&failed.id, SessionStatus::Failed).unwrap();
    assert_eq!(
        registry.begin_execution(&failed.id).unwrap_err().code,
        -32003
    );

    let deadline = registry.create("t".to_string(), None, crate::binding::ProjectBinding::None);
    registry
        .finish(&deadline.id, SessionStatus::DeadlineExceeded)
        .unwrap();
    assert_eq!(
        registry.begin_execution(&deadline.id).unwrap_err().code,
        -32003
    );
}

// ── #2344: persistent PM transcript ─────────────────────────────────────────────

/// `begin_pm_transcript` seeds a fresh system+user pair on a session's FIRST
/// call.
#[tokio::test]
async fn begin_pm_transcript_seeds_on_first_call() {
    let registry = SessionRegistry::new();
    let session = registry.create("t".to_string(), None, crate::binding::ProjectBinding::None);

    let transcript = registry
        .begin_pm_transcript(&session.id, "you are the pm", "first task")
        .unwrap();
    let messages = transcript.messages();
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0].role, "system");
    assert_eq!(messages[0].content.as_deref(), Some("you are the pm"));
    assert_eq!(messages[1].role, "user");
    assert_eq!(messages[1].content.as_deref(), Some("first task"));
}

/// `begin_pm_transcript`'s second call appends the new task as a user turn
/// onto the EXISTING history stored by `store_pm_transcript`, without
/// re-adding a system message — the core "two sequential task.run calls
/// share one growing Transcript" acceptance criterion.
#[tokio::test]
async fn begin_pm_transcript_appends_on_second_call() {
    let registry = SessionRegistry::new();
    let session = registry.create("t".to_string(), None, crate::binding::ProjectBinding::None);

    let mut transcript = registry
        .begin_pm_transcript(&session.id, "you are the pm", "first task")
        .unwrap();
    transcript.push_assistant(Some("first answer".into()), &[]);
    registry.store_pm_transcript(&session.id, transcript);

    let continued = registry
        .begin_pm_transcript(&session.id, "a DIFFERENT system prompt", "second task")
        .unwrap();
    let messages = continued.messages();
    assert_eq!(messages.len(), 4, "system, user, assistant, new user");
    assert_eq!(messages[0].role, "system");
    assert_eq!(
        messages[0].content.as_deref(),
        Some("you are the pm"),
        "the ORIGINAL seed's system message must stay authoritative, not the second call's"
    );
    assert_eq!(messages[1].content.as_deref(), Some("first task"));
    assert_eq!(messages[2].role, "assistant");
    assert_eq!(messages[2].content.as_deref(), Some("first answer"));
    assert_eq!(messages[3].role, "user");
    assert_eq!(messages[3].content.as_deref(), Some("second task"));
    assert_eq!(
        messages.iter().filter(|m| m.role == "system").count(),
        1,
        "must never end up with two system messages across runs"
    );
}

/// `begin_pm_transcript` on an unknown session must error.
#[tokio::test]
async fn begin_pm_transcript_unknown_session_errors() {
    let registry = SessionRegistry::new();
    let err = registry
        .begin_pm_transcript("nope", "sys", "task")
        .unwrap_err();
    assert_eq!(err.code, -32007);
}

/// A DURABLE (non-temp) project root for the `memory_sink_for` tests.
///
/// Why (#4638): `memory_sink_for` now refuses any project root under a system
/// temp root, so these tests can no longer use a `TempDir` — that is the very
/// input the fix rejects. The path need not exist: `memory_sink_for` does no
/// filesystem I/O, and `derive_palace_id_for_project`'s `git config` probe on a
/// missing directory simply fails and falls through to the parent/dir slug,
/// which is the branch under test. Using a synthetic path rather than a real
/// on-disk fixture also keeps the suite from writing outside its temp space.
/// What: a fixed absolute path whose last two components slugify to a stable
/// palace id (`projects-tcode-fixture`).
fn durable_project_root() -> &'static std::path::Path {
    std::path::Path::new("/nonexistent-durable-root/projects/tcode-fixture")
}

/// `memory_sink_for` (#2345) must construct exactly ONE sink for a session
/// and return the SAME `Arc` on every subsequent call — proving the sink (and
/// therefore its background drain task) survives across repeated
/// `task.run`s on one session rather than being rebuilt per run.
#[tokio::test]
async fn memory_sink_for_reuses_the_same_sink_across_calls() {
    let registry = SessionRegistry::new();
    let session = registry.create("t".to_string(), None, crate::binding::ProjectBinding::None);

    let first = registry
        .memory_sink_for(&session.id, Some(durable_project_root()))
        .expect("first call constructs a sink");
    let second = registry
        .memory_sink_for(&session.id, Some(durable_project_root()))
        .expect("second call reuses the sink");

    assert!(
        Arc::ptr_eq(&first, &second),
        "memory_sink_for must return the SAME Arc across calls on one session"
    );
}

/// (#5811) A session whose project palace cannot be RESOLVED gets no sink, so
/// nothing is recorded for it.
///
/// Why: `derive_palace_id_for_project` used to collapse every resolution failure
/// into the shared literal `"unknown-project"`. A durable project root carries
/// `PalaceCreation::Allowed`, so the drain task auto-created a palace under that
/// one id and `write_turn` posted the session's real prompt and response into
/// it — two projects with broken pins commingled their conversation content in a
/// palace neither of them chose. The fix is structural: with no palace id there
/// is no sink, so there is nothing to write into.
/// What: a project root carrying a `.trusty-tools/trusty-memory.yaml` that does
/// not parse. `.trusty-tools/` is itself a project marker, so `find_project_root`
/// stops at this directory rather than walking up into the repo under test.
///
/// This is the both-ways regression proof for #5811: at `878dbb863^` this test
/// gets `Some(sink)` because the pin was never read at all, and the derived id
/// was returned regardless.
/// Test: itself.
#[tokio::test]
async fn memory_sink_for_unresolvable_palace_returns_no_sink() {
    let tmp = tempfile::TempDir::new().unwrap();
    let pin_dir = tmp.path().join(".trusty-tools");
    std::fs::create_dir_all(&pin_dir).unwrap();
    std::fs::write(
        pin_dir.join("trusty-memory.yaml"),
        "palace: [unclosed\n\t bad: :",
    )
    .unwrap();

    let registry = SessionRegistry::new();
    let session = registry.create("t".to_string(), None, crate::binding::ProjectBinding::None);

    assert!(
        registry
            .memory_sink_for(&session.id, Some(tmp.path()))
            .is_none(),
        "a project whose pin cannot be parsed must get no sink — a shared \
         placeholder palace would commingle two projects' turn content"
    );
}

/// `memory_sink_for` on an unknown session must return `None`, not panic
/// (best-effort, mirrors `set_run_outcome`'s framing).
#[tokio::test]
async fn memory_sink_for_unknown_session_returns_none() {
    let registry = SessionRegistry::new();
    assert!(
        registry
            .memory_sink_for("nope", Some(durable_project_root()))
            .is_none()
    );
}

/// (#4638) A session bound to a project root under a system temp root must
/// still get a sink — but one that is FORBIDDEN to bring its palace into
/// existence.
///
/// Why: the palace id is derived from the project root, and every
/// `tempfile::TempDir` has a fresh random basename, so such a sink's palace id
/// is unique per run. Letting it auto-create is what minted 5,667 permanent,
/// unreadable `t-tmp<random>` orphans in three weeks (97.8% of every palace on
/// the machine). The sink itself must survive, though: it is what registers
/// #2348's `recall_session` on the PM's tool surface, so suppressing it
/// entirely would change a temp-rooted run's behavior far beyond storage.
/// What: asserts `PalaceCreation::Forbidden` for a raw `TempDir` path AND for
/// the canonicalized form `ProjectBinding::resolve` hands the real
/// `run_and_record` call path (macOS rewrites `/var/folders/…` to
/// `/private/var/folders/…`, so the guard must recognise both spellings).
#[tokio::test]
async fn memory_sink_for_temp_rooted_session_forbids_palace_create() {
    let registry = SessionRegistry::new();
    let project_dir = tempfile::TempDir::new().unwrap();

    let raw = registry.create("t".to_string(), None, crate::binding::ProjectBinding::None);
    let sink = registry
        .memory_sink_for(&raw.id, Some(project_dir.path()))
        .expect("a temp-rooted session still gets a sink");
    assert_eq!(
        sink.palace_creation(),
        crate::session::PalaceCreation::Forbidden,
        "a temp-rooted session must never be entitled to mint a palace (#4638)"
    );

    let binding = crate::binding::ProjectBinding::resolve(Some(project_dir.path().to_path_buf()))
        .expect("tempdir must bind");
    let canonical = registry.create("t".to_string(), None, crate::binding::ProjectBinding::None);
    let sink = registry
        .memory_sink_for(&canonical.id, binding.root())
        .expect("a temp-rooted session still gets a sink");
    assert_eq!(
        sink.palace_creation(),
        crate::session::PalaceCreation::Forbidden,
        "the CANONICALIZED temp root the real run_and_record path passes must \
         be recognised too (#4638)"
    );
}

/// (#4638) THE BOUND: N sessions must not be able to mint N palaces.
///
/// Why: a bound is the whole point of the fix, so it is asserted directly
/// rather than inferred from a happy path. Before #4638 this exact loop would
/// have yielded 50 distinct create-entitled palace ids — the 1:1
/// session-to-palace ratio that accumulated 5,667 orphans and pushed
/// trusty-memory's O(n) full-registry handlers (#4637) to ~90 minutes per
/// `GET /api/v1/status`.
/// What: drives 50 distinct sessions — 25 against ONE shared durable project
/// root, 25 against 25 DISTINCT temp roots — and counts the set of palace ids
/// that could actually be created, i.e. `palace()` taken only from sinks whose
/// `palace_creation()` is `Allowed`. That set must have exactly one member.
/// Counting create-entitled ids rather than sinks is deliberate: it measures
/// palaces-that-could-be-minted, which is the quantity that leaked.
#[tokio::test]
async fn memory_sink_for_many_sessions_mint_at_most_one_palace() {
    let registry = SessionRegistry::new();
    let mut creatable: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut temp_rooted = 0usize;

    for _ in 0..25 {
        let durable = registry.create("t".to_string(), None, crate::binding::ProjectBinding::None);
        let sink = registry
            .memory_sink_for(&durable.id, Some(durable_project_root()))
            .expect("a durable project root must still record turns");
        if sink.palace_creation() == crate::session::PalaceCreation::Allowed {
            creatable.insert(sink.palace().to_string());
        }

        let ephemeral =
            registry.create("t".to_string(), None, crate::binding::ProjectBinding::None);
        let tmp = tempfile::TempDir::new().unwrap();
        let sink = registry
            .memory_sink_for(&ephemeral.id, Some(tmp.path()))
            .expect("a temp-rooted session still gets a sink");
        temp_rooted += 1;
        if sink.palace_creation() == crate::session::PalaceCreation::Allowed {
            creatable.insert(sink.palace().to_string());
        }
    }

    assert_eq!(
        temp_rooted, 25,
        "sanity: every ephemeral session was exercised"
    );
    assert_eq!(
        creatable.len(),
        1,
        "50 sessions may mint at most ONE palace — the shared durable \
         project's; got {creatable:?}"
    );
}

/// `set_run_outcome` must store the transcript/usage/cost verbatim, and must
/// not panic when the session no longer exists.
#[tokio::test]
async fn set_run_outcome_stores_transcript_and_usage() {
    let registry = SessionRegistry::new();
    let session = registry.create("t".to_string(), None, crate::binding::ProjectBinding::None);

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

/// (Issue #3298) `set_run_outcome` must record+publish
/// `Event::SessionActivityUpdate` on the session's own ring buffer/live bus
/// — but ONLY when the session is workstream-bound (see the gate rationale
/// on `set_run_outcome`'s docs and the sibling test below).
#[tokio::test]
async fn set_run_outcome_publishes_activity_update_when_workstream_bound() {
    let registry = SessionRegistry::new();
    let session = registry.create("t".to_string(), None, crate::binding::ProjectBinding::None);
    registry
        .bind_workstream(&session.id, crate::workstreams::WorkstreamId::new())
        .expect("bind");

    registry.set_run_outcome(
        &session.id,
        vec![],
        crate::perf::TokenUsage::default(),
        None,
    );

    let events = registry.replay(&session.id).unwrap();
    assert!(
        events
            .iter()
            .any(|e| matches!(&e.event, Event::SessionActivityUpdate { has_running_task, .. } if !has_running_task)),
        "expected a SessionActivityUpdate event: {events:?}"
    );
}

/// (PR #3354 CI regression) An UNBOUND session's `set_run_outcome` must
/// publish NO `SessionActivityUpdate` — the M1 cutline e2e contract
/// (`tests/m1_cutline_e2e.rs`) freezes the exact event-kind sequence a
/// plain, workstream-less session emits, and this event is not in that
/// baseline; unconditional emission broke it on both transports.
#[tokio::test]
async fn set_run_outcome_stays_silent_for_unbound_session() {
    let registry = SessionRegistry::new();
    let session = registry.create("t".to_string(), None, crate::binding::ProjectBinding::None);

    registry.set_run_outcome(
        &session.id,
        vec![],
        crate::perf::TokenUsage::default(),
        None,
    );

    let events = registry.replay(&session.id).unwrap();
    assert!(
        !events
            .iter()
            .any(|e| matches!(&e.event, Event::SessionActivityUpdate { .. })),
        "an unbound session must emit no SessionActivityUpdate (frozen M1 baseline): {events:?}"
    );
}

/// `set_run_outcome` must ACCUMULATE across two calls (#2344): the second
/// run's turns are appended (not replacing the first run's), and usage/cost
/// are added onto the running total — `session.get_transcript` must reflect
/// the FULL cumulative history, not just the last run.
#[tokio::test]
async fn set_run_outcome_accumulates_across_two_calls() {
    let registry = SessionRegistry::new();
    let session = registry.create("t".to_string(), None, crate::binding::ProjectBinding::None);

    let run_one_turn = crate::run_task::TurnRecord {
        role: "pm".to_string(),
        model: "openai/gpt-4o-mini".to_string(),
        text: "run one".to_string(),
        tool_calls: vec![],
        ran_test_command: false,
        usage: crate::perf::TokenUsage::new(10, 5, 0, 0),
    };
    registry.set_run_outcome(
        &session.id,
        vec![run_one_turn.clone()],
        crate::perf::TokenUsage::new(10, 5, 0, 0),
        Some(0.01),
    );

    let run_two_turn = crate::run_task::TurnRecord {
        role: "pm".to_string(),
        model: "openai/gpt-4o-mini".to_string(),
        text: "run two".to_string(),
        tool_calls: vec![],
        ran_test_command: false,
        usage: crate::perf::TokenUsage::new(20, 8, 0, 0),
    };
    registry.set_run_outcome(
        &session.id,
        vec![run_two_turn.clone()],
        crate::perf::TokenUsage::new(20, 8, 0, 0),
        Some(0.02),
    );

    let stored = registry.get_transcript(&session.id).unwrap();
    assert_eq!(
        stored.turns,
        vec![run_one_turn, run_two_turn],
        "both runs' turns must be present, in order, not just the last run's"
    );
    assert_eq!(
        stored.usage,
        crate::perf::TokenUsage::new(30, 13, 0, 0),
        "usage must be the SUM of both runs, not the last run's alone"
    );
    assert_eq!(
        stored.cost_usd,
        Some(0.03),
        "cost must be the SUM of both runs' cost"
    );
}

/// `get_transcript` on a session that has never run a task must return an
/// empty, valid `TranscriptRecord` — not an error (#2058).
#[tokio::test]
async fn get_transcript_on_never_run_session_is_empty() {
    let registry = SessionRegistry::new();
    let session = registry.create("t".to_string(), None, crate::binding::ProjectBinding::None);

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

/// `get_transcript` surfaces `pm_transcript`'s own cumulative
/// `compaction_events` counter (#2349) — proving the one-line wiring in
/// `Self::get_transcript` actually reads the stored transcript rather than
/// always defaulting to `0`.
///
/// Why: `Transcript::record_threshold_compaction` is only ever called in
/// production from `agent_loop::AgentLoop::maybe_compact_transcript`
/// (exercised end-to-end by `agent_loop::tests::forced_degradation_*`); this
/// test isolates the SEPARATE registry-level contract — that whatever count
/// `pm_transcript` carries flows through `get_transcript` unchanged.
/// What: Seeds a `pm_transcript` via `begin_pm_transcript`, increments its
/// counter directly (the `pub(crate)` visibility this ticket adds exists
/// exactly for this), stores it back via `store_pm_transcript`, then asserts
/// `get_transcript(id).compaction_events` reflects it.
/// Test: this test.
#[tokio::test]
async fn get_transcript_reports_compaction_events() {
    let registry = SessionRegistry::new();
    let session = registry.create("t".to_string(), None, crate::binding::ProjectBinding::None);

    let mut transcript = registry
        .begin_pm_transcript(&session.id, "you are the pm", "first task")
        .unwrap();
    transcript.record_threshold_compaction();
    transcript.record_threshold_compaction();
    registry.store_pm_transcript(&session.id, transcript);

    let stored = registry.get_transcript(&session.id).unwrap();
    assert_eq!(stored.compaction_events, 2);
}

/// `set_mode` must store the mode on the session, queryable both via
/// `status`/`list` (`Session.mode`) and `get_transcript`
/// (`TranscriptRecord.mode`, the SAME field) (#2059).
#[tokio::test]
async fn set_mode_stores_on_session() {
    let registry = SessionRegistry::new();
    let session = registry.create("t".to_string(), None, crate::binding::ProjectBinding::None);
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
    let session = registry.create("t".to_string(), None, crate::binding::ProjectBinding::None);
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
    let session = registry.create("t".to_string(), None, crate::binding::ProjectBinding::None);
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
    let session = registry.create("t".to_string(), None, crate::binding::ProjectBinding::None);
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

// ── #2350: operator-facing goal slots ───────────────────────────────────────

/// Seed `id`'s `pm_transcript` the same way a real `task.run` would (via
/// `begin_pm_transcript` + `store_pm_transcript`), without needing a real
/// agent-loop execution — the shared setup every goal test in this section
/// needs to get past the "no transcript yet" guard.
fn seed_pm_transcript(registry: &SessionRegistry, id: &str) {
    let transcript = registry
        .begin_pm_transcript(id, "you are the pm", "first task")
        .unwrap();
    registry.store_pm_transcript(id, transcript);
}

/// `set_goal` on a session with a seeded transcript writes with
/// `GoalSource::Operator`, visible via both `get_goals` and
/// `get_transcript`.
#[tokio::test]
async fn set_goal_writes_operator_source() {
    let registry = SessionRegistry::new();
    let session = registry.create("t".to_string(), None, crate::binding::ProjectBinding::None);
    seed_pm_transcript(&registry, &session.id);

    registry.set_goal(&session.id, 2, "ship it").unwrap();

    let goals = registry.get_goals(&session.id).unwrap();
    assert_eq!(goals.len(), 1);
    assert_eq!(goals[0].slot, 2);
    assert_eq!(goals[0].text, "ship it");
    assert_eq!(goals[0].source, crate::agent_loop::GoalSource::Operator);
}

/// `set_goal` on a session that has never run a task (no `pm_transcript`
/// yet) must return the documented `invalid_argument` "no transcript yet"
/// error rather than panicking or silently no-op-ing.
#[tokio::test]
async fn set_goal_no_transcript_yet_errors() {
    let registry = SessionRegistry::new();
    let session = registry.create("t".to_string(), None, crate::binding::ProjectBinding::None);

    let err = registry.set_goal(&session.id, 1, "x").unwrap_err();
    assert_eq!(err.code, -32003);
    assert!(err.message.contains("no transcript yet"));
}

/// `set_goal` with slot `0` or past `GOAL_SLOT_COUNT` must map to a clean
/// `invalid_argument` error, not a panic.
#[tokio::test]
async fn set_goal_out_of_range_slot_errors() {
    let registry = SessionRegistry::new();
    let session = registry.create("t".to_string(), None, crate::binding::ProjectBinding::None);
    seed_pm_transcript(&registry, &session.id);

    assert_eq!(
        registry.set_goal(&session.id, 0, "x").unwrap_err().code,
        -32003
    );
    assert_eq!(
        registry.set_goal(&session.id, 6, "x").unwrap_err().code,
        -32003
    );
}

/// `set_goal` on an unknown session must map to `session_not_found`.
#[tokio::test]
async fn set_goal_unknown_session_errors() {
    let registry = SessionRegistry::new();
    assert_eq!(registry.set_goal("nope", 1, "x").unwrap_err().code, -32007);
}

/// `clear_goal` empties a previously-set slot.
#[tokio::test]
async fn clear_goal_clears_slot() {
    let registry = SessionRegistry::new();
    let session = registry.create("t".to_string(), None, crate::binding::ProjectBinding::None);
    seed_pm_transcript(&registry, &session.id);
    registry.set_goal(&session.id, 3, "temp").unwrap();

    registry.clear_goal(&session.id, 3).unwrap();

    assert!(registry.get_goals(&session.id).unwrap().is_empty());
}

/// `clear_goal` on a session with no transcript yet must error the same way
/// `set_goal` does.
#[tokio::test]
async fn clear_goal_no_transcript_yet_errors() {
    let registry = SessionRegistry::new();
    let session = registry.create("t".to_string(), None, crate::binding::ProjectBinding::None);

    let err = registry.clear_goal(&session.id, 1).unwrap_err();
    assert_eq!(err.code, -32003);
}

/// `clear_goal` with an out-of-range slot must map to a clean
/// `invalid_argument` error.
#[tokio::test]
async fn clear_goal_out_of_range_slot_errors() {
    let registry = SessionRegistry::new();
    let session = registry.create("t".to_string(), None, crate::binding::ProjectBinding::None);
    seed_pm_transcript(&registry, &session.id);

    assert_eq!(
        registry.clear_goal(&session.id, 0).unwrap_err().code,
        -32003
    );
}

/// `get_goals` reports BOTH an operator-written slot (via `set_goal`) and a
/// model-written slot (simulating what the `set_goal` tool writes directly
/// onto the same shared `GoalSlots` handle) — and that a later operator
/// write to the SAME slot the model wrote wins (last-write-wins).
#[tokio::test]
async fn get_goals_returns_operator_and_model_sources() {
    let registry = SessionRegistry::new();
    let session = registry.create("t".to_string(), None, crate::binding::ProjectBinding::None);
    seed_pm_transcript(&registry, &session.id);

    registry.set_goal(&session.id, 2, "operator goal").unwrap();

    // Simulate the model's own `set_goal` tool writing slot 1 directly onto
    // the same shared handle `tools::goals::SetGoalTool` would hold.
    {
        let sessions = registry.lock();
        let handle = sessions
            .get(&session.id)
            .unwrap()
            .pm_transcript
            .as_ref()
            .unwrap()
            .goals_handle();
        handle
            .lock()
            .unwrap()
            .set(1, "model goal", crate::agent_loop::GoalSource::Model)
            .unwrap();
    }

    let goals = registry.get_goals(&session.id).unwrap();
    assert_eq!(goals.len(), 2);
    let slot1 = goals.iter().find(|g| g.slot == 1).expect("slot 1 present");
    assert_eq!(slot1.source, crate::agent_loop::GoalSource::Model);
    let slot2 = goals.iter().find(|g| g.slot == 2).expect("slot 2 present");
    assert_eq!(slot2.source, crate::agent_loop::GoalSource::Operator);

    // Last-write-wins: an operator write to the model's slot replaces it.
    registry
        .set_goal(&session.id, 1, "operator overwrites model")
        .unwrap();
    let goals = registry.get_goals(&session.id).unwrap();
    let slot1 = goals.iter().find(|g| g.slot == 1).expect("slot 1 present");
    assert_eq!(slot1.text, "operator overwrites model");
    assert_eq!(slot1.source, crate::agent_loop::GoalSource::Operator);
}

/// `get_goals` on a session that has never run a task returns `[]`, not an
/// error — mirrors `get_transcript`'s never-run convention.
#[tokio::test]
async fn get_goals_on_never_run_session_is_empty() {
    let registry = SessionRegistry::new();
    let session = registry.create("t".to_string(), None, crate::binding::ProjectBinding::None);

    assert!(registry.get_goals(&session.id).unwrap().is_empty());
}

/// `get_goals` on an unknown session must map to `session_not_found`.
#[tokio::test]
async fn get_goals_unknown_session_errors() {
    let registry = SessionRegistry::new();
    assert_eq!(registry.get_goals("nope").unwrap_err().code, -32007);
}

/// `session.get_transcript` round-trips goal state (#2350 acceptance
/// criterion): a goal set via `set_goal` is visible on `TranscriptRecord.goals`
/// with the same slot/text/source it was written with.
#[tokio::test]
async fn get_transcript_round_trips_goal_state() {
    let registry = SessionRegistry::new();
    let session = registry.create("t".to_string(), None, crate::binding::ProjectBinding::None);
    seed_pm_transcript(&registry, &session.id);
    registry
        .set_goal(&session.id, 4, "finish the migration")
        .unwrap();

    let record = registry.get_transcript(&session.id).unwrap();
    assert_eq!(record.goals.len(), 1);
    assert_eq!(record.goals[0].slot, 4);
    assert_eq!(record.goals[0].text, "finish the migration");
    assert_eq!(
        record.goals[0].source,
        crate::agent_loop::GoalSource::Operator
    );

    // Round-trip through JSON, exactly as a real client would.
    let value = serde_json::to_value(&record).unwrap();
    let back: TranscriptRecord = serde_json::from_value(value).unwrap();
    assert_eq!(back.goals.len(), 1);
    assert_eq!(back.goals[0].text, "finish the migration");
}

/// `session.get_transcript` on a never-run session has an empty `goals`
/// array, matching the empty `turns`/`usage`/`cost_usd` convention.
#[tokio::test]
async fn get_transcript_goals_empty_on_never_run_session() {
    let registry = SessionRegistry::new();
    let session = registry.create("t".to_string(), None, crate::binding::ProjectBinding::None);

    let record = registry.get_transcript(&session.id).unwrap();
    assert!(record.goals.is_empty());
}

/// `create` must DERIVE `Session.project` from the binding rather than accept an
/// independent label — the two can no longer disagree (AC-16.2).
#[tokio::test]
async fn create_derives_project_label_from_binding() {
    let registry = SessionRegistry::new();
    let project_dir = tempfile::tempdir().expect("project tempdir");
    let binding = crate::binding::ProjectBinding::resolve(Some(project_dir.path().to_path_buf()))
        .expect("tempdir must bind");

    let session = registry.create("t".to_string(), None, binding.clone());
    assert_eq!(
        session.project,
        binding.label(),
        "the label must be the binding's own label, not an independent string"
    );
    assert_eq!(session.binding, binding);

    let projectless = registry.create("t".to_string(), None, crate::binding::ProjectBinding::None);
    assert_eq!(
        projectless.project, None,
        "projectless must derive no label"
    );
    assert_eq!(projectless.binding, crate::binding::ProjectBinding::None);
}

/// A PROJECTLESS session must get NO memory sink: a palace is project-scoped by
/// construction, so with no project there is nothing to scope one to. This must
/// be a clean `None`, not a panic and not a palace derived from a scratch path.
#[tokio::test]
async fn memory_sink_for_projectless_session_returns_none() {
    let registry = SessionRegistry::new();
    let session = registry.create("t".to_string(), None, crate::binding::ProjectBinding::None);

    assert!(
        registry.memory_sink_for(&session.id, None).is_none(),
        "a projectless session must have no project-scoped memory palace"
    );
}

// ── #2784 / epic #2343: index readiness + working-context budget ───────────────

/// Build an `IndexReadiness` with the given per-lane flags.
fn readiness(
    lifecycle: &str,
    chunks: u64,
    lexical: bool,
    semantic: bool,
) -> trusty_common::search_readiness::IndexReadiness {
    trusty_common::search_readiness::IndexReadiness {
        index_id: "repo".into(),
        lifecycle_status: lifecycle.into(),
        chunk_count: chunks,
        lexical_ready: lexical,
        semantic_ready: semantic,
        graph_ready: false,
    }
}

/// A still-warming index must publish `state: "warming"` with the per-lane
/// flags intact.
#[tokio::test]
async fn record_index_readiness_warming_publishes_event() {
    let registry = SessionRegistry::new();
    let session = registry.create("t".to_string(), None, crate::binding::ProjectBinding::None);
    let mut events = crate::events::subscribe();

    let r = readiness("indexed_lexical", 4096, true, false);
    registry
        .record_index_readiness(&session.id, Some(&r), &r.summary())
        .unwrap();

    let envelope = next_event_for(&mut events, &session.id).await;
    assert_eq!(envelope.kind, "index_readiness");
    assert!(matches!(
        envelope.event,
        Event::IndexReadiness { state, lexical_ready, semantic_ready, chunk_count, .. }
            if state == "warming" && lexical_ready && !semantic_ready && chunk_count == Some(4096)
    ));
}

/// A fully-warm index must publish `state: "ready"`.
#[tokio::test]
async fn record_index_readiness_ready_publishes_event() {
    let registry = SessionRegistry::new();
    let session = registry.create("t".to_string(), None, crate::binding::ProjectBinding::None);
    let mut events = crate::events::subscribe();

    let r = readiness("ready", 50_000, true, true);
    registry
        .record_index_readiness(&session.id, Some(&r), &r.summary())
        .unwrap();

    let envelope = next_event_for(&mut events, &session.id).await;
    assert!(matches!(
        envelope.event,
        Event::IndexReadiness { state, semantic_ready, .. }
            if state == "ready" && semantic_ready
    ));
}

/// THE acceptance criterion (#2784): a WARMING index and a READY index with
/// zero chunks must be distinguishable — they produce identical empty search
/// results but mean opposite things.
///
/// Why: this is the exact failure the daily-driver probe hit. `search_code`
/// returned empty during semantic warm-up, the model read that as "nothing
/// there", and hand-explored to the WRONG target. A consumer must be able to
/// tell "I can't see it YET" from "it isn't there" — and a `chunk_count` of 0
/// alone can't: a ready-but-empty index and a warming one both report few/no
/// hits. The `state` discriminant is what carries the distinction, so this
/// pins that the two cases never serialise to the same signal.
/// What: emit a warming index and a ready-with-zero-chunks index, and assert
/// their `state` values differ (`"warming"` vs `"ready"`) even though both
/// would back an empty search.
/// Test: this test.
#[tokio::test]
async fn warming_index_is_distinguishable_from_ready_with_zero_hits() {
    let registry = SessionRegistry::new();
    // Both sessions must exist BEFORE subscribing: `create` itself publishes
    // `session_started`/`session_status_changed` on the same session_id the
    // filter below matches on, which would otherwise be the first envelope
    // read back instead of the readiness event under test.
    let warming_session =
        registry.create("t".to_string(), None, crate::binding::ProjectBinding::None);
    let ready_session =
        registry.create("t".to_string(), None, crate::binding::ProjectBinding::None);
    let mut events = crate::events::subscribe();

    let warming = readiness("walking", 0, false, false);
    registry
        .record_index_readiness(&warming_session.id, Some(&warming), &warming.summary())
        .unwrap();
    let warming_ev = next_event_for(&mut events, &warming_session.id).await;

    // Ready, fully warm, but the repo genuinely has nothing indexed.
    let ready = readiness("ready", 0, true, true);
    registry
        .record_index_readiness(&ready_session.id, Some(&ready), &ready.summary())
        .unwrap();
    let ready_ev = next_event_for(&mut events, &ready_session.id).await;

    let state_of = |ev: &Event| match ev {
        Event::IndexReadiness { state, .. } => state.clone(),
        other => panic!("expected index_readiness, got {other:?}"),
    };
    let warming_state = state_of(&warming_ev.event);
    let ready_state = state_of(&ready_ev.event);

    assert_eq!(warming_state, "warming");
    assert_eq!(ready_state, "ready");
    assert_ne!(
        warming_state, ready_state,
        "a cold/warming index MUST NOT look like a ready index with zero hits — \
         they mean opposite things"
    );
}

/// A `None` probe (fail-open: no daemon / no derivable id) must publish
/// `state: "unavailable"` rather than being swallowed — an unqueryable index
/// is also not evidence of absence.
#[tokio::test]
async fn record_index_readiness_unavailable_publishes_event() {
    let registry = SessionRegistry::new();
    let session = registry.create("t".to_string(), None, crate::binding::ProjectBinding::None);
    let mut events = crate::events::subscribe();

    registry
        .record_index_readiness(&session.id, None, "no daemon")
        .unwrap();

    let envelope = next_event_for(&mut events, &session.id).await;
    assert_eq!(envelope.kind, "index_readiness");
    assert!(matches!(
        envelope.event,
        Event::IndexReadiness { state, index_id, chunk_count, semantic_ready, .. }
            if state == "unavailable"
                && index_id.is_none()
                && chunk_count.is_none()
                && !semantic_ready
    ));
}

/// Recording readiness against an unknown session must error, not panic.
#[tokio::test]
async fn record_index_readiness_unknown_session_errors() {
    let registry = SessionRegistry::new();
    assert!(registry.record_index_readiness("nope", None, "x").is_err());
}

/// (DOC-39 §5.6 Slice D) `record_index_readiness` must cache the snapshot it
/// records, so a LATER `get_readiness` call — without ever re-subscribing to
/// the event stream — retrieves the exact same state.
///
/// Why: this is the registry-level half of the late-attaching-client
/// guarantee: `Event::IndexReadiness` fires once and is easy to miss, so the
/// cache `record_index_readiness` writes (not the event itself) is what
/// `get_readiness` reads back. Exercised directly at the `SessionRegistry`
/// level (no JSON-RPC handler in between) so a regression in the caching
/// assignment itself — as opposed to the protocol layer's passthrough — is
/// caught here.
/// What: records a `Some(IndexReadiness)` probe, then calls `get_readiness`
/// and asserts it returns `ReadinessQuery::Probed` with `state`/`summary`
/// matching what was just recorded.
/// Test: this test.
#[tokio::test]
async fn record_index_readiness_caches_snapshot_for_late_query() {
    let registry = SessionRegistry::new();
    let session = registry.create("t".to_string(), None, crate::binding::ProjectBinding::None);

    let r = readiness("ready", 128, true, true);
    registry
        .record_index_readiness(&session.id, Some(&r), &r.summary())
        .unwrap();

    let queried = registry.get_readiness(&session.id).unwrap();
    match queried {
        events::ReadinessQuery::Probed(snapshot) => {
            assert_eq!(snapshot.state, "ready");
            assert_eq!(snapshot.summary, r.summary());
            assert!(snapshot.semantic_ready);
        }
        events::ReadinessQuery::NeverProbed => {
            panic!("expected a cached snapshot after record_index_readiness, got NeverProbed")
        }
    }
}

/// `get_readiness` against an unknown session must error, not panic —
/// mirrors every other registry method's `session_not_found` convention.
/// Test: this test.
#[tokio::test]
async fn get_readiness_unknown_session_errors() {
    let registry = SessionRegistry::new();
    let err = registry.get_readiness("nope").unwrap_err();
    assert_eq!(err.code, -32007);
}

/// `record_context_budget` must publish a `ContextBudget` event mapping the
/// snapshot field-for-field.
#[tokio::test]
async fn record_context_budget_publishes_event() {
    let registry = SessionRegistry::new();
    let session = registry.create("t".to_string(), None, crate::binding::ProjectBinding::None);
    let mut events = crate::events::subscribe();

    let snapshot = crate::agent_loop::ContextBudgetSnapshot {
        context_window: 200_000,
        overhead_tokens: 50_000,
        overhead_cap_tokens: 80_000,
        working_context_pct: 75,
        overhead_pct: 25,
        within_budget: true,
        fired: true,
        rounds: 2,
    };
    registry
        .record_context_budget(&session.id, &snapshot)
        .unwrap();

    let envelope = next_event_for(&mut events, &session.id).await;
    assert_eq!(envelope.kind, "context_budget");
    assert!(matches!(
        envelope.event,
        Event::ContextBudget {
            context_window_tokens,
            overhead_tokens,
            working_context_pct,
            within_budget,
            compaction_fired,
            compaction_rounds,
            ..
        } if context_window_tokens == 200_000
            && overhead_tokens == 50_000
            && working_context_pct == 75
            && within_budget
            && compaction_fired
            && compaction_rounds == 2
    ));
}

/// `record_context_budget` must cache the resulting [`events::ContextBudgetSnapshot`]
/// on the session entry (issue #3015), so a LATER `get_context_budget` call —
/// without ever re-subscribing to the event stream — retrieves the exact
/// same state. Mirrors `record_index_readiness_caches_snapshot_for_late_query`.
///
/// Why: this is the registry-level half of the late-attaching-client
/// guarantee: `Event::ContextBudget` fires once per turn and is easy to
/// miss, so the cache `record_context_budget` writes (not the event itself)
/// is what `get_context_budget` reads back.
/// What: records a measurement, then calls `get_context_budget` and asserts
/// it returns `ContextBudgetQuery::Recorded` with fields matching what was
/// just recorded.
/// Test: this test.
#[tokio::test]
async fn record_context_budget_caches_snapshot_for_late_query() {
    let registry = SessionRegistry::new();
    let session = registry.create("t".to_string(), None, crate::binding::ProjectBinding::None);

    let snapshot = crate::agent_loop::ContextBudgetSnapshot {
        context_window: 200_000,
        overhead_tokens: 50_000,
        overhead_cap_tokens: 80_000,
        working_context_pct: 75,
        overhead_pct: 25,
        within_budget: true,
        fired: true,
        rounds: 2,
    };
    registry
        .record_context_budget(&session.id, &snapshot)
        .unwrap();

    let queried = registry.get_context_budget(&session.id).unwrap();
    match queried {
        events::ContextBudgetQuery::Recorded(cached) => {
            assert_eq!(cached.context_window_tokens, 200_000);
            assert_eq!(cached.working_context_pct, 75);
            assert!(cached.within_budget);
            assert!(cached.compaction_fired);
            assert_eq!(cached.compaction_rounds, 2);
        }
        events::ContextBudgetQuery::NeverRecorded => {
            panic!("expected a cached snapshot after record_context_budget, got NeverRecorded")
        }
    }
}

/// `get_context_budget` on a session with no recorded measurement must
/// return the typed `NeverRecorded` variant, not an error or a bare `null`.
/// Test: this test.
#[tokio::test]
async fn get_context_budget_never_recorded_returns_never_recorded() {
    let registry = SessionRegistry::new();
    let session = registry.create("t".to_string(), None, crate::binding::ProjectBinding::None);

    let queried = registry.get_context_budget(&session.id).unwrap();
    assert!(matches!(queried, events::ContextBudgetQuery::NeverRecorded));
}

/// `get_context_budget` against an unknown session must error, not panic —
/// mirrors every other registry method's `session_not_found` convention.
/// Test: this test.
#[tokio::test]
async fn get_context_budget_unknown_session_errors() {
    let registry = SessionRegistry::new();
    let err = registry.get_context_budget("nope").unwrap_err();
    assert_eq!(err.code, -32007);
}

// ── issue #3948: incremental per-session working-context floor ────────────────

/// Build one authoritative cadence measurement at `working_context_pct`.
fn floor_measurement(working_context_pct: u8) -> crate::agent_loop::ContextBudgetSnapshot {
    crate::agent_loop::ContextBudgetSnapshot {
        context_window: 200_000,
        overhead_tokens: 200_000 * usize::from(100 - working_context_pct) / 100,
        overhead_cap_tokens: 80_000,
        working_context_pct,
        overhead_pct: 100 - working_context_pct,
        within_budget: working_context_pct >= 60,
        fired: false,
        rounds: 0,
    }
}

/// Issue #3948: every authoritative cadence measurement contributes exactly
/// one sample, and the lowest percentage is retained after a later turn
/// recovers.
/// Test: this test.
#[tokio::test]
async fn context_floor_tracks_every_measurement() {
    let registry = SessionRegistry::new();
    let session = registry.create("t".to_string(), None, crate::binding::ProjectBinding::None);

    for pct in [90, 48, 75] {
        registry
            .record_context_budget(&session.id, &floor_measurement(pct))
            .unwrap();
    }

    let events::ContextBudgetQuery::Recorded(snapshot) =
        registry.get_context_budget(&session.id).unwrap()
    else {
        panic!("expected a recorded context budget");
    };
    assert_eq!(snapshot.working_context_pct, 75, "latest turn still wins");
    assert_eq!(snapshot.working_context_pct_low_water_mark, Some(48));
    assert_eq!(snapshot.working_context_pct_sample_count, 3);
}

/// Issue #3948: one session's floor cannot be moved by another session's
/// measurements in the same registry.
/// Test: this test.
#[tokio::test]
async fn context_floor_is_session_scoped() {
    let registry = SessionRegistry::new();
    let first = registry.create(
        "first".to_string(),
        None,
        crate::binding::ProjectBinding::None,
    );
    let second = registry.create(
        "second".to_string(),
        None,
        crate::binding::ProjectBinding::None,
    );

    registry
        .record_context_budget(&first.id, &floor_measurement(90))
        .unwrap();
    registry
        .record_context_budget(&second.id, &floor_measurement(12))
        .unwrap();

    let events::ContextBudgetQuery::Recorded(snapshot) =
        registry.get_context_budget(&first.id).unwrap()
    else {
        panic!("expected a recorded context budget");
    };
    assert_eq!(snapshot.working_context_pct_low_water_mark, Some(90));
    assert_eq!(snapshot.working_context_pct_sample_count, 1);
}

/// Issue #3948: the sample counter is telemetry and must saturate rather
/// than wrap in a long-lived session.
/// Test: this test.
#[test]
fn context_floor_saturates_sample_count() {
    let mut floor = super::super::registry_context_floor::ContextFloorState {
        low_water_mark: Some(60),
        sample_count: usize::MAX,
    };

    floor.record(48);

    assert_eq!(floor.low_water_mark, Some(48));
    assert_eq!(floor.sample_count, usize::MAX);
}

/// Issue #3948: prove the live query performs no `compression.jsonl` open,
/// rather than only proving malformed contents do not change its answer.
///
/// A FIFO blocks a read-only `File::open` until a writer connects, so the
/// query must return while this FIFO has no writer. The timeout branch
/// connects a non-blocking writer to release a blocked reader before
/// reporting the violation; that timeout is the contract under test, not a
/// wait for eventual consistency.
/// Test: this test.
#[cfg(unix)]
#[tokio::test]
async fn context_budget_query_does_not_open_compression_log() {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    use std::sync::mpsc;

    let dir = crate::agent_loop::telemetry::test_temp_dir("context-floor-no-open");
    let path = CString::new(dir.join("compression.jsonl").as_os_str().as_bytes()).unwrap();
    // SAFETY: `path` is a valid, NUL-terminated path owned for this call.
    let created = unsafe { libc::mkfifo(path.as_ptr(), 0o600) };
    assert_eq!(
        created,
        0,
        "mkfifo failed: {}",
        std::io::Error::last_os_error()
    );

    let registry = Arc::new(SessionRegistry::new());
    let session = registry.create("t".to_string(), None, crate::binding::ProjectBinding::None);
    registry
        .record_context_budget(&session.id, &floor_measurement(73))
        .unwrap();

    let query = crate::agent_loop::telemetry::with_data_dir_env(&dir, || {
        let (tx, rx) = mpsc::sync_channel(1);
        let worker_registry = Arc::clone(&registry);
        let session_id = session.id.clone();
        let worker = std::thread::spawn(move || {
            let _ = tx.send(worker_registry.get_context_budget(&session_id));
        });

        match rx.recv_timeout(Duration::from_secs(5)) {
            Ok(result) => {
                worker.join().expect("budget query worker panicked");
                result
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                // Never let cleanup block on the FIFO itself. If the query
                // opened a reader this non-blocking writer connects and
                // releases it; if it did not, `open` returns `ENXIO` and the
                // detached worker cannot stall the test process.
                let writer = unsafe {
                    libc::open(
                        path.as_ptr(),
                        libc::O_WRONLY | libc::O_NONBLOCK | libc::O_CLOEXEC,
                    )
                };
                if writer >= 0 {
                    let payload = b"{not-json}\n";
                    // SAFETY: `writer` is an open FIFO descriptor and the
                    // payload pointer is valid for `payload.len()` bytes.
                    unsafe {
                        libc::write(writer, payload.as_ptr().cast(), payload.len());
                        libc::close(writer);
                    }
                    worker.join().expect("budget query worker panicked");
                }
                panic!("get_context_budget opened compression.jsonl");
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                worker.join().expect("budget query worker panicked");
                panic!("budget query worker disconnected without a result");
            }
        }
    })
    .await
    .unwrap();

    let events::ContextBudgetQuery::Recorded(snapshot) = query else {
        panic!("expected a recorded context budget");
    };
    assert_eq!(snapshot.working_context_pct_low_water_mark, Some(73));
    assert_eq!(snapshot.working_context_pct_sample_count, 1);

    std::fs::remove_dir_all(&dir).ok();
}

/// Issue #3948: a daemon restart drops the whole session, so the live floor
/// cannot silently reset to a wrong value — the RPC reports
/// `session_not_found`, and the durable JSONL still carries that session's
/// history for offline analysis.
/// Test: this test.
#[tokio::test]
async fn context_floor_after_restart_is_absent_not_wrong() {
    let dir = crate::agent_loop::telemetry::test_temp_dir("context-floor-restart");
    let registry = SessionRegistry::new();
    let session = registry.create("t".to_string(), None, crate::binding::ProjectBinding::None);
    registry
        .record_context_budget(&session.id, &floor_measurement(48))
        .unwrap();
    crate::agent_loop::telemetry::record_cadence_event(
        &dir,
        Some(session.id.clone()),
        100_000,
        104_000,
        200_000,
        5,
        4,
    );

    // The restart: nothing in-memory carries over.
    let restarted = SessionRegistry::new();
    let err = restarted.get_context_budget(&session.id).unwrap_err();
    assert_eq!(
        err.code, -32007,
        "the session itself is gone, not its floor"
    );

    assert_eq!(
        crate::agent_loop::telemetry::session_working_context_floor(&dir, &session.id),
        (Some(48), 1),
        "the durable log remains the offline history source across a restart"
    );

    std::fs::remove_dir_all(&dir).ok();
}
