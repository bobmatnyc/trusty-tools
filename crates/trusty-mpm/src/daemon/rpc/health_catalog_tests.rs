//! Tests for the `/health` catalog-report cache (#7968).
//!
//! Why: the cache replaces a walk that ran per request. Each promise below is a
//! way the fix could quietly regress: a walk per request (the #7968 cost back,
//! now also a thread per request), a report that never refreshes, a failed walk
//! served as a fresh catalog, and a cold cache that blocks the probe.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use super::CatalogReportCache;
use crate::core::update_check::StalenessReport;
use crate::daemon::state::DaemonState;

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
