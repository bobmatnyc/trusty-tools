//! Tests for the #10551 auto-retry of a failed semantic stage.
//!
//! Why: the closure condition is "auto-retry with backoff runs after the
//! embedder recovers". The sweep takes its clock as an argument, so each case
//! drives it without sleeping.
//! What: the backoff curve, one retry that waits out the embedder's recovery,
//! the cap on resumes that keep failing, and a down embedder that is probed
//! forever without touching the stage.
//! Test: this file.

use std::sync::atomic::Ordering;

use super::*;
use crate::service::embed_missing_support_tests::{failed_mid_stage, wait_for_stage};

/// Test: this test.
#[test]
#[serial_test::parallel]
fn backoff_doubles_from_the_base_and_stops_at_the_cap() {
    assert_eq!(backoff_delay(0), Duration::from_secs(30));
    assert_eq!(backoff_delay(1), Duration::from_secs(60));
    assert_eq!(backoff_delay(4), Duration::from_secs(480));
    assert_eq!(backoff_delay(5), Duration::from_secs(900));
    assert_eq!(backoff_delay(500), Duration::from_secs(900), "no overflow");
}

/// The embedder stays down across two due sweeps, recovers, and the next due
/// sweep resumes the stage to `Ready`.
///
/// Why: the issue's case. The child restarts by itself; nothing re-ran the
/// stage.
/// What: the first sighting schedules, not retries. A due sweep against a dead
/// embedder leaves the stage `Failed` and lengthens the delay. After the heal
/// the next due sweep queues the pass; a later sweep over the `Ready` stage
/// forgets the index.
/// Test: this test.
#[tokio::test]
#[serial_test::serial]
async fn a_failed_stage_is_retried_after_its_backoff_and_settles() {
    let (handle, embedder, store, n, _, _dir) = failed_mid_stage("embed-retry-recovers").await;
    let handles = vec![Arc::clone(&handle)];
    let mut book = RetryBook::default();
    let t0 = Instant::now();

    let calls = embedder.calls.load(Ordering::SeqCst);
    retry_sweep(&handles, &mut book, t0).await;
    assert!(book.tracks("embed-retry-recovers"));
    retry_sweep(&handles, &mut book, t0 + Duration::from_secs(10)).await;
    assert_eq!(
        embedder.calls.load(Ordering::SeqCst),
        calls,
        "not due: no probe"
    );

    retry_sweep(&handles, &mut book, t0 + Duration::from_secs(31)).await;
    assert_eq!(
        handle.stages.read().await.semantic.status,
        StageStatus::Failed,
        "a dead embedder leaves the stage alone"
    );
    assert_eq!(book.resumes("embed-retry-recovers"), Some(0));

    embedder.heal();
    // The failed probe set the next attempt 60 s after t0 + 31 s.
    retry_sweep(&handles, &mut book, t0 + Duration::from_secs(31 + 61)).await;
    assert_eq!(book.resumes("embed-retry-recovers"), Some(1));
    wait_for_stage(&handle, StageStatus::Ready).await;
    assert_eq!(store.len().await.expect("len"), n);

    retry_sweep(&handles, &mut book, t0 + Duration::from_secs(200)).await;
    assert!(
        !book.tracks("embed-retry-recovers"),
        "settled index forgotten"
    );
}

/// A stage that keeps failing under a healthy embedder is retried
/// [`MAX_RESUMES`] times, then left alone; a dead embedder is probed forever.
///
/// Why: repeated failure under a healthy probe is not an embedder fault, and
/// a bounded loop stops it from re-queuing a doomed pass for ever. A dead
/// embedder costs one tiny probe per cap interval, so it never gives up.
/// Test: this test.
#[tokio::test]
#[serial_test::serial]
async fn repeated_failed_resumes_stop_at_the_cap_but_a_dead_embedder_is_probed_forever() {
    let (handle, embedder, store, _n, committed, _dir) = failed_mid_stage("embed-retry-cap").await;
    let handles = vec![Arc::clone(&handle)];
    let id = "embed-retry-cap".to_string();
    let mut book = RetryBook::default();
    let t0 = Instant::now();

    // Dead embedder: twenty cap-spaced sweeps, still tracked, still no resume.
    for i in 0..20u64 {
        retry_sweep(&handles, &mut book, t0 + MAX_DELAY * (i as u32 + 1)).await;
    }
    assert_eq!(book.resumes(&id), Some(0));
    assert_eq!(
        handle.stages.read().await.semantic.status,
        StageStatus::Failed
    );

    // Healthy embedder, but the resume budget is spent.
    embedder.heal();
    book.entries.get_mut(&id).expect("tracked").resumes = MAX_RESUMES;
    book.entries.get_mut(&id).expect("tracked").next_at = t0;
    retry_sweep(&handles, &mut book, t0 + MAX_DELAY * 100).await;
    assert_eq!(
        handle.stages.read().await.semantic.status,
        StageStatus::Failed
    );
    assert_eq!(
        store.len().await.expect("len"),
        committed,
        "nothing embedded"
    );
}

/// A deleted index leaves the book, so a recreated id starts with a fresh budget.
/// Test: this test.
#[tokio::test]
#[serial_test::serial]
async fn a_deleted_index_is_forgotten_and_a_recreated_id_starts_fresh() {
    let (handle, _embedder, _store, _n, _, _dir) = failed_mid_stage("embed-retry-gone").await;
    let mut book = RetryBook::default();
    let t0 = Instant::now();
    retry_sweep(&[Arc::clone(&handle)], &mut book, t0).await;
    book.entries
        .get_mut("embed-retry-gone")
        .expect("tracked")
        .resumes = MAX_RESUMES;

    retry_sweep(&[], &mut book, t0 + Duration::from_secs(1)).await;
    assert!(!book.tracks("embed-retry-gone"), "deleted index forgotten");

    retry_sweep(&[handle], &mut book, t0 + Duration::from_secs(2)).await;
    assert_eq!(book.resumes("embed-retry-gone"), Some(0), "fresh budget");
}

/// Nothing to embed on a `Failed` stage is terminal, not a probe failure.
///
/// Why: a zero-chunk corpus, or one whose every gap is a refusal, measures the
/// same on every sweep; counting it a probe failure re-scanned it for ever.
/// Test: this test.
#[tokio::test]
#[serial_test::serial]
async fn nothing_to_embed_on_a_failed_stage_is_terminal() {
    use crate::service::embed_missing_support_tests::unembedded_handle;
    let embedder = Arc::new(crate::core::embed::MockEmbedder::new(8));
    let (handle, _store, _dir) = unembedded_handle("embed-retry-empty", 0, embedder).await;
    handle.stages.write().await.semantic.status = StageStatus::Failed;
    let mut book = RetryBook::default();
    let t0 = Instant::now();

    retry_sweep(&[Arc::clone(&handle)], &mut book, t0).await;
    retry_sweep(&[handle], &mut book, t0 + Duration::from_secs(31)).await;
    assert_eq!(book.resumes("embed-retry-empty"), Some(MAX_RESUMES));
}
