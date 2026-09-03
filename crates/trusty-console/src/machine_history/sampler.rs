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
//! Why each half of a tick is panic-guarded (#6642): this is ONE bare
//! `tokio::spawn`, so a panic anywhere inside it ends the task. The history then
//! freezes, every open SSE stream stays connected emitting nothing, and no log
//! line says why — the failure looks exactly like a quiet machine. `guarded`
//! contains a panic to the half that raised it, logs the payload at `error!`,
//! and lets the next tick run. `start` also keeps the loop's `JoinHandle` and
//! logs at `error!` if the loop ever ends, which after this guard it cannot do
//! except by cancellation.
//!
//! When #6284 inverts the transport, the service half of this loop becomes a
//! push handler calling `observe_services` and the host half becomes a push
//! handler calling `record_sample`; neither the ring nor any reader changes.
//! Test: the loop itself is not tested directly — it spawns a real OS sampler on
//! a timer. Its parts are: `a_panicking_step_is_contained_and_the_next_one_runs`
//! and `a_panicking_service_half_leaves_the_host_half_recording` cover the
//! guard, `machine_history::tests` the rings and the log, `host_status::tests`
//! the cache.

use std::any::Any;
use std::panic::AssertUnwindSafe;
use std::time::{Duration, Instant};

use futures_util::FutureExt;
use tracing::{debug, error, info};
use trusty_common::host_metrics::HostSampler;

use crate::server::AppState;
use crate::service_cpu::{ServiceCpuSampler, resolve_pid};

/// Render a caught panic payload as text.
///
/// Why: `catch_unwind` hands back `Box<dyn Any + Send>`, which formats as
/// nothing useful. The two shapes `panic!` and `assert!` actually produce are
/// `&'static str` and `String`; anything else is a custom payload no log line
/// can render, and saying so is better than printing a type id.
/// Test: `a_panicking_step_is_contained_and_the_next_one_runs`.
fn panic_text(payload: &(dyn Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<&'static str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "<non-string panic payload>".to_string()
    }
}

/// Run one step of a tick, containing a panic rather than ending the loop.
///
/// Why: see the module docs — an unguarded panic in this task freezes the
/// history silently. Guarding per STEP rather than per tick is what makes the
/// host half survive a panic in the service half: the graph then shows a gap in
/// one series instead of going flat in all of them.
///
/// Why `AssertUnwindSafe` is sound here: the state crossing the boundary is the
/// `AppState` handles (`tokio::sync::RwLock`, which has no poisoning — a panic
/// while a guard is held releases it) and the `&mut ServiceCpuSampler`, whose
/// fields are two maps and a `Vec` with no invariant a half-finished tick
/// breaks. The next tick re-reads both from scratch.
/// What: awaits `body` under `catch_unwind`. Returns `true` when it completed,
/// `false` after logging the panic payload at `error!` with `step` naming which
/// half raised it. Never propagates.
/// Test: `a_panicking_step_is_contained_and_the_next_one_runs`,
/// `a_panicking_service_half_leaves_the_host_half_recording`.
async fn guarded<F: Future<Output = ()>>(step: &'static str, body: F) -> bool {
    match AssertUnwindSafe(body).catch_unwind().await {
        Ok(()) => true,
        Err(payload) => {
            error!(
                step,
                panic = %panic_text(payload.as_ref()),
                "machine_history: a sampler step panicked; the loop continues and this tick \
                 records nothing for that step"
            );
            false
        }
    }
}

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
    let loop_task = tokio::spawn(async move {
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
            // #6642: each half is guarded separately, so a panic in one leaves a
            // gap in that series rather than freezing every graph.
            guarded("host", host_tick(&state, &mut sampler)).await;
            guarded("services", service_tick(&state, &mut service_cpu)).await;
            tokio::time::sleep(interval).await;
        }
    });

    // #6642: the loop above cannot return, so reaching this log means the task
    // was cancelled or aborted — which would otherwise stop the history with no
    // trace anywhere.
    tokio::spawn(async move {
        match loop_task.await {
            Ok(()) => error!("machine_history: the sampler loop ended; history has stopped"),
            Err(e) => error!(
                error = %e,
                "machine_history: the sampler task ended abnormally; history has stopped"
            ),
        }
    });
}

/// Sample the host once and write both the cache and the ring.
///
/// Why both from ONE sample: `GET /api/console/machine-status` and the history
/// window must never disagree about the newest reading.
/// Test: `host_status::tests` for the cache, `machine_history::tests` for the
/// ring; the guard around it by
/// `a_panicking_service_half_leaves_the_host_half_recording`.
async fn host_tick(state: &AppState, sampler: &mut HostSampler) {
    let metrics = sampler.sample();
    debug!(
        overall = ?metrics.overall_pressure,
        cpu_pct = metrics.cpu.usage_pct,
        mem_pct = metrics.memory.usage_pct,
        "machine_history: sampled host metrics"
    );
    state.host_metrics_cache().set(metrics.clone()).await;
    state.machine_history().record_sample(metrics).await;
}

/// Fold the warm service reports into the transition log, then sample CPU.
///
/// Why the status comes from the health poller's cache and not a fresh detection
/// pass: this runs every second and a detection pass dials six services.
/// Test: `service_cpu::tests` for the sampling; `machine_history::tests` for the
/// log.
async fn service_tick(state: &AppState, service_cpu: &mut ServiceCpuSampler) {
    let reports = state.collect_service_reports().await;
    for change in state.machine_history().observe_services(&reports).await {
        info!(
            service = %change.service_id,
            from = ?change.from,
            to = ?change.to,
            "machine_history: service changed state"
        );
    }

    if let Some(snapshot) = state.poller_cache().snapshot().await {
        sample_services(state, service_cpu, &snapshot.services).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connector::ServiceStatus;
    use crate::machine_history::MachineHistory;
    use crate::machine_history::service_samples::{ServiceSample, ServiceSampleBatch};

    /// Why not a real `HostSampler`: the subject is panic containment, and a
    /// deterministic sample keeps the assertion about the ring rather than about
    /// the machine. No load is generated — nothing here spins a CPU.
    fn stub_sample(seq: u64) -> trusty_common::host_metrics::HostMetrics {
        use trusty_common::host_metrics::{
            CpuMetrics, DiskMetrics, HostMetrics, MemoryMetrics, NetworkMetrics, Pressure,
        };
        HostMetrics {
            cpu: CpuMetrics {
                usage_pct: 0.0,
                logical_cores: 1,
                physical_cores: None,
                pressure: Pressure::Nominal,
            },
            memory: MemoryMetrics {
                total_bytes: 1,
                used_bytes: 0,
                available_bytes: 1,
                usage_pct: 0.0,
                swap_total_bytes: 0,
                swap_used_bytes: 0,
                pressure: Pressure::Nominal,
            },
            disks: DiskMetrics {
                aggregate_total_bytes: 1,
                aggregate_available_bytes: 1,
                aggregate_used_bytes: 0,
                aggregate_usage_pct: 0.0,
                pressure: Pressure::Nominal,
                mounts: Vec::new(),
            },
            network: NetworkMetrics {
                rx_bytes_per_sec: 0.0,
                tx_bytes_per_sec: 0.0,
                rx_total_bytes: 0,
                tx_total_bytes: 0,
                window_secs: 1.0,
            },
            overall_pressure: Pressure::Nominal,
            sampled_at_unix: Some(seq),
        }
    }

    /// REGRESSION (#6642): a panicking step must not end the loop.
    ///
    /// Why: the sampler is one bare `tokio::spawn`. Before the guard, a panic
    /// anywhere in a tick killed the task — the history froze, every open SSE
    /// stream stayed connected emitting nothing, and no log line said why. That
    /// is indistinguishable from a quiet machine. Remove the `catch_unwind` in
    /// `guarded` and this test aborts the runtime instead of failing.
    /// What: runs a panicking step, asserts it reports `false`, then runs a
    /// second step that records into a real history and asserts it reports
    /// `true` and the sample landed. Also pins the payload rendering both
    /// `panic!` shapes produce.
    /// Test: this test itself.
    #[tokio::test]
    async fn a_panicking_step_is_contained_and_the_next_one_runs() {
        let history = MachineHistory::new();

        let survived = guarded("panicking", async { panic!("sampler exploded") }).await;
        assert!(
            !survived,
            "a panicking step reports that it did not complete"
        );

        let survived = guarded("recording", async {
            history.record_sample(stub_sample(1)).await;
        })
        .await;
        assert!(survived, "the step after a panic still runs");
        assert_eq!(
            history.snapshot().await.samples.len(),
            1,
            "the loop kept recording after a panicking step"
        );

        assert_eq!(panic_text(&"static str payload"), "static str payload");
        assert_eq!(panic_text(&"owned payload".to_string()), "owned payload");
        assert_eq!(panic_text(&7u8), "<non-string panic payload>");
    }

    /// REGRESSION (#6642): the two halves of a tick are guarded separately.
    ///
    /// Why: `sample_services` is new in #6642 and runs in the same task as the
    /// host sample. Guarding the whole tick as one unit would let a panic in the
    /// service half discard that tick's host sample too, so the machine graph
    /// would go flat for a fault that belongs to one row of the service list.
    /// What: drives three ticks where the service half always panics and the
    /// host half always records, then a fourth where both succeed. Asserts the
    /// host ring grew on every tick, the service rings stayed empty while the
    /// service half was failing — which the UI reads as a gap, not as `0.0` —
    /// and that a later good tick still records a service sample.
    /// Test: this test itself.
    #[tokio::test]
    async fn a_panicking_service_half_leaves_the_host_half_recording() {
        let history = MachineHistory::new();

        for seq in 1..=3u64 {
            guarded("host", async {
                history.record_sample(stub_sample(seq)).await;
            })
            .await;
            guarded("services", async {
                panic!("service sampling exploded on tick {seq}")
            })
            .await;

            let snap = history.snapshot().await;
            assert_eq!(
                snap.samples.len() as u64,
                seq,
                "the host ring keeps growing while the service half panics"
            );
            assert!(
                snap.service_samples.is_empty(),
                "a panicking service half records nothing for that tick — the UI reads the gap \
                 as no measurement, never as 0.0"
            );
        }

        guarded("services", async {
            history
                .record_service_samples(ServiceSampleBatch {
                    sampled_at_unix: 4,
                    services: vec![ServiceSample {
                        id: "trusty-search".to_string(),
                        status: ServiceStatus::Running,
                        cpu_pct: Some(2.0),
                    }],
                })
                .await;
        })
        .await;

        let snap = history.snapshot().await;
        assert_eq!(snap.samples.len(), 3);
        assert_eq!(
            snap.service_samples["trusty-search"].len(),
            1,
            "the service half recovers once its next tick succeeds"
        );
    }
}
