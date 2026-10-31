//! Tests for the #10551 embed-missing resume.
//!
//! Why: the closure conditions are behavioural: only the missing chunks are
//! embedded, the stage leaves `Failed`, a repeat run is a no-op, and a dead
//! embedder never touches the stage.
//! What: every case starts from an index whose embedder died mid-stage
//! (`embed_missing_test_support::failed_mid_stage`), the failure the issue saw.
//! Test: this file.

use std::sync::atomic::Ordering;

use super::*;
use crate::service::embed_missing_test_support::{failed_mid_stage, wait_for_stage};

/// The #10551 failure path: kill the embedder mid-stage, then resume to zero
/// missing, embedding only the tail.
///
/// Why: this is the closure test. Pre-fix nothing re-ran a `Failed` stage.
/// What: after the failure the first wave's vectors are in the store and the
/// stage is `Failed`. Once the embedder is healed, one resume queues the pass,
/// the stage reads `Ready` with every chunk covered, and the embedder was asked
/// for exactly the `TAIL` missing chunk texts and nothing else.
/// Test: this test.
#[tokio::test]
#[serial_test::serial]
async fn only_the_missing_chunks_are_embedded_and_the_stage_leaves_failed() {
    let (handle, embedder, store, n, committed) = failed_mid_stage("embed-missing-resume").await;
    let missing = n - committed;

    embedder.heal();
    let report = resume_semantic(&handle, false).await.expect("resume");
    assert_eq!(report.action, ResumeAction::Queued);
    assert_eq!(report.missing, Some(missing));
    wait_for_stage(&handle, StageStatus::Ready).await;

    assert_eq!(store.len().await.expect("len"), n, "missing count is zero");
    let texts = embedder.healed_chunk_texts();
    assert_eq!(texts.len(), missing, "only the missing chunks");
}

/// A second run over a covered index changes nothing.
///
/// Why: idempotence is a closure condition.
/// Test: this test.
#[tokio::test]
#[serial_test::serial]
async fn a_repeat_run_with_nothing_missing_is_a_noop() {
    let (handle, embedder, _store, _n, _) = failed_mid_stage("embed-missing-repeat").await;
    embedder.heal();
    resume_semantic(&handle, false).await.expect("resume");
    wait_for_stage(&handle, StageStatus::Ready).await;
    let calls = embedder.calls.load(Ordering::SeqCst);

    let again = resume_semantic(&handle, false).await.expect("repeat");
    assert_eq!(again.action, ResumeAction::Noop);
    assert_eq!(again.missing, Some(0));
    assert_eq!(
        embedder.calls.load(Ordering::SeqCst),
        calls,
        "no embed call"
    );
    assert_eq!(
        handle.stages.read().await.semantic.status,
        StageStatus::Ready
    );
}

/// A dead embedder is refused before the stage is touched.
///
/// Why: flipping `InProgress` and failing again on every attempt would
/// churn the stage the auto-retry reads.
/// Test: this test.
#[tokio::test]
#[serial_test::serial]
async fn a_dead_embedder_is_refused_before_the_stage_is_touched() {
    let (handle, _embedder, store, _n, committed) = failed_mid_stage("embed-missing-dead").await;
    let before = handle.stages.read().await.semantic.clone();

    let refusal = resume_semantic(&handle, false).await.expect_err("down");
    assert!(
        matches!(refusal, ResumeRefusal::EmbedderDown(_)),
        "{refusal:?}"
    );
    let after = handle.stages.read().await.semantic.clone();
    assert_eq!(after.status, StageStatus::Failed);
    assert_eq!(after.failure, before.failure, "stage untouched");
    assert_eq!(store.len().await.expect("len"), committed);
}

/// A dry run measures the gap and queues nothing.
/// Test: this test.
#[tokio::test]
#[serial_test::serial]
async fn a_dry_run_reports_the_gap_and_queues_nothing() {
    let (handle, embedder, _store, n, committed) = failed_mid_stage("embed-missing-dry").await;
    embedder.heal();

    let report = resume_semantic(&handle, true).await.expect("dry run");
    assert_eq!(report.action, ResumeAction::WouldQueue);
    assert_eq!(
        (report.chunks, report.missing),
        (Some(n), Some(n - committed))
    );
    assert_eq!(
        handle.stages.read().await.semantic.status,
        StageStatus::Failed
    );
    assert!(embedder.healed_chunk_texts().is_empty(), "nothing embedded");
}

/// A `Failed` stage over a fully covered store settles `Ready` with no embed.
///
/// Why: "the index leaves degraded when the missing count reaches 0" also
/// holds when the gap was closed some other way, such as a reindex.
/// Test: this test.
#[tokio::test]
#[serial_test::serial]
async fn a_failed_stage_with_every_vector_present_is_settled_ready() {
    let (handle, embedder, store, n, _) = failed_mid_stage("embed-missing-settle").await;
    for i in 0..n {
        store
            .upsert(
                &format!("m{i}.rs:1:1"),
                vec![0.5; crate::service::embed_missing_test_support::DIM],
            )
            .await
            .expect("seed vector");
    }
    let calls = embedder.calls.load(Ordering::SeqCst);

    let report = resume_semantic(&handle, false).await.expect("settle");
    assert_eq!(report.action, ResumeAction::Settled);
    assert_eq!(
        embedder.calls.load(Ordering::SeqCst),
        calls,
        "no embed call"
    );
    let semantic = handle.stages.read().await.semantic.clone();
    assert_eq!(semantic.status, StageStatus::Ready, "{semantic:?}");
}

/// A handle with the vector lane off is not resumable.
/// Test: this test.
#[tokio::test]
#[serial_test::parallel]
async fn a_disabled_vector_lane_is_not_applicable() {
    let indexer = crate::core::indexer::CodeIndexer::new("embed-missing-off", "/tmp/em-off");
    let handle = Arc::new(IndexHandle {
        skip_vector: true,
        ..IndexHandle::bare(
            crate::core::registry::IndexId::new("embed-missing-off"),
            Arc::new(tokio::sync::RwLock::new(indexer)),
            std::path::PathBuf::from("/tmp/em-off"),
        )
    });
    let refusal = resume_semantic(&handle, false).await.expect_err("off");
    assert!(
        matches!(refusal, ResumeRefusal::NotApplicable(_)),
        "{refusal:?}"
    );
}
