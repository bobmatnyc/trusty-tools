//! Unified harness event envelope, bus, and filter (Wave 3 Phase 0; ADR-0005).
//!
//! Why: The three harnesses (`trusty-agents`, `trusty-mpm`, `trusty-code`) each
//!      grew their own ad-hoc event streaming. Wave 3 unifies them on one
//!      `HarnessEvent` envelope flowing over one process-global broadcast bus,
//!      so a single subscriber (UI, relay, aggregator) can consume all three.
//!      Phase 0 lands the *foundation only* — the types, the bus, the
//!      subscription API, and a lightweight filter — with no consumers wired
//!      yet. The migration of existing emit sites happens in P1–P4.
//! What: A thin facade that re-exports the public surface from three focused
//!       submodules (respecting the 500-line cap): `lifecycle` (the
//!       `HarnessSource` + `LifecycleEvent` taxonomy), `bus` (the
//!       `HarnessEvent`/`HarnessPayload` envelope, the global bus, and the
//!       lagged-receiver helper), and `filter` (the subscriber-side `Filter`).
//! Test: `tests` (below) is the comprehensive suite for this foundation type;
//!       submodules document which test exercises each item.

mod bus;
mod filter;
mod lifecycle;

pub use bus::{
    CHANNEL_CAPACITY, EVENT_LINE_PREFIX, HarnessEvent, HarnessPayload, Lag, bus, emit,
    format_event_line, publish, recv_with_lag, subscribe,
};
pub use filter::Filter;
pub use lifecycle::{HarnessSource, LifecycleEvent};

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tokio::sync::broadcast;

    fn sample_lifecycle() -> LifecycleEvent {
        LifecycleEvent::PmThinking {
            session_id: "s1".into(),
            text: "considering options".into(),
        }
    }

    fn envelope(payload: HarnessPayload, session: Option<&str>) -> HarnessEvent {
        HarnessEvent {
            source: HarnessSource::Agents,
            session: session.map(str::to_string),
            seq: 0,
            at: chrono::Utc::now(),
            payload,
        }
    }

    // ---- HarnessSource ----

    #[test]
    fn harness_source_round_trips() {
        for (src, tag) in [
            (HarnessSource::Agents, "\"agents\""),
            (HarnessSource::Mpm, "\"mpm\""),
            (HarnessSource::Code, "\"code\""),
        ] {
            let s = serde_json::to_string(&src).expect("serialize source");
            assert_eq!(s, tag);
            let back: HarnessSource = serde_json::from_str(&s).expect("deserialize source");
            assert_eq!(back, src);
        }
    }

    // ---- LifecycleEvent ----

    #[test]
    fn lifecycle_event_serializes_with_type_tag() {
        let s = serde_json::to_string(&sample_lifecycle()).expect("serialize");
        assert!(s.contains("\"type\":\"pm_thinking\""), "{s}");
        assert!(s.contains("\"session_id\":\"s1\""), "{s}");
    }

    #[test]
    fn lifecycle_session_id_returns_correct_field() {
        let ev = LifecycleEvent::AgentMessage {
            session_id: "abc".into(),
            agent: "python".into(),
            text: "hi".into(),
        };
        assert_eq!(ev.session_id(), Some("abc"));
    }

    #[test]
    fn lifecycle_recap_round_trips() {
        let ev = LifecycleEvent::RecapGenerated {
            session_id: "s9".into(),
            summary: "did a thing".into(),
            table_rows: vec![("step".into(), "ok".into())],
        };
        let s = serde_json::to_string(&ev).expect("serialize recap");
        let back: LifecycleEvent = serde_json::from_str(&s).expect("deserialize recap");
        assert_eq!(back, ev);
    }

    // ---- HarnessPayload tag shapes ----

    #[test]
    fn payload_lifecycle_round_trips() {
        let p = HarnessPayload::Lifecycle(sample_lifecycle());
        let s = serde_json::to_string(&p).expect("serialize");
        assert!(s.contains("\"domain\":\"lifecycle\""), "{s}");
        assert!(s.contains("\"event\":{"), "{s}");
        assert!(s.contains("\"type\":\"pm_thinking\""), "{s}");
        let back: HarnessPayload = serde_json::from_str(&s).expect("deserialize");
        assert_eq!(back, p);
    }

    #[test]
    fn payload_hook_round_trips() {
        let p = HarnessPayload::Hook {
            kind: "pre_tool_use".into(),
            data: json!({"tool": "bash", "ok": true}),
        };
        let s = serde_json::to_string(&p).expect("serialize");
        assert!(s.contains("\"domain\":\"hook\""), "{s}");
        assert!(s.contains("\"kind\":\"pre_tool_use\""), "{s}");
        let back: HarnessPayload = serde_json::from_str(&s).expect("deserialize");
        assert_eq!(back, p);
    }

    #[test]
    fn payload_ping_round_trips() {
        let p = HarnessPayload::Ping;
        let s = serde_json::to_string(&p).expect("serialize");
        assert_eq!(s, "{\"domain\":\"ping\"}");
        let back: HarnessPayload = serde_json::from_str(&s).expect("deserialize");
        assert_eq!(back, p);
    }

    #[test]
    fn payload_domain_matches_serde_tag() {
        assert_eq!(
            HarnessPayload::Lifecycle(sample_lifecycle()).domain(),
            "lifecycle"
        );
        assert_eq!(
            HarnessPayload::Hook {
                kind: "x".into(),
                data: json!(null)
            }
            .domain(),
            "hook"
        );
        assert_eq!(HarnessPayload::Ping.domain(), "ping");
    }

    // ---- HarnessEvent envelope ----

    #[test]
    fn harness_event_round_trips() {
        let ev = envelope(HarnessPayload::Ping, Some("sess-1"));
        let s = serde_json::to_string(&ev).expect("serialize");
        assert!(s.contains("\"source\":\"agents\""), "{s}");
        assert!(s.contains("\"session\":\"sess-1\""), "{s}");
        let back: HarnessEvent = serde_json::from_str(&s).expect("deserialize");
        assert_eq!(back, ev);
    }

    #[test]
    fn harness_event_omits_none_session() {
        let ev = envelope(HarnessPayload::Ping, None);
        let s = serde_json::to_string(&ev).expect("serialize");
        assert!(!s.contains("session"), "session should be omitted: {s}");
    }

    // ---- bus + seq ----

    #[test]
    fn bus_is_singleton() {
        let a = bus();
        let b = bus();
        let mut rx = b.subscribe();
        let _ = a.send(envelope(HarnessPayload::Ping, None));
        let got = rx.try_recv().expect("expected event");
        assert!(matches!(got.payload, HarnessPayload::Ping));
    }

    #[tokio::test]
    async fn publish_round_trips_through_subscribe() {
        let mut rx = subscribe();
        let seq = publish(
            HarnessSource::Mpm,
            Some("t1".into()),
            HarnessPayload::Lifecycle(LifecycleEvent::SessionStarted {
                session_id: "t1".into(),
                project: "demo".into(),
            }),
        );
        let got = rx.recv().await.expect("recv");
        assert_eq!(got.seq, seq);
        assert_eq!(got.source, HarnessSource::Mpm);
        assert_eq!(got.session.as_deref(), Some("t1"));
        match got.payload {
            HarnessPayload::Lifecycle(LifecycleEvent::SessionStarted { project, .. }) => {
                assert_eq!(project, "demo");
            }
            other => panic!("unexpected payload: {other:?}"),
        }
    }

    #[test]
    fn seq_is_monotonic() {
        let s1 = publish(HarnessSource::Agents, None, HarnessPayload::Ping);
        let s2 = publish(HarnessSource::Agents, None, HarnessPayload::Ping);
        let s3 = publish(HarnessSource::Agents, None, HarnessPayload::Ping);
        assert!(s1 < s2 && s2 < s3, "seq not monotonic: {s1} {s2} {s3}");
    }

    // ---- emit line formatting ----

    #[test]
    fn event_line_prefix_is_stable() {
        assert_eq!(EVENT_LINE_PREFIX, "__HARNESS_EVENT__ ");
    }

    #[test]
    fn emit_line_has_prefix_and_parses() {
        let ev = envelope(HarnessPayload::Ping, Some("sess"));
        let line = format_event_line(&ev).expect("format line");
        assert!(line.starts_with(EVENT_LINE_PREFIX), "{line}");
        let json = line.strip_prefix(EVENT_LINE_PREFIX).expect("strip prefix");
        let back: HarnessEvent = serde_json::from_str(json).expect("parse relayed line");
        assert_eq!(back, ev);
    }

    // ---- lagged receiver ----

    #[tokio::test]
    async fn lagged_receiver_yields_lag_then_resumes() {
        // Constructible bus (not the global one) so we control capacity and can
        // deterministically overflow a slow receiver.
        let (tx, mut rx) = broadcast::channel::<HarnessEvent>(2);

        // Overflow the buffer: send 5 into a capacity-2 channel without
        // receiving. The oldest 3 are dropped; the receiver will report Lagged.
        for _ in 0..5 {
            let _ = tx.send(envelope(HarnessPayload::Ping, None));
        }

        // First recv reports the lag (skipped count) instead of an event.
        match recv_with_lag(&mut rx).await {
            Ok(Err(Lag { skipped })) => assert_eq!(skipped, 3),
            other => panic!("expected Lag, got {other:?}"),
        }

        // Stream resumes: the two still-buffered events are delivered.
        for _ in 0..2 {
            match recv_with_lag(&mut rx).await {
                Ok(Ok(ev)) => assert!(matches!(ev.payload, HarnessPayload::Ping)),
                other => panic!("expected resumed event, got {other:?}"),
            }
        }
    }

    #[tokio::test]
    async fn recv_with_lag_reports_closed() {
        let (tx, mut rx) = broadcast::channel::<HarnessEvent>(2);
        drop(tx);
        assert!(recv_with_lag(&mut rx).await.is_err());
    }

    // ---- Filter matrix ----

    #[test]
    fn filter_default_matches_all() {
        let f = Filter::default();
        assert!(f.matches(&envelope(HarnessPayload::Ping, None)));
        assert!(f.matches(&envelope(
            HarnessPayload::Lifecycle(sample_lifecycle()),
            Some("x")
        )));
    }

    #[test]
    fn filter_by_source() {
        let f = Filter {
            source: Some(HarnessSource::Mpm),
            ..Default::default()
        };
        let mut ev = envelope(HarnessPayload::Ping, None);
        ev.source = HarnessSource::Mpm;
        assert!(f.matches(&ev));
        ev.source = HarnessSource::Agents;
        assert!(!f.matches(&ev));
    }

    #[test]
    fn filter_by_session() {
        let f = Filter {
            session: Some("sess-7".into()),
            ..Default::default()
        };
        assert!(f.matches(&envelope(HarnessPayload::Ping, Some("sess-7"))));
        assert!(!f.matches(&envelope(HarnessPayload::Ping, Some("other"))));
        // An event with no session never matches a session constraint.
        assert!(!f.matches(&envelope(HarnessPayload::Ping, None)));
    }

    #[test]
    fn filter_by_domain() {
        let f = Filter {
            domains: Some(vec!["hook", "ping"]),
            ..Default::default()
        };
        assert!(f.matches(&envelope(HarnessPayload::Ping, None)));
        assert!(f.matches(&envelope(
            HarnessPayload::Hook {
                kind: "k".into(),
                data: json!({})
            },
            None
        )));
        assert!(!f.matches(&envelope(
            HarnessPayload::Lifecycle(sample_lifecycle()),
            Some("x")
        )));
    }

    #[test]
    fn filter_combination() {
        let f = Filter {
            source: Some(HarnessSource::Code),
            session: Some("s".into()),
            domains: Some(vec!["lifecycle"]),
        };
        let mut ev = envelope(HarnessPayload::Lifecycle(sample_lifecycle()), Some("s"));
        ev.source = HarnessSource::Code;
        assert!(f.matches(&ev));

        // Wrong source fails the conjunction even though session+domain match.
        ev.source = HarnessSource::Mpm;
        assert!(!f.matches(&ev));
    }
}
