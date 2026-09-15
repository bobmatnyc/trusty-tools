//! Bounded-age cache of the `/health` catalog-staleness report (#7968).
//!
//! Why: `GET /health` and `mpm.health` ran
//! [`detect_for_framework`](crate::core::update_check::detect_for_framework) on
//! every request, inline on a runtime worker. That walk opens and hashes the
//! deployed agent and skill files, so under host I/O pressure — the daemon's own
//! background sweeps (#7965) on a host with 72 bases — one liveness probe took
//! ~5 s, missed the CLI's 500 ms discovery deadline, and `tm` reported a live
//! daemon as unreachable. The answer changes only when an install rewrites the
//! catalog, so recomputing it per request buys nothing.
//!
//! What: [`CatalogReportCache`] holds the last report and when it was computed.
//! A request that finds it older than [`MAX_AGE`] starts ONE background refresh
//! on the blocking pool (single-flight) and waits at most [`REFRESH_WAIT`] for
//! it; past that it answers from the previous report, or `unknown` when there is
//! none yet. A refresh that panics stores `unknown`, never a fresh-looking
//! default.
//!
//! Test: `health_catalog_tests`.

use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use tokio::sync::Notify;

use crate::core::update_check::StalenessReport;
use crate::daemon::state::DaemonState;

/// How long a computed report is served before a request refreshes it.
///
/// Why 30 s: a catalog changes only on `cargo install` / `tm catalog apply`, and
/// `tm doctor` re-reads `/health` well after either; a staleness hint that is
/// half a minute late costs nothing, a walk per liveness probe is #7968.
pub(crate) const MAX_AGE: Duration = Duration::from_secs(30);

/// How long a request waits for a refresh it found necessary.
///
/// Why 100 ms: long enough that an unloaded host answers with a current report,
/// short enough that `/health` stays under the 250 ms bound #7965 names even
/// when the walk stalls.
pub(crate) const REFRESH_WAIT: Duration = Duration::from_millis(100);

/// The cache's mutable half.
#[derive(Debug, Default)]
struct Inner {
    report: Option<StalenessReport>,
    computed_at: Option<Instant>,
    /// True while a refresh task is outstanding — the single-flight flag.
    refreshing: bool,
}

/// Single-flight, bounded-age cache of one [`StalenessReport`] (#7968).
///
/// Why: see the module doc.
/// What: a `parking_lot` mutex over [`Inner`] (never held across an await) and
/// a [`Notify`] each finished refresh wakes. At most one refresh task and one
/// blocking-pool thread exist per cache at any time, however many requests
/// arrive.
/// Test: `concurrent_requests_start_a_single_refresh`,
/// `a_report_older_than_max_age_is_recomputed`,
/// `a_panicking_refresh_reports_unknown_not_fresh`.
#[derive(Debug)]
pub(crate) struct CatalogReportCache {
    inner: Mutex<Inner>,
    refreshed: Notify,
    max_age: Duration,
}

/// The report served when no walk has answered: undetermined, never fresh.
fn unknown_report() -> StalenessReport {
    StalenessReport {
        unknown: true,
        ..StalenessReport::default()
    }
}

impl CatalogReportCache {
    /// An empty cache that serves a report for `max_age` once computed.
    pub(crate) fn new(max_age: Duration) -> Self {
        Self {
            inner: Mutex::new(Inner::default()),
            refreshed: Notify::new(),
            max_age,
        }
    }

    /// The cached report, refreshing it first when it is older than `max_age`.
    ///
    /// Why: the request path of `/health` — it must never run `compute` inline.
    /// What: a fresh report returns immediately. Otherwise a refresh is started
    /// unless one is already running, and the call waits up to `wait` for it;
    /// when it does not finish in time the previous report is returned, or
    /// `unknown` when none exists.
    /// Test: `concurrent_requests_start_a_single_refresh`,
    /// `a_report_older_than_max_age_is_recomputed`,
    /// `health_answers_while_the_catalog_read_is_stalled`.
    pub(crate) async fn get<F>(self: &Arc<Self>, compute: F, wait: Duration) -> StalenessReport
    where
        F: FnOnce() -> StalenessReport + Send + 'static,
    {
        let notified = self.refreshed.notified();
        tokio::pin!(notified);
        let previous = {
            let mut inner = self.inner.lock();
            if inner
                .computed_at
                .is_some_and(|at| at.elapsed() < self.max_age)
            {
                return inner.report.clone().unwrap_or_else(unknown_report);
            }
            // Registered before the lock is released, so a refresh finishing in
            // between still wakes this waiter.
            notified.as_mut().enable();
            if !inner.refreshing {
                self.start_refresh(&mut inner, compute);
            }
            inner.report.clone()
        };
        if tokio::time::timeout(wait, notified).await.is_ok()
            && let Some(report) = self.inner.lock().report.clone()
        {
            return report;
        }
        previous.unwrap_or_else(unknown_report)
    }

    /// Mark a refresh outstanding and run `compute` on the blocking pool.
    ///
    /// The Fail-Open boundary: a `compute` that panics stores `unknown` and says
    /// so at `warn`, so a broken walk never renders as a fresh catalog.
    fn start_refresh<F>(self: &Arc<Self>, inner: &mut Inner, compute: F)
    where
        F: FnOnce() -> StalenessReport + Send + 'static,
    {
        inner.refreshing = true;
        let cache = Arc::clone(self);
        tokio::spawn(async move {
            let report = match tokio::task::spawn_blocking(compute).await {
                Ok(report) => report,
                Err(e) => {
                    tracing::warn!(
                        "#7968: the catalog staleness walk did not complete ({e}); \
                         /health reports catalog_unknown until the next refresh"
                    );
                    unknown_report()
                }
            };
            {
                let mut inner = cache.inner.lock();
                inner.report = Some(report);
                inner.computed_at = Some(Instant::now());
                inner.refreshing = false;
            }
            cache.refreshed.notify_waiters();
        });
    }
}

/// The walk the daemon caches: the daemon-wide baseline for `state`'s root.
///
/// The framework root doubles as the "project", so only the user, catalog and
/// default manifest layers apply — no per-project override.
fn detector(state: &DaemonState) -> impl FnOnce() -> StalenessReport + Send + 'static {
    let root = state.framework_root().to_path_buf();
    move || {
        let fw = crate::core::paths::FrameworkPaths::from_root(&root);
        crate::core::update_check::detect_for_framework(&fw, &root)
    }
}

/// The catalog report `/health` serves, waiting at most [`REFRESH_WAIT`] (#7968).
///
/// Test: `health_answers_while_the_catalog_read_is_stalled`.
pub(crate) async fn report(state: &Arc<DaemonState>) -> StalenessReport {
    report_within(state, REFRESH_WAIT).await
}

/// `state`'s cached catalog report, waiting at most `wait` for a refresh.
///
/// Why a parameter: `/health` must answer inside the CLI's probe budget, while
/// a caller comparing two answers needs the walk to have finished first.
/// Test: `parity_health_agrees_across_transports` warms the cache through it.
pub(crate) async fn report_within(state: &Arc<DaemonState>, wait: Duration) -> StalenessReport {
    state
        .catalog_report_cache()
        .get(detector(state), wait)
        .await
}

#[cfg(test)]
#[path = "health_catalog_tests.rs"]
mod health_catalog_tests;
