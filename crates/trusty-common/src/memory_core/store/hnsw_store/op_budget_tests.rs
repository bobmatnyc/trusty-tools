//! #9487 AC4: the HNSW op budget gives up, and the breaker it trips holds.
//!
//! Why: before AC4 an upsert queued behind a stuck graph lock waited forever,
//! and so did every call after it. These tests park the lock from the test,
//! so the stall is deterministic rather than a race.
//! What: each test shrinks the store's budget through its own breaker (no
//! process-global env mutation), and wraps every await that can hang in a
//! watchdog, so a regression fails the test instead of hanging the suite.
//! Test: this file is the test.

use std::time::{Duration, Instant};

use uuid::Uuid;

use super::{DEFAULT_OP_BUDGET, OpBudgetError, budget_from, op_budget_error};
use crate::memory_core::store::vector::{UsearchStore, VectorStore};

const DIM: usize = 16;
/// Small enough to reach in a test, large enough to be unambiguous.
const SMALL: Duration = Duration::from_millis(300);
/// Bound on any await that could hang if the budget were not enforced.
const WATCHDOG: Duration = Duration::from_secs(20);

fn unit_vec(seed: u32) -> Vec<f32> {
    let raw: Vec<f32> = (0..DIM)
        .map(|i| ((i as u32 + seed) % 7) as f32 + 1.0)
        .collect();
    let norm: f32 = raw.iter().map(|v| v * v).sum::<f32>().sqrt();
    raw.into_iter().map(|v| v / norm).collect()
}

fn open_store() -> (tempfile::TempDir, UsearchStore) {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = UsearchStore::new(dir.path().join("idx.usearch"), DIM).expect("open store");
    store.op_breaker().set_budget_for_test(SMALL);
    (dir, store)
}

/// Park the graph lock and run one upsert under the watchdog; the guard drops
/// with the future, so a hung upsert is released before the test panics.
async fn upsert_behind_parked_graph(store: &UsearchStore) -> (anyhow::Result<()>, Duration) {
    let attempt = async {
        let _parked = store.hnsw_for_test().park_graph_for_test();
        let started = Instant::now();
        let outcome = store.upsert(Uuid::new_v4(), unit_vec(1)).await;
        (outcome, started.elapsed())
    };
    tokio::time::timeout(WATCHDOG, attempt)
        .await
        .expect("an upsert behind a parked graph lock must give up at its budget, not hang")
}

/// Why (#9487 AC4a): a stuck graph lock must cost the caller one budget, not
/// forever. On the pre-AC4 code this upsert blocked until the watchdog fired.
/// What: holds the graph lock exclusively — a new shared guard waits exactly
/// as it did behind #9487's queued writer — and asserts the upsert returns
/// the typed `BudgetExceeded` within a few budgets, and trips the breaker.
/// Test: this test.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_upsert_behind_a_parked_graph_lock_exceeds_its_budget() {
    let (_dir, store) = open_store();

    let (outcome, elapsed) = upsert_behind_parked_graph(&store).await;

    let err = outcome.expect_err("an upsert that cannot take the graph lock must fail");
    assert!(
        matches!(
            op_budget_error(&err),
            Some(OpBudgetError::BudgetExceeded { op: "upsert", .. })
        ),
        "expected BudgetExceeded for upsert, got {err:#}"
    );
    assert!(
        elapsed < SMALL * 10,
        "gave up after {elapsed:?}, budget {SMALL:?}"
    );
    assert!(store.op_breaker().is_tripped(), "the breaker must trip");
    assert!(store.op_breaker().abandoned_ops() >= 1);
}

/// Why (#9487 AC4b): once a store has wedged, retrying it must not stack up
/// another blocked thread per call. Timing alone cannot prove that, so the
/// test reads the spawn counter.
/// What: trips the breaker with a real budget overrun, releases the lock, and
/// asserts the next upsert, search and remove each return `Wedged` at once
/// with `spawned_ops` unchanged.
/// Test: this test.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_tripped_store_refuses_the_next_call_without_spawning() {
    let (_dir, store) = open_store();
    let (first, _) = upsert_behind_parked_graph(&store).await;
    first.expect_err("the first upsert must exceed its budget");
    let spawned = store.op_breaker().spawned_ops();

    let started = Instant::now();
    let calls = async {
        let upsert = store.upsert(Uuid::new_v4(), unit_vec(2)).await;
        let search = store.search(&unit_vec(2), 5).await.map(|_| ());
        let remove = store.remove(Uuid::new_v4()).await;
        [upsert, search, remove]
    };
    let outcomes = tokio::time::timeout(WATCHDOG, calls)
        .await
        .expect("a tripped store must refuse at once, not wait");

    for outcome in outcomes {
        let err = outcome.expect_err("a tripped store must refuse every operation");
        assert!(
            matches!(op_budget_error(&err), Some(OpBudgetError::Wedged { .. })),
            "expected Wedged, got {err:#}"
        );
    }
    assert_eq!(
        store.op_breaker().spawned_ops(),
        spawned,
        "a refused operation must spawn no blocking task"
    );
    assert!(
        started.elapsed() < SMALL,
        "refusals took {:?}; they must not wait on a lock",
        started.elapsed()
    );
}

/// Why (#9487): a typo in `TRUSTY_HNSW_OP_BUDGET_SECS` must not panic the
/// daemon or disable the budget.
/// What: feeds the pure parser absent, valid, zero and malformed values.
/// Test: this test.
#[test]
fn a_malformed_budget_falls_back_to_the_default() {
    assert_eq!(budget_from(None), DEFAULT_OP_BUDGET);
    assert_eq!(budget_from(Some("45")), Duration::from_secs(45));
    assert_eq!(budget_from(Some(" 7 ")), Duration::from_secs(7));
    for bad in ["0", "", "-3", "thirty", "1.5"] {
        assert_eq!(budget_from(Some(bad)), DEFAULT_OP_BUDGET, "value {bad:?}");
    }
}
