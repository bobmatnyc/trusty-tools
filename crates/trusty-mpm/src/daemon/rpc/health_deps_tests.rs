//! Tests for the `/health` dependency-probe cache and its effect on `mpm.health`.

use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;

use super::{
    Dependency, DependencyCache, DependencyOutcome, DependencyProbe, MAX_AGE, ProbeFuture,
    degraded_entries,
};
use crate::daemon::state::DaemonState;

/// What a fake dependency does when probed.
#[derive(Clone)]
enum Mode {
    Up,
    Down(&'static str),
    Hang,
}

/// A probe whose per-dependency answer a test can change mid-test.
struct Fake {
    memory: Mutex<Mode>,
    search: Mutex<Mode>,
}

impl Fake {
    fn new(memory: Mode, search: Mode) -> Arc<Self> {
        Arc::new(Self {
            memory: Mutex::new(memory),
            search: Mutex::new(search),
        })
    }
}

impl DependencyProbe for Fake {
    fn probe(&self, dep: Dependency) -> ProbeFuture {
        let mode = match dep {
            Dependency::Memory => self.memory.lock().clone(),
            Dependency::Search => self.search.lock().clone(),
        };
        Box::pin(async move {
            match mode {
                Mode::Up => DependencyOutcome::Up,
                Mode::Down(reason) => DependencyOutcome::Down(reason.to_owned()),
                Mode::Hang => std::future::pending().await,
            }
        })
    }
}

/// A daemon state whose health reads `probe`, with the dependency cache warmed.
async fn state_with(probe: Arc<Fake>) -> Arc<DaemonState> {
    let state = DaemonState::shared();
    state.dependency_cache().set_probe(probe);
    // Long enough for a hanging probe to hit its budget under paused time.
    state.dependency_cache().get(Duration::from_secs(10)).await;
    state
}

async fn health_of(probe: Arc<Fake>) -> crate::daemon::api::types::HealthResponse {
    let state = state_with(probe).await;
    super::super::core_ops::health(&state).await
}

/// Test: this test.
#[tokio::test(start_paused = true)]
async fn memory_down_reports_degraded() {
    let body = health_of(Fake::new(Mode::Down("refused"), Mode::Up)).await;
    assert_eq!(body.status, "degraded");
    assert!(body.alive, "a degraded daemon is still alive");
    assert!(
        body.degraded
            .iter()
            .any(|d| d.starts_with("trusty-memory: down") && d.contains("refused")),
        "{:?}",
        body.degraded
    );
    assert!(!body.degraded.iter().any(|d| d.contains("trusty-search")));
}

/// Test: this test.
#[tokio::test(start_paused = true)]
async fn search_down_reports_degraded() {
    let body = health_of(Fake::new(Mode::Up, Mode::Down("no socket"))).await;
    assert_eq!(body.status, "degraded");
    assert!(
        body.degraded
            .iter()
            .any(|d| d.starts_with("trusty-search: down") && d.contains("no socket")),
        "{:?}",
        body.degraded
    );
    assert!(!body.degraded.iter().any(|d| d.contains("trusty-memory")));
}

/// Test: this test.
#[tokio::test(start_paused = true)]
async fn timeout_reports_degraded_with_reason_timeout() {
    let body = health_of(Fake::new(Mode::Up, Mode::Hang)).await;
    assert_eq!(body.status, "degraded");
    assert!(
        body.degraded.contains(&"trusty-search: timeout".to_owned()),
        "{:?}",
        body.degraded
    );
}

/// Test: this test.
#[tokio::test(start_paused = true)]
async fn both_up_reports_ok() {
    let body = health_of(Fake::new(Mode::Up, Mode::Up)).await;
    assert_eq!(body.status, "ok");
    assert!(body.alive);
    assert!(
        !body.degraded.iter().any(|d| d.starts_with("trusty-")),
        "{:?}",
        body.degraded
    );
}

/// A cached `ok` is served only for `MAX_AGE`; the next request after that
/// re-probes and reports the dependency that went down.
/// Test: this test.
#[tokio::test(start_paused = true)]
async fn stale_cached_ok_expires_after_max_age() {
    let fake = Fake::new(Mode::Up, Mode::Up);
    let cache = Arc::new(DependencyCache::new(fake.clone()));
    let wait = Duration::from_secs(1);
    assert!(degraded_entries(&cache.get(wait).await).is_empty());

    *fake.search.lock() = Mode::Down("stopped");
    assert!(
        degraded_entries(&cache.get(wait).await).is_empty(),
        "inside MAX_AGE the cached report is served without probing"
    );

    tokio::time::advance(MAX_AGE + Duration::from_secs(1)).await;
    let entries = degraded_entries(&cache.get(wait).await);
    assert_eq!(entries.len(), 1, "{entries:?}");
    assert!(entries[0].starts_with("trusty-search: down"), "{entries:?}");
}

/// Before any probe finishes, a request never reads healthy.
/// Test: this test.
#[tokio::test(start_paused = true)]
async fn no_finished_probe_reads_pending_not_ok() {
    let cache = Arc::new(DependencyCache::new(Fake::new(Mode::Hang, Mode::Hang)));
    let entries = degraded_entries(&cache.get(Duration::from_millis(10)).await);
    assert_eq!(entries.len(), 2, "{entries:?}");
    assert!(entries.iter().all(|e| e.contains("pending")), "{entries:?}");
}

/// The health request does not wait out a hung probe: it answers within the
/// bounded refresh wait.
/// Test: this test.
#[tokio::test(start_paused = true)]
async fn health_does_not_block_on_a_hung_probe() {
    let state = DaemonState::shared();
    state
        .dependency_cache()
        .set_probe(Fake::new(Mode::Hang, Mode::Hang));
    let started = tokio::time::Instant::now();
    let body = super::super::core_ops::health(&state).await;
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(body.status, "degraded");
}
