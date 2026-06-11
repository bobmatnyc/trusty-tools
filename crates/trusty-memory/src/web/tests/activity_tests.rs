//! Tests for activity feed: list, clamp, filter, unknown-source, and static fallback.

use super::super::router;
use super::test_state;
use crate::{ActivitySource, DaemonEvent};
use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use serde_json::{json, Value};
use tower::util::ServiceExt;

#[tokio::test]
async fn serves_index_html_fallback() {
    let state = test_state();
    let app = router().with_state(state);
    let resp = app
        .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
        .await
        .unwrap();
    // Either OK with embedded HTML, or NOT_FOUND if assets not built.
    assert!(
        resp.status() == StatusCode::OK || resp.status() == StatusCode::NOT_FOUND,
        "got {}",
        resp.status()
    );
}

/// Why (issue #96): `GET /api/v1/activity` must return the entries
/// captured by the persistent log so the dashboard feed has history on
/// page load. This drives the endpoint with a sequence of emits that
/// model both HTTP- and MCP-origin writes, then asserts the response
/// shape, ordering, total count, and that the source labels make it
/// onto the wire.
/// What: emits four `DaemonEvent`s with mixed sources, fetches
/// `/api/v1/activity?limit=10`, and checks the structure of the
/// returned JSON.
/// Test: this test.
#[tokio::test]
async fn activity_endpoint_lists_recent_emits() {
    let state = test_state();
    // Three drawer_added (one MCP, two HTTP) and one palace_created.
    state.emit(DaemonEvent::PalaceCreated {
        id: "alpha".into(),
        name: "alpha".into(),
        source: ActivitySource::Http,
    });
    state.emit(DaemonEvent::DrawerAdded {
        palace_id: "alpha".into(),
        palace_name: "alpha".into(),
        drawer_count: 1,
        timestamp: chrono::Utc::now(),
        content_preview: "hello".into(),
        source: ActivitySource::Mcp,
    });
    state.emit(DaemonEvent::DrawerAdded {
        palace_id: "beta".into(),
        palace_name: "beta".into(),
        drawer_count: 1,
        timestamp: chrono::Utc::now(),
        content_preview: "hi there".into(),
        source: ActivitySource::Http,
    });
    state.emit(DaemonEvent::DrawerDeleted {
        palace_id: "alpha".into(),
        drawer_count: 0,
        source: ActivitySource::Http,
    });
    // Issue #232: emits now fire-and-forget the redb write on the
    // blocking pool; wait for the writes to settle before querying the
    // activity endpoint.
    state.flush_activity_writes().await;

    let app = router().with_state(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/activity?limit=10")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = to_bytes(resp.into_body(), 8192).await.unwrap();
    let v: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(v["limit"], 10);
    assert_eq!(v["offset"], 0);
    assert_eq!(v["total"], 4);
    let entries = v["entries"].as_array().expect("entries array");
    assert_eq!(entries.len(), 4);
    // Newest-first: drawer_deleted is the last event we pushed.
    assert_eq!(entries[0]["event_type"], "drawer_deleted");
    assert_eq!(entries[3]["event_type"], "palace_created");
    // Sources made it onto the wire as lowercase strings.
    let sources: Vec<&str> = entries
        .iter()
        .filter_map(|e| e["source"].as_str())
        .collect();
    assert!(sources.contains(&"http"));
    assert!(sources.contains(&"mcp"));
    // Payload is structured JSON, not an escaped string.
    assert!(entries[0]["payload"].is_object());
}

/// Why: the handler must enforce a sane upper bound on `limit` so a
/// curl with `?limit=1000000` cannot force a huge scan + response.
/// What: asks for `limit=10000`, asserts the response advertises the
/// clamped value.
/// Test: this test.
#[tokio::test]
async fn activity_endpoint_clamps_limit() {
    let state = test_state();
    let app = router().with_state(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/activity?limit=10000")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = to_bytes(resp.into_body(), 4096).await.unwrap();
    let v: Value = serde_json::from_slice(&bytes).unwrap();
    // ACTIVITY_MAX_LIMIT is 500 (see web::activity::ACTIVITY_MAX_LIMIT)
    assert_eq!(v["limit"], json!(500));
}

/// Why: filters are how the dashboard scopes the feed to a single
/// palace or to one origin (MCP vs HTTP). Confirm AND-semantics on
/// `?palace=` and `?source=`.
/// What: emits 3 events, queries with `?palace=alpha&source=mcp`, and
/// asserts only the matching row is returned.
/// Test: this test.
#[tokio::test]
async fn activity_endpoint_filters_by_source_and_palace() {
    let state = test_state();
    state.emit(DaemonEvent::DrawerAdded {
        palace_id: "alpha".into(),
        palace_name: "alpha".into(),
        drawer_count: 1,
        timestamp: chrono::Utc::now(),
        content_preview: "".into(),
        source: ActivitySource::Mcp,
    });
    state.emit(DaemonEvent::DrawerAdded {
        palace_id: "alpha".into(),
        palace_name: "alpha".into(),
        drawer_count: 2,
        timestamp: chrono::Utc::now(),
        content_preview: "".into(),
        source: ActivitySource::Http,
    });
    state.emit(DaemonEvent::DrawerAdded {
        palace_id: "beta".into(),
        palace_name: "beta".into(),
        drawer_count: 1,
        timestamp: chrono::Utc::now(),
        content_preview: "".into(),
        source: ActivitySource::Mcp,
    });
    // Issue #232: drain the spawn_blocking writes before querying.
    state.flush_activity_writes().await;

    let app = router().with_state(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/activity?palace=alpha&source=mcp&limit=50")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = to_bytes(resp.into_body(), 4096).await.unwrap();
    let v: Value = serde_json::from_slice(&bytes).unwrap();
    let entries = v["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 1, "filter should leave one row, got {v}");
    assert_eq!(entries[0]["palace_id"], "alpha");
    assert_eq!(entries[0]["source"], "mcp");
}

/// Why: unknown source values must produce a 400 so the caller sees the
/// typo instead of silently getting "no rows".
#[tokio::test]
async fn activity_endpoint_rejects_unknown_source() {
    let state = test_state();
    let app = router().with_state(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/activity?source=nope")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

/// Why (issue #96): MCP-side `memory_remember` must now emit a
/// `DrawerAdded` event with `source = Mcp`. Confirm by driving the MCP
/// dispatcher directly and reading the broadcast channel.
/// What: pre-creates a palace, calls `dispatch_tool("memory_remember",
/// ...)`, subscribes to the events channel before the call, and
/// asserts the next event tag is `drawer_added` with the MCP source.
/// Test: this test.
#[tokio::test]
async fn mcp_memory_remember_emits_drawer_added_with_mcp_source() {
    use crate::tools::dispatch_tool;
    let state = test_state();
    let mut rx = state.events.subscribe();
    // Create palace via the MCP tool so the activity log captures both
    // the palace_created and drawer_added events.
    let _ = dispatch_tool(&state, "palace_create", json!({"name": "p1"}))
        .await
        .expect("palace_create");
    // Drain the palace_created event.
    let first = tokio::time::timeout(std::time::Duration::from_millis(500), rx.recv())
        .await
        .expect("first event")
        .expect("channel open");
    assert!(
        matches!(first, DaemonEvent::PalaceCreated { ref source, .. } if *source == ActivitySource::Mcp)
    );

    let _ = dispatch_tool(
        &state,
        "memory_remember",
        json!({
            "palace": "p1",
            "text": "the quick brown fox jumps over the lazy dog and more"
        }),
    )
    .await
    .expect("memory_remember");

    // The next event from the channel should be DrawerAdded(Mcp).
    let next = tokio::time::timeout(std::time::Duration::from_millis(500), rx.recv())
        .await
        .expect("drawer_added event")
        .expect("channel open");
    match next {
        DaemonEvent::DrawerAdded {
            source, palace_id, ..
        } => {
            assert_eq!(source, ActivitySource::Mcp);
            assert_eq!(palace_id, "p1");
        }
        other => panic!("expected DrawerAdded, got {other:?}"),
    }

    // The activity log should now hold ≥ 2 entries (palace_created +
    // drawer_added). Also confirm the HTTP endpoint surfaces them with
    // `mcp` sources.
    // Issue #232: drain fire-and-forget activity-log writes first.
    state.flush_activity_writes().await;
    let app = router().with_state(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/activity?source=mcp&limit=10")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = to_bytes(resp.into_body(), 4096).await.unwrap();
    let v: Value = serde_json::from_slice(&bytes).unwrap();
    let entries = v["entries"].as_array().unwrap();
    let event_types: std::collections::HashSet<&str> = entries
        .iter()
        .filter_map(|e| e["event_type"].as_str())
        .collect();
    assert!(event_types.contains("drawer_added"));
    assert!(event_types.contains("palace_created"));
}

// -----------------------------------------------------------------
// Submission-logging tests (Part A: hook activity, Part B: drawer
// attribution).
// -----------------------------------------------------------------
