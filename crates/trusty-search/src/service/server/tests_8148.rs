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
