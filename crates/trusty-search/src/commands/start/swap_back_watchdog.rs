//! Swap-BACK watchdog: python/MPS → ort on confirmed sidecar death (epic
//! #3524 slice 6, PR 4/5).
//!
//! Why: PR-3's background orchestrator (`graceful_bootstrap`) hot-swaps
//! ort → python once the sidecar proves itself with a real embed call, then
//! is done — nothing ever watches that python sidecar again. If it later
//! dies for good (the underlying `EmbedderSupervisor` exhausts
//! `TRUSTY_EMBEDDERD_MAX_RESTARTS` and gives up), search would silently keep
//! trying to route through a dead backend forever, with no automatic
//! recovery short of a full daemon restart. This module is the other half:
//! once [`run_swap_back_watchdog`] is spawned (by
//! `graceful_bootstrap::run_graceful_python_bootstrap`, right after a
//! successful hot-swap), it watches for CONFIRMED death and swaps the
//! `SwitchableEmbedder` back to a fresh ort backend so search never degrades
//! permanently.
//!
//! ## History: the pid+stall-tracker heuristic was UNSAFE (code-critic
//! ## BLOCK on PR #3584) — fixed via a definitive supervisor signal
//!
//! The first version of this module inferred death from
//! `active.kind == Python && pid_slot == 0 && recent_timeout_count > 0`,
//! debounced across a couple of polling ticks. That heuristic has a real
//! false-positive window that code-critic caught before merge:
//!
//!   1. A single TRANSIENT embed failure leaves `recent_timeout_count > 0`
//!      (and bumps `last_use`).
//!   2. A genuine quiet period elapses with NO further embed traffic.
//!   3. `LazyEmbedderHandle`'s own idle watchdog
//!      (`embedder_supervisor::idle_watchdog`) kills the child at
//!      `idle_shutdown_secs` (up to 1800s) — this is normal, expected,
//!      memory-saving behavior, not a failure. It zeroes the pid slot AND
//!      resets the spawn gate so the next request transparently respawns.
//!      Critically, it does **not** touch `EmbedderStallTracker` —
//!      `recent_timeout_count` is cleared to `0` ONLY by a subsequent
//!      SUCCESSFUL embed (`EmbedderStallTracker::record_success`), and step 2
//!      guarantees there isn't one.
//!   4. The next two watchdog ticks (needing zero further traffic) then
//!      observe `pid == 0 && recent_timeout_count > 0` and PERMANENTLY swap a
//!      perfectly healthy, merely-idle sidecar to ort.
//!
//! The fix replaces the heuristic with a DEFINITIVE signal from the
//! supervisor itself: [`trusty_common::embedder_client::EmbedderSupervisor::terminated_signal`]
//! is an `Arc<AtomicBool>` the supervision loop sets to `true` at the exact
//! instant — and ONLY the instant — it decides to give up permanently
//! (exhausted `max_restarts` or a wedge-restart storm). It is never set on a
//! clean exit, an intentional `shutdown()` (which is exactly what
//! idle-shutdown performs), or an ordinary respawn. `LazyEmbedderHandle`
//! exposes this through `is_confirmed_terminated()` — which ALSO returns
//! `false` whenever its own spawn gate has been reset (idle-shutdown or an
//! explicit `shutdown()` already cleared `state` to `None`), so there is no
//! window where a stale flag from a previous spawn generation could leak
//! into a fresh, healthy one. This eliminates the ambiguity entirely: there
//! is no longer a heuristic to fool with a quiet period, because idle-
//! shutdown and genuine death now produce categorically different signals
//! rather than the same "pid == 0" observation plus a stale side-counter.
//!
//! **The predicate is now simply**: `active.kind == Python &&
//! python_teardown.is_confirmed_terminated()`. No debounce is needed — the
//! flag is monotonic (set at most once, never reset) and unambiguous the
//! instant it is observed `true`.
//!
//! ## Bounded and quiet
//!
//! The watchdog polls every [`POLL_INTERVAL_SECS`] (seconds, not
//! milliseconds — this is a background health check, not a hot loop) and
//! stops permanently the moment it either (a) observes `switchable`'s active
//! backend is no longer `Python` (something else already moved it — nothing
//! left to watch), or (b) it acts on confirmed death and swaps back itself.
//! It never blocks the reactor: every step is either a cheap atomic load or
//! an `.await`.
//!
//! Test: `swap_back_fires_on_confirmed_death`,
//! `swap_back_does_not_fire_on_stale_timeout_count_plus_idle_shutdown`
//! (THE false-positive regression code-critic named — stale
//! `recent_timeout_count > 0` left over from a transient failure, then a
//! real idle-shutdown to pid 0, then zero further embed traffic — must NOT
//! fire, unlike the pre-fix heuristic), `swap_back_does_not_fire_while_healthy`,
//! `swap_back_stops_watching_once_backend_already_moved` in `start/tests.rs`.

use std::sync::atomic::AtomicU32;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;

use crate::core::Embedder;
use crate::service::embedder_supervisor::{
    ActiveBackend, BackendKind, BootstrapState, SwitchableEmbedder,
};
use crate::service::SearchAppState;

use super::graceful_bootstrap::PythonAdapterTeardown;

/// Seconds between watchdog polls. Deliberately measured in seconds (not
/// milliseconds) — this is a background health check, not a hot loop; see
/// the "no tight poll loop" convention this codebase follows elsewhere
/// (`idle_watchdog`'s own 10s tick, the residency sweep, etc).
const POLL_INTERVAL_SECS: u64 = 15;

/// Abstracts the one truly real/external step the swap-back path performs —
/// standing up a fresh ort backend — so [`drive_swap_back_watchdog`] (the
/// predicate + swap-back state machine) is unit-testable with a deterministic
/// fake. No real `trusty-embedderd` binary/subprocess is ever touched in
/// tests.
pub(crate) trait SwapBackOps: Send + Sync {
    /// Build a fresh ort backend exactly like the daemon's normal default
    /// path does. Mirrors `embedder::build_ort_stdio_sidecar`'s return shape.
    #[allow(clippy::type_complexity)]
    fn build_ort(&self) -> Result<(Arc<dyn Embedder>, Option<Arc<AtomicU32>>)>;
}

/// Production [`SwapBackOps`]: delegates to the exact same
/// `build_ort_stdio_sidecar` the daemon's default/fallback paths use, so
/// there is only ever one implementation of "how to stand up the ort stdio
/// sidecar" in this crate.
struct RealSwapBackOps;

impl SwapBackOps for RealSwapBackOps {
    fn build_ort(&self) -> Result<(Arc<dyn Embedder>, Option<Arc<AtomicU32>>)> {
        super::embedder::build_ort_stdio_sidecar()
    }
}

/// Construct the production [`SwapBackOps`].
///
/// Why: `graceful_bootstrap::run_graceful_python_bootstrap` needs a
/// trait-object handle to pass into [`run_swap_back_watchdog`] without
/// reaching into this module's private `RealSwapBackOps` type.
pub(super) fn real_swap_back_ops() -> Arc<dyn SwapBackOps> {
    Arc::new(RealSwapBackOps)
}

/// Entry point spawned by `graceful_bootstrap::run_graceful_python_bootstrap`
/// right after a successful ort→python hot-swap.
///
/// Why: thin wrapper around [`drive_swap_back_watchdog`] that supplies the
/// real poll interval, mirroring
/// `graceful_bootstrap::run_graceful_python_bootstrap`'s own
/// thin-wrapper-around-a-testable-driver shape.
/// What: delegates to [`drive_swap_back_watchdog`] with [`POLL_INTERVAL_SECS`].
/// Test: `drive_swap_back_watchdog` (this function's callee) carries the
/// actual test coverage — this wrapper has no independent branches.
pub(super) async fn run_swap_back_watchdog(
    switchable: Arc<SwitchableEmbedder>,
    state: SearchAppState,
    python_teardown: Arc<dyn PythonAdapterTeardown>,
    ops: Arc<dyn SwapBackOps>,
) {
    drive_swap_back_watchdog(
        switchable,
        state,
        python_teardown,
        ops,
        Duration::from_secs(POLL_INTERVAL_SECS),
    )
    .await;
}

/// The actual swap-back state machine, parameterized so tests can inject a
/// near-zero `poll_interval` and a fake [`SwapBackOps`] and run in
/// milliseconds under no real time or subprocess.
///
/// What: polls every `poll_interval`. On each tick, reads
/// `switchable.active()`: if it is no longer [`BackendKind::Python`],
/// something else already moved the backend away — nothing left for this
/// watchdog to do, so it returns immediately (bounded: never watches a
/// backend it already abandoned). Otherwise checks
/// `python_teardown.is_confirmed_terminated()` — the DEFINITIVE signal from
/// the underlying `EmbedderSupervisor` (see the module doc's history
/// section for why this replaced the earlier pid+stall-tracker heuristic).
/// The moment it observes `true`, builds a fresh ort backend via
/// `ops.build_ort()`, hot-swaps `switchable` to it (`ActiveBackend { kind:
/// Ort, bootstrap: FellBackToOrt, .. }`), installs the new ort pid slot on
/// `state`, tears down the dead python handle via
/// `python_teardown.teardown()` (cooperative — no orphan), logs loudly at
/// `warn`, and returns (this watchdog's job is done — python was already
/// abandoned, permanently, for this daemon's lifetime, matching PR-3's own
/// "no re-bootstrap after failure" convention). If `ops.build_ort()` itself
/// fails, the dead python handle is still torn down (no orphan) but the
/// switchable is left as-is (still python, now truly unrecoverable) with a
/// loud `error` log — an extremely unlikely double-failure (the ort sidecar
/// binary going missing at the exact moment the python one died), but never
/// silently swallowed.
/// Test: `swap_back_fires_on_confirmed_death`,
/// `swap_back_does_not_fire_on_stale_timeout_count_plus_idle_shutdown`,
/// `swap_back_does_not_fire_while_healthy`,
/// `swap_back_stops_watching_once_backend_already_moved`.
pub(super) async fn drive_swap_back_watchdog(
    switchable: Arc<SwitchableEmbedder>,
    state: SearchAppState,
    python_teardown: Arc<dyn PythonAdapterTeardown>,
    ops: Arc<dyn SwapBackOps>,
    poll_interval: Duration,
) {
    loop {
        tokio::time::sleep(poll_interval).await;

        let active = switchable.active();
        if active.kind != BackendKind::Python {
            tracing::debug!(
                "swap_back_watchdog: active backend is no longer python (kind={:?}) — \
                 nothing left to watch, exiting",
                active.kind
            );
            return;
        }

        if !python_teardown.is_confirmed_terminated().await {
            continue;
        }

        tracing::warn!("python/MPS sidecar unrecoverable — fell back to ort; search unaffected");

        match ops.build_ort() {
            Ok((ort_adapter, ort_pid_slot)) => {
                let provider = ort_adapter.provider();
                switchable.swap_to(
                    ort_adapter,
                    ActiveBackend {
                        kind: BackendKind::Ort,
                        provider,
                        model: super::embedder::EMBEDDER_MODEL_NAME.to_string(),
                        quantized: super::embedder::backend_respects_quantized_env(
                            BackendKind::Ort,
                        ) && super::embedder::quantized_from_env(),
                        bootstrap: BootstrapState::FellBackToOrt,
                    },
                );
                if let Some(slot) = ort_pid_slot {
                    state.install_embedderd_pid_slot(slot).await;
                }
                // Shut down the dead python handle cleanly — no orphan. Safe
                // to do after swap_to: `switchable` no longer routes any
                // caller through this handle, so there is no live traffic
                // left to interrupt; any in-flight call that was already
                // routed through the old (dead) handle when swap_to landed
                // simply errors and is retried by the caller, landing on the
                // now-installed ort backend (both backends are
                // vector-interchangeable, verified this epic).
                python_teardown.teardown().await;
            }
            Err(e) => {
                tracing::error!(
                    "swap-back-to-ort failed to build a fresh ort sidecar ({e:#}) — \
                     search remains on the unrecoverable python backend and will error \
                     on every embed call. Restart the daemon to recover (or fix the ort \
                     binary discovery issue and it will resolve on next start)."
                );
                python_teardown.teardown().await;
            }
        }

        return;
    }
}
