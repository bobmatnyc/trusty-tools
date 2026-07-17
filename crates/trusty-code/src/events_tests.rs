//! Unit tests for the `events` wire taxonomy.
//!
//! Why: split out of `events.rs` per the crate's `_tests.rs` sibling-file
//! convention (see `session::registry_tests` for precedent) so the event
//! taxonomy's test surface can grow with the taxonomy without pushing the
//! production file past its 500-SLOC cap — test files carry the 1500-SLOC cap.
//! What: covers the serde `type`-tag wire shape, `kind()`/serde-tag parity for
//! every variant, envelope round-tripping (including the UI-Phase-1 structured
//! retrieval events), and the stderr relay prefix.
//! Test: this module is itself the test surface.

use super::*;

#[test]
fn event_serializes_with_type_tag() {
    let ev = Event::PmThinking {
        session_id: "s1".into(),
        text: "considering options".into(),
    };
    let s = serde_json::to_string(&ev).unwrap();
    assert!(s.contains("\"type\":\"pm_thinking\""), "{s}");
    assert!(s.contains("\"session_id\":\"s1\""), "{s}");
}

#[test]
fn ping_roundtrips() {
    let s = serde_json::to_string(&Event::Ping).unwrap();
    let back: Event = serde_json::from_str(&s).unwrap();
    assert!(matches!(back, Event::Ping));
}

#[test]
fn session_id_returns_correct_field() {
    let ev = Event::AgentMessage {
        session_id: "abc".into(),
        agent: "python".into(),
        text: "hi".into(),
    };
    assert_eq!(ev.session_id(), Some("abc"));
    assert_eq!(Event::Ping.session_id(), None);
}

#[test]
fn bus_is_singleton() {
    let a = bus();
    let b = bus();
    // Two senders to the same channel: a message sent on `a` should be
    // visible to a receiver from `b`.
    let mut rx = b.subscribe();
    let envelope = SessionEventEnvelope::new("s-bus".into(), 1, Utc::now(), Event::Ping);
    let _ = a.send(envelope);
    // Drain via try_recv to avoid an async runtime in this sync test.
    let got = rx.try_recv().expect("expected an envelope");
    assert!(matches!(got.event, Event::Ping));
}

#[tokio::test]
async fn publish_round_trips_through_subscribe() {
    let mut rx = subscribe();
    let envelope = SessionEventEnvelope::new(
        "t1".into(),
        1,
        Utc::now(),
        Event::SessionStarted {
            session_id: "t1".into(),
            project: "demo".into(),
        },
    );
    publish(envelope);
    let got = rx.recv().await.unwrap();
    assert_eq!(got.session_id, "t1");
    assert_eq!(got.seq, 1);
    assert_eq!(got.kind, "session_started");
    match got.event {
        Event::SessionStarted {
            session_id,
            project,
        } => {
            assert_eq!(session_id, "t1");
            assert_eq!(project, "demo");
        }
        other => panic!("unexpected event: {other:?}"),
    }
}

/// `kind()` must match the serde `"type"` tag for every variant — the
/// guard `SessionEventEnvelope::new`'s doc comment references.
#[test]
fn kind_matches_serde_tag_for_every_variant() {
    let samples: Vec<Event> = vec![
        Event::SessionStarted {
            session_id: "s".into(),
            project: "p".into(),
        },
        Event::SessionDone {
            session_id: "s".into(),
            status: "done".into(),
        },
        Event::SessionCancelled {
            session_id: "s".into(),
        },
        Event::SessionStatusChanged {
            session_id: "s".into(),
            status: "running".into(),
        },
        Event::SessionInput {
            session_id: "s".into(),
            input: "hi".into(),
        },
        Event::ToolStarted {
            session_id: "s".into(),
            agent: "pm".into(),
            agent_id: "pm-1".into(),
            tool: "bash".into(),
            call_id: "c1".into(),
            args_preview: "ls".into(),
        },
        Event::ToolFinished {
            session_id: "s".into(),
            agent: "pm".into(),
            agent_id: "pm-1".into(),
            tool: "bash".into(),
            call_id: "c1".into(),
            success: true,
            result_preview: "ok".into(),
        },
        Event::ToolError {
            session_id: "s".into(),
            agent: "pm".into(),
            agent_id: "pm-1".into(),
            tool: "bash".into(),
            call_id: "c1".into(),
            error: "boom".into(),
        },
        Event::SearchPerformed {
            session_id: "s".into(),
            agent: "python-engineer".into(),
            agent_id: "eng-1".into(),
            lane: "semantic".into(),
            query: "where is auth".into(),
            hit_count: Some(3),
            latency_ms: 42,
        },
        Event::MemoryRecalled {
            session_id: "s".into(),
            agent: "pm".into(),
            agent_id: "pm-1".into(),
            query: "pkce".into(),
            results: vec![RecalledMemory {
                score: 0.41,
                injected: false,
            }],
        },
        Event::Log {
            session_id: "s".into(),
            level: "info".into(),
            message: "m".into(),
        },
        Event::Progress {
            session_id: "s".into(),
            message: "m".into(),
            percent: None,
        },
        Event::Message {
            session_id: "s".into(),
            text: "hi".into(),
        },
        Event::IndexReadiness {
            session_id: "s".into(),
            state: "warming".into(),
            index_id: Some("repo".into()),
            lifecycle_status: Some("indexed_lexical".into()),
            chunk_count: Some(7),
            lexical_ready: true,
            semantic_ready: false,
            graph_ready: false,
            summary: "warming".into(),
        },
        Event::ContextBudget {
            session_id: "s".into(),
            context_window_tokens: 200_000,
            overhead_tokens: 1_000,
            overhead_cap_tokens: 80_000,
            working_context_pct: 99,
            overhead_pct: 1,
            within_budget: true,
            compaction_fired: false,
            compaction_rounds: 0,
        },
        Event::Ping,
    ];
    for ev in samples {
        let value = serde_json::to_value(&ev).unwrap();
        let wire_type = value["type"].as_str().unwrap_or("ping");
        assert_eq!(
            ev.kind(),
            wire_type,
            "kind() drifted from the serde tag for {ev:?}"
        );
    }
}

/// The UI-Phase-1 structured events must survive a full envelope
/// round-trip with every field intact — including each recalled memory's
/// `injected` flag.
///
/// Why: these events reach the UI over SSE and through `session.attach`'s
/// ring-buffer replay, both of which serialize. A field that silently
/// failed to round-trip would strand the UI's whole differentiating
/// surface. `hit_count: None` is covered explicitly because `Option`
/// round-tripping is where a shape regression would most plausibly hide.
/// Test: this test.
#[test]
fn recalled_memory_round_trips_through_json() {
    let event = Event::MemoryRecalled {
        session_id: "s1".into(),
        agent: "pm".into(),
        agent_id: "pm-1".into(),
        query: "pkce".into(),
        results: vec![
            RecalledMemory {
                score: 0.93,
                injected: true,
            },
            RecalledMemory {
                score: 0.41,
                injected: false,
            },
        ],
    };
    let envelope = SessionEventEnvelope::new("s1".into(), 4, Utc::now(), event);
    let value = serde_json::to_value(&envelope).unwrap();

    assert_eq!(value["kind"], "memory_recalled");
    assert_eq!(value["event"]["agent"], "pm");
    assert_eq!(value["event"]["agent_id"], "pm-1");
    assert_eq!(value["event"]["results"][1]["injected"], false);
    assert_eq!(value["event"]["results"][1]["score"], 0.41);

    let back: SessionEventEnvelope = serde_json::from_value(value).unwrap();
    let Event::MemoryRecalled {
        results,
        agent,
        agent_id,
        ..
    } = back.event
    else {
        panic!("expected MemoryRecalled");
    };
    assert_eq!(agent, "pm");
    assert_eq!(agent_id, "pm-1", "agent_id must round-trip (DOC-39 AC-13)");
    assert_eq!(
        results,
        vec![
            RecalledMemory {
                score: 0.93,
                injected: true
            },
            RecalledMemory {
                score: 0.41,
                injected: false
            },
        ],
        "the injected/held-back split must survive the wire"
    );
}

/// `SearchPerformed` must round-trip, including an uncountable
/// (`hit_count: None`) result.
#[test]
fn search_performed_round_trips_through_json() {
    let event = Event::SearchPerformed {
        session_id: "s1".into(),
        agent: "python-engineer".into(),
        agent_id: "eng-1".into(),
        lane: "lexical".into(),
        query: "where is auth".into(),
        hit_count: None,
        latency_ms: 17,
    };
    let envelope = SessionEventEnvelope::new("s1".into(), 5, Utc::now(), event);
    let value = serde_json::to_value(&envelope).unwrap();

    assert_eq!(value["kind"], "search_performed");
    assert_eq!(value["event"]["lane"], "lexical");
    assert!(
        value["event"]["hit_count"].is_null(),
        "an uncountable hit count must stay null, never coerce to 0"
    );

    let back: SessionEventEnvelope = serde_json::from_value(value).unwrap();
    assert!(matches!(
        back.event,
        Event::SearchPerformed { hit_count, latency_ms, lane, .. }
            if hit_count.is_none() && latency_ms == 17 && lane == "lexical"
    ));
}

/// A `ToolStarted` transcript recorded BEFORE `agent_id` existed (DOC-39
/// AC-13) must still deserialize — the whole point of `#[serde(default)]`.
///
/// Why: `session.get_transcript`/`session.attach` replay reads durably
/// stored JSON that may predate this field. A hard deserialization failure
/// here would break every pre-existing recorded session the moment this
/// crate upgrades, which is exactly what additive schema evolution must
/// avoid.
/// What: hand-builds the pre-#2862-successor wire shape (no `agent_id` key
/// at all) and asserts it still parses, defaulting `agent_id` to `""` (NOT
/// [`UNATTRIBUTED_AGENT_ID`]'s sentinel text — `#[serde(default)]` calls
/// `String::default()`, which is empty, not the sentinel).
/// Test: this test.
#[test]
fn tool_started_without_agent_id_field_still_deserializes() {
    let legacy_json = serde_json::json!({
        "type": "tool_started",
        "session_id": "s1",
        "agent": "pm",
        "tool": "bash",
        "call_id": "c1",
        "args_preview": "ls"
    });
    let event: Event = serde_json::from_value(legacy_json)
        .expect("a ToolStarted payload recorded before agent_id existed must still deserialize");
    let Event::ToolStarted {
        agent, agent_id, ..
    } = event
    else {
        panic!("expected ToolStarted");
    };
    assert_eq!(agent, "pm");
    assert_eq!(
        agent_id, "",
        "serde(default) yields an empty string for a pre-existing record, not the sentinel"
    );
}

/// `SessionEventEnvelope` must round-trip through JSON with every field
/// present, and `kind` must be derived from `event.kind()`.
#[test]
fn session_event_envelope_round_trips_through_json() {
    let event = Event::SessionInput {
        session_id: "s1".into(),
        input: "hello".into(),
    };
    let envelope = SessionEventEnvelope::new("s1".into(), 7, Utc::now(), event);

    let value = serde_json::to_value(&envelope).unwrap();
    assert_eq!(value["session_id"], "s1");
    assert_eq!(value["seq"], 7);
    assert_eq!(value["kind"], "session_input");
    assert_eq!(value["event"]["type"], "session_input");
    assert!(value["at"].is_string());

    let back: SessionEventEnvelope = serde_json::from_value(value).unwrap();
    assert_eq!(back.session_id, "s1");
    assert_eq!(back.seq, 7);
}

#[test]
fn preview_truncates_at_char_boundary() {
    assert_eq!(preview("hi", 5), "hi");
    let out = preview("abcdef", 3);
    assert_eq!(out.chars().count(), 4); // 3 + ellipsis
    assert!(out.starts_with("abc"));
    assert!(out.ends_with('\u{2026}'));
}

#[test]
fn event_line_prefix_is_stable() {
    // Lock in the wire constant — changing it breaks the parent/child
    // relay protocol.
    assert_eq!(EVENT_LINE_PREFIX, "__OMPM_EVENT__ ");
}
