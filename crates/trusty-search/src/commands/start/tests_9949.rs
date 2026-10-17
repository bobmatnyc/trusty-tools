//! #9949: the graceful python bootstrap keeps retrying past a slow start.
//!
//! Why: a host where the python sidecar needed longer than the startup
//! attempts allowed pinned the daemon to ort for its whole uptime. Three
//! defects combined: the scaled attempt budget never reached the supervisor,
//! giving up was permanent, and every attempt repeated the full-import venv
//! recheck. #9468 adds the launcher-before-venv order and the permanent vs
//! transient split.
//! What: deterministic fakes for `PythonBootstrap`, driven under paused tokio
//! time so the multi-minute background backoff resolves instantly.
//! Test: the tests below.

use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::graceful_bootstrap::{
    adapter_supervisor_config, background_retry_delay, drive_bootstrap, PythonAdapterTeardown,
    PythonBootstrap, VenvCheck,
};
use crate::core::Embedder;
use crate::service::embedder_supervisor::bootstrap_control::PythonBootstrapHealth;
use crate::service::embedder_supervisor::embedder_view::EmbedderView;
use crate::service::embedder_supervisor::{
    ActiveBackend, BackendKind, BootstrapState, SupervisorConfig, SwitchableEmbedder,
};
use crate::service::SearchAppState;

/// The base probe budget every fake reports, as the default
/// `TRUSTY_EMBEDDERD_STARTUP_TIMEOUT_SECS` would.
const BASE_BUDGET: Duration = Duration::from_secs(30);

/// Virtual-time bound on a `drive_bootstrap` call that must return; a loop
/// that never ends fails here instead of hanging the test.
const DRIVE_BOUND: Duration = Duration::from_secs(24 * 3600);

/// How a fake adapter's readiness probe behaves.
#[derive(Clone, Copy)]
enum Probe {
    Ok,
    Fail,
    Hang,
}

/// An embedder whose every call behaves as its [`Probe`] says.
struct ProbeEmbedder(Probe);

#[async_trait::async_trait]
impl Embedder for ProbeEmbedder {
    async fn embed(&self, text: &str) -> anyhow::Result<Vec<f32>> {
        let mut vectors = self.embed_batch(&[text]).await?;
        Ok(vectors.remove(0))
    }

    async fn embed_batch(&self, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
        match self.0 {
            Probe::Ok => Ok(texts
                .iter()
                .map(|t| vec![t.len() as f32; trusty_common::embedder::EMBED_DIM])
                .collect()),
            Probe::Fail => anyhow::bail!("fake readiness probe failure"),
            Probe::Hang => std::future::pending().await,
        }
    }

    fn dimension(&self) -> usize {
        trusty_common::embedder::EMBED_DIM
    }
}

/// Counts teardown calls.
struct CountingTeardown(Arc<AtomicUsize>);

#[async_trait::async_trait]
impl PythonAdapterTeardown for CountingTeardown {
    async fn teardown(&self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }

    async fn is_confirmed_terminated(&self) -> bool {
        false
    }
}

/// A fake bootstrap: the first `probe_failures` adapters fail their probe
/// (or hang, when `hang`), later ones pass. Records every call it sees.
struct ContendedBootstrap {
    probe_failures: usize,
    hang: bool,
    launcher_missing: bool,
    adapters_built: AtomicUsize,
    launcher_lookups: AtomicUsize,
    teardowns: Arc<AtomicUsize>,
    budgets: Mutex<Vec<Duration>>,
    venv_checks: Mutex<Vec<VenvCheck>>,
    /// `ensure_venv` fails (a decided error) from this 0-based call on.
    venv_fails_from: AtomicUsize,
    /// #9971: `ensure_venv` calls before this 0-based index fail too.
    venv_fails_before: AtomicUsize,
    /// #9971: the error a failing `ensure_venv` returns.
    venv_error: Mutex<fn() -> anyhow::Error>,
}

impl ContendedBootstrap {
    fn with(probe_failures: usize, hang: bool, launcher_missing: bool) -> Arc<Self> {
        Arc::new(Self {
            probe_failures,
            hang,
            launcher_missing,
            adapters_built: AtomicUsize::new(0),
            launcher_lookups: AtomicUsize::new(0),
            teardowns: Arc::new(AtomicUsize::new(0)),
            budgets: Mutex::new(Vec::new()),
            venv_checks: Mutex::new(Vec::new()),
            venv_fails_from: AtomicUsize::new(usize::MAX),
            venv_fails_before: AtomicUsize::new(0),
            venv_error: Mutex::new(|| anyhow::anyhow!("fake venv rebuild failure")),
        })
    }

    fn failing_probes(n: usize) -> Arc<Self> {
        Self::with(n, false, false)
    }

    fn built(&self) -> usize {
        self.adapters_built.load(Ordering::SeqCst)
    }

    fn torn_down(&self) -> usize {
        self.teardowns.load(Ordering::SeqCst)
    }
}

impl PythonBootstrap for ContendedBootstrap {
    fn ensure_venv(&self, check: VenvCheck) -> anyhow::Result<()> {
        let mut checks = self.venv_checks.lock().expect("venv_checks lock");
        let call = checks.len();
        checks.push(check);
        if call >= self.venv_fails_from.load(Ordering::SeqCst)
            || call < self.venv_fails_before.load(Ordering::SeqCst)
        {
            return Err((*self.venv_error.lock().expect("venv_error lock"))());
        }
        Ok(())
    }

    fn locate_launcher(&self) -> anyhow::Result<std::path::PathBuf> {
        self.launcher_lookups.fetch_add(1, Ordering::SeqCst);
        if self.launcher_missing {
            anyhow::bail!("trusty-embedderd-py not found on PATH");
        }
        Ok(std::path::PathBuf::from("/fake/trusty-embedderd-py"))
    }

    fn build_adapter(
        &self,
        _launcher: std::path::PathBuf,
        startup_budget: Duration,
    ) -> (
        Arc<dyn Embedder>,
        Option<Arc<AtomicU32>>,
        Arc<dyn PythonAdapterTeardown>,
    ) {
        self.budgets
            .lock()
            .expect("budgets lock")
            .push(startup_budget);
        let built_before = self.adapters_built.fetch_add(1, Ordering::SeqCst);
        let probe = match (built_before < self.probe_failures, self.hang) {
            (false, _) => Probe::Ok,
            (true, false) => Probe::Fail,
            (true, true) => Probe::Hang,
        };
        (
            Arc::new(ProbeEmbedder(probe)),
            None,
            Arc::new(CountingTeardown(Arc::clone(&self.teardowns))),
        )
    }

    fn probe_timeout(&self) -> Duration {
        BASE_BUDGET
    }
}

fn ort_switchable() -> Arc<SwitchableEmbedder> {
    Arc::new(SwitchableEmbedder::new(
        Arc::new(ProbeEmbedder(Probe::Ok)),
        ActiveBackend {
            kind: BackendKind::Ort,
            provider: trusty_common::embedder::ExecutionProvider::Cpu,
            model: "all-MiniLM-L6-v2".to_string(),
            quantized: false,
            bootstrap: BootstrapState::Bootstrapping,
        },
    ))
}

fn new_state() -> SearchAppState {
    SearchAppState::new(crate::core::registry::IndexRegistry::new())
}

/// Await `drive_bootstrap` directly, bounded by [`DRIVE_BOUND`] of virtual time.
async fn drive(
    switchable: &Arc<SwitchableEmbedder>,
    fake: &Arc<ContendedBootstrap>,
    retries: u32,
) -> Option<Arc<dyn PythonAdapterTeardown>> {
    let ops: Arc<dyn PythonBootstrap> = Arc::clone(fake) as _;
    tokio::time::timeout(
        DRIVE_BOUND,
        drive_bootstrap(Arc::clone(switchable), new_state(), ops, retries),
    )
    .await
    .expect("drive_bootstrap must return instead of retrying forever")
}

/// Spawn `drive_bootstrap` as the daemon does — detached, with its handle
/// attached to the switchable's `BootstrapControl`.
fn spawn_drive(switchable: &Arc<SwitchableEmbedder>, fake: &Arc<ContendedBootstrap>, retries: u32) {
    let ops: Arc<dyn PythonBootstrap> = Arc::clone(fake) as _;
    let task = tokio::spawn({
        let switchable = Arc::clone(switchable);
        async move {
            drive_bootstrap(switchable, new_state(), ops, retries).await;
        }
    });
    switchable.bootstrap_control().attach_task(task);
}

/// Poll (in 10 ms virtual steps) until `done` holds; panic after 1000 polls.
async fn wait_until(what: &str, done: impl Fn() -> bool) {
    for _ in 0..1000 {
        if done() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("timed out waiting for {what}");
}

/// The #9949 regression: three failed probes (two startup attempts plus the
/// first background round) followed by a good one must end on python. Only
/// the first attempt runs the full-import venv recheck.
#[tokio::test(start_paused = true)]
async fn graceful_bootstrap_promotes_to_python_after_contention_window() {
    let switchable = ort_switchable();
    let fake = ContendedBootstrap::failing_probes(3);

    let promoted = drive(&switchable, &fake, 2).await;

    let active = switchable.active();
    assert_eq!(
        active.kind,
        BackendKind::Python,
        "the bootstrap must keep retrying past the startup attempts and promote"
    );
    assert_eq!(active.bootstrap, BootstrapState::Ready);
    assert!(
        promoted.is_some(),
        "a promotion hands back the live teardown"
    );
    assert_eq!(fake.built(), 4);
    assert_eq!(
        fake.torn_down(),
        3,
        "each failed attempt tears its own adapter down"
    );
    assert_eq!(
        *fake.venv_checks.lock().expect("venv_checks lock"),
        vec![
            VenvCheck::FullImport,
            VenvCheck::Liveness,
            VenvCheck::Liveness,
            VenvCheck::Liveness
        ],
        "only the first attempt pays for the full-import recheck"
    );
    let report = switchable.bootstrap_control().report();
    assert_eq!(report.attempts, 4);
    assert_eq!(report.last_error, None, "a success clears the last error");
    assert_eq!(report.next_retry_at, None);
}

/// Attempt N's adapter is built with attempt N's budget, and the real
/// adapter config hands that budget to the supervisor as its startup timeout.
#[tokio::test(start_paused = true)]
async fn attempt_two_adapter_gets_twice_the_base_budget() {
    let switchable = ort_switchable();
    let fake = ContendedBootstrap::failing_probes(1);

    drive(&switchable, &fake, 2).await;

    assert_eq!(
        *fake.budgets.lock().expect("budgets lock"),
        vec![BASE_BUDGET, 2 * BASE_BUDGET],
        "attempt 2 must build its adapter with twice the base budget"
    );
    let base = SupervisorConfig::default();
    let scaled = adapter_supervisor_config(base.clone(), 2 * BASE_BUDGET);
    assert_eq!(scaled.startup_timeout_secs, 60);
    assert_eq!(scaled.max_restarts, base.max_restarts);
    assert_eq!(scaled.idle_shutdown_secs, base.idle_shutdown_secs);
    assert_eq!(
        adapter_supervisor_config(base, Duration::from_millis(1500)).startup_timeout_secs,
        2,
        "a fractional budget rounds up, never down below the advertised budget"
    );
}

/// #9468: a permanent failure at startup ends the bootstrap on its first
/// attempt, even with startup attempts to spare: one venv build, no retry.
#[tokio::test(start_paused = true)]
async fn graceful_bootstrap_permanent_failure_ends_the_retry_loop() {
    let switchable = ort_switchable();
    let fake = ContendedBootstrap::failing_probes(0);
    fake.venv_fails_from.store(0, Ordering::SeqCst);

    let promoted = drive(&switchable, &fake, 3).await;

    assert!(promoted.is_none());
    assert_eq!(switchable.active().kind, BackendKind::Ort);
    assert_eq!(switchable.active().bootstrap, BootstrapState::Failed);
    assert_eq!(
        *fake.venv_checks.lock().expect("venv_checks lock"),
        vec![VenvCheck::FullImport],
        "the failed venv build must not be repeated"
    );
    assert_eq!(fake.launcher_lookups.load(Ordering::SeqCst), 1);
    assert_eq!(fake.built(), 0);
    let report = switchable.bootstrap_control().report();
    assert_eq!(report.attempts, 1);
    assert_eq!(report.next_retry_at, None);
    assert!(
        report
            .last_error
            .as_deref()
            .is_some_and(|e| e.contains("fake venv rebuild failure")),
        "the report must say why: {report:?}"
    );
}

/// #9468: a transient failure keeps the #9949 behaviour — every startup
/// attempt runs, then background rounds keep attempting while ort serves.
#[tokio::test(start_paused = true)]
async fn graceful_bootstrap_transient_failure_keeps_retrying() {
    let switchable = ort_switchable();
    let fake = ContendedBootstrap::failing_probes(usize::MAX);
    spawn_drive(&switchable, &fake, 2);
    wait_until("a scheduled retry", || {
        switchable.active().bootstrap == BootstrapState::Retrying
    })
    .await;
    assert_eq!(fake.built(), 2, "both startup attempts ran");

    tokio::time::sleep(background_retry_delay(0)).await;
    wait_until("the first background attempt", || fake.built() == 3).await;
    wait_until("the next scheduled retry", || {
        switchable
            .bootstrap_control()
            .report()
            .next_retry_at
            .is_some()
    })
    .await;

    assert_eq!(switchable.active().kind, BackendKind::Ort);
    assert_eq!(switchable.active().bootstrap, BootstrapState::Retrying);
    assert_eq!(fake.launcher_lookups.load(Ordering::SeqCst), 3);
    assert_eq!(switchable.bootstrap_control().report().attempts, 3);
    assert!(
        switchable
            .bootstrap_control()
            .shutdown(Duration::from_secs(5))
            .await
    );
}

/// #9468 regression: a missing launcher ends the bootstrap before any venv
/// work. Before the fix every attempt built the venv first (with `uv` present,
/// a ~3 GB torch download) and only then failed the launcher lookup.
#[tokio::test(start_paused = true)]
async fn graceful_bootstrap_missing_launcher_builds_no_venv() {
    let switchable = ort_switchable();
    let fake = ContendedBootstrap::with(0, false, true);

    let promoted = drive(&switchable, &fake, 2).await;

    assert!(promoted.is_none());
    assert_eq!(
        *fake.venv_checks.lock().expect("venv_checks lock"),
        Vec::<VenvCheck>::new(),
        "no venv step may run while the launcher is missing"
    );
    assert_eq!(
        fake.launcher_lookups.load(Ordering::SeqCst),
        1,
        "a permanent failure is not retried"
    );
    assert_eq!(fake.built(), 0, "no adapter, so no sidecar, was ever built");
    assert_eq!(
        switchable.active().kind,
        BackendKind::Ort,
        "ort keeps serving"
    );
    assert_eq!(switchable.active().bootstrap, BootstrapState::Failed);
    let report = switchable.bootstrap_control().report();
    assert_eq!(
        report.next_retry_at, None,
        "a permanent failure schedules nothing"
    );
    let health = PythonBootstrapHealth::new("failed", &report);
    assert!(
        health.last_error.as_deref().is_some_and(|e| {
            e.contains("launcher not found") && e.contains("trusty-embedderd-py")
        }),
        "/health must name the missing launcher: {health:?}"
    );
}

/// #9949: a decided error in a background round (the probe failed at startup,
/// then the venv check fails) is terminal: `Failed`, no retry scheduled, and
/// no further attempt ever runs.
#[tokio::test(start_paused = true)]
async fn graceful_bootstrap_background_decided_failure_is_terminal() {
    let switchable = ort_switchable();
    let fake = ContendedBootstrap::failing_probes(usize::MAX);
    // Call 0 is the startup attempt (probe fails); call 1 is the first
    // background round, where the venv check decides.
    fake.venv_fails_from.store(1, Ordering::SeqCst);

    let promoted = drive(&switchable, &fake, 1).await;

    assert!(promoted.is_none());
    assert_eq!(switchable.active().kind, BackendKind::Ort);
    assert_eq!(switchable.active().bootstrap, BootstrapState::Failed);
    let report = switchable.bootstrap_control().report();
    assert_eq!(report.next_retry_at, None, "a terminal failure clears it");
    assert_eq!(report.attempts, 2);
    assert!(
        report
            .last_error
            .as_deref()
            .is_some_and(|e| e.contains("fake venv rebuild failure")),
        "the report must say why: {report:?}"
    );
    tokio::time::sleep(Duration::from_secs(3 * 3600)).await;
    assert_eq!(fake.venv_checks.lock().expect("venv_checks lock").len(), 2);
    assert_eq!(
        fake.built(),
        1,
        "no attempt may run after the decided failure"
    );
}

/// Shutdown during the background wait stops the loop: the task finishes
/// inside the bound and no further attempt ever runs.
#[tokio::test(start_paused = true)]
async fn graceful_bootstrap_shutdown_cancels_a_pending_retry() {
    let switchable = ort_switchable();
    let fake = ContendedBootstrap::failing_probes(usize::MAX);
    spawn_drive(&switchable, &fake, 1);
    wait_until("a scheduled retry", || {
        switchable.active().bootstrap == BootstrapState::Retrying
    })
    .await;
    assert!(switchable
        .bootstrap_control()
        .report()
        .next_retry_at
        .is_some());

    assert!(
        switchable
            .bootstrap_control()
            .shutdown(Duration::from_secs(5))
            .await,
        "the retry task must stop inside the bound"
    );
    tokio::time::sleep(Duration::from_secs(3 * 3600)).await;
    assert_eq!(fake.built(), 1, "no attempt may run after shutdown");
    assert_eq!(fake.torn_down(), 1);
}

/// Shutdown during an attempt in flight tears that attempt's adapter down
/// before the task ends, so its python child cannot outlive the daemon.
#[tokio::test(start_paused = true)]
async fn graceful_bootstrap_shutdown_tears_down_an_attempt_in_flight() {
    let switchable = ort_switchable();
    let fake = ContendedBootstrap::with(usize::MAX, true, false);
    spawn_drive(&switchable, &fake, 1);
    wait_until("an attempt in flight", || fake.built() == 1).await;

    assert!(
        switchable
            .bootstrap_control()
            .shutdown(Duration::from_secs(5))
            .await
    );
    assert_eq!(
        fake.torn_down(),
        1,
        "the in-flight adapter must be torn down"
    );
    assert_eq!(
        switchable.active().kind,
        BackendKind::Ort,
        "ort keeps serving throughout"
    );
}

/// The background backoff is 2, 5, 10, then 30 minutes, holding at 30.
#[test]
fn background_retry_delay_holds_at_thirty_minutes() {
    let minutes: Vec<u64> = (0..7)
        .map(|round| background_retry_delay(round).as_secs() / 60)
        .collect();
    assert_eq!(minutes, vec![2, 5, 10, 30, 30, 30, 30]);
    assert_eq!(background_retry_delay(usize::MAX).as_secs(), 30 * 60);
}

/// #9970: after a transient probe failure, the wait before the next attempt
/// reads `retrying` and its note names the error. `run_attempt` resets the
/// step to `Idle` on a failed attempt; without that reset the wait would read
/// `probing` and the note would omit the error.
#[tokio::test(start_paused = true)]
#[serial_test::parallel]
async fn retry_wait_after_a_failed_probe_reads_retrying_with_the_error() {
    let switchable = ort_switchable();
    let fake = ContendedBootstrap::failing_probes(1);
    spawn_drive(&switchable, &fake, 1);
    wait_until("a scheduled retry", || {
        switchable.active().bootstrap == BootstrapState::Retrying
    })
    .await;

    let view = EmbedderView::of(&switchable);
    assert_eq!(view.bootstrap, "retrying", "{view:?}");
    let note = view.note.expect("ort serves while python is intended");
    assert!(note.contains("fake readiness probe failure"), "{note}");
    assert_eq!(
        fake.built(),
        1,
        "the next attempt waits for the retry delay"
    );
    assert!(
        switchable
            .bootstrap_control()
            .shutdown(Duration::from_secs(5))
            .await
    );
}

// #9971: the venv-build retry tests reuse this file's fakes.
#[path = "venv_retry_9971_tests.rs"]
mod venv_retry_9971;
