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
//! ## The false-positive hazard — why "pid == 0" alone is not the trigger
//!
//! The python sidecar's pid slot (`SearchAppState::current_embedderd_pid`)
//! reads `0` (`None`) in TWO completely different situations:
//!
//!   1. **Intentional idle-shutdown** — `LazyEmbedderHandle`'s own idle
//!      watchdog (`embedder_supervisor::idle_watchdog`) killed the child
//!      after `TRUSTY_EMBEDDERD_PY_IDLE_SHUTDOWN_SECS` of no requests, and
//!      reset its internal spawn gate. The NEXT embed request transparently
//!      triggers a fresh spawn and succeeds — this is normal, expected,
//!      memory-saving behavior, not a failure.
//!   2. **Genuine, unrecoverable death** — the `EmbedderSupervisor`'s
//!      crash-restart loop exhausted `TRUSTY_EMBEDDERD_MAX_RESTARTS` and gave
//!      up. Critically, `LazyEmbedderHandle`'s OWN spawn gate (the
//!      `state: Arc<Mutex<Option<SpawnedState>>>` cell) is untouched in this
//!      case — only the idle watchdog or `LazyEmbedderHandle::shutdown()`
//!      ever clears it. So the handle keeps reusing the same, now-dead
//!      `client_slot` on every subsequent embed call: every one of those
//!      calls fails (the underlying stdio pipe is gone), and NONE of them
//!      trigger a fresh respawn.
//!
//! Swapping back to ort on case 1 would be actively wrong — it would
//! permanently abandon a perfectly healthy python sidecar just because it
//! happened to be idle for a moment, thrashing the two backends back and
//! forth. The predicate below is deliberately conservative to tell the two
//! apart without adding any new signal: it composes the pid slot with
//! [`EmbedderStallTracker::recent_timeout_count`] (`service/stall_tracker.rs`,
//! already wired onto every embed call via `EmbedPool`).
//!
//! **The predicate**: `active.kind == Python && pid == 0 && recent_timeout_count > 0`,
//! confirmed across [`CONFIRM_TICKS`] CONSECUTIVE polling ticks before acting.
//!
//! Why this composition is safe against case 1 (idle-shutdown): a successful
//! embed call (the respawn succeeding) calls
//! `EmbedderStallTracker::record_success`, which unconditionally resets
//! `recent_timeout_count` to `0` — see `stall_tracker.rs`. So the moment an
//! idle-shutdown's automatic respawn serves one successful request,
//! `recent_timeout_count` drops back to `0` and the predicate goes false
//! again, even though the pid slot may still read `0` for the brief window
//! between the respawn starting and the pid-forwarder task's next tick. Only
//! a SUSTAINED run of embed failures — which only happens in case 2, because
//! case 1's respawn always succeeds cold-restart-cheaply — keeps
//! `recent_timeout_count > 0` across multiple consecutive watchdog polls.
//! The `CONFIRM_TICKS`-consecutive-poll debounce is an extra conservative
//! margin on top of that: a single blip (one transient timeout right before
//! a respawn lands) cannot trip the watchdog; it must observe the composed
//! condition on `CONFIRM_TICKS` back-to-back ticks, `POLL_INTERVAL` apart.
//!
//! ## Bounded and quiet
//!
//! The watchdog polls every [`POLL_INTERVAL`] (seconds, not milliseconds —
//! this is a background health check, not a hot loop) and stops permanently
//! the moment it either (a) observes `switchable`'s active backend is no
//! longer `Python` (something else already moved it — nothing left to
//! watch), or (b) it acts on confirmed death and swaps back itself. It never
//! blocks the reactor: every step is either a cheap atomic load or an
//! `.await`.
//!
//! Test: `swap_back_fires_on_confirmed_death`,
//! `swap_back_does_not_fire_on_idle_shutdown_respawn`,
//! `swap_back_does_not_fire_while_healthy`,
//! `is_confirmed_dead_predicate_matrix` in `start/tests.rs`.

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

/// Number of CONSECUTIVE confirming polls required before the watchdog acts.
/// A conservative debounce margin on top of the `recent_timeout_count`
/// signal itself — see the module doc's false-positive-hazard section.
const CONFIRM_TICKS: u32 = 2;

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

/// Pure death predicate — see the module doc's "the predicate" section for
/// the full reasoning.
///
/// Why: extracted as a pure function so the exact false-positive-avoidance
/// logic (idle-shutdown vs. genuine death) has a small, deterministic,
/// exhaustive unit test independent of the polling loop / real time.
/// What: `true` only when the switchable's active backend is still `Python`,
/// its pid slot reads `0` (no live child), AND at least one embed has failed
/// since the last success (`recent_timeout_count > 0`). Any other
/// combination — healthy python (pid > 0), or pid == 0 with no failures yet
/// (idle-shutdown that hasn't been asked to respawn, or a respawn that
/// already succeeded) — is `false`.
/// Test: `is_confirmed_dead_predicate_matrix`.
fn is_confirmed_dead(
    active_kind: BackendKind,
    python_pid: Option<u32>,
    recent_timeout_count: u32,
) -> bool {
    active_kind == BackendKind::Python && python_pid.is_none() && recent_timeout_count > 0
}

/// Entry point spawned by `graceful_bootstrap::run_graceful_python_bootstrap`
/// right after a successful ort→python hot-swap.
///
/// Why: thin wrapper around [`drive_swap_back_watchdog`] that supplies the
/// real timing constants, mirroring `graceful_bootstrap::run_graceful_python_bootstrap`'s
/// own thin-wrapper-around-a-testable-driver shape.
/// What: delegates to [`drive_swap_back_watchdog`] with [`POLL_INTERVAL_SECS`]
/// and [`CONFIRM_TICKS`].
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
        CONFIRM_TICKS,
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
/// backend it already abandoned). Otherwise composes
/// [`is_confirmed_dead`] from the current pid slot
/// (`SearchAppState::current_embedderd_pid`) and
/// `state.embedder_stall_tracker.recent_timeout_count()`. A confirming tick
/// increments a consecutive-ticks counter; a non-confirming tick resets it
/// to `0`. Once the counter reaches `confirm_ticks`, builds a fresh ort
/// backend via `ops.build_ort()`, hot-swaps `switchable` to it
/// (`ActiveBackend { kind: Ort, bootstrap: FellBackToOrt, .. }`), installs
/// the new ort pid slot on `state`, tears down the dead python handle via
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
/// `swap_back_does_not_fire_on_idle_shutdown_respawn`,
/// `swap_back_does_not_fire_while_healthy`.
pub(super) async fn drive_swap_back_watchdog(
    switchable: Arc<SwitchableEmbedder>,
    state: SearchAppState,
    python_teardown: Arc<dyn PythonAdapterTeardown>,
    ops: Arc<dyn SwapBackOps>,
    poll_interval: Duration,
    confirm_ticks: u32,
) {
    let mut consecutive_confirms = 0u32;

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

        let python_pid = state.current_embedderd_pid();
        let recent_timeouts = state.embedder_stall_tracker.recent_timeout_count();

        if is_confirmed_dead(active.kind, python_pid, recent_timeouts) {
            consecutive_confirms += 1;
        } else {
            consecutive_confirms = 0;
        }

        if consecutive_confirms < confirm_ticks {
            continue;
        }

        tracing::warn!(
            "python/MPS sidecar unrecoverable — fell back to ort; search unaffected \
             (recent_timeout_count={recent_timeouts}, confirmed over {confirm_ticks} \
             consecutive checks)"
        );

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

#[cfg(test)]
mod tests {
    use super::*;

    /// Exhaustive matrix over the predicate's three inputs — every
    /// combination is spelled out explicitly rather than looped, so a
    /// reviewer can see at a glance exactly which combination is/isn't a
    /// confirmed death.
    #[test]
    fn is_confirmed_dead_predicate_matrix() {
        // Healthy python, serving fine.
        assert!(!is_confirmed_dead(BackendKind::Python, Some(1234), 0));
        // Healthy python but a transient timeout blip (pid still alive) —
        // not a death signal, the process itself is still up.
        assert!(!is_confirmed_dead(BackendKind::Python, Some(1234), 3));
        // Idle-shutdown, not yet asked to respawn (or respawn already
        // succeeded and reset the counter) — pid 0, no failures recorded.
        assert!(!is_confirmed_dead(BackendKind::Python, None, 0));
        // THE confirmed-death case: pid 0 AND failures piling up.
        assert!(is_confirmed_dead(BackendKind::Python, None, 1));
        assert!(is_confirmed_dead(BackendKind::Python, None, 5));
        // Never fires for any other active backend kind, regardless of the
        // other two signals — this predicate only ever judges the python arm.
        assert!(!is_confirmed_dead(BackendKind::Ort, None, 5));
        assert!(!is_confirmed_dead(BackendKind::Remote, None, 5));
        assert!(!is_confirmed_dead(BackendKind::InProcess, None, 5));
        assert!(!is_confirmed_dead(BackendKind::Candle, None, 5));
    }
}
