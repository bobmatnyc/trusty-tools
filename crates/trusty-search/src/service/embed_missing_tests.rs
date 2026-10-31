//! Tests for the #10551 embed-missing resume.
//!
//! Why: the closure conditions are behavioural: only the missing chunks are
//! embedded, the stage leaves `Failed`, a repeat run is a no-op, and a dead
//! embedder never touches the stage.
//! What: every case starts from an index whose embedder died mid-stage
//! (`embed_missing_support_tests::failed_mid_stage`), the failure the issue saw.
//! Test: this file.

use super::*;
use crate::service::embed_missing_support_tests::{failed_mid_stage, wait_for_stage};

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
    let (handle, embedder, store, n, committed, _dir) =
        failed_mid_stage("embed-missing-resume").await;
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
    let (handle, embedder, _store, _n, _, _dir) = failed_mid_stage("embed-missing-repeat").await;
    embedder.heal();
    resume_semantic(&handle, false).await.expect("resume");
    wait_for_stage(&handle, StageStatus::Ready).await;
    let embedded = embedder.healed_chunk_texts().len();

    let again = resume_semantic(&handle, false).await.expect("repeat");
    assert_eq!(again.action, ResumeAction::Noop);
    assert_eq!(again.missing, Some(0));
    // The probe is one call; no chunk text is embedded again.
    assert_eq!(embedder.healed_chunk_texts().len(), embedded, "no chunk");
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
    let (handle, _embedder, store, _n, committed, _dir) =
        failed_mid_stage("embed-missing-dead").await;
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
    let (handle, embedder, _store, n, committed, _dir) =
        failed_mid_stage("embed-missing-dry").await;
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
    let (handle, embedder, store, n, _, _dir) = failed_mid_stage("embed-missing-settle").await;
    for i in 0..n {
        store
            .upsert(
                &format!("m{i}.rs:1:1"),
                vec![0.5; crate::service::embed_missing_support_tests::DIM],
            )
            .await
            .expect("seed vector");
    }
    // The probe runs before any scan, so a settle needs a live embedder.
    embedder.heal();

    let report = resume_semantic(&handle, false).await.expect("settle");
    assert_eq!(report.action, ResumeAction::Settled);
    assert!(
        embedder.healed_chunk_texts().is_empty(),
        "no chunk embedded"
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

/// An embedder whose probe flips the stage to `Skipped`, as a concurrent
/// `PATCH {vector:false}` does during the resume's window.
struct SkipDuringProbe {
    inner: crate::core::embed::MockEmbedder,
    stages: std::sync::Mutex<Option<Arc<tokio::sync::RwLock<crate::core::registry::IndexStages>>>>,
}

#[async_trait::async_trait]
impl crate::core::embed::Embedder for SkipDuringProbe {
    async fn embed(&self, text: &str) -> anyhow::Result<Vec<f32>> {
        self.inner.embed(text).await
    }
    async fn embed_batch(&self, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
        let stages = self.stages.lock().expect("hook lock").clone();
        if let Some(stages) = stages {
            stages.write().await.semantic.status = StageStatus::Skipped;
        }
        self.inner.embed_batch(texts).await
    }
    fn dimension(&self) -> usize {
        self.inner.dimension()
    }
}

/// A stage set `Skipped` while the resume probes is not overwritten.
///
/// Why: the status is read before the probe, which can take 30 s. Pre-fix the
/// write lock only re-checked `InProgress` and then set `InProgress`
/// unconditionally, undoing a `PATCH {vector:false}` that landed in between.
/// What: the embedder's probe flips the stage to `Skipped`. The call is
/// refused, the stage stays `Skipped`, nothing is queued or embedded.
/// Test: this test.
#[tokio::test]
#[serial_test::serial]
async fn a_skipped_stage_set_during_the_window_is_not_overwritten() {
    use crate::service::embed_missing_support_tests::unembedded_handle;
    let hook = Arc::new(SkipDuringProbe {
        inner: crate::core::embed::MockEmbedder::new(8),
        stages: std::sync::Mutex::new(None),
    });
    let (handle, store, _dir) = unembedded_handle("embed-missing-race", 5, hook.clone()).await;
    handle.stages.write().await.semantic.status = StageStatus::Failed;
    *hook.stages.lock().expect("hook lock") = Some(Arc::clone(&handle.stages));

    let refusal = resume_semantic(&handle, false).await.expect_err("changed");
    assert!(matches!(refusal, ResumeRefusal::Busy(_)), "{refusal:?}");
    assert_eq!(
        handle.stages.read().await.semantic.status,
        StageStatus::Skipped,
        "the concurrent toggle must survive"
    );
    assert_eq!(store.len().await.expect("len"), 0, "nothing embedded");
    assert_eq!(crate::service::reindex::deferred_embed_queue_depth(), 0);
}

/// A second call while a pass runs reports `already_running` and queues nothing.
/// Test: this test.
#[tokio::test]
#[serial_test::serial]
async fn a_second_call_while_in_progress_reports_already_running() {
    use crate::service::embed_missing_support_tests::unembedded_handle;
    let embedder = Arc::new(crate::core::embed::MockEmbedder::new(8));
    let (handle, store, _dir) = unembedded_handle("embed-missing-running", 5, embedder).await;
    handle.stages.write().await.semantic.status = StageStatus::InProgress;

    let report = resume_semantic(&handle, false).await.expect("running");
    assert_eq!(report.action, ResumeAction::AlreadyRunning);
    assert_eq!(store.len().await.expect("len"), 0);
    assert_eq!(crate::service::reindex::deferred_embed_queue_depth(), 0);
}

/// An index whose per-index permit is held is refused as busy.
/// Test: this test.
#[tokio::test]
#[serial_test::serial]
async fn a_held_index_is_refused_as_busy() {
    use crate::service::embed_missing_support_tests::unembedded_handle;
    let embedder = Arc::new(crate::core::embed::MockEmbedder::new(8));
    let (handle, store, _dir) = unembedded_handle("embed-missing-busy", 5, embedder).await;
    handle.stages.write().await.semantic.status = StageStatus::Failed;
    let _permit = crate::service::reindex::index_semaphore(&handle.id)
        .try_acquire_owned()
        .expect("the permit is free");

    let refusal = resume_semantic(&handle, false).await.expect_err("held");
    assert!(matches!(refusal, ResumeRefusal::Busy(_)), "{refusal:?}");
    assert_eq!(store.len().await.expect("len"), 0);
}

/// A `Skipped` stage is not resumable.
/// Test: this test.
#[tokio::test]
#[serial_test::serial]
async fn a_skipped_stage_is_not_applicable() {
    use crate::service::embed_missing_support_tests::unembedded_handle;
    let embedder = Arc::new(crate::core::embed::MockEmbedder::new(8));
    let (handle, _store, _dir) = unembedded_handle("embed-missing-skipped", 5, embedder).await;
    handle.stages.write().await.semantic.status = StageStatus::Skipped;
    let refusal = resume_semantic(&handle, false).await.expect_err("skipped");
    assert!(
        matches!(refusal, ResumeRefusal::NotApplicable(_)),
        "{refusal:?}"
    );
}
