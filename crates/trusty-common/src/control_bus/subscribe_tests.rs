//! Golden frames and error arms for the `events.subscribe` wire contract.
//!
//! Why (#9075): ADR-0065 decision 3 puts the subscribe half of the bus
//!      contract in this module, and `trusty-crate-contracts` does not link
//!      `trusty-events` (owner ruling, df-D70 Q3). These literal frames are
//!      therefore the only place both sides of the socket are held to one
//!      shape: the daemon (#9075 PR2) and console's client (#9076) each build
//!      against the types these tests pin.
//! What: the socket-path helper, golden JSON for the request, the params, the
//!       filter and both response items, the `SubscribeFilter`/`Filter`
//!       agreement matrix plus the new `kind` and `actor` axes, and one
//!       rejection test per error arm.
//! Test: this file.

use super::*;
use serde_json::{Value, json};

/// A fixed UUIDv7, so a golden frame carries a byte-stable `id`.
const GOLDEN_ID: &str = "01929d3c-7b8e-7c3a-9f00-000000000001";

fn golden_event_json() -> Value {
    json!({
        "source": "mpm",
        "session": "s-1",
        "seq": 42,
        "at": "2026-10-19T07:48:00Z",
        "payload": { "domain": "ping" },
        "id": GOLDEN_ID,
    })
}

fn event(source: HarnessSource, session: Option<&str>, payload: HarnessPayload) -> HarnessEvent {
    HarnessEvent {
        source,
        session: session.map(str::to_string),
        seq: 1,
        at: chrono::Utc::now(),
        payload,
        id: EventId::new(),
        parent_id: None,
    }
}

fn meta(actor: Actor) -> ActionMeta {
    ActionMeta {
        id: EventId::new(),
        at: chrono::Utc::now(),
        source: HarnessSource::Mpm,
        session: Some("s-1".into()),
        parent_id: None,
        actor,
        objects: Vec::new(),
        schema_version: 1,
    }
}

fn agent(name: &str) -> Actor {
    Actor::Agent {
        name: name.into(),
        agent_id: format!("{name}-id"),
    }
}

fn file_action(actor: Actor) -> HarnessPayload {
    HarnessPayload::Action(ActionEvent::File {
        meta: meta(actor),
        phase: FilePhase::Written,
        path: PathRef {
            path: "src/lib.rs".into(),
            diff_ref: None,
        },
    })
}

fn tool_action(actor: Actor) -> HarnessPayload {
    HarnessPayload::Action(ActionEvent::Tool {
        meta: meta(actor),
        phase: CallPhase::Started,
        tool: "Bash".into(),
        call_id: "c-1".into(),
    })
}

/// Every event shape the filter tests run a predicate over.
fn corpus() -> Vec<HarnessEvent> {
    vec![
        event(HarnessSource::Mpm, Some("s-1"), HarnessPayload::Ping),
        event(HarnessSource::Code, Some("s-2"), HarnessPayload::Ping),
        event(HarnessSource::Agents, None, HarnessPayload::Ping),
        event(
            HarnessSource::Mpm,
            Some("s-1"),
            HarnessPayload::Hook {
                kind: "PreToolUse".into(),
                data: json!({}),
            },
        ),
        event(
            HarnessSource::Mpm,
            Some("s-1"),
            file_action(agent("rust-engineer")),
        ),
        event(
            HarnessSource::Code,
            Some("s-2"),
            tool_action(Actor::Operator),
        ),
        event(HarnessSource::Agents, None, tool_action(Actor::System)),
    ]
}

/// Serialise `value` and compare it with `golden` as JSON values, so key order
/// cannot make a correct frame fail.
fn assert_serializes_to<T: serde::Serialize>(value: &T, golden: &Value) {
    let actual = serde_json::to_value(value).expect("serialize");
    assert_eq!(&actual, golden, "serialised frame drifted from the golden");
}

// ---- socket path ----

/// The socket is named after the bus, never its host (ADR-0065 decision 2).
#[test]
fn events_socket_path_is_named_after_the_bus() {
    use crate::data_dir::{DATA_DIR_OVERRIDE_ENV, ENV_LOCK};

    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().expect("tempdir");
    // SAFETY: guarded by ENV_LOCK, this crate's lock for env mutation.
    unsafe { std::env::set_var(DATA_DIR_OVERRIDE_ENV, tmp.path()) };

    let events = events_socket_path();
    let expected = crate::daemon_addr::daemon_socket_path("trusty-events");
    let console = crate::daemon_addr::daemon_socket_path("trusty-console");

    unsafe { std::env::remove_var(DATA_DIR_OVERRIDE_ENV) };

    let events = events.expect("resolve events socket");
    assert_eq!(EVENTS_SERVICE, "trusty-events");
    assert_eq!(events, expected.expect("resolve daemon socket"));
    assert_ne!(events, console.expect("resolve console socket"));
    assert!(
        events.ends_with("trusty-events/trusty-events.sock"),
        "{} must be <data dir>/trusty-events/trusty-events.sock",
        events.display()
    );
    assert!(events.starts_with(tmp.path()), "the override must apply");
}

// ---- params and filter golden frames ----

#[test]
fn subscribe_params_golden_roundtrip() {
    let golden = json!({
        "since_seq": 41,
        "filters": {
            "source": "mpm",
            "session": "s-1",
            "domains": ["action", "hook"],
            "kind": "file",
            "actor": "rust-engineer",
        },
    });

    let params: SubscribeParams = serde_json::from_value(golden.clone()).expect("decode golden");
    assert_eq!(params.since_seq, Some(41));
    assert_eq!(params.filters.source, Some(HarnessSource::Mpm));
    assert_eq!(params.filters.session.as_deref(), Some("s-1"));
    assert_eq!(
        params.filters.domains,
        Some(vec!["action".to_string(), "hook".to_string()])
    );
    assert_eq!(params.filters.kind.as_deref(), Some("file"));
    assert_eq!(params.filters.actor.as_deref(), Some("rust-engineer"));

    assert_serializes_to(&params, &golden);
}

// #9075: the resume check's id travels beside `since_seq`, and is omitted
// when absent so an older frame stays byte-identical.
#[test]
fn subscribe_params_since_id_golden_roundtrip() {
    let id = EventId::new();
    let golden = json!({ "since_seq": 9, "since_id": id, "filters": {} });
    let params: SubscribeParams = serde_json::from_value(golden.clone()).expect("decode golden");
    assert_eq!(params.since_id, Some(id));
    assert_serializes_to(&params, &golden);

    let without: SubscribeParams =
        serde_json::from_value(json!({ "since_seq": 9 })).expect("decode without since_id");
    assert_eq!(without.since_id, None);
    assert!(
        serde_json::to_value(&without)
            .expect("encode")
            .get("since_id")
            .is_none(),
        "an absent since_id is not written"
    );
}

/// `{}` is "live only, everything": no replay, a match-all filter.
#[test]
fn subscribe_params_empty_object_is_live_and_match_all() {
    let params: SubscribeParams = serde_json::from_value(json!({})).expect("decode {}");
    assert_eq!(params, SubscribeParams::default());
    assert_eq!(params.since_seq, None);
    assert_eq!(params.filters, SubscribeFilter::default());
    assert_serializes_to(&params, &json!({ "filters": {} }));
}

#[test]
fn subscribe_filter_golden_roundtrip() {
    let golden = json!({ "source": "code", "kind": "tool" });
    let filter: SubscribeFilter = serde_json::from_value(golden.clone()).expect("decode filter");
    assert_eq!(filter.source, Some(HarnessSource::Code));
    assert_eq!(filter.kind.as_deref(), Some("tool"));
    assert_eq!(filter.session, None);
    assert_eq!(filter.domains, None);
    assert_eq!(filter.actor, None);
    assert_serializes_to(&filter, &golden);
}

// ---- response item golden frames ----

#[test]
fn subscribe_item_event_golden_roundtrip() {
    let golden = json!({
        "type": "event",
        "persisted": true,
        "event": golden_event_json(),
    });

    let item: SubscribeItem = serde_json::from_value(golden.clone()).expect("decode item");
    match &item {
        SubscribeItem::Event { event, persisted } => {
            assert!(*persisted);
            assert_eq!(event.seq, 42);
            assert_eq!(event.source, HarnessSource::Mpm);
            assert_eq!(event.id.to_string(), GOLDEN_ID);
            assert_eq!(event.payload, HarnessPayload::Ping);
        }
        other => panic!("expected an event item, got {other:?}"),
    }
    assert_serializes_to(&item, &golden);
}

#[test]
fn subscribe_item_gap_golden_roundtrip() {
    let golden = json!({ "type": "gap", "skipped": 12 });
    let item: SubscribeItem = serde_json::from_value(golden.clone()).expect("decode gap");
    assert_eq!(item, SubscribeItem::Gap { skipped: 12 });
    assert_serializes_to(&item, &golden);
}

/// Server-to-client frames are additive (DOC-73 §3.3): an older client reads a
/// newer daemon's item and skips the field it does not know.
#[test]
fn subscribe_item_ignores_an_unknown_field() {
    let item: SubscribeItem =
        serde_json::from_value(json!({ "type": "gap", "skipped": 3, "resume_seq": 90 }))
            .expect("an added field must not break an older client");
    assert_eq!(item, SubscribeItem::Gap { skipped: 3 });
}

// ---- filter semantics ----

/// `SubscribeFilter` is the serde twin of `Filter`; on the axes both carry
/// they must answer identically, or a server-side filter and a client-side
/// filter disagree about the same subscription.
#[test]
fn subscribe_filter_agrees_with_filter() {
    let cases = [
        Filter::default(),
        Filter {
            source: Some(HarnessSource::Mpm),
            ..Filter::default()
        },
        Filter {
            session: Some("s-1".into()),
            ..Filter::default()
        },
        Filter {
            domains: Some(vec!["action"]),
            ..Filter::default()
        },
        Filter {
            source: Some(HarnessSource::Code),
            session: Some("s-2".into()),
            domains: Some(vec!["ping", "action"]),
        },
        Filter {
            session: Some("absent".into()),
            domains: Some(vec![]),
            ..Filter::default()
        },
    ];

    for (case, filter) in cases.iter().enumerate() {
        let twin = SubscribeFilter::from(filter);
        for (idx, ev) in corpus().iter().enumerate() {
            assert_eq!(
                twin.matches(ev),
                filter.matches(ev),
                "case {case}, event {idx}: SubscribeFilter and Filter disagree"
            );
        }
    }
}

#[test]
fn subscribe_filter_kind_axis_keeps_only_that_action_kind() {
    let filter = SubscribeFilter {
        kind: Some("file".into()),
        ..SubscribeFilter::default()
    };
    let hits: Vec<bool> = corpus().iter().map(|ev| filter.matches(ev)).collect();
    // Only the File action matches; Tool actions and every non-action event,
    // which has no kind, are excluded.
    assert_eq!(hits, [false, false, false, false, true, false, false]);
}

#[test]
fn subscribe_filter_actor_axis_keeps_only_that_actor() {
    let by_name = SubscribeFilter {
        actor: Some("rust-engineer".into()),
        ..SubscribeFilter::default()
    };
    let hits: Vec<bool> = corpus().iter().map(|ev| by_name.matches(ev)).collect();
    assert_eq!(hits, [false, false, false, false, true, false, false]);

    let by_agent_id = SubscribeFilter {
        actor: Some("rust-engineer-id".into()),
        ..SubscribeFilter::default()
    };
    let hits: Vec<bool> = corpus().iter().map(|ev| by_agent_id.matches(ev)).collect();
    assert_eq!(hits, [false, false, false, false, true, false, false]);

    let operator = SubscribeFilter {
        actor: Some("operator".into()),
        ..SubscribeFilter::default()
    };
    let hits: Vec<bool> = corpus().iter().map(|ev| operator.matches(ev)).collect();
    assert_eq!(hits, [false, false, false, false, false, true, false]);
}

// ---- error arms: malformed frames are errors, never defaults ----

/// A filter field the server does not know would otherwise be dropped, and
/// the subscription would silently widen to events the caller excluded.
#[test]
fn subscribe_filter_rejects_an_unknown_field() {
    let err = serde_json::from_value::<SubscribeFilter>(json!({ "sorce": "mpm" }))
        .expect_err("a misspelt filter axis must be an error");
    assert!(err.to_string().contains("sorce"), "{err}");

    let err = serde_json::from_value::<SubscribeParams>(json!({ "filters": { "agent": "x" } }))
        .expect_err("an unknown field nested in params must be an error");
    assert!(err.to_string().contains("agent"), "{err}");
}

#[test]
fn subscribe_params_rejects_an_unknown_field() {
    let err = serde_json::from_value::<SubscribeParams>(json!({ "since": 4 }))
        .expect_err("an unknown params field must be an error");
    assert!(err.to_string().contains("since"), "{err}");
}

#[test]
fn subscribe_params_rejects_a_mistyped_field() {
    serde_json::from_value::<SubscribeParams>(json!({ "since_seq": "4" }))
        .expect_err("a string since_seq must be an error");
    serde_json::from_value::<SubscribeParams>(json!({ "since_seq": -1 }))
        .expect_err("a negative since_seq must be an error");
    serde_json::from_value::<SubscribeFilter>(json!({ "source": "nobody" }))
        .expect_err("an unknown harness source must be an error");
}

#[test]
fn subscribe_item_rejects_a_malformed_item() {
    serde_json::from_value::<SubscribeItem>(json!({ "type": "flood" }))
        .expect_err("an unknown item type must be an error");
    serde_json::from_value::<SubscribeItem>(json!({ "type": "event", "persisted": true }))
        .expect_err("an event item without its event must be an error");
    serde_json::from_value::<SubscribeItem>(
        json!({ "type": "event", "event": golden_event_json() }),
    )
    .expect_err("an event item without `persisted` must be an error");
    serde_json::from_value::<SubscribeItem>(json!({ "type": "gap" }))
        .expect_err("a gap without its count must be an error");
}

// ---- the request frame (UDS-gated: built on uds::server's envelope) ----

#[cfg(all(unix, feature = "uds"))]
mod frame {
    use super::*;

    fn golden_request() -> Value {
        json!({
            "jsonrpc": "2.0",
            "id": 7,
            "method": "events.subscribe",
            "stream": true,
            "params": { "since_seq": 41, "filters": { "session": "s-1" } },
        })
    }

    fn decode(frame: &Value) -> Result<SubscribeRequest, SubscribeFrameError> {
        SubscribeRequest::decode(frame.to_string().as_bytes())
    }

    #[test]
    fn subscribe_request_golden_roundtrip() {
        let params = SubscribeParams {
            since_seq: Some(41),
            since_id: None,
            filters: SubscribeFilter {
                session: Some("s-1".into()),
                ..SubscribeFilter::default()
            },
        };
        let request = SubscribeRequest::new(7, params.clone());
        assert_eq!(METHOD_EVENTS_SUBSCRIBE, "events.subscribe");
        assert_serializes_to(&request, &golden_request());

        let decoded = decode(&golden_request()).expect("decode golden request");
        assert_eq!(decoded, request);
        assert_eq!(decoded.params, params);
        assert_eq!(decoded.id, json!(7));
    }

    #[test]
    fn decode_rejects_a_frame_that_is_not_json() {
        let err = SubscribeRequest::decode(b"{\"jsonrpc\":\"2.0\",").expect_err("truncated");
        assert!(matches!(err, SubscribeFrameError::Malformed(_)), "{err:?}");

        let err = decode(&json!({ "jsonrpc": "2.0", "id": 1 })).expect_err("no method");
        assert!(matches!(err, SubscribeFrameError::Malformed(_)), "{err:?}");
    }

    #[test]
    fn decode_rejects_a_wrong_jsonrpc_version() {
        let mut frame = golden_request();
        frame["jsonrpc"] = json!("1.0");
        let err = decode(&frame).expect_err("jsonrpc 1.0");
        assert!(
            matches!(&err, SubscribeFrameError::UnsupportedVersion { found } if found == "1.0"),
            "{err:?}"
        );
    }

    #[test]
    fn decode_rejects_another_method() {
        let mut frame = golden_request();
        frame["method"] = json!("events.publish");
        let err = decode(&frame).expect_err("wrong method");
        assert!(
            matches!(&err, SubscribeFrameError::WrongMethod { found } if found == "events.publish"),
            "{err:?}"
        );
    }

    /// A unary `events.subscribe` would make the server answer in one frame
    /// and close — a subscription that silently ends at once.
    #[test]
    fn decode_rejects_a_unary_subscribe() {
        let mut frame = golden_request();
        frame
            .as_object_mut()
            .expect("golden is an object")
            .remove("stream");
        let err = decode(&frame).expect_err("no stream flag");
        assert!(matches!(err, SubscribeFrameError::NotStreaming), "{err:?}");
    }

    #[test]
    fn decode_rejects_an_unknown_filter_field() {
        let mut frame = golden_request();
        frame["params"]["filters"]["sorce"] = json!("mpm");
        let err = decode(&frame).expect_err("unknown filter field");
        assert!(
            matches!(err, SubscribeFrameError::InvalidParams(_)),
            "{err:?}"
        );
        assert!(err.to_string().contains("sorce"), "{err}");
    }

    /// Absent `params` is not "subscribe to everything"; the caller sends `{}`
    /// for that, exactly as `RpcRouter::typed_stream` requires.
    #[test]
    fn decode_rejects_missing_params() {
        let mut frame = golden_request();
        frame
            .as_object_mut()
            .expect("golden is an object")
            .remove("params");
        let err = decode(&frame).expect_err("no params");
        assert!(
            matches!(err, SubscribeFrameError::InvalidParams(_)),
            "{err:?}"
        );
    }
}
