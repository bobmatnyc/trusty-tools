//! RSS poller tasks for the reindex orchestrator.
//!
//! Why: extracted from `orchestrator.rs` (issue #1175 SLOC cap) to keep each file
//! under the 500-SLOC production limit. These background tasks sample process RSS
//! to enforce `TRUSTY_MEMORY_LIMIT_MB` and track embedderd peak memory.
//!
//! What: two helpers — `spawn_memory_poller` watches the daemon's own RSS and trips
//! a shared `AtomicBool` abort flag; `spawn_embedderd_rss_poller` samples the
//! embedderd sidecar PID's RSS.
//!
//! Test: `memory_limit_aborts_reindex_mid_batch` and
//! `embedderd_peak_rss_captured_on_complete` (marked `#[ignore]`).

use crate::core::memguard::{current_rss_mb, current_rss_mb_for_pid};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering as AtomicOrdering};
use std::sync::Arc;
use std::time::Duration;

/// Spawn the background RSS poller that watches for `TRUSTY_MEMORY_LIMIT_MB`
/// breaches.
///
/// Why: extracted from `spawn_reindex_with_cleanup` (issue #98) so the
/// memory-protection plumbing is isolated from the batch loop. Always run
/// even when no `mem_limit` is configured so `peak_rss_mb` is accurate for
/// the final log line.
/// What: ticks every `MEM_POLL_INTERVAL`, updates `peak_rss` monotonically,
/// and trips `mem_abort` the first time RSS crosses `mem_limit`. Returns the
/// join handle plus a stop-flag the caller flips when the reindex finishes.
/// Test: `memory_limit_aborts_reindex_mid_batch` (memory-abort integration test).
pub(super) fn spawn_memory_poller(
    mem_limit: Option<u64>,
    mem_abort: Arc<AtomicBool>,
    peak_rss: Arc<AtomicU64>,
    index_id: String,
) -> (tokio::task::JoinHandle<()>, Arc<AtomicBool>) {
    /// How often the background poller samples RSS.
    const MEM_POLL_INTERVAL: Duration = Duration::from_secs(1);

    let stop = Arc::new(AtomicBool::new(false));
    let stop_clone = stop.clone();
    let handle = tokio::spawn(async move {
        let mut ticker = tokio::time::interval(MEM_POLL_INTERVAL);
        // Drop the immediate first tick so we don't double-sample with the
        // synchronous `current_rss_mb()` already done before spawning.
        ticker.tick().await;
        loop {
            if stop_clone.load(AtomicOrdering::Acquire) {
                break;
            }
            if let Some(rss) = current_rss_mb() {
                // Update peak monotonically (CAS loop).
                let mut prev = peak_rss.load(AtomicOrdering::Acquire);
                while rss > prev {
                    match peak_rss.compare_exchange_weak(
                        prev,
                        rss,
                        AtomicOrdering::AcqRel,
                        AtomicOrdering::Acquire,
                    ) {
                        Ok(_) => break,
                        Err(cur) => prev = cur,
                    }
                }
                if let Some(limit) = mem_limit {
                    if rss >= limit && !mem_abort.load(AtomicOrdering::Acquire) {
                        tracing::warn!(
                            "reindex memory poller: rss={}MB >= limit={}MB \
                             — tripping abort flag for index {}",
                            rss,
                            limit,
                            index_id,
                        );
                        mem_abort.store(true, AtomicOrdering::Release);
                    }
                }
            }
            ticker.tick().await;
        }
    });
    (handle, stop)
}

/// Spawn a background poller that tracks the peak RSS of the embedderd sidecar
/// during a reindex run (issue #282).
///
/// Why: the daemon's own RSS poller covers only the daemon parent process. The
/// embedderd sidecar process owns the ONNX arena and routinely uses 2–3 GB
/// more than the daemon during active embedding; omitting it leaves operators
/// with an incomplete picture for capacity planning and regression testing.
/// What: reads the current sidecar PID from `embedderd_pid_slot` on each tick.
/// A PID of 0 (no sidecar, or sidecar exited mid-run) causes the sample to be
/// skipped gracefully. Stops when `stop` is set to `true` by the orchestrator.
/// Test: `embedderd_peak_rss_captured_on_complete` (marked `#[ignore]`).
pub(super) fn spawn_embedderd_rss_poller(
    embedderd_pid_slot: Arc<AtomicU32>,
    peak_embedderd_rss: Arc<AtomicU64>,
) -> (tokio::task::JoinHandle<()>, Arc<AtomicBool>) {
    /// Polling cadence for the embedderd RSS sampler.
    const EMBEDDERD_POLL_INTERVAL: Duration = Duration::from_millis(500);

    let stop = Arc::new(AtomicBool::new(false));
    let stop_clone = stop.clone();
    let handle = tokio::spawn(async move {
        let mut ticker = tokio::time::interval(EMBEDDERD_POLL_INTERVAL);
        ticker.tick().await;
        loop {
            if stop_clone.load(AtomicOrdering::Acquire) {
                break;
            }
            let pid = embedderd_pid_slot.load(AtomicOrdering::Acquire);
            if let Some(rss) = current_rss_mb_for_pid(pid) {
                // Monotonic peak update (same CAS loop as the main poller).
                let mut prev = peak_embedderd_rss.load(AtomicOrdering::Acquire);
                while rss > prev {
                    match peak_embedderd_rss.compare_exchange_weak(
                        prev,
                        rss,
                        AtomicOrdering::AcqRel,
                        AtomicOrdering::Acquire,
                    ) {
                        Ok(_) => break,
                        Err(cur) => prev = cur,
                    }
                }
            }
            ticker.tick().await;
        }
    });
    (handle, stop)
}
