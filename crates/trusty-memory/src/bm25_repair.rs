//! Runtime repair for palaces whose BM25 coverage is known to be broken.
//!
//! Why: the live write path (`tools::bm25::bm25_index_enqueue`) writes into a
//! 256-slot bounded channel with `try_send` and DROPS on full. That trade is
//! defensible only if something eventually repairs a drop — and before this
//! module, nothing did. `spawn_startup_backfill` was the sole production
//! caller of backfill, so a drop stayed invisible until the next trusty-memory
//! restart, which on a long-lived daemon is measured in weeks. "Recoverable
//! across a restart" is not recoverability; it is a promise the process makes
//! and does not keep.
//!
//! What: a dirty set on [`AppState`] plus one periodic task. Every place that
//! observes lost coverage — a dropped enqueue, a startup sweep that could not
//! verify a palace, a repair pass that came up short — calls [`mark_dirty`].
//! [`spawn_repair_sweep`] drains the set on an interval and re-runs the
//! lossless backfill for each entry, re-marking any palace whose coverage is
//! still unverified afterwards. The sweep is idempotent and cheap when nothing
//! dropped: an empty set costs one wakeup.
//!
//! The sweep is bounded the same way the startup sweep is — serial, one palace
//! at a time, so it cannot outrun the supervisor's three-daemon cap.
//!
//! Test: `bm25_repair_tests.rs`.

use std::sync::Arc;
use std::time::Duration;

use crate::AppState;

/// Default interval between repair passes.
///
/// Why: a drop happens under write burst, and the burst is what makes the
/// repair expensive — so the sweep should wait out the burst rather than race
/// it. Five minutes is short enough that a dropped drawer is lexically
/// searchable within one coffee break and long enough that a sustained burst
/// does not trigger a repair per minute.
/// What: 300 seconds.
/// Test: `repair_interval_honours_env_override`.
pub const DEFAULT_REPAIR_INTERVAL_SECS: u64 = 300;

/// Environment override for the repair interval, in seconds.
///
/// Why: an operator on a write-heavy host may want repairs further apart, and
/// the integration test wants them closer together. `0` disables the sweep
/// entirely — an explicit, documented opt-out rather than an accident of
/// parsing, matching how `TRUSTY_BM25_RSS_LIMIT_MB=0` disables its limit.
/// What: env var `TRUSTY_BM25_REPAIR_INTERVAL_SECS`. Unparseable values fall
/// back to [`DEFAULT_REPAIR_INTERVAL_SECS`].
/// Test: `repair_interval_honours_env_override`.
pub const ENV_REPAIR_INTERVAL_SECS: &str = "TRUSTY_BM25_REPAIR_INTERVAL_SECS";

/// Set of palace ids whose BM25 coverage is known to be incomplete.
///
/// Why: `Arc<DashSet>` rather than a channel because the signal is idempotent
/// — a palace that dropped forty writes needs exactly one repair, not forty —
/// and because a set cannot overflow the way the bounded channel it exists to
/// compensate for can. Bounded by the palace count, which is bounded by disk.
/// What: shared by every `AppState` clone.
/// Test: `mark_dirty_is_idempotent`.
pub type DirtyPalaces = Arc<dashmap::DashSet<String>>;

/// Resolve the repair interval from the environment.
///
/// Why: same knob-with-a-typo argument as the supervisor's limits — silently
/// ignoring a malformed value is worse than not offering the knob.
/// What: parses as `u64`; `0` means disabled (`None`); unparseable falls back
/// to the default.
/// Test: `repair_interval_honours_env_override`.
pub fn repair_interval() -> Option<Duration> {
    match std::env::var(ENV_REPAIR_INTERVAL_SECS) {
        Ok(raw) => match raw.trim().parse::<u64>() {
            Ok(0) => None,
            Ok(n) => Some(Duration::from_secs(n)),
            Err(_) => {
                tracing::warn!(
                    "{ENV_REPAIR_INTERVAL_SECS}={raw:?} is not an integer — \
                     using default {DEFAULT_REPAIR_INTERVAL_SECS}s"
                );
                Some(Duration::from_secs(DEFAULT_REPAIR_INTERVAL_SECS))
            }
        },
        Err(_) => Some(Duration::from_secs(DEFAULT_REPAIR_INTERVAL_SECS)),
    }
}

/// Record that `palace` has lost BM25 coverage and needs a repair pass.
///
/// Why: this is the half of the drop-on-full trade that was missing. The write
/// path may keep dropping — that is the right call for `memory_remember`'s
/// latency — but each drop now leaves a durable-enough marker that a later
/// pass acts on, instead of a `warn!` nobody reads.
/// What: inserts the palace id. Idempotent: repeated drops for one palace
/// queue exactly one repair. Costs one hash insert on the write path, which is
/// why it is safe to call from `bm25_index_enqueue`'s `Full` arm.
/// Test: `mark_dirty_is_idempotent`, `bm25_index_queue_drops_when_full`.
pub fn mark_dirty(state: &AppState, palace: &str) {
    if state.bm25_dirty.insert(palace.to_string()) {
        tracing::debug!(
            palace = %palace,
            "bm25: palace queued for coverage repair"
        );
    }
}

/// Palaces currently queued for repair. Observability and tests.
pub fn dirty_palaces(state: &AppState) -> Vec<String> {
    state.bm25_dirty.iter().map(|e| e.key().clone()).collect()
}

/// Run one repair pass over the queued palaces.
///
/// Why: exposed separately from the timer so a test can drive a pass
/// deterministically rather than sleeping out an interval.
/// What: for each queued palace, resolves the handle with `open_palace` —
/// which HYDRATES from disk — and re-runs the lossless backfill. An entry is
/// removed only on verified coverage, or when the palace is genuinely absent
/// from disk. Everything else stays queued, so an interruption at any point
/// loses nothing.
///
/// `open_palace`, not `registry.get`: `get` is a bare LRU lookup that misses a
/// palace which has gone idle and been evicted — and a dirty palace is exactly
/// the kind that goes idle, because its writes are failing. Dropping it there
/// left its gap waiting for a restart, which is the outcome this module exists
/// to prevent. Telling "evicted" from "deleted" needs the on-disk palace list;
/// without it, a transient open failure and a deleted palace look identical and
/// one of the two answers is always wrong.
/// Returns `(attempted, repaired)`.
/// Test: `an_evicted_palace_is_rehydrated_not_dropped`,
/// `a_palace_absent_from_disk_is_dropped_from_the_queue`,
/// `an_unrepairable_palace_stays_queued`.
pub async fn run_repair_pass(state: &AppState) -> (usize, usize) {
    // Snapshot rather than drain: an entry leaves the set only once its
    // coverage is verified, so a panic or cancellation mid-pass cannot silently
    // discard the very gap the set exists to remember.
    let queued: Vec<String> = state.bm25_dirty.iter().map(|e| e.key().clone()).collect();
    if queued.is_empty() {
        return (0, 0);
    }

    let root = state.data_root.clone();
    let on_disk = match tokio::task::spawn_blocking(move || {
        trusty_common::memory_core::registry::PalaceRegistry::list_palaces(&root)
    })
    .await
    {
        Ok(Ok(palaces)) => palaces
            .into_iter()
            .map(|p| p.id.0)
            .collect::<std::collections::HashSet<String>>(),
        Ok(Err(e)) => {
            // Without the on-disk list we cannot tell a deleted palace from an
            // evicted one. Keep everything queued and try again next pass —
            // guessing here would drop a real gap.
            tracing::warn!("bm25 repair: could not enumerate palaces, deferring pass: {e:#}");
            return (0, 0);
        }
        Err(e) => {
            tracing::warn!("bm25 repair: enumeration task failed, deferring pass: {e}");
            return (0, 0);
        }
    };

    let mut repaired = 0usize;
    for palace in &queued {
        if !on_disk.contains(palace) {
            // Genuinely gone. Re-queueing a deleted palace spins forever.
            state.bm25_dirty.remove(palace);
            tracing::debug!(palace = %palace, "bm25 repair: palace no longer on disk — dropping");
            continue;
        }

        let id = trusty_common::memory_core::palace::PalaceId::new(palace.clone());
        let registry = Arc::clone(&state.registry);
        let root = state.data_root.clone();
        let handle = match tokio::task::spawn_blocking(move || registry.open_palace(&root, &id))
            .await
        {
            Ok(Ok(h)) => h,
            Ok(Err(e)) => {
                tracing::warn!(palace = %palace, "bm25 repair: open failed, staying queued: {e:#}");
                continue;
            }
            Err(e) => {
                tracing::warn!(palace = %palace, "bm25 repair: open task failed, staying queued: {e}");
                continue;
            }
        };

        let report =
            crate::bm25_backfill::backfill_state_palace(state, &handle, palace, false).await;
        if report.fully_indexed() {
            state.bm25_dirty.remove(palace);
            repaired += 1;
            tracing::info!(
                palace = %palace,
                indexed = report.indexed,
                "bm25 repair: coverage restored"
            );
        } else {
            // Stays queued — that is what makes "a drop is recoverable" true
            // across a daemon outage rather than only across a lucky one.
            tracing::warn!(
                palace = %palace,
                status = ?report.status,
                missing_after = ?report.missing_after,
                "bm25 repair: coverage still unverified — staying queued"
            );
        }
    }
    (queued.len(), repaired)
}

/// Start the periodic repair sweep.
///
/// Why: without a live trigger, a `try_send` drop is only repaired by a
/// restart, and the write path's justification for dropping ("it is
/// recoverable") is not true of the running process. This is that trigger.
/// What: no-op when the BM25 lane is off or the interval is disabled. Otherwise
/// spawns one task that ticks and calls [`run_repair_pass`]. The first tick is
/// one full interval away, so startup work is not competing with the startup
/// sweep.
/// Test: `repair_sweep_is_a_noop_without_the_lane`.
pub fn spawn_repair_sweep(state: &AppState) {
    if state.bm25.is_none() {
        tracing::debug!("bm25 repair: lane disabled — no repair sweep");
        return;
    }
    let Some(interval) = repair_interval() else {
        tracing::info!("bm25 repair: {ENV_REPAIR_INTERVAL_SECS}=0 — repair sweep disabled");
        return;
    };
    let state = state.clone();
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(interval);
        // The immediate first tick would race the startup sweep; skip it.
        ticker.tick().await;
        loop {
            ticker.tick().await;
            // Hardening note (#5048 re-review), not a guarantee: a panic
            // inside `run_repair_pass` aborts this task and silently disarms
            // the sweep for the process lifetime, with the queue still growing
            // and nothing draining it. No panic source is identified in the
            // pass today — every fallible step returns `Result` — so this is
            // recorded rather than guarded. What the pass DOES guarantee is
            // that such an abort loses no work: entries leave the queue only
            // on verified coverage, so every gap is still queued for whatever
            // runs next.
            let (attempted, repaired) = run_repair_pass(&state).await;
            if attempted > 0 {
                tracing::info!(attempted, repaired, "bm25 repair: pass complete");
            }
        }
    });
    tracing::info!(
        interval_secs = interval.as_secs(),
        "bm25 repair: coverage repair sweep armed"
    );
}

#[cfg(test)]
#[path = "bm25_repair_tests.rs"]
mod tests;
