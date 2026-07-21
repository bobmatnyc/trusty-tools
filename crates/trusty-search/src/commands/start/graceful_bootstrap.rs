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
//! This PR implements SWAP-IN ONLY: once hot-swapped to python, this module
//! is done — it never runs again for the lifetime of the daemon. Detecting a
//! live python sidecar dying later and swapping back to ort is PR-4's scope;
//! the seam for that is the same `switchable` handle and the pid-slot /
//! stall-tracker machinery [`drive_bootstrap`] already wires up on success
//! (see the `TODO(PR-4)` below).
//!
//! Test: `graceful_bootstrap_swaps_to_python_on_success`,
//! `graceful_bootstrap_stays_ort_and_failed_after_probe_failure`,
//! `graceful_bootstrap_stays_ort_and_failed_after_bootstrap_failure`,
//! `graceful_bootstrap_retries_before_giving_up` in `start/tests.rs`.

use std::path::PathBuf;
use std::sync::atomic::AtomicU32;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;

use crate::core::Embedder;
use crate::service::embedder_supervisor::{
    ActiveBackend, BackendKind, BootstrapState, SwitchableEmbedder,
};
use crate::service::SearchAppState;

/// Default number of bootstrap attempts when `TRUSTY_PY_BOOTSTRAP_RETRIES`
/// is unset or malformed.
const DEFAULT_BOOTSTRAP_RETRIES: u32 = 2;

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
/// What: `ensure_venv` (blocking bootstrap), `locate_launcher` (binary
/// discovery), `build_adapter` (constructs the live `Arc<dyn Embedder>` +
/// optional pid slot from the located launcher), `probe_timeout` (bound on
/// the one real embed call `try_bootstrap_once` makes before hot-swapping).
/// Test: see the module-level `Test:` pointer.
pub(crate) trait PythonBootstrap: Send + Sync {
    fn ensure_venv(&self) -> Result<()>;
    fn locate_launcher(&self) -> Result<PathBuf>;
    fn build_adapter(&self, launcher: PathBuf) -> (Arc<dyn Embedder>, Option<Arc<AtomicU32>>);
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
    fn ensure_venv(&self) -> Result<()> {
        trusty_embedderd_py::ensure_venv_eager()
            .map(|_layout| ())
            .map_err(|e| e.context("py-embedder venv bootstrap"))
    }

    fn locate_launcher(&self) -> Result<PathBuf> {
        trusty_embedderd_py::locate_launcher_binary()
    }

    fn build_adapter(&self, launcher: PathBuf) -> (Arc<dyn Embedder>, Option<Arc<AtomicU32>>) {
        use crate::service::embedder_supervisor::LazyEmbedderHandle;

        let config = super::embedder::resolve_python_supervisor_config();
        let handle = Arc::new(LazyEmbedderHandle::new(launcher, config));
        let pid_slot = handle.app_pid_slot();
        let adapter: Arc<dyn Embedder> = Arc::new(super::embedder::LazySlotEmbedderAdapter {
            handle,
            is_python: true,
        });
        (adapter, Some(pid_slot))
    }

    fn probe_timeout(&self) -> Duration {
        Duration::from_secs(
            super::embedder::resolve_python_supervisor_config().startup_timeout_secs,
        )
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
/// completely unaffected regardless of outcome.
/// Test: `drive_bootstrap` (this function's callee) is what carries the
/// actual test coverage — this wrapper has no independent branches.
pub(super) async fn run_graceful_python_bootstrap(
    switchable: Arc<SwitchableEmbedder>,
    state: SearchAppState,
) {
    let ops: Arc<dyn PythonBootstrap> = Arc::new(RealPythonBootstrap);
    drive_bootstrap(switchable, state, ops, resolve_bootstrap_retries()).await;
}

/// Drive the ort→python hot-swap state machine: bootstrap the venv, locate
/// the launcher, build the adapter, probe it once, and hot-swap on success.
///
/// Why: isolated from [`run_graceful_python_bootstrap`] so tests can inject
/// a fake [`PythonBootstrap`] and a hand-built `SwitchableEmbedder` /
/// `SearchAppState` without touching any real subprocess, venv, or GPU.
/// What: retries up to `retries` times (linear backoff between attempts —
/// `2 * attempt` seconds); on success hot-swaps `switchable` to the python
/// adapter (`ActiveBackend::bootstrap = Ready`) and installs the pid slot on
/// `state`; on final exhaustion marks `switchable`'s bootstrap state
/// `Failed` via `SwitchableEmbedder::set_bootstrap_state` — the still-live
/// ort backend (`inner`) is never touched, so search keeps serving on ort
/// exactly as it was before this task ever ran.
///
/// TODO(PR-4): this function currently has no way to notice the python
/// sidecar dying AFTER a successful hot-swap and fall back to ort. The seam
/// for that is here: `switchable` and the installed pid slot / the daemon's
/// existing `embedder_stall_tracker` are already wired up by the time this
/// function returns `Ok` from a caller's perspective — PR-4 only needs to
/// add a supervising task that watches those and calls
/// `switchable.swap_to`/`set_bootstrap_state` again on death.
///
/// Test: `graceful_bootstrap_swaps_to_python_on_success`,
/// `graceful_bootstrap_stays_ort_and_failed_after_probe_failure`,
/// `graceful_bootstrap_stays_ort_and_failed_after_bootstrap_failure`,
/// `graceful_bootstrap_retries_before_giving_up` in `start/tests.rs`.
pub(super) async fn drive_bootstrap(
    switchable: Arc<SwitchableEmbedder>,
    state: SearchAppState,
    ops: Arc<dyn PythonBootstrap>,
    retries: u32,
) {
    let retries = retries.max(1);
    let mut last_err = None;

    for attempt in 1..=retries {
        match try_bootstrap_once(&switchable, &state, &ops).await {
            Ok(()) => {
                tracing::info!("embedder hot-swapped ort -> python/MPS (sidecar ready)");
                return;
            }
            Err(e) => {
                tracing::warn!(
                    "graceful python bootstrap attempt {attempt}/{retries} failed \
                     ({e:#}) — staying on the ort sidecar"
                );
                last_err = Some(e);
                if attempt < retries {
                    tokio::time::sleep(Duration::from_secs(2 * u64::from(attempt))).await;
                }
            }
        }
    }

    switchable.set_bootstrap_state(BootstrapState::Failed);
    tracing::warn!(
        "graceful python bootstrap exhausted {retries} attempt(s) — staying permanently \
         on the ort sidecar for this daemon's lifetime (last error: {:#}); restart to \
         retry, or set TRUSTY_EMBEDDER=python for the eager blocking path",
        last_err.expect("at least one attempt runs when retries >= 1"),
    );
}

/// One bootstrap→probe→swap attempt. See [`drive_bootstrap`] for the retry
/// loop around this.
async fn try_bootstrap_once(
    switchable: &SwitchableEmbedder,
    state: &SearchAppState,
    ops: &Arc<dyn PythonBootstrap>,
) -> Result<()> {
    // Step a: blocking venv bootstrap off the async runtime — mirrors the
    // eager `TRUSTY_EMBEDDER=python` arm's `ensure_venv_eager` off-thread call.
    let venv_ops = Arc::clone(ops);
    tokio::task::spawn_blocking(move || venv_ops.ensure_venv())
        .await
        .map_err(|e| anyhow::anyhow!("py-embedder bootstrap task panicked: {e}"))??;

    // Step b: locate the launcher binary.
    let launcher = ops.locate_launcher()?;

    // Step c: build the python `LazyEmbedderHandle` + adapter exactly like
    // the existing eager `python` arm.
    let (adapter, pid_slot) = ops.build_adapter(launcher);
    let probe_timeout = ops.probe_timeout();

    // Step d: readiness probe — force the lazy spawn + torch import + model
    // load + one real embed THROUGH the supervisor, bounded by a timeout.
    let probe = tokio::time::timeout(
        probe_timeout,
        adapter.embed_batch(&["trusty readiness probe"]),
    )
    .await;

    match probe {
        Ok(Ok(_vectors)) => {}
        Ok(Err(e)) => {
            // Drop our only reference to the adapter so the underlying
            // `LazyEmbedderHandle` (if the probe spawned a child) is
            // reclaimed via its own idle-shutdown watchdog rather than left
            // dangling — see PR-4's seam note on `drive_bootstrap`.
            drop(adapter);
            return Err(e.context("python readiness probe failed"));
        }
        Err(_elapsed) => {
            drop(adapter);
            anyhow::bail!(
                "python readiness probe timed out after {}s",
                probe_timeout.as_secs()
            );
        }
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

    Ok(())
}
