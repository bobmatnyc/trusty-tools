//! The daemon's disk-budget sweep over the builder slot pool (#8451).
//!
//! Why: owner ruling 2026-09-28 (option A) makes slot-pool eviction a daemon
//! function, not an operator chore — nobody was watching the pool when it
//! reached 2.6 TB. The policy and the in-use guard live in
//! [`crate::core::build_lease::evict`]; this module binds them to the
//! operator's config and runs them on a cadence.
//! What: [`spawn_if_enabled`] reads [`ENV_ENABLED`] and [`ENV_INTERVAL_SECS`]
//! and spawns [`evict_loop`], which runs [`run_one_tick`] at once and then every
//! interval. A tick measures the volume holding `builders.slot_pool_root`
//! against [`effective_evict_pct`] and, at or over it, evicts idle `slot-N`
//! directories oldest-first.
//! Fail direction: keep. A scratch-rooted daemon, an unusable lease store or an
//! unmeasurable volume evicts nothing and says so at `warn`.
//! Test: `slot_pool_evict_tests` below; the sweep itself is `evict_tests.rs`.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use tokio::time::MissedTickBehavior;
use tracing::{info, warn};

use crate::core::build_lease::config::BuildLeaseConfig;
use crate::core::build_lease::evict::{SweepOutcome, effective_evict_pct, sweep};
use crate::core::build_lease::slots::SlotDir;
use crate::core::disk_usage_guard;
use crate::daemon::state::DaemonState;

/// `0`/`false`/`off`/`no` disables the sweep; anything else, or unset, enables.
pub(crate) const ENV_ENABLED: &str = "TRUSTY_MPM_SLOT_POOL_EVICT";

/// Overrides the sweep cadence, in seconds.
pub(crate) const ENV_INTERVAL_SECS: &str = "TRUSTY_MPM_SLOT_POOL_EVICT_INTERVAL_SECS";

/// Default cadence: ten minutes.
///
/// Why: a cold build adds tens of GB in minutes; a tick under the threshold
/// costs one `statvfs`, so a short cadence is cheap.
pub(crate) const DEFAULT_INTERVAL_SECS: u64 = 600;

/// Pure parse of [`ENV_INTERVAL_SECS`]; zero or junk is the default.
///
/// Test: `the_interval_falls_back_on_junk_and_zero`.
pub(crate) fn parse_interval_secs(raw: Option<&str>) -> u64 {
    raw.and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(DEFAULT_INTERVAL_SECS)
}

/// Spawn the sweep unless [`ENV_ENABLED`] switched it off.
///
/// Test: the policy is `parse_enabled_defaults_on_and_honours_the_off_switch`
/// (shared with the merged-PR reclaim); the loop is `evict_loop_exits_on_cancel`.
pub(crate) fn spawn_if_enabled(
    state: Arc<DaemonState>,
    cancel: tokio_util::sync::CancellationToken,
) {
    let raw = std::env::var(ENV_ENABLED).ok();
    if !super::merged_pr_reclaim::parse_enabled(raw.as_deref()) {
        info!("slot-pool eviction disabled via {ENV_ENABLED}");
        return;
    }
    let secs = parse_interval_secs(std::env::var(ENV_INTERVAL_SECS).ok().as_deref());
    tokio::spawn(evict_loop(state, Duration::from_secs(secs), cancel));
}

/// One tick now, then one per `interval`, until `cancel` fires.
///
/// Test: `evict_loop_exits_on_cancel`.
pub(crate) async fn evict_loop(
    state: Arc<DaemonState>,
    interval: Duration,
    cancel: tokio_util::sync::CancellationToken,
) {
    info!(
        interval_secs = interval.as_secs(),
        "slot-pool eviction enabled (#8451)"
    );
    let mut tick = tokio::time::interval(interval);
    tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            _ = tick.tick() => run_one_tick(&state).await,
        }
    }
}

/// One sweep against the operator's real pool, gated on host state.
///
/// Test: `a_scratch_rooted_daemon_evicts_nothing`.
pub(crate) async fn run_one_tick(state: &Arc<DaemonState>) {
    // #6348: a scratch-rooted daemon is a test process; the pool is real.
    if let Some(reason) = crate::daemon::host_state_refusal(state) {
        warn!("slot-pool eviction: skipped — {reason}");
        return;
    }
    let Some(home) = dirs::home_dir() else {
        warn!("slot-pool eviction: skipped — no home directory to resolve the pool from");
        return;
    };
    let Ok(_permit) = super::sweep_status::lane().acquire().await else {
        warn!("slot-pool eviction: maintenance lane closed; pass not run");
        return;
    };
    let joined = tokio::task::spawn_blocking(move || evict_in(&home)).await;
    match joined {
        Ok(line) => log_tick(&line),
        Err(e) => warn!("slot-pool eviction: the pass did not complete: {e}"),
    }
}

/// What one tick logs: the level and the line.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum TickLog {
    /// Nothing worth a line (under threshold, no pool).
    Quiet,
    /// Evicted without failure.
    Info(String),
    /// A failure, an unusable store, or an unmeasurable volume.
    Warn(String),
}

fn log_tick(line: &TickLog) {
    match line {
        TickLog::Quiet => {}
        TickLog::Info(msg) => info!("slot-pool eviction: {msg}"),
        TickLog::Warn(msg) => warn!("slot-pool eviction: {msg}"),
    }
}

/// Resolve the pool, lease store and threshold under `home`, then sweep with
/// the volume measured live.
///
/// What: the binding only — the pool root from `builders.slot_pool_root`, the
/// guard from `disk.max_usage_pct`, the threshold from
/// `builders.slot_pool_evict_pct` — then [`evict_measured`].
/// Test: `evict_measured`'s tests; this reads the operator's config files.
pub(crate) fn evict_in(home: &Path) -> TickLog {
    let builders = crate::core::config::MpmConfig::load_default().builders;
    let lease = BuildLeaseConfig::load_default();
    let pool = builders.effective_slot_pool_root(home);
    let guard = disk_usage_guard::active_threshold_at(home).threshold_pct;
    let threshold = effective_evict_pct(lease.slot_pool_evict_pct, guard);
    let store = SlotDir::resolve(Some(home));
    let mut measure = |p: &Path| disk_usage_guard::measure(p).map(|m| m.usage_pct);
    evict_measured(
        &pool,
        store.as_ref().map_err(ToString::to_string),
        threshold,
        &mut measure,
    )
}

/// One sweep of `pool`, reduced to the line the tick logs.
///
/// What: an unusable `store` evicts nothing (no slot can be proven idle);
/// otherwise [`sweep`] runs and its outcome becomes a [`TickLog`]. A removal
/// that did not finish is always a `Warn`, never folded into the success line.
/// Test: `an_unusable_store_evicts_nothing`, `a_tick_evicts_over_the_threshold`,
/// `an_unmeasurable_tick_warns`, `a_failed_removal_tick_warns`.
pub(crate) fn evict_measured(
    pool: &Path,
    store: Result<&SlotDir, String>,
    threshold: u8,
    measure: &mut dyn FnMut(&Path) -> Option<f32>,
) -> TickLog {
    // Without the lease store no slot's flock can be taken, so no slot can be
    // proven idle: evict nothing.
    let store = match store {
        Ok(store) => store,
        Err(e) => return TickLog::Warn(format!("no usable lease store ({e}); nothing evicted")),
    };
    match sweep(pool, store, threshold, measure) {
        SweepOutcome::NoPool | SweepOutcome::UnderThreshold(_) => TickLog::Quiet,
        SweepOutcome::Unmeasurable => TickLog::Warn(format!(
            "the volume holding {} cannot be measured; nothing evicted",
            pool.display()
        )),
        SweepOutcome::Unlistable(e) => TickLog::Warn(format!(
            "{} cannot be listed ({e}); nothing evicted",
            pool.display()
        )),
        SweepOutcome::Swept(report) => {
            let after = report.usage_after.map_or_else(
                || "unmeasured".to_string(),
                |u| disk_usage_guard::fmt_pct(&u),
            );
            let summary = format!(
                "threshold {threshold}%: evicted {} slot dir(s), spared {} in use, \
                 finished {} earlier removal(s); volume now {after}",
                report.evicted.len(),
                report.spared.len(),
                report.leftovers_removed,
            );
            if report.failed() {
                let failed: Vec<String> = report
                    .failed
                    .iter()
                    .map(|(p, e)| format!("{} ({e})", p.display()))
                    .collect();
                TickLog::Warn(format!(
                    "{summary}; {} removal(s) did not complete: {}",
                    failed.len(),
                    failed.join("; ")
                ))
            } else if report.evicted.is_empty() && report.leftovers_removed == 0 {
                TickLog::Warn(format!(
                    "{summary}; no idle slot was left to evict, so the volume stays over \
                     the threshold"
                ))
            } else {
                TickLog::Info(summary)
            }
        }
    }
}

#[cfg(test)]
#[path = "slot_pool_evict_tests.rs"]
mod slot_pool_evict_tests;
