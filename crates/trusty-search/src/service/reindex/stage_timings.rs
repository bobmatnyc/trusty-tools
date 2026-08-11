//! Coarse per-stage wall-clock timings for a reindex run (issue #5024).
//!
//! Why: before this module the run reported `walk_ms` plus four *subsystem*
//! accumulators summed inside the batch loop (`parse_ms`, `embed_ms`,
//! `bm25_ms`, `vector_upsert_ms`) and `kg_ms`. Everything else — the hash-cache
//! load, the incremental carryover copy of the live corpus into staging, the
//! prune pass, and the two staged-swap commits — landed in an undifferentiated
//! `model_load_approx_ms` residual computed by subtraction in `finish.rs`. That
//! residual is exactly the part a corpus-copy/delta-reindex scheme would target,
//! so it could not be sized without measuring it. See #5024.
//!
//! What: `StageTimings`, a plain accumulator the runner and the finish phase
//! stamp with `Instant` deltas at stage boundaries, plus `other_ms` — the
//! still-unattributed remainder. Cost is one `Instant::now()` pair per stage
//! per run, not per file.
//!
//! Test: `stage_timings_other_ms_is_saturating`,
//! `stage_timings_named_total_sums_every_field`, and the end-to-end assertion in
//! `super::tests::reindex_emits_per_stage_timings`.

/// Coarse wall-clock cost of each reindex stage outside the batch loop's own
/// subsystem accumulators.
///
/// Why: gives the `complete` event and the phase-timing log a per-stage
/// breakdown instead of one opaque residual (issue #5024).
/// What: milliseconds per stage, stamped once per run. Zero means the stage did
/// not run — a force reindex skips carryover and prune, a BM25-only index skips
/// both swap commits.
/// Test: `stage_timings_named_total_sums_every_field`.
#[derive(Debug, Default, Clone, Copy)]
pub(super) struct StageTimings {
    /// Loading the persisted SHA-256 file-hash cache out of redb, or seeding it
    /// from an adopted staging corpus on a resume.
    pub(super) hash_cache_ms: u64,
    /// `begin_staged_corpus_swap` — dominated by the incremental carryover copy
    /// of every live-corpus row into the fresh staging store. Always 0 for a
    /// `force` run, which stages an empty corpus.
    pub(super) carryover_ms: u64,
    /// Wall time of the producer/consumer batch loop, from spawning the producer
    /// to the consumer draining the channel.
    ///
    /// This is NOT the sum of `parse_ms + embed_ms + bm25_ms +
    /// vector_upsert_ms`: parse/embed run in the producer task while the
    /// consumer commits the previous batch, so those four can total more than
    /// this wall figure. The gap in the other direction — this figure exceeding
    /// them — is embedder warm-up and channel stalls.
    pub(super) pipeline_ms: u64,
    /// Prune pass: deleting chunks for files that vanished from disk.
    pub(super) prune_ms: u64,
    /// Committing (or rolling back) the staged redb corpus.
    pub(super) corpus_commit_ms: u64,
    /// Resolving the staged HNSW snapshot, including the awaited final save.
    pub(super) hnsw_commit_ms: u64,
    /// Awaiting the RSS poller tasks after setting their stop flag.
    ///
    /// This is pure teardown latency, not work: each poller checks its stop flag
    /// only after its `tokio::time::interval` fires, so the join waits out the
    /// remainder of the current tick (1s for the daemon poller, 500ms for the
    /// embedderd one). It is charged to every reindex regardless of size, which
    /// makes it invisible on a cold index and dominant on a warm delta pass —
    /// the reason it gets its own field instead of hiding in `other_ms`.
    pub(super) poller_stop_ms: u64,
}

impl StageTimings {
    /// Sum of every named stage, including the caller-supplied walk and KG
    /// figures that live outside this struct.
    ///
    /// Why: `other_ms` needs it, and the phase-timing log prints it so an
    /// operator can see at a glance how much of the run is still unattributed.
    /// What: saturating sum — a `u64` millisecond total cannot realistically
    /// overflow, but saturation keeps this total-free of panics in release and
    /// debug alike.
    /// Test: `stage_timings_named_total_sums_every_field`.
    pub(super) fn named_total_ms(&self, walk_ms: u64, kg_ms: u64) -> u64 {
        walk_ms
            .saturating_add(self.hash_cache_ms)
            .saturating_add(self.carryover_ms)
            .saturating_add(self.pipeline_ms)
            .saturating_add(self.prune_ms)
            .saturating_add(self.corpus_commit_ms)
            .saturating_add(self.hnsw_commit_ms)
            .saturating_add(self.poller_stop_ms)
            .saturating_add(kg_ms)
    }

    /// Wall-clock time not attributed to any named stage.
    ///
    /// Why: keeps the breakdown honest — the named stages are checkpoints, not
    /// an exhaustive partition, so the leftover has to be visible rather than
    /// silently absorbed into whichever stage happens to be measured last.
    /// Covers stage-status flips, the checkpoint probe, `refresh_context_embedding`,
    /// poller teardown, and lock waits.
    /// What: `elapsed_ms - named_total_ms`, saturating at 0. Saturation matters:
    /// the KG rebuild and the swap commits are measured on nested clocks, so
    /// rounding can make the named total exceed `elapsed_ms` by a millisecond or
    /// two on a very short run.
    /// Test: `stage_timings_other_ms_is_saturating`.
    pub(super) fn other_ms(&self, elapsed_ms: u64, walk_ms: u64, kg_ms: u64) -> u64 {
        elapsed_ms.saturating_sub(self.named_total_ms(walk_ms, kg_ms))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stage_timings_named_total_sums_every_field() {
        let t = StageTimings {
            hash_cache_ms: 1,
            carryover_ms: 2,
            pipeline_ms: 4,
            prune_ms: 8,
            corpus_commit_ms: 16,
            hnsw_commit_ms: 32,
            poller_stop_ms: 64,
        };
        // 1+2+4+8+16+32+64 = 127, plus walk=128 and kg=256.
        assert_eq!(t.named_total_ms(128, 256), 511);
    }

    #[test]
    fn stage_timings_other_ms_is_saturating() {
        let t = StageTimings {
            pipeline_ms: 100,
            ..Default::default()
        };
        assert_eq!(t.other_ms(150, 10, 5), 35);
        // Nested clocks can overshoot the outer elapsed figure — must not wrap.
        assert_eq!(t.other_ms(50, 10, 5), 0);
    }

    #[test]
    fn stage_timings_default_is_all_zero() {
        let t = StageTimings::default();
        assert_eq!(t.named_total_ms(0, 0), 0);
        assert_eq!(t.other_ms(500, 0, 0), 500);
    }
}
