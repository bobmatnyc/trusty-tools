//! The background loop that feeds the history and the point-in-time cache
//! (#6641).
//!
//! Why one loop rather than two: the host cache
//! ([`crate::host_status::HostMetricsCache`], which
//! `GET /api/console/machine-status` serves) and the history ring must never
//! disagree about the newest sample. Sampling once and writing both is the only
//! way to guarantee that; the stateful [`HostSampler`] also cannot be shared
//! across two tasks, because its CPU and network readings are deltas between
//! refreshes.
//! What: [`start`] spawns a task that samples the host, writes the cache, feeds
//! the ring through
//! [`MachineHistory::record_sample`](crate::machine_history::MachineHistory::record_sample),
//! and folds the current
//! service reports into the transition log — then sleeps for `interval`. The
//! same task publishes `interval` to the history so the payload advertises the
//! cadence actually in use.
//!
//! When #6284 inverts the transport, the service half of this loop becomes a
//! push handler calling `observe_services` and the host half becomes a push
//! handler calling `record_sample`; neither the ring nor any reader changes.
//! Test: not tested directly — it spawns a real OS sampler on a timer. Both
//! halves it calls are covered: `machine_history::tests` for the ring and the
//! log, `host_status::tests` for the cache.

use std::time::{Duration, Instant};

use tracing::{debug, info};
use trusty_common::host_metrics::HostSampler;

use crate::server::AppState;
use crate::service_cpu::{ServiceCpuSampler, resolve_pid};

/// Resolve any missing pids, then record one per-service sample (#6642).
///
/// Why the two steps are split: resolving a pid reads a lock file or dials a
/// socket, which must not run on the reactor thread; taking a CPU reading on an
/// already-known pid is a `sysinfo` refresh that must not pay for a thread hop.
/// Steady state does the second only — every service either has a pid or is
/// inside its backoff window, so `pending` is empty and no blocking task is
/// spawned at all.
///
/// Fail-open: a panic inside the lookup task degrades to "no pids resolved this
/// tick", and the sample is still recorded for every service. Nothing here can
/// stop the loop.
/// What: asks the sampler which ids need a lookup, runs
/// [`resolve_pid`] for each on the blocking pool, feeds the answers back, then
/// records the batch through
/// [`MachineHistory::record_service_samples`](super::MachineHistory::record_service_samples).
/// Test: `service_cpu::tests` covers the sampler's own behaviour; this function
/// is the wiring, exercised whenever the daemon runs.
async fn sample_services(
    state: &AppState,
    service_cpu: &mut ServiceCpuSampler,
    services: &[crate::connector::ServiceInfo],
) {
    let pending = service_cpu.pending_lookups(services, Instant::now());
    if !pending.is_empty() {
        let found = tokio::task::spawn_blocking(move || {
            pending
                .into_iter()
                .map(|id| {
                    let pid = resolve_pid(&id);
                    (id, pid)
                })
                .collect::<Vec<_>>()
        })
        .await
        .unwrap_or_else(|e| {
            debug!("machine_history: service pid lookup task failed: {e}");
            Vec::new()
        });
        service_cpu.record_lookups(found, Instant::now());
    }

    let now_unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let batch = service_cpu.sample(services, now_unix);
    state.machine_history().record_service_samples(batch).await;
}

/// Spawn the host sampling + service observation loop.
///
/// Why `AppState` rather than the three handles: the loop needs the host cache,
/// the history, and whichever per-service reports are currently warm, and the
/// state already owns all three.
/// What: spawns a tokio task that constructs one [`HostSampler`], samples it
/// immediately (so the first request finds a warm cache), then repeats every
/// `interval`. Sampling itself never fails, so there is no error path to log.
/// Test: see the module docs.
pub fn start(state: AppState, interval: Duration) {
    state
        .machine_history()
        .set_sample_interval(interval.as_secs());
    tokio::spawn(async move {
        info!(
            "machine_history: sampling host metrics every {}s (window={} points)",
            interval.as_secs(),
            state.machine_history().snapshot().await.sample_capacity
        );
        let mut sampler = HostSampler::new();
        // #6642: one sampler across ticks — `sysinfo` derives CPU% from the
        // delta between two refreshes, so it cannot be rebuilt per tick.
        let mut service_cpu = ServiceCpuSampler::new();
        loop {
            let metrics = sampler.sample();
            debug!(
                overall = ?metrics.overall_pressure,
                cpu_pct = metrics.cpu.usage_pct,
                mem_pct = metrics.memory.usage_pct,
                "machine_history: sampled host metrics"
            );
            state.host_metrics_cache().set(metrics.clone()).await;
            state.machine_history().record_sample(metrics).await;

            let reports = state.collect_service_reports().await;
            for change in state.machine_history().observe_services(&reports).await {
                info!(
                    service = %change.service_id,
                    from = ?change.from,
                    to = ?change.to,
                    "machine_history: service changed state"
                );
            }

            // #6642: one per-service sample per tick. The status comes from the
            // health poller's cache rather than a fresh detection pass — this
            // loop runs every second and a detection pass dials six services.
            if let Some(snapshot) = state.poller_cache().snapshot().await {
                sample_services(&state, &mut service_cpu, &snapshot.services).await;
            }

            tokio::time::sleep(interval).await;
        }
    });
}
