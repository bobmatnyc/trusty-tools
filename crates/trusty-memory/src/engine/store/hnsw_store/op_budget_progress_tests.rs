//! #9728: a group write's budget is a stall watchdog, not a wall clock.
//!
//! Why: a legal 4096-chunk `upsert_chunks` indexes one point after another
//! and can outlast the op budget while making steady progress. The wall-clock
//! budget tripped the sticky breaker on it and wedged the palace.
//! What: each test shrinks its store's budget and paces the group's index
//! loop through the `index_pace` seam (a per-point delay, or a park at one
//! point), so slowness and stalls are deterministic and cost milliseconds.
//! Every await that can hang runs under a watchdog.
//! Test: this file is the test.

use std::time::{Duration, Instant};

use uuid::Uuid;

use super::{OpBudgetError, op_budget_error};
use crate::engine::store::hnsw_store::groups::index_pace;
use crate::engine::store::vector::{UsearchStore, VectorStore};

const DIM: usize = 16;
/// The budget every test but the at-cap one runs under.
const SMALL: Duration = Duration::from_millis(300);
/// Bound on any await that could hang if the code under test were wrong.
const WATCHDOG: Duration = Duration::from_secs(30);

/// `n` distinct, non-degenerate `dim`-wide vectors.
fn spread(n: usize, dim: usize, salt: usize) -> Vec<Vec<f32>> {
    (0..n)
        .map(|i| {
            (0..dim)
                .map(|j| (((i + salt * 7919) * dim + j) as f32 * 0.618_034 + 0.5).sin())
                .collect()
        })
        .collect()
}

fn open_store(dim: usize) -> (tempfile::TempDir, UsearchStore) {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = UsearchStore::new(dir.path().join("idx.usearch"), dim).expect("open store");
    (dir, store)
}

fn small_store() -> (tempfile::TempDir, UsearchStore) {
    let (dir, store) = open_store(DIM);
    store.op_breaker().set_budget_for_test(SMALL);
    (dir, store)
}

/// Points in the in-memory graph.
fn graph_points(store: &UsearchStore) -> usize {
    store.hnsw_for_test().index.read().get_nb_point()
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

/// Why (#9728): the defect. A group that indexes one point every 5 ms keeps
/// making progress, so it must complete although its total time is three
/// budgets.
/// What: 200 chunks at 5 ms a point under a 300 ms budget. Asserts `Ok`, an
/// untripped breaker, no abandoned op and every chunk in the graph, and that
/// the call really did outlast the budget. A raised default budget does not
/// pass it (the test sets its own), nor a lower chunk cap (200 is under any
/// cap), nor a non-sticky per-op failure (it returns `Err`).
/// Test: this test.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_legal_group_slower_than_the_budget_completes_without_tripping() {
    let (_dir, store) = small_store();
    let id = Uuid::new_v4();
    let _pace = index_pace::arm(&id.to_string(), Duration::from_millis(5), None);

    let started = Instant::now();
    let outcome = tokio::time::timeout(WATCHDOG, store.upsert_chunks(id, spread(200, DIM, 1)))
        .await
        .expect("a progressing group must finish, not hang");
    let elapsed = started.elapsed();

    outcome.expect("a group that keeps making progress must complete");
    assert!(
        elapsed > SMALL,
        "the group took {elapsed:?}; it must outlast the {SMALL:?} budget to prove anything"
    );
    let breaker = store.op_breaker();
    assert!(!breaker.is_tripped(), "a progressing group must not trip");
    assert_eq!(breaker.abandoned_ops(), 0);
    assert_eq!(graph_points(&store), 200, "every chunk must be indexed");
    let shapes = store.chunk_shapes().expect("an untripped store answers");
    assert_eq!(shapes[&id].rows, 200);
}

/// Why (#9728): health reads `oldest_op` and reports an op older than its
/// wedge line (twice the budget by default) as a wedged `hnsw` lock. A group
/// that keeps making progress may legally outlast the budget, so its op's age
/// must follow progress, not the call's start.
/// What: 200 chunks at 5 ms a point under a 300 ms budget; samples the op's
/// age every 5 ms while the group runs. Asserts the group outlasted the
/// budget, the op was seen, and no sampled age reached one budget.
/// Test: this test.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_progressing_group_keeps_its_op_age_under_one_budget() {
    let (_dir, store) = small_store();
    let id = Uuid::new_v4();
    let _pace = index_pace::arm(&id.to_string(), Duration::from_millis(5), None);

    let started = Instant::now();
    let write = store.upsert_chunks(id, spread(200, DIM, 7));
    tokio::pin!(write);
    let mut oldest_age = Duration::ZERO;
    let mut samples = 0usize;
    let sampled = async {
        loop {
            tokio::select! {
                outcome = &mut write => break outcome,
                () = tokio::time::sleep(Duration::from_millis(5)) => {
                    if let Some(op) = store.hnsw_for_test().oldest_op() {
                        oldest_age = oldest_age.max(op.since.elapsed());
                        samples += 1;
                    }
                }
            }
        }
    };
    let outcome = tokio::time::timeout(WATCHDOG, sampled)
        .await
        .expect("a progressing group must finish, not hang");
    let elapsed = started.elapsed();

    outcome.expect("a group that keeps making progress must complete");
    assert!(
        elapsed > SMALL,
        "the group took {elapsed:?}; it must outlast the {SMALL:?} budget to prove anything"
    );
    assert!(samples > 0, "the running op was never observed");
    assert!(
        oldest_age < SMALL,
        "a progressing op aged {oldest_age:?} over {samples} samples; health must see \
         under one {SMALL:?} budget"
    );
}

/// Why (#9728 guard): the watchdog must still catch a real stall inside a
/// large group within about one budget. A budget scaled by the chunk count
/// would hold the thread and `write_mutex` for many budgets first.
/// What: a 2000-chunk group parks before point 50. Asserts `BudgetExceeded`
/// for `upsert`, a tripped breaker, and that the trip came within three
/// budgets of the park; a budget scaled by 2000 chunks cannot meet that.
/// Test: this test.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stall_inside_a_large_group_trips_one_budget_after_the_stall() {
    let (_dir, store) = small_store();
    let id = Uuid::new_v4();
    let parked = index_pace::arm(&id.to_string(), Duration::ZERO, Some(50));
    let reached = parked.reached;

    let write = async {
        let outcome = store.upsert_chunks(id, spread(2000, DIM, 2)).await;
        (outcome, Instant::now())
    };
    // #9728: bounded inside the outer watchdog. A trip before the park point
    // ends the loop early and the sender in the static seam never drops, so
    // a bare `recv` would block this thread and the runtime's shutdown.
    let stall = tokio::task::spawn_blocking(move || {
        reached.recv_timeout(WATCHDOG / 2).map(|()| Instant::now())
    });
    let ((outcome, tripped_at), stalled_at) =
        tokio::time::timeout(WATCHDOG, async { tokio::join!(write, stall) })
            .await
            .expect("a stalled group must cost its caller about one budget, not hang");
    drop(parked.release);
    let stalled_at = stalled_at
        .expect("park watcher")
        .unwrap_or_else(|e| panic!("the loop never reached the park point ({e}); got {outcome:?}"));

    let err = outcome.expect_err("a stalled group must fail");
    assert!(
        matches!(
            op_budget_error(&err),
            Some(OpBudgetError::BudgetExceeded { op: "upsert", .. })
        ),
        "expected BudgetExceeded for upsert, got {err:#}"
    );
    assert!(store.op_breaker().is_tripped(), "a stall must trip");
    let waited = tripped_at.saturating_duration_since(stalled_at);
    assert!(
        waited < SMALL * 3,
        "tripped {waited:?} after the stall; the budget is {SMALL:?}"
    );
}

/// Why (#9728 Fail-Open Check): on the error arm a stalled group's redb rows
/// are committed but its tail never reached the graph. Nothing may treat the
/// group as finished: not search, and not chunk health, whose complete count
/// would let the backfill dequeue the drawer.
/// What: a 20-chunk group parks before point 5 and trips. Asserts exactly 5
/// points indexed; then search, upsert, remove, `upsert_chunks` and
/// `chunk_shapes` each refuse with `Wedged`; then, after the parked task is
/// released and ends, the breaker is still tripped and the tail still absent.
/// Test: this test.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stalled_group_trips_for_good_and_is_never_counted_complete() {
    let (_dir, store) = small_store();
    let id = Uuid::new_v4();
    let parked = index_pace::arm(&id.to_string(), Duration::ZERO, Some(5));

    let outcome = tokio::time::timeout(WATCHDOG, store.upsert_chunks(id, spread(20, DIM, 3)))
        .await
        .expect("a stalled group must give up at its budget");
    outcome.expect_err("a stalled group must fail");
    let breaker = store.op_breaker();
    assert!(breaker.is_tripped());
    assert_eq!(breaker.abandoned_ops(), 1, "the stalled op counts once");
    assert_eq!(
        graph_points(&store),
        5,
        "points before the park are indexed"
    );

    let wedged =
        |err: &anyhow::Error| matches!(op_budget_error(err), Some(OpBudgetError::Wedged { .. }));
    let shapes = store
        .chunk_shapes()
        .expect_err("chunk health must not count a tripped store's groups");
    assert!(wedged(&shapes), "expected Wedged, got {shapes:#}");
    let refusals = async {
        [
            store.search(&spread(1, DIM, 3)[0], 5).await.map(|_| ()),
            store
                .upsert(Uuid::new_v4(), spread(1, DIM, 4).remove(0))
                .await,
            store.remove(id).await,
            store.upsert_chunks(id, spread(2, DIM, 5)).await,
        ]
    };
    for refused in tokio::time::timeout(WATCHDOG, refusals)
        .await
        .expect("refusals")
    {
        let err = refused.expect_err("a tripped store must refuse");
        assert!(wedged(&err), "expected Wedged, got {err:#}");
    }

    let spawned = breaker.spawned_ops();
    drop(parked.release);
    tokio::time::sleep(SMALL).await;
    assert!(
        breaker.is_tripped(),
        "the trip is sticky after the stall ends"
    );
    assert_eq!(
        graph_points(&store),
        5,
        "the abandoned tail is never indexed"
    );
    assert_eq!(breaker.spawned_ops(), spawned);
}

/// Why (#9728 guard): touching the op on progress must not hide a stall.
/// Health has to keep seeing a stalled group's op age past its wedge line.
/// What: a 20-chunk group parks before point 5 and trips. Reads the parked
/// op, waits one more budget, and asserts its start never moved and its age
/// passed twice the budget, the default wedge line.
/// Test: this test.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stalled_group_op_age_keeps_growing_past_the_budget() {
    let (_dir, store) = small_store();
    let id = Uuid::new_v4();
    let parked = index_pace::arm(&id.to_string(), Duration::ZERO, Some(5));

    let outcome = tokio::time::timeout(WATCHDOG, store.upsert_chunks(id, spread(20, DIM, 8)))
        .await
        .expect("a stalled group must give up at its budget");
    outcome.expect_err("a stalled group must fail");
    let at_trip = store.hnsw_for_test().oldest_op();
    tokio::time::sleep(SMALL + Duration::from_millis(50)).await;
    let later = store.hnsw_for_test().oldest_op();
    drop(parked.release);

    let at_trip = at_trip.expect("the parked op stays registered after the trip");
    let later = later.expect("the parked op stays registered while it stalls");
    assert_eq!(later.since, at_trip.since, "a stalled op is never touched");
    let age = later.since.elapsed();
    assert!(
        age > SMALL * 2,
        "a stalled op aged only {age:?}; health must see it past twice the {SMALL:?} budget"
    );
}

/// Why (#9728 guard): a caller that drops its future (an HTTP disconnect,
/// the remember ceiling) must not stop a progressing group half-indexed, and
/// must not trip the breaker for it.
/// What: a 200-chunk group at 5 ms a point; the caller is dropped at half
/// the budget. Asserts every chunk reaches the graph, the breaker stays
/// untripped and nothing is abandoned.
/// Test: this test.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_dropped_caller_of_a_slow_group_leaves_every_chunk_indexed() {
    let (_dir, store) = small_store();
    let id = Uuid::new_v4();
    let _pace = index_pace::arm(&id.to_string(), Duration::from_millis(5), None);

    let gave_up =
        tokio::time::timeout(SMALL / 2, store.upsert_chunks(id, spread(200, DIM, 6))).await;
    assert!(
        gave_up.is_err(),
        "200 points at 5 ms cannot finish in 150 ms"
    );

    let indexed = eventually(WATCHDOG, || graph_points(&store) == 200).await;
    let breaker = store.op_breaker();
    assert!(
        indexed,
        "the detached group stopped at {} of 200 points (tripped: {})",
        graph_points(&store),
        breaker.is_tripped()
    );
    tokio::time::sleep(SMALL).await;
    assert!(!breaker.is_tripped(), "a progressing group must not trip");
    assert_eq!(breaker.abandoned_ops(), 0);
}

/// Why (#9728 AC): the production input itself, a 4096-chunk group at the
/// production dimension and the default budget, on a palace already holding
/// about 20K points (Architect ruling). Ignored: it costs minutes, and is the
/// only HNSW measurement at this size.
/// What: commits 5 x 4096 rows unindexed, reopens so replay builds the graph
/// in parallel, then times one `upsert_chunks` of 4096 chunks. Asserts `Ok`,
/// an untripped breaker and every point indexed; prints the elapsed time.
/// Test: this test.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "#9728: minutes of HNSW work at 384 dims; rung-5 --include-ignored only"]
async fn exactly_cap_group_completes_through_the_real_budget_path() {
    use crate::engine::store::vector_key::MAX_CHUNKS_PER_DRAWER;
    const PROD_DIM: usize = 384;
    const PRELOAD_GROUPS: usize = 5;
    let (dir, store) = open_store(PROD_DIM);
    for g in 0..PRELOAD_GROUPS {
        let rows = spread(MAX_CHUNKS_PER_DRAWER, PROD_DIM, 100 + g);
        store
            .hnsw_for_test()
            .commit_group_unindexed_for_test(&Uuid::new_v4().to_string(), &rows)
            .expect("preload rows");
    }
    drop(store);
    let reopened = UsearchStore::new(dir.path().join("idx.usearch"), PROD_DIM).expect("reopen");
    let preloaded = PRELOAD_GROUPS * MAX_CHUNKS_PER_DRAWER;
    assert_eq!(
        graph_points(&reopened),
        preloaded,
        "replay indexed the preload"
    );
    let budget = reopened.op_breaker().budget();

    let started = Instant::now();
    let outcome = reopened
        .upsert_chunks(Uuid::new_v4(), spread(MAX_CHUNKS_PER_DRAWER, PROD_DIM, 999))
        .await;
    let elapsed = started.elapsed();
    eprintln!(
        "#9728 at-cap: {MAX_CHUNKS_PER_DRAWER} chunks at dim {PROD_DIM} onto {preloaded} \
         points took {elapsed:?} (budget {budget:?})"
    );

    outcome.expect("a legal at-cap group must complete");
    assert!(!reopened.op_breaker().is_tripped());
    assert_eq!(graph_points(&reopened), preloaded + MAX_CHUNKS_PER_DRAWER);
}
