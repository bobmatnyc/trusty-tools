//! Tests for the `/health` catalog-report cache (#7968).
//!
//! Why: the cache replaces a walk that ran per request. Each promise below is a
//! way the fix could quietly regress: a walk per request (the #7968 cost back,
//! now also a thread per request), a report that never refreshes, a failed walk
//! served as a fresh catalog, and a cold cache that blocks the probe.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use super::{CatalogReportCache, STUCK_REFRESH_BOUND};
// `Inner.refresh_started_at` is `tokio::time::Instant` (paused-clock aware);
// aliased to avoid clashing with `std::time::Instant` above, which the
// stalled-read test uses to measure genuine wall-clock latency.
use crate::core::update_check::StalenessReport;
use crate::daemon::state::DaemonState;
use tokio::time::Instant as TokioInstant;

/// Long enough that no test here times out waiting on its own refresh.
const GENEROUS_WAIT: Duration = Duration::from_secs(10);

/// The latency bound #7965 names for `/health`: half the CLI's 500 ms probe.
const BOUND: Duration = Duration::from_millis(250);

fn stale_report() -> StalenessReport {
    StalenessReport {
        stale: true,
        ..StalenessReport::default()
    }
}

/// Eight concurrent cold requests start ONE walk, and a later request inside
/// the max age reuses it (#7968).
///
/// Fails if the single-flight flag is dropped (eight walks, eight blocking
/// threads) or if a fresh report is recomputed per request.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_requests_start_a_single_refresh() {
    let cache = Arc::new(CatalogReportCache::new(Duration::from_secs(30)));
    let calls = Arc::new(AtomicUsize::new(0));
    let mut requests = Vec::new();
    for _ in 0..8 {
        let cache = Arc::clone(&cache);
        let calls = Arc::clone(&calls);
        requests.push(tokio::spawn(async move {
            cache
                .get(
                    move || {
                        calls.fetch_add(1, Ordering::SeqCst);
                        std::thread::sleep(Duration::from_millis(200));
                        stale_report()
                    },
                    GENEROUS_WAIT,
                )
                .await
        }));
    }
    for request in requests {
        let report = request.await.expect("request task");
        assert!(report.stale, "every waiter sees the one walk: {report:?}");
    }
    let calls_for_late = Arc::clone(&calls);
    let late = cache
        .get(
            move || {
                calls_for_late.fetch_add(1, Ordering::SeqCst);
                StalenessReport::default()
            },
            GENEROUS_WAIT,
        )
        .await;
    assert!(late.stale, "a request inside the max age reuses the report");
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "one walk for nine requests"
    );
}

/// A report older than the max age is recomputed (#7968).
///
/// Fails if the cache serves its first answer forever, which would hide a
/// `cargo install` that changed the catalog.
#[tokio::test]
async fn a_report_older_than_max_age_is_recomputed() {
    let cache = Arc::new(CatalogReportCache::new(Duration::ZERO));
    let first = cache.get(stale_report, GENEROUS_WAIT).await;
    assert!(first.stale);
    let second = cache.get(StalenessReport::default, GENEROUS_WAIT).await;
    assert!(
        !second.stale && !second.unknown,
        "an expired report must be replaced by the new walk: {second:?}"
    );
}

/// A walk that panics is served as `unknown`, never as a fresh catalog (#7968).
///
/// The Fail-Open check: `StalenessReport::default()` reads "not stale, not
/// unknown", so an error arm that fell back to the default would tell `tm
/// doctor` the catalog is current when nothing was measured.
#[tokio::test]
async fn a_panicking_refresh_reports_unknown_not_fresh() {
    let cache = Arc::new(CatalogReportCache::new(Duration::from_secs(30)));
    let report = cache
        .get(
            || -> StalenessReport { panic!("stand-in for a walk that crashed") },
            GENEROUS_WAIT,
        )
        .await;
    assert!(report.unknown, "a crashed walk is undetermined: {report:?}");
    assert!(!report.stale);
}

/// A refresh that never returns is presumed hung once outstanding longer than
/// `STUCK_REFRESH_BOUND`; a warm cache then answers `unknown` instead of
/// re-serving the pre-hang report forever (#7968).
///
/// The Fail-Open check this closes: on `c151219ae` a `compute` that blocks
/// without panicking never clears the single-flight `refreshing` flag, so
/// every later call keeps falling through to `previous` — the last good, now
/// arbitrarily stale, "not stale" report — with no bound and nothing telling
/// the caller the walk never finished. This test fails against that commit:
/// the `after_bound` call below still returns the pre-hang report there,
/// never `unknown`.
///
/// Why the hang is marked directly on `Inner` rather than via a real blocked
/// `compute`: this module is `health_catalog`'s child, so its private state is
/// reachable, and the bug under test lives entirely in `get`'s bound check —
/// not in how a refresh is spawned. A genuinely blocked `spawn_blocking`
/// thread here would out-survive the test (nothing ever signals it), and
/// `#[tokio::test]`'s runtime teardown waits for outstanding blocking-pool
/// work, hanging the whole binary. Marking `refreshing` directly reproduces
/// the same state with nothing left to wait for at teardown. Uses
/// `start_paused` and `tokio::time::advance` (no wall-clock sleeps) so a 60 s
/// bound costs nothing real.
#[tokio::test(start_paused = true)]
async fn a_refresh_stuck_past_the_bound_is_reported_unknown() {
    let cache = Arc::new(CatalogReportCache::new(Duration::from_secs(30)));

    // Warm the cache with one good, non-stale report.
    let warm = cache.get(StalenessReport::default, GENEROUS_WAIT).await;
    assert!(
        !warm.stale && !warm.unknown,
        "warm-up must succeed: {warm:?}"
    );

    // Expire it, then mark a refresh outstanding without ever completing it.
    tokio::time::advance(Duration::from_secs(31)).await;
    {
        let mut inner = cache.inner.lock();
        inner.refreshing = true;
        inner.refresh_started_at = Some(TokioInstant::now());
    }
    let unreachable = || -> StalenessReport {
        unreachable!("refreshing=true must never let `get` start a second walk")
    };

    // Under the bound: still serves the pre-hang report, and does not start a
    // second walk (single-flight held).
    let still_warm = cache.get(unreachable, Duration::from_millis(100)).await;
    assert!(
        !still_warm.unknown && !still_warm.stale,
        "a refresh outstanding under the bound still serves the pre-hang \
         report: {still_warm:?}"
    );

    // Push the outstanding refresh's age past the stuck bound.
    tokio::time::advance(STUCK_REFRESH_BOUND + Duration::from_secs(1)).await;
    let after_bound = cache.get(unreachable, Duration::from_millis(100)).await;
    assert!(
        after_bound.unknown && !after_bound.stale,
        "a refresh stuck past the bound must read unknown, never the \
         pre-hang report: {after_bound:?}"
    );
}

/// `/health` answers inside the bound, twice, while the catalog read is
/// stalled, and reports `unknown` before any walk has finished (#7968).
///
/// Why: under host I/O pressure every `/health` re-walked the catalog inline on
/// a runtime worker and a live daemon read as unreachable to the CLI's 500 ms
/// probe. A FIFO at the project manifest layer is a deterministic stand-in for
/// that stalled read: opening it blocks until a writer arrives, `STALL` later.
/// On a current-thread runtime a walk that runs inline blocks the only runtime
/// thread, so the handler cannot return before the writer does. The second
/// call pins that a request arriving while the walk is still stalled neither
/// waits for it nor starts another. Fails against the per-request walk
/// (measured 1.42 s on the pre-fix `core_ops::health`).
#[cfg(unix)]
#[tokio::test(flavor = "current_thread")]
async fn health_answers_while_the_catalog_read_is_stalled() {
    const STALL: Duration = Duration::from_millis(1500);
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let fifo = crate::core::harness_root::framework_dir(tmp.path())
        .join(crate::core::manifest::MANIFEST_FILE);
    std::fs::create_dir_all(fifo.parent().expect("manifest path has a parent")).expect("mkdir");
    let made = std::process::Command::new("mkfifo")
        .arg(&fifo)
        .status()
        .expect("run mkfifo");
    assert!(made.success(), "mkfifo failed: {made}");
    let writer_path = fifo.clone();
    let writer = std::thread::spawn(move || {
        std::thread::sleep(STALL);
        // Opening for write releases the blocked reader; dropping it sends EOF.
        drop(std::fs::OpenOptions::new().write(true).open(&writer_path));
    });
    let state = Arc::new(DaemonState::with_root(tmp.path().to_path_buf()));

    for call in ["first", "second"] {
        let asked = Instant::now();
        let body = crate::daemon::rpc::core_ops::health(&state).await;
        let latency = asked.elapsed();
        assert!(
            latency < BOUND,
            "{call} /health took {latency:?} while the catalog read was stalled; bound {BOUND:?}"
        );
        assert_eq!(body.status, "ok");
        assert!(
            body.catalog_unknown && !body.catalog_stale,
            "{call}: an unanswered walk must read unknown, never fresh or stale: {body:?}"
        );
    }
    writer.join().expect("writer thread");
}
