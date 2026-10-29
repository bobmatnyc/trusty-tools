//! #9728: a group write's budget is a stall watchdog, not a wall clock.
//!
//! Why: a legal 4096-chunk `upsert_chunks` indexes one point after another
//! and can outlast the op budget while making steady progress. The wall-clock
//! budget tripped the sticky breaker on it and wedged the palace.
//! What: each test shrinks its store's budget and runs on a runtime whose
//! clock starts paused (#10335). The watcher's clock moves only when a test
//! calls `advance`, so the commit, each indexed point and any host scheduling
//! delay cost no budget time. The `index_pace` seam stops the index loop
//! before each point (step mode) or at one point (a park), so a test sets
//! exactly how much budget time passes between progress ticks.
//! Test: this file is the test.

use std::future::Future;
use std::pin::Pin;
use std::sync::mpsc::RecvTimeoutError;
use std::task::Poll;
use std::time::Duration;

use tokio::time::Instant;
use uuid::Uuid;

use super::{OpBudgetError, op_budget_error};
use crate::engine::store::hnsw_store::groups::index_pace::{self, Steps};
use crate::engine::store::vector::{UsearchStore, VectorStore};

const DIM: usize = 16;
/// The budget every test but the at-cap one runs under.
const SMALL: Duration = Duration::from_millis(300);
/// Paused-clock time between two progress ticks in a progressing group.
const GAP: Duration = Duration::from_millis(100);
/// Points in a progressing group: `POINTS * GAP` is over three budgets.
const POINTS: usize = 10;
/// Real-time bound on any wait that could hang if the code under test were wrong.
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

/// Run `body` on a current-thread runtime whose clock starts paused.
///
/// Why (#10335): with a running clock, a host that descheduled the test for
/// one budget tripped a progressing group. Paused, the clock moves only on
/// `advance`; a blocking task holds off auto-advance while it runs.
/// What: runs `body` on its own thread and fails the test if it has not
/// finished within [`WATCHDOG`] of real time, so a hang is a failure.
fn paused<F, Fut>(body: F)
where
    F: FnOnce() -> Fut + Send + 'static,
    Fut: Future<Output = ()>,
{
    let (done, finished) = std::sync::mpsc::channel();
    let runner = std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .start_paused(true)
            .build()
            .expect("paused runtime");
        rt.block_on(body());
        let _ = done.send(());
    });
    match finished.recv_timeout(WATCHDOG) {
        Ok(()) => runner.join().expect("test body"),
        Err(RecvTimeoutError::Disconnected) => match runner.join() {
            Err(panic) => std::panic::resume_unwind(panic),
            Ok(()) => panic!("test body ended without finishing"),
        },
        Err(RecvTimeoutError::Timeout) => panic!("test body hung past {WATCHDOG:?}"),
    }
}

/// Poll `fut` once; the first poll starts a group write's blocking task.
async fn poll_once<F: Future + Unpin>(fut: &mut F) -> Poll<F::Output> {
    std::future::poll_fn(|cx| Poll::Ready(Pin::new(&mut *fut).poll(cx))).await
}

/// Wait (in real time) for the step-mode loop to stop before point `k`.
fn next_point(steps: &Steps, k: usize) {
    let got = steps
        .reached
        .recv_timeout(WATCHDOG / 2)
        .unwrap_or_else(|e| panic!("the index loop never reached point {k} ({e})"));
    assert_eq!(got, k, "points are indexed in order");
}

/// Let the waiting point run after `gap` of budget time has passed.
async fn release_after(steps: &Steps, gap: Duration) {
    tokio::time::advance(gap).await;
    steps.go.send(()).expect("the index loop waits for a go");
}

/// Let the runtime fire due timers and run the tasks they wake.
async fn settle() {
    for _ in 0..16 {
        tokio::task::yield_now().await;
    }
}

/// Poll `done` every 5 ms of real time until it holds or [`WATCHDOG`] passes.
fn until(done: impl Fn() -> bool) -> bool {
    let deadline = std::time::Instant::now() + WATCHDOG;
    while std::time::Instant::now() < deadline {
        if done() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    done()
}

/// Why (#9728): the defect. A group that ticks every `GAP` keeps making
/// progress, so it must complete although its total time is over three
/// budgets.
/// What: `POINTS` chunks, `GAP` of paused-clock time before each, under a
/// `SMALL` budget. Asserts `Ok`, an untripped breaker, no abandoned op and
/// every chunk in the graph, and that the call really did outlast the budget.
/// A clock measured from the call's start, not the last tick, trips at the
/// third point and fails it.
/// Test: this test.
#[test]
fn a_legal_group_slower_than_the_budget_completes_without_tripping() {
    paused(|| async {
        let (_dir, store) = small_store();
        let id = Uuid::new_v4();
        let steps = index_pace::arm_steps(&id.to_string());

        let started = Instant::now();
        let mut write = Box::pin(store.upsert_chunks(id, spread(POINTS, DIM, 1)));
        for k in 0..POINTS {
            if let Poll::Ready(outcome) = poll_once(&mut write).await {
                panic!("the group ended before point {k}: {outcome:?}");
            }
            next_point(&steps, k);
            release_after(&steps, GAP).await;
        }
        let outcome = write.await;
        let elapsed = started.elapsed();

        outcome.expect("a group that keeps making progress must complete");
        assert!(
            elapsed > SMALL * 3,
            "the group took {elapsed:?}; it must outlast the {SMALL:?} budget to prove anything"
        );
        let breaker = store.op_breaker();
        assert!(!breaker.is_tripped(), "a progressing group must not trip");
        assert_eq!(breaker.abandoned_ops(), 0);
        assert_eq!(graph_points(&store), POINTS, "every chunk must be indexed");
        let shapes = store.chunk_shapes().expect("an untripped store answers");
        assert_eq!(shapes[&id].rows, POINTS);
    });
}

/// Why (#9728): health reads `oldest_op` and reports an op older than its
/// wedge line as a wedged `hnsw` lock. A group that keeps making progress may
/// legally outlast the budget, so its op's age must follow progress, not the
/// call's start.
/// What: `POINTS` chunks, `GAP` of paused-clock time before each. At every
/// stop the op must be registered, stamped later than the stop before, and
/// not earlier than the previous point's release, so its age never covers
/// more than one point (#10335: an ordering, not a real-time age). Asserts
/// the group completed untripped after outlasting the budget.
/// Test: this test.
#[test]
fn a_progressing_group_keeps_its_op_age_under_one_budget() {
    paused(|| async {
        let (_dir, store) = small_store();
        let id = Uuid::new_v4();
        let steps = index_pace::arm_steps(&id.to_string());

        let started = Instant::now();
        let mut write = Box::pin(store.upsert_chunks(id, spread(POINTS, DIM, 7)));
        let mut stamps: Vec<std::time::Instant> = Vec::new();
        let mut released: Option<std::time::Instant> = None;
        for k in 0..POINTS {
            if let Poll::Ready(outcome) = poll_once(&mut write).await {
                panic!("the group ended before point {k}: {outcome:?}");
            }
            next_point(&steps, k);
            let op = store
                .hnsw_for_test()
                .oldest_op()
                .unwrap_or_else(|| panic!("the running op was not registered at point {k}"));
            if let (Some(prev), Some(go)) = (stamps.last(), released) {
                assert!(
                    op.since > *prev && op.since >= go,
                    "at point {k} the op's start did not move past point {}'s release; \
                     health would see it age across points",
                    k - 1
                );
            }
            stamps.push(op.since);
            released = Some(std::time::Instant::now());
            release_after(&steps, GAP).await;
        }
        let outcome = write.await;
        let elapsed = started.elapsed();

        outcome.expect("a group that keeps making progress must complete");
        assert!(
            elapsed > SMALL * 3,
            "the group took {elapsed:?}; it must outlast the {SMALL:?} budget to prove anything"
        );
        assert_eq!(stamps.len(), POINTS, "the op was seen at every point");
        assert!(store.hnsw_for_test().oldest_op().is_none(), "the op ended");
    });
}

/// Why (#9728 guard): the watchdog must still catch a real stall inside a
/// large group within one budget. A budget scaled by the chunk count would
/// hold the thread and `write_mutex` for many budgets first.
/// What: a 2000-chunk group parks before point 50. One millisecond short of a
/// budget after the park the call must still be pending and the breaker
/// closed; at one budget it must fail with `BudgetExceeded` for `upsert` and
/// a tripped breaker that counted one abandoned op.
/// Test: this test.
#[test]
fn a_stall_inside_a_large_group_trips_one_budget_after_the_stall() {
    paused(|| async {
        let (_dir, store) = small_store();
        let id = Uuid::new_v4();
        let parked = index_pace::arm(&id.to_string(), Duration::ZERO, Some(50));

        let mut write = Box::pin(store.upsert_chunks(id, spread(2000, DIM, 2)));
        if let Poll::Ready(outcome) = poll_once(&mut write).await {
            panic!("the group ended before it started: {outcome:?}");
        }
        // #9728: bounded. A trip before the park ends the loop early, and the
        // sender in the static seam never drops.
        parked
            .reached
            .recv_timeout(WATCHDOG / 2)
            .unwrap_or_else(|e| panic!("the loop never reached the park point ({e})"));
        let stalled_at = Instant::now();

        tokio::time::advance(SMALL - Duration::from_millis(1)).await;
        settle().await;
        if let Poll::Ready(outcome) = poll_once(&mut write).await {
            panic!("the stall tripped before one budget: {outcome:?}");
        }
        assert!(
            !store.op_breaker().is_tripped(),
            "tripped before one budget"
        );

        tokio::time::advance(Duration::from_millis(1)).await;
        let outcome = write.await;
        let waited = stalled_at.elapsed();
        drop(parked.release);

        let err = outcome.expect_err("a stalled group must fail");
        assert!(
            matches!(
                op_budget_error(&err),
                Some(OpBudgetError::BudgetExceeded { op: "upsert", .. })
            ),
            "expected BudgetExceeded for upsert, got {err:#}"
        );
        assert_eq!(waited, SMALL, "the trip comes one budget after the stall");
        let breaker = store.op_breaker();
        assert!(breaker.is_tripped(), "a stall must trip");
        assert_eq!(breaker.abandoned_ops(), 1, "the stalled op counts once");
    });
}

/// Why (#9728 Fail-Open Check): on the error arm a stalled group's redb rows
/// are committed but its tail never reached the graph. Nothing may treat the
/// group as finished: not search, and not chunk health, whose complete count
/// would let the backfill dequeue the drawer.
/// What: a 20-chunk group parks before point 5 and trips one budget later.
/// Asserts exactly 5 points indexed; then search, upsert, remove,
/// `upsert_chunks` and `chunk_shapes` each refuse with `Wedged`; then, after
/// the parked task is released and its op ends, the breaker is still tripped
/// and the tail still absent.
/// Test: this test.
#[test]
fn a_stalled_group_trips_for_good_and_is_never_counted_complete() {
    paused(|| async {
        let (_dir, store) = small_store();
        let id = Uuid::new_v4();
        let parked = index_pace::arm(&id.to_string(), Duration::ZERO, Some(5));

        let mut write = Box::pin(store.upsert_chunks(id, spread(20, DIM, 3)));
        assert!(poll_once(&mut write).await.is_pending());
        parked
            .reached
            .recv_timeout(WATCHDOG / 2)
            .expect("the loop reaches the park point");
        tokio::time::advance(SMALL).await;
        write.await.expect_err("a stalled group must fail");
        let breaker = store.op_breaker();
        assert!(breaker.is_tripped());
        assert_eq!(breaker.abandoned_ops(), 1, "the stalled op counts once");
        assert_eq!(
            graph_points(&store),
            5,
            "points before the park are indexed"
        );

        // #9278 T2: `chunk_shapes` and `upsert_chunks` return `HnswStoreError`.
        let wedged = |err: &(dyn std::error::Error + 'static)| {
            matches!(op_budget_error(err), Some(OpBudgetError::Wedged { .. }))
        };
        let shapes = store
            .chunk_shapes()
            .expect_err("chunk health must not count a tripped store's groups");
        assert!(wedged(&shapes), "expected Wedged, got {shapes:#}");
        let refusals = [
            store.search(&spread(1, DIM, 3)[0], 5).await.map(|_| ()),
            store
                .upsert(Uuid::new_v4(), spread(1, DIM, 4).remove(0))
                .await,
            store.remove(id).await,
            store
                .upsert_chunks(id, spread(2, DIM, 5))
                .await
                .map_err(anyhow::Error::from),
        ];
        for refused in refusals {
            let err = refused.expect_err("a tripped store must refuse");
            assert!(wedged(&*err), "expected Wedged, got {err:#}");
        }

        let spawned = breaker.spawned_ops();
        drop(parked.release);
        // #10335: wait for the released task's op to end, not a fixed sleep.
        assert!(
            until(|| store.hnsw_for_test().oldest_op().is_none()),
            "the released task never ended"
        );
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
    });
}

/// Why (#9728 guard): touching the op on progress must not hide a stall.
/// Health has to keep seeing a stalled group's op age past its wedge line.
/// What: a 20-chunk group parks before point 5 and trips one budget later.
/// Reads the parked op, waits twice the budget of real time (health's age is
/// real time), and asserts its start never moved and its age passed twice
/// the budget.
/// Test: this test.
#[test]
fn a_stalled_group_op_age_keeps_growing_past_the_budget() {
    paused(|| async {
        let (_dir, store) = small_store();
        let id = Uuid::new_v4();
        let parked = index_pace::arm(&id.to_string(), Duration::ZERO, Some(5));

        let mut write = Box::pin(store.upsert_chunks(id, spread(20, DIM, 8)));
        assert!(poll_once(&mut write).await.is_pending());
        parked
            .reached
            .recv_timeout(WATCHDOG / 2)
            .expect("the loop reaches the park point");
        tokio::time::advance(SMALL).await;
        write.await.expect_err("a stalled group must fail");
        let at_trip = store.hnsw_for_test().oldest_op();
        // #10335: a lower bound only; host load can only lengthen it.
        std::thread::sleep(SMALL * 2 + Duration::from_millis(50));
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
    });
}

/// Why (#9728 guard): a caller that drops its future (an HTTP disconnect,
/// the remember ceiling) must not stop a progressing group half-indexed, and
/// must not trip the breaker for it.
/// What: `POINTS` chunks, `GAP` of paused-clock time before each; the caller
/// is dropped after its first poll. Asserts every chunk reaches the graph,
/// the breaker stays untripped through two more budgets and nothing is
/// abandoned.
/// Test: this test.
#[test]
fn a_dropped_caller_of_a_slow_group_leaves_every_chunk_indexed() {
    paused(|| async {
        let (_dir, store) = small_store();
        let id = Uuid::new_v4();
        let steps = index_pace::arm_steps(&id.to_string());

        let mut write = Box::pin(store.upsert_chunks(id, spread(POINTS, DIM, 6)));
        assert!(poll_once(&mut write).await.is_pending());
        drop(write);

        for k in 0..POINTS {
            next_point(&steps, k);
            release_after(&steps, GAP).await;
        }
        let indexed = until(|| store.hnsw_for_test().oldest_op().is_none());
        let breaker = store.op_breaker();
        assert!(
            indexed && graph_points(&store) == POINTS,
            "the detached group stopped at {} of {POINTS} points (tripped: {})",
            graph_points(&store),
            breaker.is_tripped()
        );
        tokio::time::advance(SMALL * 2).await;
        settle().await;
        assert!(!breaker.is_tripped(), "a progressing group must not trip");
        assert_eq!(breaker.abandoned_ops(), 0);
    });
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
