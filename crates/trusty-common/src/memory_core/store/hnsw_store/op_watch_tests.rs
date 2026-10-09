//! Tests for the #9487 in-flight op registry.

use std::sync::Arc;
use std::time::{Duration, Instant};

use super::{HnswOpKind, OpPark, OpWatch};
use crate::memory_core::store::hnsw_store::HnswStore;
use crate::memory_core::store::vector::{UsearchStore, VectorStore};

/// Upper bound on any wait for a parked call to arrive; never a sync sleep.
const ENTER_BOUND: Duration = Duration::from_secs(30);

fn open_store(dim: usize) -> (tempfile::TempDir, HnswStore) {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = Arc::new(redb::Database::create(dir.path().join("hnsw.redb")).expect("create db"));
    (dir, HnswStore::open(db, dim).expect("open store"))
}

/// Why (#9487): a search blocked inside the graph must be visible to health
/// for as long as it is blocked, and must stop being visible once it returns.
/// What: parks a real `search` on another thread, reads `oldest_op` while it is
/// held (aged against an injected `now`), releases it, and reads again.
/// Test: this test.
#[test]
fn a_parked_search_is_reported_until_it_returns() {
    let (_dir, store) = open_store(4);
    store.upsert("a", &[1.0, 0.0, 0.0, 0.0]).expect("seed");
    assert_eq!(store.oldest_op(), None, "nothing runs before the search");
    let park = OpPark::new();
    store.set_op_park(Some(Arc::clone(&park)));

    std::thread::scope(|s| {
        let search = s.spawn(|| store.search(&[1.0, 0.0, 0.0, 0.0], 1));
        let entered = park.wait_entered(ENTER_BOUND);
        let op = store.oldest_op();
        // Released before any assert, so a failure cannot hang the scope join.
        park.release();
        let hits = search.join().expect("search thread").expect("search");
        assert!(entered, "the search never parked");

        let op = op.expect("a parked search is in flight");
        assert_eq!(op.kind, HnswOpKind::Search);
        let threshold = Duration::from_millis(50);
        let later = Instant::now() + threshold * 2;
        assert!(
            later.saturating_duration_since(op.since) > threshold,
            "a parked op ages past the threshold"
        );
        assert_eq!(hits.len(), 1);
    });
    assert_eq!(store.oldest_op(), None, "a returned search is unregistered");
}

/// Why (#9487): the incident shape. The pipeline timeout drops the awaiting
/// future, but the `spawn_blocking` thread keeps running; the registry must
/// still report it.
/// What: parks a `UsearchStore::search`, aborts the task awaiting it, and
/// asserts the op is still registered; then releases the park.
/// Test: this test.
#[tokio::test]
async fn a_dropped_future_leaves_its_blocking_op_registered() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Arc::new(UsearchStore::new(dir.path().join("idx.usearch"), 4).expect("store"));
    let park = OpPark::new();
    store.set_hnsw_op_park(Some(Arc::clone(&park)));

    let task = {
        let store = Arc::clone(&store);
        tokio::spawn(async move { store.search(&[1.0, 0.0, 0.0, 0.0], 1).await })
    };
    let waiter = Arc::clone(&park);
    let entered = tokio::task::spawn_blocking(move || waiter.wait_entered(ENTER_BOUND))
        .await
        .expect("wait task");
    assert!(entered, "the search never parked");

    task.abort();
    assert!(
        task.await.expect_err("aborted").is_cancelled(),
        "the awaiting future was dropped"
    );
    let op = store.oldest_hnsw_op();
    park.release();
    assert_eq!(
        op.map(|o| o.kind),
        Some(HnswOpKind::Search),
        "the blocking op outlives the dropped future"
    );
}

/// Why: a call that panics inside the graph must not leave a phantom op that
/// reads as a permanent wedge.
/// What: registers an op inside `catch_unwind`, panics, and asserts the
/// registry is empty afterwards.
/// Test: this test.
#[test]
fn a_panicking_op_unregisters_on_unwind() {
    let watch = OpWatch::default();
    let mut during = None;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _op = watch.begin(HnswOpKind::Upsert);
        during = watch.oldest();
        panic!("graph insert panicked");
    }));
    assert!(result.is_err());
    assert_eq!(
        during.map(|o| o.kind),
        Some(HnswOpKind::Upsert),
        "the op was registered before the panic"
    );
    assert_eq!(watch.oldest(), None, "unwind removed the op");
}
