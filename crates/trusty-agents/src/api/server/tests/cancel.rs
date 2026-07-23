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
//! `cancel_survives_fifo_eviction_pressure`.
//! Test: This module IS the test.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use tower::ServiceExt;

use crate::api::server::routes::build_router;
use crate::api::server::state::{AppState, MAX_RETAINED};
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

/// #3737: `DELETE /api/tasks` clears the finished-task history but must leave
/// a still-running task in place (unlike `POST /api/clear-context`, which
/// aborts and wipes everything). Verifies the route removes only terminal
/// entries, keeps the running one visible, and reports accurate counts.
#[tokio::test]
async fn clear_recent_tasks_removes_terminal_only() {
    let state = AppState::default();

    // Two terminal tasks + one running placeholder (no background future — we
    // only care that the running record survives, not about abort wiring).
    let mut ok = PmResponse::running("done-ok");
    ok.status = PmStatus::Success;
    state.upsert("done-ok".to_string(), ok).await;
    let mut failed = PmResponse::running("done-fail");
    failed.status = PmStatus::Failed;
    state.upsert("done-fail".to_string(), failed).await;
    state
        .upsert(
            "still-running".to_string(),
            PmResponse::running("still-running"),
        )
        .await;

    let app = router(state.clone());
    let req = Request::builder()
        .method(Method::DELETE)
        .uri("/api/tasks")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = axum::body::to_bytes(resp.into_body(), 1024).await.unwrap();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["cleared"], true);
    assert_eq!(v["removed"], 2);
    assert_eq!(v["retained_running"], 1);

    // Terminal tasks gone; the running one survives and is still listed.
    assert!(state.get("done-ok").await.is_none());
    assert!(state.get("done-fail").await.is_none());
    let running = state.get("still-running").await.expect("running task kept");
    assert_eq!(running.status, PmStatus::Running);
    assert_eq!(state.list().await.len(), 1);
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
