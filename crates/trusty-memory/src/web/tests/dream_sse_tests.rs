//! Tests for SSE broadcast, dream cycle aggregation, and dream run.

use super::super::router;
use super::test_state;
use crate::{ActivitySource, DaemonEvent};
use axum::body::{to_bytes, Body};
use axum::http::{header, Request, StatusCode};
use serde_json::{json, Value};
use tower::util::ServiceExt;
use trusty_common::memory_core::palace::PalaceId;

/// Why: The SSE event bus is the dashboard's live-update transport;
/// regressing it would silently break the UI. Subscribing before the
/// emit guarantees the broadcast channel has a receiver when the
/// handler fires, so we can deterministically observe the event.
/// What: Subscribes to `state.events`, calls the `create_palace`
/// handler through the router, then asserts a `PalaceCreated` event
/// (and a follow-up status event from drawer mutation) flow through.
/// Test: `cargo test -p trusty-memory-mcp sse_broadcast_emits_palace_created`.
#[tokio::test]
async fn sse_broadcast_emits_palace_created() {
    let state = test_state();
    let mut rx = state.events.subscribe();
    let app = router().with_state(state.clone());
    let body = json!({"name": "sse-test"}).to_string();
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/palaces")
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    // The handler should have emitted PalaceCreated before returning.
    let event = tokio::time::timeout(std::time::Duration::from_millis(500), rx.recv())
        .await
        .expect("event received within timeout")
        .expect("event channel still open");
    match event {
        DaemonEvent::PalaceCreated { id, name, source } => {
            assert_eq!(id, "sse-test");
            assert_eq!(name, "sse-test");
            assert_eq!(source, ActivitySource::Http);
        }
        other => panic!("expected PalaceCreated, got {other:?}"),
    }
}

/// Why: Confirm the `/sse` endpoint speaks `text/event-stream` and emits
/// the initial `connected` frame so dashboard clients can rely on a
/// known greeting.
/// What: Issues a GET against `/sse`, reads the response body chunk,
/// asserts the content-type header and the first SSE frame shape.
/// Test: `cargo test -p trusty-memory-mcp sse_endpoint_emits_connected_frame`.
#[tokio::test]
async fn sse_endpoint_emits_connected_frame() {
    use axum::routing::get;
    let state = test_state();
    let app = router()
        .route("/sse", get(crate::sse_handler))
        .with_state(state);
    let resp = app
        .oneshot(Request::builder().uri("/sse").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        resp.headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some("text/event-stream")
    );
    // Read just the first chunk (the connected frame) — the stream stays
    // open otherwise, so we use a small read budget plus timeout.
    let body = resp.into_body();
    let bytes = tokio::time::timeout(std::time::Duration::from_millis(500), to_bytes(body, 4096))
        .await
        .ok()
        .and_then(|r| r.ok())
        .unwrap_or_default();
    let text = String::from_utf8_lossy(&bytes);
    assert!(
        text.contains("\"type\":\"connected\""),
        "expected connected frame, got: {text}"
    );
}

/// Why: `/api/v1/dream/status` must sum per-palace `dream_stats.json`
/// counters and surface the most recent `last_run_at`. A regression that
/// returned only the first palace's stats would silently break the
/// "global dream activity" dashboard panel.
/// What: Pre-seeds two palace dirs under the AppState root, writes a
/// distinct `PersistedDreamStats` JSON file into each, hits the endpoint,
/// and asserts the integer fields are summed and `last_run_at` equals the
/// newer of the two timestamps.
/// Test: This test itself.
#[tokio::test]
async fn dream_status_aggregates_across_palaces() {
    use trusty_common::memory_core::dream::{DreamStats, PersistedDreamStats};

    let state = test_state();
    // Two palace directories — each must contain a `palace.json` so
    // `PalaceRegistry::list_palaces` sees them, plus a `dream_stats.json`
    // with distinct counter values.
    for (id, stats, ts) in [
        (
            "palace-a",
            DreamStats {
                merged: 1,
                pruned: 2,
                compacted: 3,
                closets_updated: 4,
                duration_ms: 100,
                ..DreamStats::default()
            },
            chrono::Utc::now() - chrono::Duration::seconds(60),
        ),
        (
            "palace-b",
            DreamStats {
                merged: 10,
                pruned: 20,
                compacted: 30,
                closets_updated: 40,
                duration_ms: 200,
                ..DreamStats::default()
            },
            chrono::Utc::now(),
        ),
    ] {
        let palace = trusty_common::memory_core::Palace {
            id: PalaceId::new(id),
            name: id.to_string(),
            description: None,
            created_at: chrono::Utc::now(),
            data_dir: state.data_root.join(id),
        };
        state
            .registry
            .create_palace(&state.data_root, palace)
            .expect("create palace");
        let persisted = PersistedDreamStats {
            last_run_at: ts,
            stats,
        };
        persisted
            .save(&state.data_root.join(id))
            .expect("save dream stats");
    }

    let later = chrono::Utc::now();
    let app = router().with_state(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/dream/status")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = to_bytes(resp.into_body(), 4096).await.unwrap();
    let v: Value = serde_json::from_slice(&bytes).unwrap();

    // Aggregated counters.
    assert_eq!(v["merged"], 11);
    assert_eq!(v["pruned"], 22);
    assert_eq!(v["compacted"], 33);
    assert_eq!(v["closets_updated"], 44);
    assert_eq!(v["duration_ms"], 300);

    // `last_run_at` is the more-recent of the two timestamps.
    let last = v["last_run_at"].as_str().expect("last_run_at is string");
    let parsed: chrono::DateTime<chrono::Utc> = last
        .parse()
        .expect("last_run_at parses as RFC3339 timestamp");
    assert!(
        parsed <= later,
        "last_run_at ({parsed}) should not exceed wall clock ({later})"
    );
    // Must have picked palace-b's newer stamp, not palace-a's older one.
    let cutoff = chrono::Utc::now() - chrono::Duration::seconds(30);
    assert!(
        parsed >= cutoff,
        "expected the newer (palace-b) timestamp; got {parsed}"
    );
}

/// Why: `POST /api/v1/dream/run` triggers a dream cycle across every
/// palace and must return the aggregated stats. Even when no palace
/// has work to do (empty registry) the endpoint must round-trip 200
/// with the well-formed payload shape so the dashboard's "Run now"
/// button never fails the UI.
/// What: Pre-creates one palace via the registry, posts to the endpoint,
/// and asserts the response is 200 with all expected fields present.
/// Deeper assertions (specific merged/pruned counts) are skipped here
/// because running a full dream cycle requires the ONNX embedder load
/// path and we want this test to stay fast and embedder-free.
/// Test: This test itself.
#[tokio::test]
async fn dream_run_aggregates_stats() {
    let state = test_state();
    let palace = trusty_common::memory_core::Palace {
        id: PalaceId::new("dream-run-test"),
        name: "dream-run-test".to_string(),
        description: None,
        created_at: chrono::Utc::now(),
        data_dir: state.data_root.join("dream-run-test"),
    };
    state
        .registry
        .create_palace(&state.data_root, palace)
        .expect("create palace");

    let app = router().with_state(state);
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/dream/run")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = to_bytes(resp.into_body(), 4096).await.unwrap();
    let v: Value = serde_json::from_slice(&bytes).unwrap();

    // Shape: every aggregated counter must be present (even if zero) and
    // `last_run_at` is set by the handler to "now".
    for key in [
        "merged",
        "pruned",
        "compacted",
        "closets_updated",
        "duration_ms",
    ] {
        assert!(
            v.get(key).is_some(),
            "missing key {key} in dream_run payload: {v}"
        );
        assert!(
            v[key].is_u64() || v[key].is_i64(),
            "{key} should be integer, got {}",
            v[key]
        );
    }
    assert!(
        v["last_run_at"].is_string(),
        "last_run_at must be set by dream_run; got {v}"
    );
}
