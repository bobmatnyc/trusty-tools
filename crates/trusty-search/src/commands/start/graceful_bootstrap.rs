//! Background bootstrap→hot-swap orchestrator for the graceful Apple-Silicon
//! default (epic #3524 slice 6, PR 3/5).
//!
//! Why: `DefaultEmbedderMode::GracefulPython` (see `embedder.rs`) serves on
//! the ort stdio sidecar immediately so the HTTP listener never blocks on a
//! multi-second-to-multi-minute python/MPS venv bootstrap. Something still
//! has to DO that bootstrap, though — off the request path, with retries,
//! and hot-swap the `SwitchableEmbedder` (epic #3524 PR-1) over to the
//! python adapter once (and only once) it has proven itself with a real
//! embed call through the supervisor. This module is that "something":
//! `commands::start::daemon` spawns [`run_graceful_python_bootstrap`] as a
//! detached `tokio::spawn` task right after installing the switchable
//! handle, and this module owns the whole retry/probe/swap state machine.
//!
//! What: [`PythonBootstrap`] abstracts the three fallible, slow, real-world
//! steps (venv bootstrap, launcher discovery, adapter construction) behind a
//! trait so [`drive_bootstrap`] — the actual retry/probe/swap logic — can be
//! unit tested with deterministic fakes; no real `uv`/torch/MPS ever runs in
//! CI for these tests. [`RealPythonBootstrap`] is the only production
//! implementation, reusing the exact same steps the pre-existing eager
//! `TRUSTY_EMBEDDER=python` arm performs
//! (`embedder::build_eager_python_embedder`).
//!
//! PR-3 implemented SWAP-IN ONLY. PR-4 (epic #3524 slice 6) adds the other
//! half: once [`drive_bootstrap`] hot-swaps to python, [`run_graceful_python_bootstrap`]
//! spawns `start::swap_back_watchdog::run_swap_back_watchdog` — a sibling
//! detached task that watches the same `switchable` handle and the pid-slot /
//! stall-tracker machinery this module already wires up on success, and
//! swaps back to ort if the python sidecar is ever confirmed dead beyond
//! recovery. See that module's docs for the swap-back predicate.
//!
//! #9949: a probe that fails every startup attempt no longer pins the daemon
//! to ort. [`drive_bootstrap`] keeps retrying in the background with capped
//! backoff ([`background_retry_delay`]) until one attempt succeeds; only a
//! permanent failure (missing launcher, venv build error) is terminal.
//! #9468: each attempt locates the launcher before any venv work, and a
//! permanent failure ends the bootstrap on its first occurrence.
//!
//! Test: `graceful_bootstrap_swaps_to_python_on_success`,
//! `graceful_bootstrap_stays_ort_and_retrying_after_probe_failure`,
//! `graceful_bootstrap_stays_ort_and_failed_after_bootstrap_failure`,
//! `graceful_bootstrap_retries_before_giving_up`,
//! `graceful_bootstrap_probe_failure_tears_down_the_handle`,
//! `graceful_bootstrap_probe_timeout_tears_down_the_handle` in
//! `start/tests.rs`; `graceful_bootstrap_promotes_to_python_after_contention_window`
//! and its siblings in `start/tests_9949.rs`.

use std::path::PathBuf;
use std::sync::atomic::AtomicU32;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use tokio_util::sync::CancellationToken;

use crate::core::Embedder;
use crate::service::embedder_supervisor::bootstrap_control::{BootstrapPhase, BootstrapReport};
use crate::service::embedder_supervisor::{
    ActiveBackend, BackendKind, BootstrapState, SupervisorConfig, SwitchableEmbedder,
};
use crate::service::SearchAppState;

/// Default number of bootstrap attempts when `TRUSTY_PY_BOOTSTRAP_RETRIES`
/// is unset or malformed.
const DEFAULT_BOOTSTRAP_RETRIES: u32 = 2;

/// Ceiling on the per-attempt readiness-probe budget multiplier (#4125
/// follow-up, review finding 1 on PR #4771).
///
/// Why 3, and why a multiplier rather than an absolute duration: the base is
/// `TRUSTY_EMBEDDERD_STARTUP_TIMEOUT_SECS` (default 30 s) — an operator's own
/// knob. Capping the multiplier bounds only the amplification THIS module
/// invented and leaves the operator's base authoritative; an absolute ceiling
/// would silently override a deliberately-raised base on a slow machine.
///
/// 3 is generous enough for the failure this scaling exists to survive. The
/// observed incident was a 30 s probe dying under a warm-boot storm of
/// hundreds of index restores, and the very next daemon start — without that
/// storm — succeeded on the same unchanged 30 s budget. The deficit is
/// therefore startup contention, which ends, not a fundamentally larger cost:
/// 90 s at the default is three times the budget that was observed to fail and
/// comfortably outlasts the contention window.
///
/// And it keeps the worst case bounded. `TRUSTY_PY_BOOTSTRAP_RETRIES` is
/// operator-settable with no upper bound, so an unbounded `base * attempt`
/// grows without limit: attempt 100 would hold one live python child on a
/// single probe for 50 minutes. Capped, every attempt is at most 3x the base
/// no matter how many are configured, while the default 2-retry case is
/// unchanged (30 s + 60 s).
const MAX_PROBE_TIMEOUT_MULTIPLIER: u32 = 3;

/// #9949: delays before each background retry, once the startup attempts are
/// spent. The last entry repeats for as long as the daemon runs.
const BACKGROUND_RETRY_DELAYS: [Duration; 4] = [
    Duration::from_secs(2 * 60),
    Duration::from_secs(5 * 60),
    Duration::from_secs(10 * 60),
    Duration::from_secs(30 * 60),
];

/// Delay before background retry `round` (0-based): 2, 5, 10, then 30
/// minutes for every later round.
/// Test: `background_retry_delay_holds_at_thirty_minutes` in `start/tests_9949.rs`.
pub(super) fn background_retry_delay(round: usize) -> Duration {
    let last = BACKGROUND_RETRY_DELAYS[BACKGROUND_RETRY_DELAYS.len() - 1];
    BACKGROUND_RETRY_DELAYS.get(round).copied().unwrap_or(last)
}

/// Which venv recheck an attempt runs before spawning (#9949).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum VenvCheck {
    /// `ensure_venv_eager`: a full `import sentence_transformers` recheck,
    /// about 70 s on a contended host. Only the daemon's first attempt.
    FullImport,
    /// `ensure_venv`: the torch-free liveness check the launcher itself runs
    /// on every spawn. Every later attempt.
    Liveness,
}

/// Why an attempt did not swap to python (#9949). Build it with
/// [`AttemptError::at`], which decides permanent vs transient.
#[derive(Debug)]
enum AttemptError {
    /// Retrying cannot help, so the bootstrap ends on the first one.
    Permanent(anyhow::Error),
    /// A later attempt may pass; the #9949 retry loop keeps going.
    Transient(anyhow::Error),
    /// Daemon shutdown cancelled the attempt.
    Cancelled,
}

/// The step an attempt failed at (#9468).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FailedStep {
    /// `locate_launcher`: no `trusty-embedderd-py` binary.
    Launcher,
    /// `ensure_venv` returned an error, or its blocking task panicked.
    Venv,
    /// The readiness probe failed or timed out.
    Probe,
}

impl AttemptError {
    /// The one place that decides whether a failed step is retried (#9468).
    ///
    /// Why: a missing launcher was retried, and each retry rebuilt the venv
    /// (a ~3 GB torch download) before failing the same lookup again.
    /// What: `Launcher` and `Venv` are permanent: another attempt repeats the
    /// same missing binary or the same failed build. `Probe` is transient: a
    /// contended host can answer minutes later (#9949).
    /// Test: `graceful_bootstrap_permanent_failure_ends_the_retry_loop`,
    /// `graceful_bootstrap_transient_failure_keeps_retrying` in `start/tests_9949.rs`.
    fn at(step: FailedStep, e: anyhow::Error) -> Self {
        match step {
            FailedStep::Launcher | FailedStep::Venv => Self::Permanent(e),
            FailedStep::Probe => Self::Transient(e),
        }
    }
}

/// Abstracts "the thing that must be torn down if the readiness probe fails
/// or times out after an adapter/handle has already been built" — real:
/// `LazyEmbedderHandle::shutdown()`; fakes: a spy that records the call
/// (epic #3524 slice 6 PR-3 fix — code-critic HIGH: orphaned python child on
/// bootstrap-probe failure).
///
/// Why: the readiness probe (`adapter.embed_batch(...)` in
/// [`try_bootstrap_once`]) forces `LazyEmbedderHandle` to spawn a REAL child
/// process (torch+MPS, hundreds of MB-GB). If the probe then fails or times
/// out, simply dropping the adapter left that child running until the
/// detached idle watchdog reaped it — up to 1800s later for the python arm,
/// with up to `TRUSTY_PY_BOOTSTRAP_RETRIES` such orphans alive concurrently.
/// This trait is the seam that lets [`try_bootstrap_once`] force an
/// IMMEDIATE, cooperative kill on every failure path that occurs after a
/// handle may have spawned, before ever dropping it.
/// What: `teardown()`, async so the real implementation can await
/// `SupervisorHandle::shutdown()`'s "confirmed dead" guarantee; plus
/// `is_confirmed_terminated()` (epic #3524 slice 6 PR-4 follow-up,
/// code-critic BLOCK on PR #3584) — the DEFINITIVE "this python sidecar's
/// supervisor gave up permanently" signal the swap-back watchdog gates on,
/// replacing the earlier ambiguous pid==0 + stall-tracker heuristic. See
/// `LazyEmbedderHandle::is_confirmed_terminated`'s doc for the full
/// false-positive analysis that signal fixes.
/// Test: see the module-level `Test:` pointer.
#[async_trait::async_trait]
pub(crate) trait PythonAdapterTeardown: Send + Sync {
    async fn teardown(&self);
    async fn is_confirmed_terminated(&self) -> bool;
}

#[async_trait::async_trait]
impl PythonAdapterTeardown for crate::service::embedder_supervisor::LazyEmbedderHandle {
    async fn teardown(&self) {
        self.shutdown().await;
    }

    async fn is_confirmed_terminated(&self) -> bool {
        crate::service::embedder_supervisor::LazyEmbedderHandle::is_confirmed_terminated(self).await
    }
}

/// Abstracts the three fallible, real-world steps of standing up the python
/// sidecar (venv bootstrap, launcher discovery, adapter construction) plus
/// the probe timeout, so [`drive_bootstrap`] — the retry/probe/swap state
/// machine — is unit-testable with deterministic fakes.
///
/// Why: `trusty_embedderd_py::ensure_venv_eager` and `locate_launcher_binary`
/// do real, slow, disk/network-dependent work (see their own docs); a real
/// python/MPS adapter needs actual torch + a real GPU to serve the readiness
/// probe. None of that is available or desirable in CI. [`RealPythonBootstrap`]
/// is the only production implementation; tests substitute fakes that fail or
/// succeed deterministically at each step.
/// What: `ensure_venv` (blocking bootstrap, at the [`VenvCheck`] depth the
/// attempt asks for), `locate_launcher` (binary discovery), `build_adapter`
/// (constructs the live `Arc<dyn Embedder>` + optional pid slot + the
/// [`PythonAdapterTeardown`] handle for that same adapter, from the located
/// launcher, with `startup_budget` as the supervisor's own spawn timeout —
/// #9949), `probe_timeout` (the BASE bound on the one real embed call
/// `try_bootstrap_once` makes before hot-swapping; see [`probe_budget`]).
/// Test: see the module-level `Test:` pointer.
pub(crate) trait PythonBootstrap: Send + Sync {
    fn ensure_venv(&self, check: VenvCheck) -> Result<()>;
    fn locate_launcher(&self) -> Result<PathBuf>;
    #[allow(clippy::type_complexity)]
    fn build_adapter(
        &self,
        launcher: PathBuf,
        startup_budget: Duration,
    ) -> (
        Arc<dyn Embedder>,
        Option<Arc<AtomicU32>>,
        Arc<dyn PythonAdapterTeardown>,
    );
    fn probe_timeout(&self) -> Duration;
}

/// Production [`PythonBootstrap`]: the exact same steps the pre-existing
/// eager `TRUSTY_EMBEDDER=python` arm performs
/// (`embedder::build_eager_python_embedder`), reused here for the background
/// graceful path. The probe timeout reuses the existing, already-tunable
/// `TRUSTY_EMBEDDERD_STARTUP_TIMEOUT_SECS` (via `SupervisorConfig`) rather
/// than introducing a new env var for a single bounded async call.
struct RealPythonBootstrap;

impl PythonBootstrap for RealPythonBootstrap {
    fn ensure_venv(&self, check: VenvCheck) -> Result<()> {
        // #9949: only the first attempt pays for the full-import recheck.
        let ensured = match check {
            VenvCheck::FullImport => trusty_embedderd_py::ensure_venv_eager(),
            VenvCheck::Liveness => trusty_embedderd_py::ensure_venv(),
        };
        ensured
            .map(|_layout| ())
            .map_err(|e| e.context("py-embedder venv bootstrap"))
    }

    fn locate_launcher(&self) -> Result<PathBuf> {
        trusty_embedderd_py::locate_launcher_binary()
    }

    fn build_adapter(
        &self,
        launcher: PathBuf,
        startup_budget: Duration,
    ) -> (
        Arc<dyn Embedder>,
        Option<Arc<AtomicU32>>,
        Arc<dyn PythonAdapterTeardown>,
    ) {
        use crate::service::embedder_supervisor::LazyEmbedderHandle;

        let config = adapter_supervisor_config(
            super::embedder::resolve_python_supervisor_config(),
            startup_budget,
        );
        let handle = Arc::new(LazyEmbedderHandle::new(launcher, config));
        let pid_slot = handle.app_pid_slot();
        let teardown: Arc<dyn PythonAdapterTeardown> = Arc::clone(&handle) as _;
        let adapter: Arc<dyn Embedder> = Arc::new(super::embedder::LazySlotEmbedderAdapter {
            handle,
            is_python: true,
        });
        (adapter, Some(pid_slot), teardown)
    }

    fn probe_timeout(&self) -> Duration {
        Duration::from_secs(
            super::embedder::resolve_python_supervisor_config().startup_timeout_secs,
        )
    }
}

/// The python supervisor config for one attempt: `base` with
/// `startup_timeout_secs` raised to that attempt's budget (#9949).
///
/// Why: the attempt budget used to scale only the outer probe timeout. The
/// supervisor kept the flat `TRUSTY_EMBEDDERD_STARTUP_TIMEOUT_SECS` and killed
/// every spawn at 30 s, so attempt 2's advertised 60 s never took effect.
/// What: sets `startup_timeout_secs` to `startup_budget` in whole seconds,
/// rounded up and at least 1; every other field is kept.
/// Test: `attempt_two_adapter_gets_twice_the_base_budget` in `start/tests_9949.rs`.
pub(super) fn adapter_supervisor_config(
    mut base: SupervisorConfig,
    startup_budget: Duration,
) -> SupervisorConfig {
    let secs = startup_budget.as_secs() + u64::from(startup_budget.subsec_nanos() > 0);
    base.startup_timeout_secs = secs.max(1);
    base
}

/// The readiness-probe budget for one attempt, plus enough context for the
/// timeout log line to say whether the ceiling was applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ProbeBudget {
    /// The budget actually handed to `tokio::time::timeout`.
    pub timeout: Duration,
    /// The multiplier applied to the base — always in `1..=MAX_PROBE_TIMEOUT_MULTIPLIER`.
    pub multiplier: u32,
    /// True when `attempt` exceeded [`MAX_PROBE_TIMEOUT_MULTIPLIER`] and the
    /// ceiling, rather than the attempt number, decided the budget.
    pub capped: bool,
}

/// Scale the readiness-probe budget by attempt number, bounded by
/// [`MAX_PROBE_TIMEOUT_MULTIPLIER`].
///
/// Why: see [`MAX_PROBE_TIMEOUT_MULTIPLIER`] for the ceiling's justification.
/// What: multiplier = `attempt` clamped to `1..=MAX_PROBE_TIMEOUT_MULTIPLIER`;
/// the product is computed with `checked_mul` so a pathologically large
/// operator base cannot panic on `Duration` overflow (which `Duration * u32`
/// does, in release as well as debug) — an un-multipliable base is already
/// effectively unbounded, so it is used as-is.
/// Test: `probe_budget_is_capped_at_the_multiplier_ceiling`,
/// `probe_budget_saturates_instead_of_overflowing` in `start/tests.rs`.
pub(super) fn probe_budget(base: Duration, attempt: u32) -> ProbeBudget {
    let multiplier = attempt.clamp(1, MAX_PROBE_TIMEOUT_MULTIPLIER);
    ProbeBudget {
        timeout: base.checked_mul(multiplier).unwrap_or(base),
        multiplier,
        capped: attempt > MAX_PROBE_TIMEOUT_MULTIPLIER,
    }
}

/// Resolve `TRUSTY_PY_BOOTSTRAP_RETRIES` (default 2, clamped to a minimum of
/// 1 — a malformed or zero value falls back to the default rather than
/// silently never retrying).
/// Test: `resolve_bootstrap_retries_*` in `start/tests.rs`.
pub(super) fn resolve_bootstrap_retries() -> u32 {
    std::env::var("TRUSTY_PY_BOOTSTRAP_RETRIES")
        .ok()
        .and_then(|v| v.trim().parse::<u32>().ok())
        .filter(|&n| n >= 1)
        .unwrap_or(DEFAULT_BOOTSTRAP_RETRIES)
}

/// Entry point spawned by `commands::start::daemon` right after the embedder
/// + switchable handle are installed (epic #3524 slice 6, PR 3/5).
///
/// Why: the daemon's init task is the only place that has both the
/// `Arc<SwitchableEmbedder>` handle and the `SearchAppState` (needed to
/// install the python pid slot on success) at the same time.
/// What: constructs the production [`RealPythonBootstrap`] and delegates to
/// [`drive_bootstrap`]. Never panics — every failure path is caught and
/// logged; the daemon and the ort backend it is already serving on are
/// completely unaffected regardless of outcome. On a successful hot-swap
/// (epic #3524 slice 6, PR 4/5), spawns the swap-back watchdog as a detached
/// sibling task so a later confirmed death of the python sidecar falls back
/// to ort without anyone needing to restart the daemon.
/// Test: `drive_bootstrap` (this function's callee) is what carries the
/// actual test coverage for the bootstrap state machine itself; the
/// watchdog hand-off has no independent branches worth a dedicated test here
/// (it is one `if let Some`) — `start::swap_back_watchdog`'s own tests cover
/// the watchdog's behavior.
pub(super) async fn run_graceful_python_bootstrap(
    switchable: Arc<SwitchableEmbedder>,
    state: SearchAppState,
) {
    let ops: Arc<dyn PythonBootstrap> = Arc::new(RealPythonBootstrap);
    if let Some(python_teardown) = drive_bootstrap(
        Arc::clone(&switchable),
        state.clone(),
        ops,
        resolve_bootstrap_retries(),
    )
    .await
    {
        let swap_back_ops = super::swap_back_watchdog::real_swap_back_ops();
        tokio::spawn(super::swap_back_watchdog::run_swap_back_watchdog(
            switchable,
            state,
            python_teardown,
            swap_back_ops,
        ));
    }
}

/// Drive the ort→python hot-swap state machine: locate the launcher,
/// bootstrap the venv, build the adapter, probe it once, and hot-swap on success.
///
/// Why: isolated from [`run_graceful_python_bootstrap`] so tests can inject
/// a fake [`PythonBootstrap`] and a hand-built `SwitchableEmbedder` /
/// `SearchAppState` without touching any real subprocess, venv, or GPU.
/// #9949: on a slow host every startup attempt can time out while python
/// would answer minutes later; giving up then pinned the daemon to ort for
/// its whole uptime.
/// What: makes up to `retries` startup attempts (linear backoff, `2 *
/// attempt` seconds). The first permanent error ([`AttemptError::at`]) marks
/// the bootstrap `Failed` and returns, in a startup or a background attempt
/// (#9468). If every startup attempt failed on the probe, marks it `Retrying`
/// and keeps attempting in the background, waiting [`background_retry_delay`]
/// between rounds, until an attempt succeeds, a permanent error ends it, or
/// the switchable's `BootstrapControl` is shut down. Every failed attempt records its error in
/// that control's report before logging it. On success hot-swaps `switchable`
/// to the python adapter (`ActiveBackend::bootstrap = Ready`) and installs the
/// pid slot on `state`. The live ort backend (`inner`) is never touched
/// otherwise, so search keeps serving on ort throughout.
///
/// Returns the python adapter's [`PythonAdapterTeardown`] handle on a
/// successful hot-swap (epic #3524 slice 6, PR 4/5); `None` on a permanent
/// failure or a shutdown. [`run_graceful_python_bootstrap`] uses the `Some`
/// case to hand the live python handle's teardown to the swap-back watchdog.
///
/// Test: `graceful_bootstrap_swaps_to_python_on_success`,
/// `graceful_bootstrap_stays_ort_and_retrying_after_probe_failure`,
/// `graceful_bootstrap_stays_ort_and_failed_after_bootstrap_failure`,
/// `graceful_bootstrap_retries_before_giving_up`,
/// `graceful_bootstrap_gives_up_after_exhausting_retries` in `start/tests.rs`;
/// `graceful_bootstrap_promotes_to_python_after_contention_window`,
/// `graceful_bootstrap_missing_launcher_builds_no_venv`,
/// `graceful_bootstrap_permanent_failure_ends_the_retry_loop`,
/// `graceful_bootstrap_transient_failure_keeps_retrying`,
/// `graceful_bootstrap_shutdown_cancels_a_pending_retry`,
/// `graceful_bootstrap_background_decided_failure_is_terminal` in `start/tests_9949.rs`.
pub(super) async fn drive_bootstrap(
    switchable: Arc<SwitchableEmbedder>,
    state: SearchAppState,
    ops: Arc<dyn PythonBootstrap>,
    retries: u32,
) -> Option<Arc<dyn PythonAdapterTeardown>> {
    let retries = retries.max(1);
    let cancel = switchable.bootstrap_control().cancel_token();
    let mut attempt = 0u32;

    loop {
        attempt += 1;
        match run_attempt(&switchable, &state, &ops, attempt, &cancel).await {
            Ok(python_teardown) => return Some(python_teardown),
            Err(AttemptError::Cancelled) => return None,
            // #9468: a permanent failure ends the bootstrap at once; another
            // startup attempt would repeat it, venv build included.
            Err(AttemptError::Permanent(e)) => {
                give_up(&switchable, attempt, &e);
                return None;
            }
            Err(AttemptError::Transient(e)) => {
                tracing::warn!(
                    "graceful python bootstrap attempt {attempt}/{retries} failed \
                     ({e:#}) — staying on the ort sidecar"
                );
                if attempt >= retries {
                    break;
                }
                let backoff = Duration::from_secs(2 * u64::from(attempt));
                if !sleep_unless_cancelled(backoff, &cancel).await {
                    return None;
                }
            }
        }
    }

    // #9949: the probe failed, not the setup — keep retrying while ort serves.
    let mut round = 0usize;
    loop {
        let delay = background_retry_delay(round);
        round = round.saturating_add(1);
        let next_retry_at = std::time::SystemTime::now() + delay;
        switchable
            .bootstrap_control()
            .update_report(|r| BootstrapReport {
                next_retry_at: Some(next_retry_at),
                ..r.clone()
            });
        switchable.set_bootstrap_state(BootstrapState::Retrying);
        tracing::warn!(
            "graceful python bootstrap: {attempt} attempt(s) failed — still serving on \
             the ort sidecar; retrying in {}s",
            delay.as_secs()
        );
        if !sleep_unless_cancelled(delay, &cancel).await {
            return None;
        }
        attempt += 1;
        match run_attempt(&switchable, &state, &ops, attempt, &cancel).await {
            Ok(python_teardown) => return Some(python_teardown),
            Err(AttemptError::Cancelled) => return None,
            Err(AttemptError::Permanent(e)) => {
                give_up(&switchable, attempt, &e);
                return None;
            }
            Err(AttemptError::Transient(e)) => tracing::warn!(
                "graceful python bootstrap background attempt {attempt} failed ({e:#}) — \
                 staying on the ort sidecar"
            ),
        }
    }
}

/// The log/report text for a failed attempt.
fn attempt_error_text(e: &AttemptError) -> String {
    match e {
        AttemptError::Permanent(e) | AttemptError::Transient(e) => format!("{e:#}"),
        AttemptError::Cancelled => "cancelled by daemon shutdown".to_string(),
    }
}

/// Sleep for `delay`; `false` when `cancel` fired first.
async fn sleep_unless_cancelled(delay: Duration, cancel: &CancellationToken) -> bool {
    tokio::select! {
        biased;
        () = cancel.cancelled() => false,
        () = tokio::time::sleep(delay) => true,
    }
}

/// Record the step the running attempt is on (#9970).
fn set_phase(switchable: &SwitchableEmbedder, phase: BootstrapPhase) {
    switchable
        .bootstrap_control()
        .update_report(|r| BootstrapReport { phase, ..r.clone() });
}

/// Mark the bootstrap terminally `Failed` on a permanent error (#9949).
fn give_up(switchable: &SwitchableEmbedder, attempts: u32, e: &anyhow::Error) {
    switchable
        .bootstrap_control()
        .update_report(|r| BootstrapReport {
            next_retry_at: None,
            ..r.clone()
        });
    switchable.set_bootstrap_state(BootstrapState::Failed);
    tracing::warn!(
        "graceful python bootstrap gave up after {attempts} attempt(s) — staying \
         permanently on the ort sidecar for this daemon's lifetime (last error: {e:#}); \
         retrying cannot fix this, so fix the cause and restart, or set \
         TRUSTY_EMBEDDER=python for the eager blocking path"
    );
}

/// One attempt plus its report bookkeeping (#9949).
///
/// Fail-open check: a failed attempt writes its error into the report BEFORE
/// the caller logs it, so `/health` always says why, never only a WARN line.
async fn run_attempt(
    switchable: &SwitchableEmbedder,
    state: &SearchAppState,
    ops: &Arc<dyn PythonBootstrap>,
    attempt: u32,
    cancel: &CancellationToken,
) -> std::result::Result<Arc<dyn PythonAdapterTeardown>, AttemptError> {
    let control = switchable.bootstrap_control();
    control.update_report(|r| BootstrapReport {
        attempts: attempt,
        next_retry_at: None,
        ..r.clone()
    });
    let check = if attempt == 1 {
        VenvCheck::FullImport
    } else {
        VenvCheck::Liveness
    };
    let outcome = try_bootstrap_once(switchable, state, ops, attempt, check, cancel).await;
    match &outcome {
        // #9970: the attempt is over, so no step is running.
        Ok(_) => control.update_report(|r| BootstrapReport {
            last_error: None,
            phase: BootstrapPhase::Idle,
            ..r.clone()
        }),
        Err(AttemptError::Cancelled) => {}
        Err(e) => {
            let text = attempt_error_text(e);
            control.update_report(|r| BootstrapReport {
                last_error: Some(text.clone()),
                phase: BootstrapPhase::Idle,
                ..r.clone()
            });
        }
    }
    outcome
}

/// One bootstrap→probe→swap attempt. See [`drive_bootstrap`] for the retry
/// loop around this. Returns the newly-installed python adapter's
/// [`PythonAdapterTeardown`] handle on success (epic #3524 slice 6, PR 4/5)
/// — the SAME underlying handle just installed into `switchable`, viewed
/// through the teardown trait so the swap-back watchdog can shut it down
/// later without needing to downcast the trait object `switchable` now
/// holds.
///
/// #4125: `attempt` (1-based) scales the readiness-probe budget. The probe
/// forces a cold python spawn — torch import, model load, one real embed —
/// while the daemon is simultaneously warm-booting its indexes, and the flat
/// `TRUSTY_EMBEDDERD_STARTUP_TIMEOUT_SECS` budget is measurably too tight under
/// that self-inflicted load. The budget grows with the retry, bounded by
/// [`MAX_PROBE_TIMEOUT_MULTIPLIER`]; see [`probe_budget`]. #9949: the same
/// budget now reaches the supervisor through `build_adapter`, so the spawn is
/// not cut off at the flat base first.
///
/// Errors are classified for the retry loop by [`AttemptError::at`], and
/// `cancel` firing is [`AttemptError::Cancelled`]. A cancel during the probe
/// tears the just-spawned handle down before returning, like a failed probe
/// does.
async fn try_bootstrap_once(
    switchable: &SwitchableEmbedder,
    state: &SearchAppState,
    ops: &Arc<dyn PythonBootstrap>,
    attempt: u32,
    check: VenvCheck,
    cancel: &CancellationToken,
) -> std::result::Result<Arc<dyn PythonAdapterTeardown>, AttemptError> {
    // Step a: locate the launcher binary.
    // #9468: before any venv work, so a missing launcher creates no venv
    // directory, calls no uv and downloads no torch.
    let launcher = ops.locate_launcher().map_err(|e| {
        AttemptError::at(
            FailedStep::Launcher,
            e.context("python sidecar launcher not found"),
        )
    })?;

    // Step b: blocking venv bootstrap off the async runtime. A shutdown does
    // not wait for it: the blocking thread finishes on its own and spawns no
    // sidecar. #9970: the agent note says the venv is installing.
    set_phase(switchable, BootstrapPhase::InstallingVenv);
    let venv_ops = Arc::clone(ops);
    let venv = tokio::task::spawn_blocking(move || venv_ops.ensure_venv(check));
    let venv = tokio::select! {
        biased;
        () = cancel.cancelled() => return Err(AttemptError::Cancelled),
        joined = venv => joined,
    };
    match venv {
        Ok(Ok(())) => {}
        Ok(Err(e)) => return Err(AttemptError::at(FailedStep::Venv, e)),
        Err(e) => {
            return Err(AttemptError::at(
                FailedStep::Venv,
                anyhow::anyhow!("py-embedder bootstrap task panicked: {e}"),
            ))
        }
    }

    // Step c: build the python `LazyEmbedderHandle` + adapter. `teardown` is
    // the SAME underlying handle as `adapter` (viewed through
    // `PythonAdapterTeardown`) — see that trait's doc for why an explicit,
    // awaited teardown is required on every failure path below.
    // #4125: attempt N gets N x the base budget, bounded by
    // MAX_PROBE_TIMEOUT_MULTIPLIER. #9949: the supervisor gets it too.
    let base = ops.probe_timeout();
    let budget = probe_budget(base, attempt);
    let (adapter, pid_slot, teardown) = ops.build_adapter(launcher, budget.timeout);

    // Step d: readiness probe — force the lazy spawn + torch import + model
    // load + one real embed THROUGH the supervisor, bounded by the budget.
    set_phase(switchable, BootstrapPhase::Probing);
    let probe = tokio::select! {
        biased;
        () = cancel.cancelled() => None,
        probed = tokio::time::timeout(
            budget.timeout,
            adapter.embed_batch(&["trusty readiness probe"]),
        ) => Some(probed),
    };

    let failure = match probe {
        Some(Ok(Ok(_vectors))) => None,
        // #9949: shutdown landed mid-probe; the dropped spawn future already
        // killed a half-started child, and teardown stops a started one.
        None => Some(AttemptError::Cancelled),
        Some(Ok(Err(e))) => Some(AttemptError::at(
            FailedStep::Probe,
            e.context("python readiness probe failed"),
        )),
        // #4125: name the multiplier AND whether the ceiling decided it.
        Some(Err(_elapsed)) => Some(AttemptError::at(
            FailedStep::Probe,
            anyhow::anyhow!(
                "python readiness probe timed out after {}s (attempt {attempt}: {}x the {}s \
             base budget{})",
                budget.timeout.as_secs(),
                budget.multiplier,
                base.as_secs(),
                if budget.capped {
                    format!(", capped at the {MAX_PROBE_TIMEOUT_MULTIPLIER}x ceiling")
                } else {
                    String::new()
                }
            ),
        )),
    };
    if let Some(failure) = failure {
        // code-critic HIGH fix (epic #3524 slice 6 PR-3): the probe may have
        // forced a REAL child spawn. Kill it now, BEFORE dropping our only
        // references, so a failed or cancelled attempt never orphans a
        // python/MPS process waiting on the idle watchdog.
        teardown.teardown().await;
        drop(adapter);
        return Err(failure);
    }

    // Step e: hot-swap. Read `provider()` before moving `adapter` into
    // `swap_to`.
    let provider = adapter.provider();
    switchable.swap_to(
        adapter,
        ActiveBackend {
            kind: BackendKind::Python,
            provider,
            model: super::embedder::EMBEDDER_MODEL_NAME.to_string(),
            quantized: false,
            bootstrap: BootstrapState::Ready,
        },
    );

    // Install the python pid slot so `/health` RSS tracking follows the new
    // child instead of the (still technically alive, but no longer serving)
    // ort sidecar.
    if let Some(slot) = pid_slot {
        state.install_embedderd_pid_slot(slot).await;
    }

    Ok(teardown)
}
