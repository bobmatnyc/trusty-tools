//! Bounded-age cache of trusty-memory / trusty-search reachability for `/health`.
//!
//! Why: the owner ruled that "mpm health should be degraded if either [memory
//! or search] are not running". A tm session without either has no memory or
//! no code search, yet `mpm.health` answered `status: "ok"` regardless. Probing
//! both daemons inline on every health request would put up to two multi-second
//! socket round trips on the liveness path, which is the #7968 failure class.
//!
//! What: [`DependencyCache`] holds the last probe of both daemons and when it
//! was taken. A request that finds it older than [`MAX_AGE`] starts ONE
//! background refresh (single-flight) and waits at most [`REFRESH_WAIT`] for
//! it; past that it answers from the previous result, which is at most
//! [`MAX_AGE`] plus [`PROBE_BUDGET`] old because every probe runs under that
//! budget (one that outlives it is `timeout`). With no result yet, each
//! dependency reads `pending`, never healthy.
//! The probe itself is a [`DependencyProbe`] trait object, so tests inject a
//! fake instead of dialling live daemons.
//!
//! Test: `health_deps_tests`.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use tokio::sync::Notify;
use tokio::time::Instant;

/// How long a probe result is served before a request refreshes it.
pub(crate) const MAX_AGE: Duration = Duration::from_secs(10);

/// How long a request waits for a refresh it found necessary.
///
/// Why 150 ms: `/health` must stay inside the CLI's 500 ms discovery deadline
/// (#7968) even when both probes stall.
pub(crate) const REFRESH_WAIT: Duration = Duration::from_millis(150);

/// Upper bound on one dependency probe; an unanswered probe is `timeout`.
pub(crate) const PROBE_BUDGET: Duration = Duration::from_secs(2);

/// A daemon `mpm.health` depends on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dependency {
    /// trusty-memory, probed with `memory.health`.
    Memory,
    /// trusty-search, probed with `search.health`.
    Search,
}

impl Dependency {
    /// Both dependencies, in the order `degraded[]` lists them.
    pub const ALL: [Dependency; 2] = [Dependency::Memory, Dependency::Search];

    /// The service name an operator recognises.
    pub fn name(self) -> &'static str {
        match self {
            Self::Memory => "trusty-memory",
            Self::Search => "trusty-search",
        }
    }
}

/// What one probe of one dependency observed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DependencyOutcome {
    /// Answered its health call with a healthy status.
    Up,
    /// Did not answer, refused, or answered unhealthy; carries the reason.
    Down(String),
    /// Did not answer within [`PROBE_BUDGET`].
    Timeout,
    /// No probe has finished since the daemon started.
    Pending,
}

/// The boxed future a [`DependencyProbe`] returns.
pub type ProbeFuture = Pin<Box<dyn Future<Output = DependencyOutcome> + Send>>;

/// Probes one dependency; the injection seam for tests.
///
/// Why: the production probe dials host sockets, which a unit test must not.
/// What: `probe` returns a future resolving to the observed outcome. The cache
/// applies [`PROBE_BUDGET`] around it, so an implementation need not.
/// Test: `health_deps_tests` injects `Fixed` and `Hang` fakes.
pub trait DependencyProbe: Send + Sync {
    /// Probe `dep` once.
    fn probe(&self, dep: Dependency) -> ProbeFuture;
}

/// One dependency's outcome, as the cache serves it.
pub type DependencyReport = Vec<(Dependency, DependencyOutcome)>;

#[derive(Default)]
struct Inner {
    report: Option<DependencyReport>,
    computed_at: Option<Instant>,
    refreshing: bool,
}

/// Single-flight, bounded-age cache of both dependency probes.
///
/// Why: see the module doc.
/// What: a `parking_lot` mutex over the last report (never held across an
/// await), a [`Notify`] each finished refresh wakes, and the swappable probe.
/// Test: `stale_cached_ok_expires_after_max_age`,
/// `no_finished_probe_reads_pending_not_ok`.
pub struct DependencyCache {
    inner: Mutex<Inner>,
    refreshed: Notify,
    probe: Mutex<Arc<dyn DependencyProbe>>,
}

impl std::fmt::Debug for DependencyCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DependencyCache").finish_non_exhaustive()
    }
}

impl DependencyCache {
    /// An empty cache that probes through `probe`.
    pub fn new(probe: Arc<dyn DependencyProbe>) -> Self {
        Self {
            inner: Mutex::new(Inner::default()),
            refreshed: Notify::new(),
            probe: Mutex::new(probe),
        }
    }

    /// Replace the probe and drop the cached report, so the next request probes.
    #[cfg(test)]
    pub fn set_probe(&self, probe: Arc<dyn DependencyProbe>) {
        *self.probe.lock() = probe;
        *self.inner.lock() = Inner::default();
    }

    /// The current dependency report, refreshing first when older than [`MAX_AGE`].
    ///
    /// Why: the request path of `/health`; it must never wait out a probe.
    /// What: a fresh report returns at once. Otherwise one refresh starts unless
    /// one is running, and the call waits up to `wait`. Without a finished
    /// refresh it serves the previous report (bounded by the probe budget), and
    /// with no report at all every dependency reads `Pending`.
    /// Test: `memory_down_reports_degraded`, `stale_cached_ok_expires_after_max_age`,
    /// `no_finished_probe_reads_pending_not_ok`.
    pub async fn get(self: &Arc<Self>, wait: Duration) -> DependencyReport {
        let notified = self.refreshed.notified();
        tokio::pin!(notified);
        {
            let mut inner = self.inner.lock();
            let age = inner.computed_at.map(|at| at.elapsed());
            if let (Some(report), Some(age)) = (&inner.report, age)
                && age < MAX_AGE
            {
                return report.clone();
            }
            notified.as_mut().enable();
            if !inner.refreshing {
                inner.refreshing = true;
                self.start_refresh();
            }
        }
        if tokio::time::timeout(wait, notified).await.is_ok() {
            let inner = self.inner.lock();
            if let (Some(report), Some(at)) = (&inner.report, inner.computed_at)
                && at.elapsed() < MAX_AGE
            {
                return report.clone();
            }
        }
        let inner = self.inner.lock();
        inner
            .report
            .clone()
            .unwrap_or_else(|| uniform(DependencyOutcome::Pending))
    }

    /// Run both probes concurrently on a spawned task and store the result.
    fn start_refresh(self: &Arc<Self>) {
        let cache = Arc::clone(self);
        let probe = Arc::clone(&*self.probe.lock());
        tokio::spawn(async move {
            let (memory, search) = tokio::join!(
                bounded(&*probe, Dependency::Memory),
                bounded(&*probe, Dependency::Search)
            );
            {
                let mut inner = cache.inner.lock();
                inner.report = Some(vec![
                    (Dependency::Memory, memory),
                    (Dependency::Search, search),
                ]);
                inner.computed_at = Some(Instant::now());
                inner.refreshing = false;
            }
            cache.refreshed.notify_waiters();
        });
    }
}

/// One probe under [`PROBE_BUDGET`]; an elapsed budget is `Timeout`.
async fn bounded(probe: &dyn DependencyProbe, dep: Dependency) -> DependencyOutcome {
    tokio::time::timeout(PROBE_BUDGET, probe.probe(dep))
        .await
        .unwrap_or(DependencyOutcome::Timeout)
}

fn uniform(outcome: DependencyOutcome) -> DependencyReport {
    Dependency::ALL
        .iter()
        .map(|dep| (*dep, outcome.clone()))
        .collect()
}

/// The `degraded[]` entries for `report`: one per dependency that is not up.
///
/// Why: `degraded[]` is the operator-facing list `tm status` already prints.
/// What: `"<service>: timeout"`, `"<service>: pending (first probe still
/// running)"`, or `"<service>: down — <reason>"`; nothing for an up dependency.
/// Test: `timeout_reports_degraded_with_reason_timeout`.
pub fn degraded_entries(report: &DependencyReport) -> Vec<String> {
    report
        .iter()
        .filter_map(|(dep, outcome)| match outcome {
            DependencyOutcome::Up => None,
            DependencyOutcome::Timeout => Some(format!("{}: timeout", dep.name())),
            DependencyOutcome::Pending => Some(format!(
                "{}: pending (first probe still running)",
                dep.name()
            )),
            DependencyOutcome::Down(reason) => Some(format!("{}: down — {reason}", dep.name())),
        })
        .collect()
}

/// The production probe: `memory.health` and `search.health` on the host sockets.
///
/// Why: the daemon's real answer to "are memory and search running".
/// What: dials each socket with an inner budget longer than [`PROBE_BUDGET`],
/// so a slow daemon surfaces as the cache's `Timeout`, not as a dial error.
/// Memory reports `Up` only for `status: "ok"`; search for any answer.
/// Test: the binary smoke run (`scripts/sandbox_daemon.sh`); unit tests inject
/// fakes instead.
// The test build starts with `AlwaysUp` (see `default_probe`), so the socket
// probe is only constructed by the daemon binary.
#[cfg_attr(test, allow(dead_code))]
pub struct SocketProbe;

/// Inner call budget, deliberately above [`PROBE_BUDGET`].
#[cfg_attr(test, allow(dead_code))]
const INNER_BUDGET: Duration = Duration::from_secs(3);

impl DependencyProbe for SocketProbe {
    fn probe(&self, dep: Dependency) -> ProbeFuture {
        Box::pin(async move {
            match dep {
                Dependency::Memory => probe_memory().await,
                Dependency::Search => probe_search().await,
            }
        })
    }
}

#[cfg_attr(test, allow(dead_code))]
async fn probe_memory() -> DependencyOutcome {
    use trusty_common::memory_rpc::MemoryHealthStatus;
    let socket = trusty_common::memory_rpc::resolve_memory_socket_or_unreachable();
    match crate::core::memory_checked::call_with_timeout(
        &socket,
        "memory.health",
        serde_json::json!({}),
        INNER_BUDGET,
    )
    .await
    {
        Ok(body) => match MemoryHealthStatus::from_health_body(&body) {
            MemoryHealthStatus::Ok => DependencyOutcome::Up,
            other => DependencyOutcome::Down(format!("reports status {other:?}")),
        },
        Err(e) => DependencyOutcome::Down(format!("not answering at {}: {e:#}", socket.display())),
    }
}

#[cfg_attr(test, allow(dead_code))]
async fn probe_search() -> DependencyOutcome {
    use trusty_common::search_rpc;
    let socket = match search_rpc::search_socket() {
        Ok(socket) => socket,
        Err(e) => return DependencyOutcome::Down(format!("cannot resolve its socket: {e:#}")),
    };
    match search_rpc::call_at(
        &socket,
        search_rpc::METHOD_HEALTH,
        serde_json::json!({}),
        INNER_BUDGET,
    )
    .await
    {
        Ok(_) => DependencyOutcome::Up,
        Err(e) => DependencyOutcome::Down(format!("not answering at {}: {e:#}", socket.display())),
    }
}

/// The probe a new daemon state starts with.
///
/// Why: unit tests build `DaemonState` by the hundred and must not dial the
/// host's daemons, so the test build starts with every dependency up; a test
/// that exercises the dependency path injects its own probe.
pub(crate) fn default_probe() -> Arc<dyn DependencyProbe> {
    #[cfg(test)]
    {
        Arc::new(AlwaysUp)
    }
    #[cfg(not(test))]
    {
        Arc::new(SocketProbe)
    }
}

/// Test-build default: every dependency answers.
#[cfg(test)]
pub(crate) struct AlwaysUp;

#[cfg(test)]
impl DependencyProbe for AlwaysUp {
    fn probe(&self, _dep: Dependency) -> ProbeFuture {
        Box::pin(async { DependencyOutcome::Up })
    }
}

#[cfg(test)]
#[path = "health_deps_tests.rs"]
mod health_deps_tests;
