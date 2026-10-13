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

use super::{
    OpBudgetError, abandoned_ops_in_flight, budget_from, op_budget_error,
    op_budget_for_txn_deadline,
};
use crate::memory_core::store::hnsw_store::{HnswStore, HnswStoreError};
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
#[allow(clippy::await_holding_lock)] // #9487: parking the lock is the point
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
    // #9487 F2: the outer join and the inner lock bound both fire; one count.
    assert_eq!(store.op_breaker().abandoned_ops(), 1);
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

/// Poll `done` every 10 ms until it holds or `limit` passes.
async fn eventually(limit: Duration, done: impl Fn() -> bool) -> bool {
    let deadline = Instant::now() + limit;
    while Instant::now() < deadline {
        if done() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    done()
}

/// Why (#9487 F1): an HTTP client that disconnects, or the remember pipeline
/// ceiling, drops the caller's future. With the budget clock inside that
/// future, the stuck task was detached uncounted and the breaker never
/// tripped.
/// What: a `run_bounded` closure parks on a channel; the caller's future is
/// dropped at half the budget. The breaker must still trip at the budget and
/// count the operation exactly once, and the in-flight gauge must hold it.
/// Test: this test.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_dropped_caller_still_trips_the_breaker_at_the_budget() {
    let (_dir, store) = open_store();
    let breaker = store.op_breaker();
    let (release, parked) = std::sync::mpsc::channel::<()>();
    let caller = store.hnsw_for_test().run_bounded("upsert", move || {
        let _ = parked.recv();
        Ok(())
    });

    // The timeout drops the caller's future at half the budget.
    let gave_up = tokio::time::timeout(SMALL / 2, caller).await;
    assert!(gave_up.is_err(), "the closure is parked; it cannot finish");

    let tripped = eventually(SMALL * 10, || breaker.is_tripped()).await;
    assert!(
        tripped,
        "the breaker must trip at the budget even after the caller is dropped"
    );
    assert_eq!(breaker.abandoned_ops(), 1, "the abandoned op counts once");
    assert!(
        abandoned_ops_in_flight() >= 1,
        "the parked task is still running, so the gauge must hold it"
    );
    drop(release);
}

/// Why (#9487 F3): the join bound must hold on its own, for a closure that
/// hangs somewhere no inner lock bound reaches (inside `hnsw_rs`, or on
/// `begin_write`).
/// What: a `run_bounded` closure parks on a channel, with no store lock
/// involved, under a watchdog. It must return `BudgetExceeded` within a few
/// budgets and trip the breaker, counting the operation once.
/// Test: this test.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_hung_closure_exceeds_the_join_budget_without_any_lock() {
    let (_dir, store) = open_store();
    let (release, parked) = std::sync::mpsc::channel::<()>();
    let started = Instant::now();
    let attempt = store.hnsw_for_test().run_bounded("search", move || {
        let _ = parked.recv();
        Ok(())
    });
    let outcome = tokio::time::timeout(WATCHDOG, attempt)
        .await
        .expect("a hung closure must cost its caller one budget, not hang");
    let elapsed = started.elapsed();

    let err = outcome.expect_err("a closure that never returns must fail");
    assert!(
        matches!(
            op_budget_error(&err),
            Some(OpBudgetError::BudgetExceeded { op: "search", .. })
        ),
        "expected BudgetExceeded for search, got {err:#}"
    );
    assert!(elapsed < SMALL * 10, "gave up after {elapsed:?}");
    assert!(store.op_breaker().is_tripped(), "the breaker must trip");
    assert_eq!(store.op_breaker().abandoned_ops(), 1);
    assert!(abandoned_ops_in_flight() >= 1, "the parked task still runs");
    drop(release);
}

/// Run `op` on a scoped std thread while this thread holds `hnsw`'s graph lock.
/// At the watchdog the lock is released, so a hang fails the test instead of
/// blocking the suite.
fn direct_behind_parked_graph(
    hnsw: &HnswStore,
    op: impl FnOnce(&HnswStore) -> Result<(), HnswStoreError> + Send,
) -> Result<(), HnswStoreError> {
    let parked = hnsw.park_graph_for_test();
    std::thread::scope(|scope| {
        let worker = scope.spawn(|| op(hnsw));
        let deadline = Instant::now() + WATCHDOG;
        while !worker.is_finished() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        let hung = !worker.is_finished();
        drop(parked);
        let outcome = worker.join().expect("the direct operation panicked");
        assert!(
            !hung,
            "a direct HNSW operation behind a parked graph lock must give up at \
             its budget, not hang"
        );
        outcome
    })
}

/// Why (#9487 F3): the inner lock bounds must hold on their own, without
/// `run_bounded`'s join bound behind them.
/// What: calls the synchronous `HnswStore::upsert` and `search` directly, each
/// on a fresh store whose graph lock is parked, and asserts `BudgetExceeded`
/// for that op and a tripped breaker. An inner bound trips but never counts;
/// only the `run_bounded` watcher counts (#9487 F2).
/// Test: this test.
#[test]
fn direct_upsert_and_search_behind_a_parked_graph_lock_exceed_their_budget() {
    for op in ["upsert", "search"] {
        let (_dir, store) = open_store();
        let hnsw = store.hnsw_for_test();
        let outcome = direct_behind_parked_graph(hnsw, |h| match op {
            "upsert" => h
                .upsert(&Uuid::new_v4().to_string(), &unit_vec(3))
                .map(|_| ()),
            _ => h.search(&unit_vec(3), 5).map(|_| ()),
        });

        let err = outcome.expect_err("an operation behind a parked lock must fail");
        assert!(
            matches!(
                &err,
                HnswStoreError::OpBudget(OpBudgetError::BudgetExceeded { op: got, .. })
                    if *got == op
            ),
            "{op}: expected BudgetExceeded, got {err}"
        );
        assert!(
            hnsw.op_breaker().is_tripped(),
            "{op}: the breaker must trip"
        );
        assert_eq!(
            hnsw.op_breaker().abandoned_ops(),
            0,
            "{op}: inner bounds never count"
        );
    }
}

/// Why (#9487 F4): an upsert can wait out one deadlined redb transaction on
/// `begin_write` before its own runs. A budget equal to that deadline would
/// trip the sticky breaker on a healthy store.
/// What: the default is twice the write-transaction deadline it is given.
/// Test: this test.
#[test]
fn the_default_budget_outlasts_the_write_txn_deadline() {
    let txn = Duration::from_secs(30);
    let budget = op_budget_for_txn_deadline(txn);
    assert!(
        budget > txn,
        "budget {budget:?} must exceed the {txn:?} deadline"
    );
    assert_eq!(budget, Duration::from_secs(60));
}

/// Why (#9487): a typo in `TRUSTY_HNSW_OP_BUDGET_SECS` must not panic the
/// daemon or disable the budget.
/// What: feeds the pure parser absent, valid, zero and malformed values.
/// Test: this test.
#[test]
fn a_malformed_budget_falls_back_to_the_default() {
    let default = Duration::from_secs(60);
    assert_eq!(budget_from(None, default), default);
    assert_eq!(budget_from(Some("45"), default), Duration::from_secs(45));
    assert_eq!(budget_from(Some(" 7 "), default), Duration::from_secs(7));
    for bad in ["0", "", "-3", "thirty", "1.5"] {
        assert_eq!(budget_from(Some(bad), default), default, "value {bad:?}");
    }
}
