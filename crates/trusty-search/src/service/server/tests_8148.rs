//! Handler-level tests for the embed-only catch-up trigger (#8148).
//!
//! Why: an index registered over an `index.redb` whose chunks carry no vectors
//! already has `skip_vector: false`, so `resolve_component_toggle` reported no
//! transition and `PATCH /indexes/:id/config {"vector": true}` answered
//! `catch_up_started: false`. Nothing else queues the C2 pass for that corpus —
//! the boot re-arm fires only on the `deferred_embed_pending` marker a reindex
//! leaves — so the semantic stage stayed `Pending` forever and a full reindex
//! was the only route to vectors.
//! What: two handler-level tests differing in ONE input, the semantic stage's
//! status, so what is under test is the DECISION and not a literal. They live
//! here rather than in `tests_components` because that file sits at the
//! 500-SLOC production cap (`check_line_cap.sh` counts a `tests_*.rs`
//! basename as production), and they reuse its helpers rather than restating
//! them.
//! Test: this module. Run with `cargo test -p trusty-search tests_8148`.

use super::index_config::{patch_index_config_handler, PatchIndexConfigRequest};
use super::tests_components::{body_json, poll_stages, state_with_index, IsolatedDataDir};
use crate::core::registry::{IndexId, StageStatus};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use std::sync::Arc;

/// #8148: `PATCH { vector: true }` on an index whose vector lane is ALREADY
/// enabled but whose semantic stage never got built must run the embed
/// catch-up — this is the embed-only trigger.
///
/// Why: this is the reported defect, and the response's `catch_up_started` is
/// the only signal a caller has that anything was queued at all.
/// What: PATCHes `vector: true` against a freshly registered (never embedded)
/// index and asserts the catch-up started and the stage left `Pending`.
/// Pre-fix both assertions fail.
/// Test: this test.
#[tokio::test]
#[serial_test::serial]
async fn patch_vector_true_runs_the_catch_up_when_semantic_never_built() {
    let _isolated = IsolatedDataDir::new();
    let state = state_with_index("comp-8148-pending");
    let handle = state
        .registry
        .get(&IndexId::new("comp-8148-pending"))
        .expect("handle");
    assert!(
        !handle.skip_vector,
        "sanity: the lane is already enabled, which is why this is not a turn-on"
    );
    assert_eq!(
        handle.stages.read().await.semantic.status,
        StageStatus::Pending,
        "sanity: nothing has embedded this corpus yet"
    );

    let resp = patch_index_config_handler(
        State(Arc::clone(&state)),
        Path("comp-8148-pending".to_string()),
        Json(PatchIndexConfigRequest {
            vector: Some(true),
            ..Default::default()
        }),
    )
    .await;

    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(
        body["components"]["catch_up_started"].as_bool(),
        Some(true),
        "#8148: an enabled-but-unbuilt semantic lane must start a catch-up"
    );
    poll_stages(&handle, |s| s.semantic.status == StageStatus::Ready).await;
}

/// #8148 (the other direction): once the semantic stage is `Ready`, the same
/// PATCH is a no-op.
///
/// Why: guards the fix against turning every `vector: true` into a re-embed —
/// an idempotent config call must not start work, and a second pass would
/// contend for the same per-index permit as the first.
/// What: marks the stage `Ready`, then sends the identical PATCH and asserts
/// `catch_up_started: false` with the stage untouched.
/// Test: this test.
#[tokio::test]
#[serial_test::serial]
async fn patch_vector_true_is_a_no_op_once_semantic_is_ready() {
    let _isolated = IsolatedDataDir::new();
    let state = state_with_index("comp-8148-ready");
    let handle = state
        .registry
        .get(&IndexId::new("comp-8148-ready"))
        .expect("handle");
    {
        let mut stages = handle.stages.write().await;
        stages.semantic.status = StageStatus::Ready;
    }

    let resp = patch_index_config_handler(
        State(Arc::clone(&state)),
        Path("comp-8148-ready".to_string()),
        Json(PatchIndexConfigRequest {
            vector: Some(true),
            ..Default::default()
        }),
    )
    .await;

    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(
        body["components"]["catch_up_started"].as_bool(),
        Some(false),
        "#8148: a built semantic lane has nothing to catch up on"
    );
    assert_eq!(
        handle.stages.read().await.semantic.status,
        StageStatus::Ready,
        "a no-op PATCH must not knock a Ready stage back to InProgress"
    );
}

/// #8148 criterion: a PATCH that does not ask for `vector: true`, and a
/// `vector: true` against a pass already `InProgress`, both change nothing.
///
/// Why: the trigger must fire only on an explicit request against an unbuilt
/// stage. A hygiene-only PATCH (here `include_docs`) must never start an embed
/// pass, and a second pass over an `InProgress` stage would contend for the
/// per-index permit the first pass holds.
/// What: for each (request, starting status) case, sends the PATCH and asserts
/// `catch_up_started: false` with the semantic stage still at its starting
/// status.
/// Test: this test.
#[tokio::test]
#[serial_test::serial]
async fn patch_without_vector_true_or_against_in_progress_changes_nothing() {
    let _isolated = IsolatedDataDir::new();
    let cases = [
        (
            "comp-8148-hygiene",
            PatchIndexConfigRequest {
                include_docs: Some(true),
                ..Default::default()
            },
            StageStatus::Pending,
        ),
        (
            "comp-8148-in-progress",
            PatchIndexConfigRequest {
                vector: Some(true),
                ..Default::default()
            },
            StageStatus::InProgress,
        ),
    ];
    for (id, req, start) in cases {
        let state = state_with_index(id);
        let handle = state.registry.get(&IndexId::new(id)).expect("handle");
        handle.stages.write().await.semantic.status = start;

        let resp =
            patch_index_config_handler(State(Arc::clone(&state)), Path(id.to_string()), Json(req))
                .await;

        assert_eq!(resp.status(), StatusCode::OK, "{id}");
        let body = body_json(resp).await;
        assert_eq!(
            body["components"]["catch_up_started"].as_bool(),
            Some(false),
            "{id}: must not start a catch-up"
        );
        assert_eq!(
            handle.stages.read().await.semantic.status,
            start,
            "{id}: the semantic stage must be untouched"
        );
    }
}

/// Send `PATCH { vector: true }` for `id` and return the status and body.
async fn patch_vector_true(
    state: &Arc<crate::service::server::SearchAppState>,
    id: &str,
) -> (StatusCode, serde_json::Value) {
    let resp = patch_index_config_handler(
        State(Arc::clone(state)),
        Path(id.to_string()),
        Json(PatchIndexConfigRequest {
            vector: Some(true),
            ..Default::default()
        }),
    )
    .await;
    let status = resp.status();
    (status, body_json(resp).await)
}

/// #8148 review finding 1: re-arm PATCHes across different indexes queue
/// behind the one background permit instead of running concurrent embed passes.
///
/// Why: the first re-arm ran through the component catch-up, which never takes
/// `background_reindex_semaphore`, so N PATCHes started N embed passes at once.
/// What: holds the background permit (standing in for any other embed pass),
/// re-arms three indexes, and asserts none of them reaches `Ready` while the
/// permit is held. After the permit is released, all three do. Pre-fix the
/// passes ignore the permit and reach `Ready` at once.
/// Test: this test.
#[tokio::test]
#[serial_test::serial]
async fn rearm_patches_across_indexes_wait_for_the_background_permit() {
    let _isolated = IsolatedDataDir::new();
    let held = crate::service::reindex::background_reindex_semaphore()
        .acquire()
        .await
        .expect("background semaphore is never closed");
    let mut handles = Vec::new();
    let mut states = Vec::new();
    for id in ["comp-8148-q1", "comp-8148-q2", "comp-8148-q3"] {
        let state = state_with_index(id);
        let handle = state.registry.get(&IndexId::new(id)).expect("handle");
        let (status, body) = patch_vector_true(&state, id).await;
        assert_eq!(status, StatusCode::OK, "{id}: {body}");
        assert_eq!(
            body["components"]["catch_up_started"].as_bool(),
            Some(true),
            "{id}: {body}"
        );
        handles.push(handle);
        states.push(state);
    }

    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    for handle in &handles {
        assert_eq!(
            handle.stages.read().await.semantic.status,
            StageStatus::InProgress,
            "{}: no embed pass may run while another pass holds the background permit",
            handle.id.0
        );
    }

    drop(held);
    for handle in &handles {
        poll_stages(handle, |s| s.semantic.status == StageStatus::Ready).await;
    }
}

/// #8148 review finding 2: a persist failure on a re-arm still runs the pass,
/// so the stage is never left `InProgress` with nothing running, and a retry
/// succeeds.
///
/// Why: the stage was flipped to `InProgress` before the persist; a 500 then
/// returned without spawning, and the predicate refused every retry because
/// the stage read `InProgress`.
/// What: points `TRUSTY_DATA_DIR` under a regular file so the persist fails,
/// PATCHes `vector: true` (500), restores a writable data dir, retries (200),
/// and waits for `Ready`. Pre-fix the stage never leaves `InProgress`.
/// Test: this test.
#[tokio::test]
#[serial_test::serial]
async fn a_persist_failure_still_runs_the_rearm_and_a_retry_succeeds() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let blocker = tmp.path().join("not-a-dir");
    std::fs::write(&blocker, b"x").expect("write blocker");
    // SAFETY: serial test; `IsolatedDataDir` below re-points and removes it.
    unsafe { std::env::set_var("TRUSTY_DATA_DIR", blocker.join("data")) };

    let id = "comp-8148-persist-fail";
    let state = state_with_index(id);
    let handle = state.registry.get(&IndexId::new(id)).expect("handle");
    let (status, body) = patch_vector_true(&state, id).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{body}");
    assert_eq!(body["persisted"].as_bool(), Some(false), "{body}");

    let _isolated = IsolatedDataDir::new();
    let (status, body) = patch_vector_true(&state, id).await;
    assert_eq!(status, StatusCode::OK, "the retry must succeed: {body}");
    poll_stages(&handle, |s| s.semantic.status == StageStatus::Ready).await;
}
