//! The crash path's kill, and the load-aware startup-probe timeout.
//!
//! Why (#10207): a footprint recycle whose replacement never answered its
//! startup probe kept the old sidecar, and nothing bounded it after that; it
//! reached 10 GB. The replacements failed because a cold start that takes
//! 47 s on a quiet host missed a 90 s probe at load 6.6-8.8. Split from
//! `supervisor.rs` for the 500-SLOC cap.
//! What: [`kill_sidecar`] (shared by the wedge kill and the hard-limit kill),
//! [`startup_probe_timeout`], its pure core [`scaled_startup_timeout`], and
//! [`SupervisorConfig::first_spawn_probe_timeout`] for callers outside the crate.
//! Test: `supervisor_recycle_tests.rs`.

use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use tokio::process::Child;

use super::SupervisorConfig;

/// #10207: the least time a recycle replacement or a crash respawn gets to
/// answer its startup probe — more than a quiet cold start (about 47 s) needs.
pub(crate) const RESPAWN_STARTUP_FLOOR: Duration = Duration::from_secs(120);

/// Which spawn a startup probe guards.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SpawnKind {
    /// The supervisor's first spawn; its caller chose the timeout.
    First,
    /// A recycle replacement or a crash respawn.
    Respawn,
}

/// The startup-probe timeout for one spawn, read against the live host load.
/// Test: `startup_timeout_scales_with_load_and_floors_respawns`.
pub(crate) fn startup_probe_timeout(config: &SupervisorConfig, kind: SpawnKind) -> Duration {
    scaled_startup_timeout(
        Duration::from_secs(config.startup_timeout_secs),
        kind,
        crate::sys_metrics::load_per_cpu(),
        config.recycle.startup_load_scale_max,
    )
}

/// Startup-probe timeout for `base` under `load_per_cpu`.
///
/// Why (#10207): on a CPU-bound host a cold start gets about
/// `1 / (1 + load1/ncpu)` of a core it would have alone, so its time grows by
/// `1 + load1/ncpu`. A multiplier of `load1/ncpu` clamped at 1 would not move
/// at all on the host that failed: load 6.6-8.8 on 16 CPUs is 0.4-0.55 per CPU.
/// trusty-search also leaves the bootstrap budget (at most 90 s) in
/// `startup_timeout_secs` for every later spawn, so a respawn is floored.
/// What: `max(base, RESPAWN_STARTUP_FLOOR)` for [`SpawnKind::Respawn`],
/// `base` for [`SpawnKind::First`], times `clamp(1 + load, 1, scale_max)`.
/// An unreadable, negative or non-finite load counts as an idle host;
/// `scale_max` 0 or 1 turns the scaling off. A product too large for a
/// `Duration` returns the unscaled base.
/// Test: `startup_timeout_scales_with_load_and_floors_respawns`,
/// `startup_timeout_with_an_extreme_base_does_not_panic`.
pub(crate) fn scaled_startup_timeout(
    base: Duration,
    kind: SpawnKind,
    load_per_cpu: Option<f64>,
    scale_max: u32,
) -> Duration {
    let base = match kind {
        SpawnKind::First => base,
        SpawnKind::Respawn => base.max(RESPAWN_STARTUP_FLOOR),
    };
    let load = load_per_cpu
        .filter(|l| l.is_finite() && *l >= 0.0)
        .unwrap_or(0.0);
    let factor = (1.0 + load).clamp(1.0, f64::from(scale_max.max(1)));
    // #10207: `mul_f64` panics on overflow; a base too large to scale is
    // already effectively unbounded, so it is used as-is.
    Duration::try_from_secs_f64(base.as_secs_f64() * factor).unwrap_or(base)
}

impl SupervisorConfig {
    /// The supervisor's first-spawn startup-probe timeout under
    /// `load_per_cpu` (pass [`crate::sys_metrics::load_per_cpu`]).
    ///
    /// Why (#10207): a caller that wraps the first spawn in its own timeout
    /// (trusty-search's python bootstrap) has to scale it the same way, or it
    /// fires before the probe on a loaded host.
    /// What: [`scaled_startup_timeout`] of `startup_timeout_secs` for a first
    /// spawn, capped by `recycle.startup_load_scale_max`.
    /// Test: `startup_timeout_with_an_extreme_base_does_not_panic`.
    #[must_use]
    pub fn first_spawn_probe_timeout(&self, load_per_cpu: Option<f64>) -> Duration {
        scaled_startup_timeout(
            Duration::from_secs(self.startup_timeout_secs),
            SpawnKind::First,
            load_per_cpu,
            self.recycle.startup_load_scale_max,
        )
    }
}

/// Kill and reap the live sidecar, then clear the published pid.
///
/// Why: the wedge kill and the hard-limit kill (#10207) both have to stop a
/// sidecar that may still be running before the crash path respawns it.
/// What: takes the child out of `child_slot`, so a failed respawn is seen as
/// `RespawnFailed` next time round; `what` names the sidecar in the WARN.
/// Test: `hard_limit_kills_a_sidecar_whose_replacement_cannot_start`.
pub(crate) async fn kill_sidecar(
    child_slot: &tokio::sync::Mutex<Option<Child>>,
    child_pid_slot: &AtomicU32,
    what: &str,
) {
    let mut guard = child_slot.lock().await;
    if let Some(mut child) = guard.take() {
        if let Err(e) = child.start_kill() {
            tracing::warn!(
                "EmbedderSupervisor: start_kill on {what} sidecar failed (may have already \
                 exited): {e}"
            );
        }
        let _ = child.wait().await;
    }
    drop(guard);
    child_pid_slot.store(0, Ordering::Release);
}
