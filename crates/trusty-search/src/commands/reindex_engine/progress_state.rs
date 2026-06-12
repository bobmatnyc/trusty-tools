//! Shared atomic counters published by the SSE event loop and read by the
//! wall-clock ticker.
//!
//! Why: the ticker refreshes the stats line every second even when SSE events
//! are sparse, so it needs lock-free access to the live file/chunk/skip/cps
//! counts and the active phase. Bundling the `Arc<Atomic*>` handles in one
//! struct keeps the driver from juggling eight separate clones.
//! What: `SharedProgress` holds the cloneable atomic handles plus the run
//! `started` instant; `new` zero-initialises them with the `Connecting` phase.
//! Test: counter behaviour covered by `tests::total_files_atomic_zero_until_set`.

use super::phase_map::phase_to_u64;
use crate::commands::reindex_ui::ReindexPhase;
use std::sync::atomic::AtomicU64;
use std::sync::Arc;
use std::time::Instant;

/// Lock-free progress counters shared by the SSE event loop (sole writer) and
/// the wall-clock ticker (reader).
///
/// Why: the ticker must render movement without locking `ReindexUi`; these
/// atomics are the single-writer/many-reader channel for that.
/// What: `Arc`-wrapped atomics for the file/chunk/skip/cps/total counters, an
/// in-flight chunk preview, the active-phase discriminant, and the shared
/// `started` instant. Cloning is cheap (`Arc` bumps).
/// Test: `tests::total_files_atomic_zero_until_set`.
#[derive(Clone)]
pub(super) struct SharedProgress {
    /// Wall-clock instant the run started; ticker derives elapsed/ETA from it.
    pub started: Instant,
    /// Cumulative files indexed (advanced by `batch`/`skip`/`chunk_progress`).
    pub indexed_now: Arc<AtomicU64>,
    /// Committed chunk count (advanced by `batch` events).
    pub chunks_now: Arc<AtomicU64>,
    /// In-flight chunk preview (advanced by `chunk_progress`, reset on `batch`).
    pub chunks_embed_preview: Arc<AtomicU64>,
    /// Cumulative skipped files.
    pub skipped_now: Arc<AtomicU64>,
    /// Latest per-batch embed throughput (chunks/sec).
    pub cps_now: Arc<AtomicU64>,
    /// Authoritative total file count (denominator for Files N/total + ETA).
    pub total_files_now: Arc<AtomicU64>,
    /// Active-phase discriminant (see [`super::phase_map`]).
    pub phase_disc: Arc<AtomicU64>,
}

impl SharedProgress {
    /// Construct a zero-initialised counter bundle anchored at `started`.
    ///
    /// Why: gives the event loop and ticker a common, lock-free state channel.
    /// What: allocates each atomic at 0 and seeds the phase to `Connecting`.
    /// Test: `tests::total_files_atomic_zero_until_set` asserts the zero-init.
    pub(super) fn new(started: Instant) -> Self {
        Self {
            started,
            indexed_now: Arc::new(AtomicU64::new(0)),
            chunks_now: Arc::new(AtomicU64::new(0)),
            chunks_embed_preview: Arc::new(AtomicU64::new(0)),
            skipped_now: Arc::new(AtomicU64::new(0)),
            cps_now: Arc::new(AtomicU64::new(0)),
            total_files_now: Arc::new(AtomicU64::new(0)),
            phase_disc: Arc::new(AtomicU64::new(phase_to_u64(ReindexPhase::Connecting))),
        }
    }
}
