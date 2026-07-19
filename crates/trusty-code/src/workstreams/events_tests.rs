//! Unit tests for `workstreams::events` (issue #3297).
//!
//! Why: split out per the crate's `_tests.rs` sibling-file convention (see
//! `events_tests` at crate root for the session-event-taxonomy precedent) so
//! the aggregation layer's test surface can grow without pushing the
//! production file past its 500-SLOC cap.
//! What: covers the envelope constructors, [`super::aggregate`] against a
//! fake [`super::SessionEventSource`] (proving the merge logic never touches
//! `SessionRegistry`), [`super::RegistrySessionEventSource`] against a real
//! `SessionRegistry`, and the workstream-level bus round trip. The bus tests
//! loop-until-match (bounded by a timeout) rather than trusting the first
//! `recv()`, since `WORKSTREAM_BUS` is process-global and other tests in this
//! binary may publish concurrently — mirrors `crate::events_tests`'
//! established tolerance for that same global-bus sharing.
//! Test: this module is itself the test surface.

use std::collections::HashMap;
use std::time::Duration;

use futures_util::StreamExt;
use serde_json::json;

use super::*;
use crate::session::SessionRegistry;

/// A fake [`SessionEventSource`] backed by an in-memory map — proves
/// [`aggregate`] depends only on the trait, never on `SessionRegistry`.
struct FakeSource {
    data: HashMap<String, Vec<WorkstreamEventEnvelope>>,
}

impl SessionEventSource for FakeSource {
    fn subscribe(&self, session_id: &str) -> Option<EventStream> {
        let events = self.data.get(session_id)?.clone();
        Some(Box::pin(stream::iter(events)))
    }
}

#[test]
fn session_envelope_carries_session_id() {
    let env = WorkstreamEventEnvelope::for_session("s-1", "tool_started", json!({"a": 1}));
    assert_eq!(env.session_id, Some("s-1".to_string()));
    assert_eq!(env.event_type, "tool_started");
    assert_eq!(env.payload, json!({"a": 1}));
}

#[test]
fn workstream_level_envelope_has_no_session_id() {
    let env = WorkstreamEventEnvelope::workstream_level("workstream_activation_changed", json!({}));
    assert_eq!(env.session_id, None);
    assert_eq!(env.event_type, "workstream_activation_changed");
}

#[tokio::test]
async fn aggregate_merges_per_session_and_workstream_streams() {
    let mut data = HashMap::new();
    data.insert(
        "s-1".to_string(),
        vec![WorkstreamEventEnvelope::for_session(
            "s-1",
            "tool_started",
            json!({"tool": "grep"}),
        )],
    );
    let source = FakeSource { data };
    let session_ids = vec!["s-1".to_string()];
    let workstream_events: EventStream = Box::pin(stream::iter(vec![
        WorkstreamEventEnvelope::workstream_level("workstream_state_inferred", json!({})),
    ]));

    let merged: Vec<_> = aggregate(&source, &session_ids, workstream_events)
        .collect()
        .await;

    assert_eq!(merged.len(), 2);
    assert!(merged.iter().any(|e| e.event_type == "tool_started"));
    assert!(
        merged
            .iter()
            .any(|e| e.event_type == "workstream_state_inferred")
    );
}

#[tokio::test]
async fn aggregate_with_no_sessions_still_streams_workstream_events() {
    let source = FakeSource {
        data: HashMap::new(),
    };
    let workstream_events: EventStream = Box::pin(stream::iter(vec![
        WorkstreamEventEnvelope::workstream_level("workstream_activation_changed", json!({})),
    ]));

    let merged: Vec<_> = aggregate(&source, &[], workstream_events).collect().await;

    assert_eq!(merged.len(), 1);
    assert_eq!(merged[0].event_type, "workstream_activation_changed");
    assert_eq!(merged[0].session_id, None);
}

#[tokio::test]
async fn aggregate_skips_unknown_session_ids() {
    let source = FakeSource {
        data: HashMap::new(),
    };
    let session_ids = vec!["ghost".to_string()];
    let workstream_events: EventStream = Box::pin(stream::iter(vec![
        WorkstreamEventEnvelope::workstream_level("workstream_state_inferred", json!({})),
    ]));

    // Must not panic on the unknown id; the workstream-level stream alone
    // still comes through.
    let merged: Vec<_> = aggregate(&source, &session_ids, workstream_events)
        .collect()
        .await;
    assert_eq!(merged.len(), 1);
}

#[tokio::test]
async fn registry_source_replays_then_streams_live_events() {
    let sessions = std::sync::Arc::new(SessionRegistry::new());
    let session = sessions.create("t".to_string(), None, crate::binding::ProjectBinding::None);
    let source = RegistrySessionEventSource::new(sessions);

    let stream = source
        .subscribe(&session.id)
        .expect("known session must yield a stream");
    // `session.create` publishes exactly `SessionStarted` +
    // `SessionStatusChanged` into the ring buffer (mirrors
    // `crate::serve::http::tests::http_session_events_sse_streams_replay`) —
    // `take(2)` is satisfied entirely by the replay half, so this never
    // touches the live (global-bus) half and cannot race other tests.
    let replayed: Vec<_> = stream.take(2).collect().await;
    assert_eq!(replayed.len(), 2);
    assert_eq!(replayed[0].session_id.as_deref(), Some(session.id.as_str()));
    assert!(replayed.iter().any(|e| e.event_type == "session_started"));
    assert!(
        replayed
            .iter()
            .any(|e| e.event_type == "session_status_changed")
    );
}

#[tokio::test]
async fn registry_source_unknown_session_returns_none() {
    let sessions = std::sync::Arc::new(SessionRegistry::new());
    let source = RegistrySessionEventSource::new(sessions);
    assert!(source.subscribe("does-not-exist").is_none());
}

#[tokio::test]
async fn workstream_bus_round_trips_through_subscribe() {
    let mut stream = subscribe_workstream_bus();
    let marker = WorkstreamId::new();
    publish_activation_changed(marker, None);

    let found = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let env = stream.next().await.expect("bus must not close");
            if env.payload["new_active_id"] == json!(marker) {
                return env;
            }
        }
    })
    .await
    .expect("expected our publish to arrive within the timeout");

    assert_eq!(found.event_type, "workstream_activation_changed");
    assert_eq!(found.session_id, None);
}

#[tokio::test]
async fn publish_activation_changed_reaches_subscriber() {
    let mut stream = subscribe_workstream_bus();
    let new_id = WorkstreamId::new();
    let prior_id = WorkstreamId::new();
    publish_activation_changed(new_id, Some(prior_id));

    let found = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let env = stream.next().await.expect("bus must not close");
            if env.payload["new_active_id"] == json!(new_id) {
                return env;
            }
        }
    })
    .await
    .expect("expected our publish to arrive within the timeout");

    assert_eq!(found.payload["prior_id"], json!(prior_id));
}

#[tokio::test]
async fn publish_state_inferred_reaches_subscriber() {
    let mut stream = subscribe_workstream_bus();
    let id = WorkstreamId::new();
    let marker_reason = format!("test-reason-{id}");
    publish_state_inferred(id, WorkstreamState::Idle, &marker_reason);

    let found = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let env = stream.next().await.expect("bus must not close");
            if env.payload["reason"] == json!(marker_reason) {
                return env;
            }
        }
    })
    .await
    .expect("expected our publish to arrive within the timeout");

    assert_eq!(found.event_type, "workstream_state_inferred");
    assert_eq!(found.payload["state"], json!("idle"));
    assert_eq!(found.payload["workstream_id"], json!(id));
}
