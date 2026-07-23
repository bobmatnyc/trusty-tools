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

/// (issue #3298) `SessionAdded`/`SessionActivityUpdate` ARE session-scoped —
/// unlike the daemon-scoped workstream events below.
#[test]
fn session_added_and_activity_update_are_session_scoped() {
    let added = Event::SessionAdded {
        session_id: "s-1".into(),
        workstream_id: "ws-1".into(),
        binding_time: Utc::now(),
    };
    assert_eq!(added.session_id(), Some("s-1"));
    assert_eq!(added.kind(), "session_added");

    let activity = Event::SessionActivityUpdate {
        session_id: "s-1".into(),
        last_turn_at: Utc::now(),
        has_running_task: false,
    };
    assert_eq!(activity.session_id(), Some("s-1"));
    assert_eq!(activity.kind(), "session_activity_update");
}

/// (issue #3297) `WorkstreamActivationChanged` is daemon-scoped, not
/// session-scoped — mirrors `Event::Ping`'s `None`.
#[test]
fn workstream_activation_changed_is_not_session_scoped() {
    let ev = Event::WorkstreamActivationChanged {
        new_active_id: Some("ws-2".into()),
        prior_id: Some("ws-1".into()),
    };
    assert_eq!(ev.session_id(), None);
    assert_eq!(ev.kind(), "workstream_activation_changed");
}

/// (issue #3297) `WorkstreamStateInferred` is likewise daemon-scoped.
#[test]
fn workstream_state_inferred_is_not_session_scoped() {
    let ev = Event::WorkstreamStateInferred {
        workstream_id: "ws-1".into(),
        state: "closed".into(),
        reason: "closed".into(),
    };
    assert_eq!(ev.session_id(), None);
    assert_eq!(ev.kind(), "workstream_state_inferred");
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
            hits: vec![],
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
                text: "PKCE required".into(),
                run_id: None,
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
        Event::SessionAdded {
            session_id: "s".into(),
            workstream_id: "ws-1".into(),
            binding_time: Utc::now(),
        },
        Event::SessionActivityUpdate {
            session_id: "s".into(),
            last_turn_at: Utc::now(),
            has_running_task: false,
        },
        Event::WorkstreamActivationChanged {
            new_active_id: Some("ws-2".into()),
            prior_id: Some("ws-1".into()),
        },
        Event::WorkstreamStateInferred {
            workstream_id: "ws-1".into(),
            state: "idle".into(),
            reason: "deactivated".into(),
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
/// `injected` flag and (DOC-39 Slice C) its recalled `text` and `run_id`.
///
/// Why: these events reach the UI over SSE and through `session.attach`'s
/// ring-buffer replay, both of which serialize. A field that silently
/// failed to round-trip would strand the UI's whole differentiating
/// surface. `hit_count: None` is covered explicitly because `Option`
/// round-tripping is where a shape regression would most plausibly hide;
/// `run_id: None` on the held-back entry covers the same risk for Slice C's
/// new optional field.
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
                text: "PKCE is required for the OAuth flow".into(),
                run_id: Some("run-42".into()),
            },
            RecalledMemory {
                score: 0.41,
                injected: false,
                text: "held-back memory text".into(),
                run_id: None,
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
    assert_eq!(
        value["event"]["results"][1]["text"], "held-back memory text",
        "the held-back result's TEXT must be present on the wire"
    );
    assert_eq!(value["event"]["results"][0]["run_id"], "run-42");
    assert_eq!(
        value["event"]["results"][1]["run_id"],
        serde_json::Value::Null
    );

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
                injected: true,
                text: "PKCE is required for the OAuth flow".into(),
                run_id: Some("run-42".into()),
            },
            RecalledMemory {
                score: 0.41,
                injected: false,
                text: "held-back memory text".into(),
                run_id: None,
            },
        ],
        "the injected/held-back split must survive the wire"
    );
}

/// A `RecalledMemory` recorded before DOC-39 Slice C (no `text`/`run_id` on
/// the wire) must still deserialize, defaulting the new fields.
///
/// Why: `#[serde(default)]` on both new fields is the back-compat contract —
/// a ring-buffer entry or persisted transcript recorded by an older binary
/// must not fail to load just because Slice C added fields.
/// Test: this test.
#[test]
fn recalled_memory_deserializes_without_text_or_run_id() {
    let old_shape = serde_json::json!({"score": 0.5, "injected": true});
    let back: RecalledMemory = serde_json::from_value(old_shape).unwrap();
    assert_eq!(
        back,
        RecalledMemory {
            score: 0.5,
            injected: true,
            text: String::new(),
            run_id: None,
        }
    );
}

/// `SearchPerformed` must round-trip, including an uncountable
/// (`hit_count: None`) result and its (empty, in the uncountable case) `hits`.
#[test]
fn search_performed_round_trips_through_json() {
    let event = Event::SearchPerformed {
        session_id: "s1".into(),
        agent: "python-engineer".into(),
        agent_id: "eng-1".into(),
        lane: "lexical".into(),
        query: "where is auth".into(),
        hit_count: None,
        hits: vec![],
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
    assert_eq!(value["event"]["hits"], serde_json::json!([]));

    let back: SessionEventEnvelope = serde_json::from_value(value).unwrap();
    assert!(matches!(
        back.event,
        Event::SearchPerformed { hit_count, latency_ms, lane, hits, .. }
            if hit_count.is_none() && latency_ms == 17 && lane == "lexical" && hits.is_empty()
    ));
}

/// `SearchPerformed.hits` must round-trip each hit's `path` AND `score`
/// (DOC-39 Slice B) — the search-audit UI's whole point is telling hits
/// apart by both fields, not just knowing how many there were.
#[test]
fn search_performed_hits_round_trip_through_json() {
    let event = Event::SearchPerformed {
        session_id: "s1".into(),
        agent: "python-engineer".into(),
        agent_id: "eng-1".into(),
        lane: "semantic".into(),
        query: "where is auth".into(),
        hit_count: Some(2),
        hits: vec![
            SearchHit {
                path: "src/auth.rs".into(),
                score: 0.87,
            },
            SearchHit {
                path: "src/session/session.rs".into(),
                score: 0.52,
            },
        ],
        latency_ms: 9,
    };
    let envelope = SessionEventEnvelope::new("s1".into(), 6, Utc::now(), event);
    let value = serde_json::to_value(&envelope).unwrap();

    assert_eq!(value["event"]["hits"][0]["path"], "src/auth.rs");
    assert_eq!(value["event"]["hits"][0]["score"], 0.87);
    assert_eq!(value["event"]["hits"][1]["path"], "src/session/session.rs");
    assert_eq!(value["event"]["hits"][1]["score"], 0.52);

    let back: SessionEventEnvelope = serde_json::from_value(value).unwrap();
    let Event::SearchPerformed { hits, .. } = back.event else {
        panic!("expected SearchPerformed");
    };
    assert_eq!(
        hits,
        vec![
            SearchHit {
                path: "src/auth.rs".into(),
                score: 0.87,
            },
            SearchHit {
                path: "src/session/session.rs".into(),
                score: 0.52,
            },
        ]
    );
}

/// A `SearchPerformed` transcript recorded BEFORE `hits` existed (DOC-39
/// Slice B) must still deserialize — the whole point of `#[serde(default)]`,
/// mirroring `tool_started_without_agent_id_field_still_deserializes`.
#[test]
fn search_performed_without_hits_field_still_deserializes() {
    let legacy_json = serde_json::json!({
        "type": "search_performed",
        "session_id": "s1",
        "agent": "python-engineer",
        "agent_id": "eng-1",
        "lane": "grep",
        "query": "where is auth",
        "hit_count": 3,
        "latency_ms": 12
    });
    let event: Event = serde_json::from_value(legacy_json)
        .expect("a SearchPerformed payload recorded before `hits` existed must still deserialize");
    let Event::SearchPerformed {
        hits, hit_count, ..
    } = event
    else {
        panic!("expected SearchPerformed");
    };
    assert!(
        hits.is_empty(),
        "serde(default) yields an empty Vec for a pre-existing record"
    );
    assert_eq!(hit_count, Some(3));
}

/// `SearchHit` itself must round-trip through JSON with `path`/`score` intact.
#[test]
fn search_hit_round_trips_through_json() {
    let hit = SearchHit {
        path: "src/auth.rs".into(),
        score: 0.87,
    };
    let value = serde_json::to_value(&hit).unwrap();
    assert_eq!(value["path"], "src/auth.rs");
    assert_eq!(value["score"], 0.87);
    let back: SearchHit = serde_json::from_value(value).unwrap();
    assert_eq!(back, hit);
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

/// `AgentMessageDelta` (tcode streaming epic #3696 Slice 0) must round-trip
/// through JSON with every field intact, and `kind()`/`session_id()` must
/// agree with the serde tag and the carried `session_id`.
///
/// Why: this variant is the wire contract every downstream streaming slice
/// builds on — a field that silently failed to round-trip (or a `kind()`/
/// `session_id()` arm that drifted from the serde tag) would strand every
/// later slice on a broken foundation. `done: false` is exercised here since
/// it is the more failure-prone case (`bool` defaults can hide a missed
/// field); the Gap A single-delta `done: true` shape is documented on the
/// variant itself and is structurally identical.
/// Test: this test.
#[test]
fn agent_message_delta_round_trips_through_json() {
    let event = Event::AgentMessageDelta {
        session_id: "s1".into(),
        agent: "python-engineer".into(),
        agent_id: "eng-1".into(),
        turn_id: "turn-42".into(),
        delta: "partial toke".into(),
        done: false,
    };
    assert_eq!(event.kind(), "agent_message_delta");
    assert_eq!(event.session_id(), Some("s1"));

    let envelope = SessionEventEnvelope::new("s1".into(), 9, Utc::now(), event);
    let value = serde_json::to_value(&envelope).unwrap();

    assert_eq!(value["kind"], "agent_message_delta");
    assert_eq!(value["event"]["type"], "agent_message_delta");
    assert_eq!(value["event"]["agent"], "python-engineer");
    assert_eq!(value["event"]["agent_id"], "eng-1");
    assert_eq!(value["event"]["turn_id"], "turn-42");
    assert_eq!(value["event"]["delta"], "partial toke");
    assert_eq!(value["event"]["done"], false);

    let back: SessionEventEnvelope = serde_json::from_value(value).unwrap();
    let Event::AgentMessageDelta {
        session_id,
        agent,
        agent_id,
        turn_id,
        delta,
        done,
    } = back.event
    else {
        panic!("expected AgentMessageDelta");
    };
    assert_eq!(session_id, "s1");
    assert_eq!(agent, "python-engineer");
    assert_eq!(agent_id, "eng-1");
    assert_eq!(turn_id, "turn-42");
    assert_eq!(delta, "partial toke");
    assert!(!done);
}

/// Gap A shape: a harness that only exposes a turn's full text once it is
/// done emits exactly ONE delta carrying the ENTIRE turn text with
/// `done: true`. This must round-trip identically to the Gap B (many small
/// chunks) shape above — the wire format makes no distinction between "one
/// long delta" and "one short delta", so pinning a full-paragraph-sized
/// `delta` here guards against a future producer/consumer accidentally
/// assuming deltas are always short token chunks (e.g. truncating, or
/// chunking on write).
#[test]
fn agent_message_delta_full_text_in_one_delta_round_trips() {
    let full_turn_text = "This is the entire assistant turn's text, delivered \
        as a single delta because the underlying harness (Gap A) only \
        exposes a turn once it has finished generating — there is no partial \
        token stream to forward, so the whole turn arrives in one shot."
        .to_string();
    let event = Event::AgentMessageDelta {
        session_id: "s1".into(),
        agent: "pm".into(),
        agent_id: "pm-1".into(),
        turn_id: "turn-99".into(),
        delta: full_turn_text.clone(),
        done: true,
    };
    let envelope = SessionEventEnvelope::new("s1".into(), 10, Utc::now(), event);
    let value = serde_json::to_value(&envelope).unwrap();

    assert_eq!(value["event"]["delta"], full_turn_text);
    assert_eq!(value["event"]["done"], true);

    let back: SessionEventEnvelope = serde_json::from_value(value).unwrap();
    let Event::AgentMessageDelta { delta, done, .. } = back.event else {
        panic!("expected AgentMessageDelta");
    };
    assert_eq!(
        delta, full_turn_text,
        "a full-turn-sized delta must survive the wire byte-for-byte, same as a short chunk"
    );
    assert!(done, "Gap A's single delta must carry done: true");
}

/// `agent_id` on `AgentMessageDelta` is `#[serde(default)]` — a producer
/// that predates per-spawn ids (or a stripped-down test fixture) must still
/// deserialize, defaulting to an empty string rather than failing.
#[test]
fn agent_message_delta_deserializes_without_agent_id() {
    let old_shape = serde_json::json!({
        "type": "agent_message_delta",
        "session_id": "s1",
        "agent": "pm",
        "turn_id": "turn-1",
        "delta": "hello",
        "done": true
    });
    let back: Event = serde_json::from_value(old_shape).unwrap();
    match back {
        Event::AgentMessageDelta { agent_id, done, .. } => {
            assert_eq!(agent_id, "");
            assert!(done);
        }
        other => panic!("unexpected event: {other:?}"),
    }
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
