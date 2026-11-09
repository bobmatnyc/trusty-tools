//! Tests for the per-source and per-project counters (#10906).

use serde_json::{Value, json};
use trusty_common::console_metrics::check_by_convention;
use trusty_common::control_bus::{
    EventId, HarnessEvent, HarnessPayload, HarnessSource, MessageChannel, MessageDirection,
    MessageEvent, OVERFLOW_PROJECT, UNATTRIBUTED_PROJECT,
};

use super::breakdown::Breakdown;

fn event(source: HarnessSource, payload: HarnessPayload, project: Option<&str>) -> HarnessEvent {
    HarnessEvent {
        source,
        session: None,
        seq: 0,
        at: chrono::Utc::now(),
        payload,
        id: EventId::new(),
        parent_id: None,
        project: project.map(str::to_string),
    }
}

fn message(direction: MessageDirection) -> HarnessPayload {
    HarnessPayload::Message(MessageEvent {
        at: chrono::Utc::now(),
        channel: MessageChannel::SendMessage,
        direction,
        from: "a".into(),
        to: "b".into(),
        relay: None,
        bytes: 0,
        sha256: String::new(),
        body: None,
    })
}

fn ping(project: Option<&str>) -> HarnessEvent {
    event(HarnessSource::Code, HarnessPayload::Ping, project)
}

/// The `by_project` keys a breakdown currently holds.
fn projects(metrics: &Value) -> Vec<String> {
    metrics["by_project"]
        .as_object()
        .expect("by_project is an object")
        .keys()
        .cloned()
        .collect()
}

#[test]
fn events_count_by_source_domain_and_project() {
    let mut b = Breakdown::default();
    let p = Some("acme/web");
    b.record(&event(
        HarnessSource::Mpm,
        message(MessageDirection::Sent),
        p,
    ));
    b.record(&event(
        HarnessSource::Mpm,
        message(MessageDirection::Received),
        p,
    ));
    b.record(&event(
        HarnessSource::Mpm,
        message(MessageDirection::Sent),
        None,
    ));
    b.record(&ping(p));

    let metrics = b.to_json(7);
    let web = json!({
        "events": 3,
        "by_source": {
            "code": {"events": 1, "by_domain": {"ping": 1}},
            "mpm": {"events": 2, "by_domain": {"message": 2}},
        },
        "messages": {"sent": 1, "received": 1},
    });
    let unattributed = json!({
        "events": 1,
        "by_source": {"mpm": {"events": 1, "by_domain": {"message": 1}}},
        "messages": {"sent": 1, "received": 0},
    });
    assert_eq!(
        metrics,
        json!({
            "uptime_secs": 7,
            "events": 4,
            "by_source": {
                "code": {"events": 1, "by_domain": {"ping": 1}},
                "mpm": {"events": 3, "by_domain": {"message": 3}},
            },
            "messages": {"sent": 2, "received": 1},
            "by_project": {"acme/web": web, "unattributed": unattributed},
        })
    );
    assert_eq!(check_by_convention(&metrics), Ok(()));
}

#[test]
fn an_empty_bus_reports_every_key() {
    let metrics = Breakdown::default().to_json(0);
    assert_eq!(
        metrics,
        json!({
            "uptime_secs": 0,
            "events": 0,
            "by_source": {},
            "messages": {"sent": 0, "received": 0},
            "by_project": {},
        })
    );
    assert_eq!(check_by_convention(&metrics), Ok(()));
}

#[test]
fn a_missing_or_blank_project_counts_as_unattributed() {
    let mut b = Breakdown::default();
    for project in [None, Some(""), Some("  ")] {
        b.record(&ping(project));
    }
    let metrics = b.to_json(0);
    assert_eq!(projects(&metrics), [UNATTRIBUTED_PROJECT]);
    assert_eq!(metrics["by_project"][UNATTRIBUTED_PROJECT]["events"], 3);
}

#[test]
fn projects_past_the_cap_count_as_overflow() {
    let mut b = Breakdown::with_max_projects(2);
    for project in ["a", "b", "c", "d", "a"] {
        b.record(&ping(Some(project)));
    }
    let metrics = b.to_json(0);
    assert_eq!(projects(&metrics), ["a", "b", OVERFLOW_PROJECT]);
    assert_eq!(
        metrics["by_project"]["a"]["events"], 2,
        "a known one still counts"
    );
    assert_eq!(metrics["by_project"][OVERFLOW_PROJECT]["events"], 2);
    assert_eq!(metrics["events"], 5, "the total counts every event");
}
