//! Per-assistant task-stream attribution, retention, and retrieval (#4355).
//!
//! Why: "switching assistants loads that assistant's most recent task stream"
//! is a server-side guarantee (owner decision), so the association, the
//! retention bound, and the query that reads them back all need pinning here
//! rather than in a client. The retention tests matter most: a server-wide cap
//! divided across a six-assistant roster leaves ~3 turns of history each, which
//! is the failure mode that makes the feature useless the moment it ships.
//! What: route-level assertions against the axum router for the `?agent=`
//! query, plus `AppState`-level assertions for per-stream and global eviction.
//! Test: This module IS the test.

use super::super::routes::build_router;
use crate::api::server::state::{AppState, MAX_RETAINED_PER_AGENT, MAX_RETAINED_TOTAL};
use crate::api::types::{PmResponse, PmStatus};
use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt;

/// Store one terminal (`Success`) task addressed to `agent`.
async fn store_task(state: &AppState, id: &str, agent: &str) {
    let mut r = PmResponse::running(id).addressed_to(agent);
    r.status = PmStatus::Success;
    state.upsert(id.to_string(), r).await;
}

/// `GET /api/tasks<query>` against `state` → the returned ids, in order.
async fn task_ids(state: &AppState, query: &str) -> Vec<String> {
    let app = build_router(state.clone());
    let req = Request::builder()
        .uri(format!("/api/tasks{query}"))
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(resp.into_body(), 8 * 1024 * 1024)
        .await
        .unwrap();
    let rows: Vec<serde_json::Value> = serde_json::from_slice(&bytes).unwrap();
    rows.iter()
        .map(|r| r["id"].as_str().unwrap().to_string())
        .collect()
}

/// A store holding two interleaved streams: `ctrl-1`, `izzie-1`, `ctrl-2`,
/// `izzie-2`, oldest first.
async fn two_stream_state() -> AppState {
    let state = AppState::default();
    store_task(&state, "ctrl-1", "ctrl").await;
    store_task(&state, "izzie-1", "izzie").await;
    store_task(&state, "ctrl-2", "ctrl").await;
    store_task(&state, "izzie-2", "izzie").await;
    state
}

/// #4355: an `agent=` filter returns ONLY that assistant's stream, newest
/// first — the one call the client makes when the user switches assistants.
/// The router is exercised end to end (not `list_stream` directly) because the
/// query-parameter decoding and the `Query` extractor are part of the contract
/// a client depends on.
#[tokio::test]
async fn tasks_filtered_by_agent_returns_only_that_stream_newest_first() {
    let state = two_stream_state().await;
    assert_eq!(
        task_ids(&state, "?agent=izzie").await,
        vec!["izzie-2", "izzie-1"],
        "only izzie's stream, newest first"
    );
    assert_eq!(
        task_ids(&state, "?agent=ctrl").await,
        vec!["ctrl-2", "ctrl-1"]
    );
    // The rows carry the stream they were returned for, so a client never has
    // to re-derive the attribution it just queried by.
    assert!(
        state
            .list_stream(Some("izzie"), None)
            .await
            .iter()
            .all(|r| r.addressed_agent == "izzie")
    );
}

/// #4355: submitting a task records the assistant it was addressed to. This is
/// the association the pre-#4355 server discarded — `TaskRequest.agent` was
/// read once to choose a dispatch path and never written anywhere. Asserted
/// end to end through `POST /api/task` because the stamp has to survive both
/// the accepted placeholder and whatever the background turn finalizes.
#[tokio::test]
async fn submitting_with_an_agent_records_it_as_the_addressed_assistant() {
    let state = AppState::default();
    let app = build_router(state.clone());
    let req = Request::builder()
        .method("POST")
        .uri("/api/task")
        .header("content-type", "application/json")
        .body(Body::from(r#"{"task":"hello","agent":"izzie"}"#))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::ACCEPTED);

    let rows = state.list_stream(Some("izzie"), None).await;
    assert_eq!(rows.len(), 1, "the task belongs to izzie's stream");
    assert!(
        state.list_stream(Some("ctrl"), None).await.is_empty(),
        "and not to the Concierge's"
    );
}

/// #4355: submitting with NO roster selection is not unattributed — it belongs
/// to the Concierge, which is what a null selection already means in the
/// client. Without this the default assistant, whose turns are the common
/// case, would have no stream at all.
#[tokio::test]
async fn submitting_without_an_agent_records_the_concierge() {
    let state = AppState::default();
    let app = build_router(state.clone());
    let req = Request::builder()
        .method("POST")
        .uri("/api/task")
        .header("content-type", "application/json")
        .body(Body::from(r#"{"task":"hello"}"#))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::ACCEPTED);

    assert_eq!(state.list_stream(Some("ctrl"), None).await.len(), 1);
}

/// #4355: an assistant with no history is not an error — a roster entry the
/// user just added, or one they have never talked to, gets an empty stream so
/// the client renders an empty conversation rather than an error state.
#[tokio::test]
async fn tasks_for_an_unknown_agent_returns_an_empty_stream() {
    let state = two_stream_state().await;
    assert!(task_ids(&state, "?agent=never-used").await.is_empty());
}

/// #4355: omitting the query, or sending a blank `agent` (a cleared roster
/// selection serialized as `agent=`), must reproduce the unfiltered listing —
/// this is what every pre-#4355 caller sends and its response must not change.
#[tokio::test]
async fn tasks_query_defaults_match_the_unfiltered_listing() {
    let state = two_stream_state().await;
    let unfiltered = vec!["izzie-2", "ctrl-2", "izzie-1", "ctrl-1"];
    assert_eq!(task_ids(&state, "").await, unfiltered);
    assert_eq!(task_ids(&state, "?agent=").await, unfiltered);
    assert_eq!(task_ids(&state, "?agent=%20%20").await, unfiltered);
    assert_eq!(task_ids(&state, "?limit=99").await, unfiltered);
}

/// #4355: `limit` caps the returned rows without disturbing recency ordering,
/// so a client can ask for just the tail of a stream.
#[tokio::test]
async fn tasks_limit_returns_the_newest_n_of_a_stream() {
    let state = AppState::default();
    for i in 0..5 {
        store_task(&state, &format!("t-{i}"), "izzie").await;
    }
    let rows = state.list_stream(Some("izzie"), Some(2)).await;
    let ids: Vec<&str> = rows.iter().map(|r| r.id.as_str()).collect();
    assert_eq!(ids, vec!["t-4", "t-3"]);
}

/// 🔴 The load-bearing retention assertion (#4355). Before this change the cap
/// was a single server-wide 20, so these two assistants would have shared it —
/// 40 submissions would leave 20 rows total and the older assistant's stream
/// would be almost entirely gone. Retention is now per-stream: each assistant
/// keeps its own `MAX_RETAINED_PER_AGENT`, and a busy assistant cannot evict an
/// idle one's history.
#[tokio::test]
async fn per_agent_retention_keeps_n_for_each_assistant() {
    let state = AppState::default();
    // Interleaved, so a global cap would evict from both streams rather than
    // conveniently truncating one.
    for i in 0..(MAX_RETAINED_PER_AGENT + 5) {
        store_task(&state, &format!("ctrl-{i}"), "ctrl").await;
        store_task(&state, &format!("izzie-{i}"), "izzie").await;
    }

    let ctrl = state.list_stream(Some("ctrl"), None).await;
    let izzie = state.list_stream(Some("izzie"), None).await;
    assert_eq!(ctrl.len(), MAX_RETAINED_PER_AGENT);
    assert_eq!(izzie.len(), MAX_RETAINED_PER_AGENT);
    assert_eq!(
        state.list_stream(None, None).await.len(),
        MAX_RETAINED_PER_AGENT * 2,
        "two assistants retain 2N rows, not the old server-wide N"
    );
    // Each stream kept its own newest rows.
    assert_eq!(ctrl[0].id, format!("ctrl-{}", MAX_RETAINED_PER_AGENT + 4));
    assert_eq!(izzie[0].id, format!("izzie-{}", MAX_RETAINED_PER_AGENT + 4));
}

/// #4355: per-assistant retention is unbounded in the number of DISTINCT
/// assistant ids — `TaskRequest.agent` is free-form, so a client minting a new
/// name per request would grow the store forever. The global backstop bounds
/// total memory (and the `tasks.json` rewritten in full on every upsert)
/// regardless of how many streams exist.
#[tokio::test]
async fn global_backstop_caps_total_across_many_assistants() {
    let state = AppState::default();
    // One task each for far more assistants than the backstop allows rows.
    for i in 0..(MAX_RETAINED_TOTAL + 25) {
        store_task(&state, &format!("t-{i}"), &format!("agent-{i}")).await;
    }
    let all = state.list_stream(None, None).await;
    assert_eq!(all.len(), MAX_RETAINED_TOTAL);
    // Oldest streams were dropped; the newest survives.
    assert_eq!(all[0].id, format!("t-{}", MAX_RETAINED_TOTAL + 24));
    assert!(
        state.list_stream(Some("agent-0"), None).await.is_empty(),
        "the oldest stream is the one evicted"
    );
}

/// #4355 regression guard: eviction must never take a `Running` row, and the
/// per-stream pass must not change that. A live task in a full stream stays
/// reachable — the #3063 contract, restated for the per-agent trim.
#[tokio::test]
async fn per_agent_trim_never_evicts_a_running_task() {
    let state = AppState::default();
    state
        .upsert(
            "live".to_string(),
            PmResponse::running("live").addressed_to("izzie"),
        )
        .await;
    for i in 0..(MAX_RETAINED_PER_AGENT + 5) {
        store_task(&state, &format!("izzie-{i}"), "izzie").await;
    }
    let live = state.get("live").await.expect("running task must survive");
    assert_eq!(live.status, PmStatus::Running);
    assert_eq!(live.addressed_agent, "izzie");
}

/// #4355: cancelling a task must not move it between streams. `cancelled()`
/// starts from the Concierge default, so `try_cancel` carries the original
/// stream over; without that, cancelling an Izzie task would make it vanish
/// from Izzie's history and appear in the Concierge's.
#[tokio::test]
async fn cancelling_a_task_keeps_it_in_its_own_stream() {
    let state = AppState::default();
    state
        .upsert(
            "izzie-live".to_string(),
            PmResponse::running("izzie-live").addressed_to("izzie"),
        )
        .await;
    state.try_cancel("izzie-live").await;

    let stored = state.get("izzie-live").await.unwrap();
    assert_eq!(stored.status, PmStatus::Cancelled);
    assert_eq!(stored.addressed_agent, "izzie");
    assert_eq!(state.list_stream(Some("izzie"), None).await.len(), 1);
    assert!(state.list_stream(Some("ctrl"), None).await.is_empty());
}
