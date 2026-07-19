//! `DELETE /api/task/:id` + clear-context abort tests (#3063).
//!
//! Why: The cancellation primitive's entire point is that an aborted task
//! actually stops — not just that its HTTP-visible record gets overwritten.
//! `spawn_fake_running_task` stands in for the real background bodies in
//! `handlers.rs` (`run_pm_task_with_session` / `run_task`) with a plain
//! `tokio::sleep` + a shared flag, so these tests can assert the abort
//! happened (the flag never flips) without needing an LLM or a real
//! subprocess — exactly the same `AbortHandle` wiring
//! (`register_handle` -> `try_cancel`/`clear_tasks` -> `.abort()`) production
//! code uses.
//! What: `cancel_running_task_marks_cancelled`, `cancel_unknown_id_404`,
//! `cancel_already_done_409`, `clear_context_now_aborts_running_task`,
//! `cancel_survives_fifo_eviction_pressure`,
//! `finalize_task_does_not_clobber_cancelled`,
//! `register_handle_skips_already_terminal_task`.
//! Test: This module IS the test.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use tower::ServiceExt;

use crate::api::server::routes::build_router;
use crate::api::server::state::{AppState, CancelOutcome, MAX_RETAINED};
use crate::api::types::{PmResponse, PmStatus};

fn router(state: AppState) -> Router {
    build_router(state)
}

/// Register a `Running` placeholder plus a background future that sleeps
/// well past every assertion window below, then (if never aborted) flips
/// `completed` and calls `finalize_task` with a fabricated success result.
/// Returns the shared flag so callers can assert the future never ran to
/// completion after a cancel/clear.
async fn spawn_fake_running_task(state: &AppState, id: &str) -> Arc<AtomicBool> {
    state.upsert(id.to_string(), PmResponse::running(id)).await;
    let completed = Arc::new(AtomicBool::new(false));
    let completed_bg = completed.clone();
    let state_bg = state.clone();
    let id_bg = id.to_string();
    let join = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(30)).await;
        completed_bg.store(true, Ordering::SeqCst);
        let mut resp = PmResponse::running(&id_bg);
        resp.status = PmStatus::Success;
        state_bg.finalize_task(id_bg, resp).await;
    });
    state.register_handle(id, join.abort_handle()).await;
    completed
}

#[tokio::test]
async fn cancel_running_task_marks_cancelled() {
    let state = AppState::default();
    let completed = spawn_fake_running_task(&state, "task-1").await;

    let app = router(state.clone());
    let req = Request::builder()
        .method(Method::DELETE)
        .uri("/api/task/task-1")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = axum::body::to_bytes(resp.into_body(), 1024).await.unwrap();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["status"], "cancelled");

    // GET must see the cancelled terminal state, not a hang or fake success.
    let stored = state.get("task-1").await.unwrap();
    assert_eq!(stored.status, PmStatus::Cancelled);

    // Give the aborted future a window to run if the abort didn't actually
    // stop it; it must not flip `completed` or clobber the cancelled record.
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        !completed.load(Ordering::SeqCst),
        "task should have been aborted before its sleep finished"
    );
    let stored_after = state.get("task-1").await.unwrap();
    assert_eq!(
        stored_after.status,
        PmStatus::Cancelled,
        "a late finalize_task from the (aborted) future must not clobber the cancelled record"
    );
}

#[tokio::test]
async fn cancel_unknown_id_404() {
    let app = router(AppState::default());
    let req = Request::builder()
        .method(Method::DELETE)
        .uri("/api/task/does-not-exist")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn cancel_already_done_409() {
    let state = AppState::default();
    let mut done = PmResponse::running("task-2");
    done.status = PmStatus::Success;
    state.upsert("task-2".to_string(), done).await;

    let app = router(state.clone());
    let req = Request::builder()
        .method(Method::DELETE)
        .uri("/api/task/task-2")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::CONFLICT);
    let body = axum::body::to_bytes(resp.into_body(), 1024).await.unwrap();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["status"], "success");

    // A failed cancel attempt must not mutate the existing terminal state.
    let stored = state.get("task-2").await.unwrap();
    assert_eq!(stored.status, PmStatus::Success);
}

#[tokio::test]
async fn clear_context_now_aborts_running_task() {
    let state = AppState::default();
    let completed = spawn_fake_running_task(&state, "task-3").await;

    let app = router(state.clone());
    let req = Request::builder()
        .method(Method::POST)
        .uri("/api/clear-context")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = axum::body::to_bytes(resp.into_body(), 1024).await.unwrap();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["tasks_cancelled"], 1);

    // clear-context wipes the whole record (unlike DELETE /api/task/:id,
    // which leaves a Cancelled tombstone) — GET must miss, not hang or
    // resurrect a stale success.
    assert!(state.get("task-3").await.is_none());

    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        !completed.load(Ordering::SeqCst),
        "clear-context must actually abort the in-flight future, not just drop its record"
    );
}

/// Regression test for the FIFO-eviction bug found in code review (#3063):
/// `insert_and_trim` used to evict strictly by insertion order (index 0),
/// so a long-running task submitted first and left running while
/// `MAX_RETAINED` unrelated tasks completed would get silently evicted from
/// `responses`/`order` — orphaning its `AbortHandle` in `handles` forever
/// and making `DELETE /api/task/:id` 404 a task that was genuinely still
/// running. `insert_and_trim` now walks forward past `Running` rows to
/// evict the next-oldest terminal one instead.
#[tokio::test]
async fn cancel_survives_fifo_eviction_pressure() {
    let state = AppState::default();
    let completed = spawn_fake_running_task(&state, "long-runner").await;

    // Push MAX_RETAINED more terminal tasks after it. Under naive index-0
    // FIFO this alone would already have evicted "long-runner" (the oldest
    // entry) before we ever get a chance to cancel it.
    for i in 0..MAX_RETAINED {
        let id = format!("filler-{i}");
        let mut r = PmResponse::running(&id);
        r.status = PmStatus::Success;
        state.upsert(id, r).await;
    }

    let stored = state.get("long-runner").await;
    assert!(
        stored.is_some(),
        "a Running task must survive FIFO eviction pressure from unrelated task churn"
    );
    assert_eq!(stored.unwrap().status, PmStatus::Running);

    let app = router(state.clone());
    let req = Request::builder()
        .method(Method::DELETE)
        .uri("/api/task/long-runner")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "a still-running task must remain cancellable after eviction pressure, not 404"
    );
    let body = axum::body::to_bytes(resp.into_body(), 1024).await.unwrap();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["status"], "cancelled");

    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        !completed.load(Ordering::SeqCst),
        "the surviving task's AbortHandle must still be live and effective"
    );
}

/// Direct unit test of `AppState::finalize_task`'s cancellation guard
/// (#3063 code review), isolated from `spawn_fake_running_task`'s
/// abort-timing race: `cancel_running_task_marks_cancelled` above proves the
/// end-to-end abort path stops the future before it ever calls
/// `finalize_task`, so it never actually exercises the `already_cancelled`
/// branch inside `finalize_task` itself. This test calls `finalize_task`
/// directly, after the record is already `Cancelled`, to prove the guard
/// itself — not just that abort prevents the call from happening at all.
#[tokio::test]
async fn finalize_task_does_not_clobber_cancelled() {
    let state = AppState::default();
    state
        .upsert("task-4".to_string(), PmResponse::running("task-4"))
        .await;
    let outcome = state.try_cancel("task-4").await;
    assert!(matches!(outcome, CancelOutcome::Cancelled));

    // Simulate a late `finalize_task` call from a background future that
    // raced the cancel and lost — it must not overwrite the cancelled
    // record with its own (fabricated) success result.
    let mut late = PmResponse::running("task-4");
    late.status = PmStatus::Success;
    state.finalize_task("task-4".to_string(), late).await;

    let stored = state.get("task-4").await.unwrap();
    assert_eq!(
        stored.status,
        PmStatus::Cancelled,
        "finalize_task must not clobber an already-cancelled record"
    );
}

/// Direct unit test of `AppState::register_handle`'s terminal-status guard
/// (#3063 code review): registering a handle for a task whose stored
/// response already reached a terminal status (a pathologically fast task
/// completing before `register_handle` runs after `tokio::spawn`) must be a
/// no-op, not silently orphan the handle in `handles` forever.
#[tokio::test]
async fn register_handle_skips_already_terminal_task() {
    let state = AppState::default();
    let mut done = PmResponse::running("task-5");
    done.status = PmStatus::Success;
    state.upsert("task-5".to_string(), done).await;

    let join = tokio::spawn(async { tokio::time::sleep(Duration::from_secs(30)).await });
    state.register_handle("task-5", join.abort_handle()).await;
    join.abort();

    assert!(
        !state.inner.lock().await.handles.contains_key("task-5"),
        "register_handle must skip storing a handle for an already-terminal task"
    );
}
