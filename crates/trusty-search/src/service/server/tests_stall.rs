//! Tests for embedder stall observability on `GET /health` (issue #1003).
//!
//! Why: the ANE/CoreML stall scenario (sidecar alive but unresponsive) was
//! previously invisible — `/health` reported `embedder:ready` even while
//! embed calls timed out for 42 minutes. These tests verify that:
//!   1. `EmbedderStallTracker` correctly records and clears stall state.
//!   2. `health_handler` reports `embedder:"stalled"` when the tracker has
//!      recent timeouts, and `embedder:"ready"` after recovery.
//!   3. The new health fields (`embedder_last_ok_secs_ago`,
//!      `embedder_recent_timeout_count`) are present in the response.

use super::*;
use crate::core::registry::IndexRegistry;
use crate::service::stall_tracker::EmbedderStallTracker;
use axum::extract::State;
use axum::Json;
use std::sync::Arc;

/// Build a minimal `SearchAppState` with an embedder in "ready" state
/// (bypasses the actual ONNX model) and returns it alongside the stall tracker.
fn ready_state_with_tracker() -> (Arc<SearchAppState>, Arc<EmbedderStallTracker>) {
    use crate::core::embed::MockEmbedder;
    let registry = IndexRegistry::new();
    let state =
        Arc::new(SearchAppState::new(registry).with_embedder(Arc::new(MockEmbedder::new(4))));
    let tracker = Arc::clone(&state.embedder_stall_tracker);
    (state, tracker)
}

/// `health_handler` must report `embedder:"ready"` and
/// `embedder_recent_timeout_count:0` when no embed errors have been recorded.
///
/// Why: baseline correctness — a newly-started daemon with a live embedder
/// must not falsely report degraded state.
/// What: build a ready state, call health_handler, assert fields.
/// Test: this test.
#[tokio::test]
async fn health_reports_ready_when_no_timeouts() {
    let (state, _tracker) = ready_state_with_tracker();
    let Json(resp) = health_handler(State(state)).await;
    assert_eq!(resp.embedder, "ready", "no timeouts → must be ready");
    assert_eq!(
        resp.embedder_recent_timeout_count, 0,
        "zero timeouts expected"
    );
    // Before any embed call, last_ok_secs_ago is absent.
    assert!(
        resp.embedder_last_ok_secs_ago.is_none(),
        "no embed call yet → last_ok_secs_ago must be absent"
    );
}

/// `health_handler` must report `embedder:"stalled"` when the tracker has
/// recorded one or more recent timeouts.
///
/// Why: the primary fix for issue #1003 — the stall must be visible on
/// `/health` so operators can detect the degraded BM25-only fallback without
/// tailing logs.
/// What: record two timeouts, call health_handler, assert `"stalled"`.
/// Test: this test.
#[tokio::test]
async fn health_reports_stalled_after_embed_timeouts() {
    let (state, tracker) = ready_state_with_tracker();

    // Simulate two embed-call timeouts (as the embed pool records them).
    tracker.record_timeout();
    tracker.record_timeout();

    let Json(resp) = health_handler(State(state)).await;
    assert_eq!(
        resp.embedder, "stalled",
        "recent timeouts → embedder must be \"stalled\"; got {:?}",
        resp.embedder
    );
    assert_eq!(
        resp.embedder_recent_timeout_count, 2,
        "two timeouts must be reflected in the count"
    );
    // No successful embed → last_ok_secs_ago absent.
    assert!(
        resp.embedder_last_ok_secs_ago.is_none(),
        "no success yet → last_ok_secs_ago must be absent"
    );
}

/// After a stall self-clears (embedder responds again), `health_handler`
/// must flip back to `embedder:"ready"`.
///
/// Why: the ANE stall self-heals once fresh requests kick the backend; the
/// health endpoint must reflect recovery immediately (not linger as "stalled").
/// What: record timeout then success; assert `"ready"` and count == 0.
/// Test: this test.
#[tokio::test]
async fn health_recovers_to_ready_after_stall_clears() {
    let (state, tracker) = ready_state_with_tracker();

    // Stall scenario.
    tracker.record_timeout();
    tracker.record_timeout();
    tracker.record_timeout();

    // Stall self-heals — next embed succeeds.
    tracker.record_success();

    let Json(resp) = health_handler(State(state)).await;
    assert_eq!(
        resp.embedder, "ready",
        "after success the stall must clear; got {:?}",
        resp.embedder
    );
    assert_eq!(
        resp.embedder_recent_timeout_count, 0,
        "success must reset timeout count to 0"
    );
    // After a success, last_ok_secs_ago should be present and near-zero.
    let ago = resp
        .embedder_last_ok_secs_ago
        .expect("last_ok_secs_ago must be present after a success");
    assert!(
        ago < 5,
        "last_ok_secs_ago should be near-zero seconds; got {ago}"
    );
}

/// `embedder_recent_timeout_count` and `embedder_last_ok_secs_ago` must be
/// present as JSON fields in the serialized response.
///
/// Why: integration contract — external monitors scrape `/health` as JSON;
/// field presence must be stable.
/// What: serialize `HealthResponse` to JSON and verify field presence.
/// Test: this test.
#[test]
fn health_response_contains_stall_fields() {
    use crate::service::server::health::HealthResponse;
    use crate::service::server::state::WarmBootSummary;
    use serde_json::Value;

    let resp = HealthResponse {
        status: "ok",
        version: "0.0.0",
        indexes: 1,
        uptime_secs: 10,
        embedder: "stalled",
        embedder_error: None,
        embedder_last_ok_secs_ago: Some(120),
        embedder_recent_timeout_count: 3,
        rss_mb: 0,
        rss_limit_mb: 0,
        disk_bytes: 0,
        cpu_pct: 0.0,
        embedder_info: None,
        embedderd_rss_mb: None,
        background_reindex_queue_depth: 0,
        deferred_embed_queue_depth: 0,
        update_available: None,
        indexes_kg_disabled: 0,
        indexes_vector_disabled: 0,
        indexes_component_catch_up_in_progress: 0,
        indexes_embed_pool_missing: 0,
        indexes_stuck_empty: 0,
        indexes_populated: 0,
        indexes_empty: 0,
        total_chunks: 0,
        warmboot_summary: WarmBootSummary::default(),
        boot_reconcile: None,
        indexes_watcher_network_degraded: 0,
        embedder_bootstrap: "n/a",
    };

    let json: Value = serde_json::to_value(&resp).expect("serialize");
    assert_eq!(
        json["embedder"].as_str(),
        Some("stalled"),
        "embedder field must be 'stalled'"
    );
    assert_eq!(
        json["embedder_recent_timeout_count"].as_u64(),
        Some(3),
        "timeout count must be present"
    );
    assert_eq!(
        json["embedder_last_ok_secs_ago"].as_u64(),
        Some(120),
        "last_ok_secs_ago must be present when Some"
    );
}

/// When `embedder_last_ok_secs_ago` is `None`, the field must be absent from
/// the serialized JSON (not null).
///
/// Why: `#[serde(skip_serializing_if = "Option::is_none")]` must be applied
/// — absent means "never succeeded", null would be misleading.
/// What: serialize with `None` and assert the key is missing.
/// Test: this test.
#[test]
fn health_response_omits_last_ok_when_none() {
    use crate::service::server::health::HealthResponse;
    use crate::service::server::state::WarmBootSummary;
    use serde_json::Value;

    let resp = HealthResponse {
        status: "ok",
        version: "0.0.0",
        indexes: 0,
        uptime_secs: 0,
        embedder: "initializing",
        embedder_error: None,
        embedder_last_ok_secs_ago: None,
        embedder_recent_timeout_count: 0,
        rss_mb: 0,
        rss_limit_mb: 0,
        disk_bytes: 0,
        cpu_pct: 0.0,
        embedder_info: None,
        embedderd_rss_mb: None,
        background_reindex_queue_depth: 0,
        deferred_embed_queue_depth: 0,
        update_available: None,
        indexes_kg_disabled: 0,
        indexes_vector_disabled: 0,
        indexes_component_catch_up_in_progress: 0,
        indexes_embed_pool_missing: 0,
        indexes_stuck_empty: 0,
        indexes_populated: 0,
        indexes_empty: 0,
        total_chunks: 0,
        warmboot_summary: WarmBootSummary::default(),
        boot_reconcile: None,
        indexes_watcher_network_degraded: 0,
        embedder_bootstrap: "n/a",
    };

    let json: Value = serde_json::to_value(&resp).expect("serialize");
    assert!(
        json.get("embedder_last_ok_secs_ago").is_none(),
        "embedder_last_ok_secs_ago must be absent (not null) when None; \
         json={json}"
    );
}

/// `/health`'s `deferred_embed_queue_depth` must report a REAL queued
/// deferred-embed catch-up job, end to end — not just round-trip whatever
/// value a struct literal was built with.
///
/// Why: `health_response_contains_stall_fields` and
/// `health_response_omits_last_ok_when_none` above both build a
/// `HealthResponse` literal with `deferred_embed_queue_depth: 0` and assert
/// nothing about that field — that proves serde serializes the field, not
/// that `health_handler` reads the live
/// `reindex::deferred_embed_queue::QUEUE_DEPTH` gauge. #5546 caught this: the
/// doc comment on the field (`health.rs`) cited a test,
/// `health_reports_the_deferred_embed_queue_depth`, that was never written.
/// This test closes that gap by driving the real queue: it holds the single
/// `background_reindex_semaphore` permit itself so a job enqueued via
/// `spawn_deferred_embed_pass` reaches `wait_for_turn` — which increments
/// `QUEUE_DEPTH` synchronously, inside `enqueue`, before `wait_for_turn` ever
/// runs — but cannot proceed past its own semaphore acquire. That makes the
/// nonzero reading deterministic rather than a race against the pass's own
/// (otherwise near-instant, no-embedder) completion.
/// What: reads a baseline `/health` depth (tolerant of a nonzero value left
/// by another test's in-flight job — `QUEUE_DEPTH` is a process-global
/// static shared by the whole test binary, per `defer_embed_queue`'s module
/// docs), acquires the background semaphore, enqueues one bare-handle job,
/// and asserts `/health` immediately reports `baseline + 1`. Then releases
/// the semaphore and polls `/health` until the depth drops back to
/// `baseline`, proving the field tracks the queue draining too, not only its
/// creation.
/// Test: this test.
#[tokio::test(flavor = "multi_thread")]
async fn health_reports_the_deferred_embed_queue_depth_end_to_end() {
    let (state, _tracker) = ready_state_with_tracker();

    let Json(baseline_resp) = health_handler(State(Arc::clone(&state))).await;
    let baseline = baseline_resp.deferred_embed_queue_depth;

    // Hold the ONE background-reindex permit ourselves so the job we are
    // about to enqueue is counted (QUEUE_DEPTH is incremented synchronously
    // inside `enqueue`, before the spawned `wait_for_turn` task ever runs)
    // but cannot race ahead and complete before the assertion below runs.
    let permit = crate::service::reindex::background_reindex_semaphore()
        .acquire()
        .await
        .expect("background_reindex_semaphore is never closed");

    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().to_path_buf();
    let indexer =
        crate::core::indexer::CodeIndexer::new("health-deferred-depth-test", root.clone());
    let handle = Arc::new(crate::core::registry::IndexHandle::bare(
        crate::core::registry::IndexId::new("health-deferred-depth-test"),
        Arc::new(tokio::sync::RwLock::new(indexer)),
        root,
    ));
    let progress = Arc::new(crate::service::reindex::ReindexProgress::new());
    crate::service::reindex::spawn_deferred_embed_pass(handle, progress, 0);

    let Json(during_resp) = health_handler(State(Arc::clone(&state))).await;
    assert_eq!(
        during_resp.deferred_embed_queue_depth,
        baseline + 1,
        "/health must report the real queued job while it is pending — not a value \
         frozen at whatever a fixture happened to hardcode"
    );

    // Release the permit so the job can drain, then confirm the field
    // reflects the drain too — the round trip a fixture round-trip cannot
    // prove.
    drop(permit);
    let mut drained = during_resp.deferred_embed_queue_depth;
    for _ in 0..500 {
        let Json(resp) = health_handler(State(Arc::clone(&state))).await;
        drained = resp.deferred_embed_queue_depth;
        if drained == baseline {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert_eq!(
        drained, baseline,
        "/health must reflect the queue draining back down once the job completes"
    );
}
