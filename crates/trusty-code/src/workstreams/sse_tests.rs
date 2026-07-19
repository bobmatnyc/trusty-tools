//! Tests for `sse::aggregate_live`/`sse::routes` (DOC-48 §5.3, §5.3.1; issue
//! #3297).

use std::sync::Arc;
use std::time::Duration;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use serde_json::Value;
use tokio::sync::Mutex;
use tower::util::ServiceExt;

use super::*;
use crate::events::{Event, SessionEventEnvelope};
use crate::workstreams::WorkstreamStore;

/// Build a `SharedWorkstreamStore` seeded with one workstream directly on
/// disk (bypassing the store's public API, which has no session-binding
/// method yet — session binding is issue #3298's scope, not this ticket's;
/// see `workstreams::model::Workstream::session_ids`'s field docs).
async fn seeded_store(
    session_ids: Vec<&str>,
) -> (SharedWorkstreamStore, WorkstreamId, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("workstreams-test.json");
    let id = WorkstreamId::new();
    let now = chrono::Utc::now();
    let raw = serde_json::json!({
        "version": "1.0",
        "active_workstream_id": Value::Null,
        "workstreams": [{
            "id": id,
            "name": "t",
            "session_ids": session_ids,
            "created_at": now,
            "updated_at": now,
            "metadata": {},
        }],
    });
    tokio::fs::write(&path, serde_json::to_string_pretty(&raw).unwrap())
        .await
        .expect("seed raw store file");
    let store = WorkstreamStore::load(&path)
        .await
        .expect("load seeded store");
    (Arc::new(Mutex::new(store)), id, dir)
}

fn session_started(session_id: &str) -> SessionEventEnvelope {
    SessionEventEnvelope::new(
        session_id.to_string(),
        1,
        chrono::Utc::now(),
        Event::SessionStarted {
            session_id: session_id.to_string(),
            project: "p".to_string(),
        },
    )
}

/// The fan-out must forward events from BOTH bound sessions, tagged with
/// their own `session_id`/`event_type`, and must NOT forward an event from a
/// third, unbound session.
#[tokio::test]
async fn fan_out_tags_events_from_bound_sessions_only() {
    let (store, id, _dir) = seeded_store(vec!["s1", "s2"]).await;
    let mut stream = std::pin::pin!(aggregate_live(id, store));

    crate::events::publish(session_started("s1"));
    crate::events::publish(session_started("s2"));
    crate::events::publish(session_started("s3")); // not bound — must be filtered out

    let first = tokio::time::timeout(Duration::from_secs(2), stream.next())
        .await
        .expect("timed out")
        .expect("stream ended early");
    assert_eq!(first.session_id, "s1");
    assert_eq!(first.event_type, "session_started");

    let second = tokio::time::timeout(Duration::from_secs(2), stream.next())
        .await
        .expect("timed out")
        .expect("stream ended early");
    assert_eq!(second.session_id, "s2");

    // s3's event must never arrive — confirmed by a bounded wait timing out.
    let third = tokio::time::timeout(Duration::from_millis(200), stream.next()).await;
    assert!(
        third.is_err(),
        "an event from an unbound session must not be forwarded"
    );
}

/// A `WorkstreamActivationChanged` naming this workstream (as either the new
/// active id or the prior one) must be forwarded even though it carries no
/// bound session id at all.
#[tokio::test]
async fn activation_changed_event_is_forwarded_regardless_of_session_binding() {
    let (store, id, _dir) = seeded_store(vec![]).await;
    let mut stream = std::pin::pin!(aggregate_live(id, store));

    let event = Event::WorkstreamActivationChanged {
        new_active_id: Some(id.to_string()),
        prior_id: None,
    };
    crate::events::publish(SessionEventEnvelope::new(
        String::new(),
        0,
        chrono::Utc::now(),
        event,
    ));

    let forwarded = tokio::time::timeout(Duration::from_secs(2), stream.next())
        .await
        .expect("timed out waiting for the activation-changed event")
        .expect("stream ended early");
    assert_eq!(forwarded.event_type, "workstream_activation_changed");
    match forwarded.payload {
        Event::WorkstreamActivationChanged { new_active_id, .. } => {
            assert_eq!(new_active_id, Some(id.to_string()));
        }
        other => panic!("expected WorkstreamActivationChanged, got {other:?}"),
    }
}

/// A `WorkstreamStateInferred` naming THIS workstream must be forwarded even
/// with zero bound sessions; one naming a DIFFERENT workstream must not.
#[tokio::test]
async fn state_inferred_event_is_forwarded_for_this_workstream_only() {
    let (store, id, _dir) = seeded_store(vec![]).await;
    let mut stream = std::pin::pin!(aggregate_live(id, store));

    let other = WorkstreamId::new();
    crate::events::publish(SessionEventEnvelope::new(
        String::new(),
        0,
        chrono::Utc::now(),
        Event::WorkstreamStateInferred {
            workstream_id: other.to_string(),
            state: "closed".to_string(),
            reason: "closed".to_string(),
        },
    ));
    crate::events::publish(SessionEventEnvelope::new(
        String::new(),
        0,
        chrono::Utc::now(),
        Event::WorkstreamStateInferred {
            workstream_id: id.to_string(),
            state: "idle".to_string(),
            reason: "deactivated".to_string(),
        },
    ));

    let forwarded = tokio::time::timeout(Duration::from_secs(2), stream.next())
        .await
        .expect("timed out waiting for the state-inferred event")
        .expect("stream ended early");
    match forwarded.payload {
        Event::WorkstreamStateInferred {
            workstream_id,
            state,
            ..
        } => {
            assert_eq!(workstream_id, id.to_string());
            assert_eq!(state, "idle");
        }
        other => panic!("expected WorkstreamStateInferred, got {other:?}"),
    }
}

/// A zero-session workstream's stream must stay open (never close, never
/// error) with no events flowing — it is not an empty/closed stream, just a
/// quiet one until a session is bound or its activation state changes.
#[tokio::test]
async fn empty_workstream_stream_yields_nothing() {
    let (store, id, _dir) = seeded_store(vec![]).await;
    let mut stream = std::pin::pin!(aggregate_live(id, store));

    crate::events::publish(session_started("unrelated"));

    let outcome = tokio::time::timeout(Duration::from_millis(200), stream.next()).await;
    assert!(
        outcome.is_err(),
        "an empty workstream must not forward any session's events"
    );
}

async fn app_with_store(session_ids: Vec<&str>) -> (axum::Router, WorkstreamId, tempfile::TempDir) {
    let (store, id, dir) = seeded_store(session_ids).await;
    (routes(store), id, dir)
}

/// `GET /workstreams/{unknown-id}/events` must return a real HTTP `404` with
/// a `-32002 not_found` envelope.
#[tokio::test]
async fn unknown_id_returns_404() {
    let (app, _id, _dir) = app_with_store(vec![]).await;

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/workstreams/{}/events", WorkstreamId::new()))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let bytes = to_bytes(resp.into_body(), 1024 * 1024).await.unwrap();
    let v: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(v["error"]["code"], -32002);
}

/// A malformed (non-UUID) workstream id must return `400`, not `404` or a
/// panic.
#[tokio::test]
async fn malformed_id_returns_400() {
    let (app, _id, _dir) = app_with_store(vec![]).await;

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/workstreams/not-a-uuid/events")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

/// `GET /workstreams/{id}/events` on a workstream with bound sessions must
/// stream a live event tagged for one of them as an SSE `data:` frame.
#[tokio::test]
async fn route_streams_live_tagged_event_for_bound_session() {
    let (app, id, _dir) = app_with_store(vec!["s1"]).await;

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/workstreams/{id}/events"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // Safe to publish only now: `aggregate_live` calls `crate::events::
    // subscribe()` synchronously while building the response (before
    // `.oneshot()`'s future resolves), so the subscription is already live
    // by the time this `.await` above returns — no race to guard against.
    crate::events::publish(session_started("s1"));

    let mut stream = resp.into_body().into_data_stream();
    let mut collected = Vec::new();
    let read = tokio::time::timeout(Duration::from_secs(5), async {
        while !String::from_utf8_lossy(&collected).contains("session_started") {
            match stream.next().await {
                Some(Ok(chunk)) => collected.extend_from_slice(&chunk),
                _ => break,
            }
        }
    })
    .await;

    assert!(read.is_ok(), "timed out waiting for the tagged SSE frame");
    let text = String::from_utf8(collected).unwrap();
    assert!(
        text.contains("\"session_id\":\"s1\""),
        "body so far: {text}"
    );
    assert!(
        text.contains("\"event_type\":\"session_started\""),
        "body so far: {text}"
    );
}
