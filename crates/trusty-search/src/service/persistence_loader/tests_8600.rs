//! #8600: warm-boot waits out a corpus lock the previous process still holds.
//!
//! Why: one 50 ms retry lost the corpus to a full cold start whenever the lock
//! outlived it.
//! What: holds the redb file open, releases it after the first retry would have
//! given up, and asserts the open still succeeds.
//! Test: this file.

use super::*;
use std::time::Duration;

/// Test: this IS the test.
#[tokio::test(flavor = "multi_thread")]
async fn a_lock_released_after_the_first_retry_still_opens_the_corpus() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("index.redb");
    let holder = CorpusStore::open(&path).expect("first open");
    let release = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(400)).await;
        drop(holder);
    });

    let opened = open_corpus_with_retry_within(&path, Duration::from_secs(5)).await;
    release.await.expect("release task");
    assert!(
        opened.is_ok(),
        "a lock released within the retry budget must not cost a cold start: {:?}",
        opened.err()
    );
}

/// A lock that is never released still fails, once the budget is spent.
/// Test: this IS the test.
#[tokio::test(flavor = "multi_thread")]
async fn a_lock_held_past_the_budget_still_fails() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("index.redb");
    let _holder = CorpusStore::open(&path).expect("first open");
    let opened = open_corpus_with_retry_within(&path, Duration::from_millis(300)).await;
    assert!(opened.is_err(), "the retry is bounded");
}
