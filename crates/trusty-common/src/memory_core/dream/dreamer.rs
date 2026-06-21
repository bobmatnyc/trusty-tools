//! `Dreamer` — background idle-time memory consolidation driver.
//!
//! Why: Extracted from dream.rs to keep each file under the 500-SLOC cap
//! (#607). The Dreamer owns the idle clock, the optional injected
//! SemanticConsolidator, and the background loop logic.
//! What: `Dreamer` struct with `new`, `with_consolidator`, `touch`,
//! `is_idle`, `start`, `start_with_shutdown`, and `dream_cycle`.
//! Test: `dreamer_touch_resets_idle`, `dreamer_shutdown_terminates_loop`,
//! `dream_cycle_merges_duplicates`, etc.

use super::config::{DreamConfig, DreamStats};
use super::cycle::{
    compact_pass, content_prune_pass, dedup_pass, prune_pass, refresh_closets,
    semantic_consolidation_pass,
};
use super::guard::CompactionGuard;
use super::recall_benchmark::run_benchmark;
use crate::memory_core::retrieval::PalaceHandle;
use crate::memory_core::semantic_consolidation::SemanticConsolidator;
use anyhow::{Context, Result};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use super::helpers::now_secs;

/// Background memory consolidator.
///
/// Why: We need a small, testable unit that owns the idle clock and the
/// consolidation logic — separate from the daemon that schedules it.
/// What: `last_activity` is a unix-seconds atomic touched on every recall /
/// remember; `dream_cycle` runs synchronously and returns stats. The optional
/// `consolidator` field allows tests to inject a `MockInference`-backed
/// `SemanticConsolidator` without touching the real LLM.
/// Test: `dreamer_touch_resets_idle` plus the cycle tests below.
pub struct Dreamer {
    pub config: DreamConfig,
    pub(super) last_activity: Arc<AtomicU64>,
    /// Injected semantic consolidator (used in tests via `with_consolidator`).
    /// When `None`, `semantic_consolidation_pass` builds the consolidator from
    /// `config` at runtime.
    pub(super) consolidator: Option<Arc<SemanticConsolidator>>,
}

impl Dreamer {
    /// Build a new dreamer with the given config and `last_activity = now`.
    ///
    /// Why: A fresh palace shouldn't immediately dream — start the idle clock
    /// from "now" so the first cycle waits a full `idle_secs`.
    /// What: Captures `SystemTime::now()` as unix seconds. The `consolidator`
    /// field is `None`; the semantic phase will construct it lazily from config.
    /// Test: `dreamer_touch_resets_idle`.
    pub fn new(config: DreamConfig) -> Self {
        Self {
            config,
            last_activity: Arc::new(AtomicU64::new(now_secs())),
            consolidator: None,
        }
    }

    /// Build a new dreamer with an injected `SemanticConsolidator`.
    ///
    /// Why: Tests need to supply a `MockInference`-backed consolidator so the
    /// dream cycle can be verified without making real LLM calls. Production
    /// code always uses `Dreamer::new`.
    /// What: Stores the provided `Arc<SemanticConsolidator>` so
    /// `semantic_consolidation_pass` uses it instead of building one from
    /// config. The semantic phase is always attempted when the consolidator is
    /// injected (ignoring `inference_available`).
    /// Test: `dream_cycle_semantic_consolidation_with_mock`.
    pub fn with_consolidator(config: DreamConfig, consolidator: Arc<SemanticConsolidator>) -> Self {
        Self {
            config,
            last_activity: Arc::new(AtomicU64::new(now_secs())),
            consolidator: Some(consolidator),
        }
    }

    /// Record activity (call from recall / remember paths).
    pub fn touch(&self) {
        self.last_activity.store(now_secs(), Ordering::Relaxed);
    }

    /// Has the palace been idle longer than `idle_secs`?
    pub fn is_idle(&self) -> bool {
        let last = self.last_activity.load(Ordering::Relaxed);
        now_secs().saturating_sub(last) >= self.config.idle_secs
    }

    /// Spawn the background dream loop.
    ///
    /// Why: A long-lived daemon needs a per-palace task that wakes periodically,
    /// checks the idle clock, and runs one cycle when appropriate.
    /// What: Spawns a tokio task that sleeps `idle_secs`, calls `dream_cycle`
    /// when `is_idle`, and logs the resulting stats. Runs forever; cancel by
    /// dropping the daemon.
    /// Test: Behavioral coverage via direct `dream_cycle` calls; the loop
    /// itself is just a sleep + dispatch.
    pub fn start(self: Arc<Self>, handle: Arc<PalaceHandle>) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let interval = Duration::from_secs(self.config.idle_secs.max(1));
            loop {
                tokio::time::sleep(interval).await;
                if !self.is_idle() {
                    continue;
                }
                match self.dream_cycle(&handle).await {
                    Ok(stats) => tracing::info!(
                        palace = %handle.id,
                        merged = stats.merged,
                        pruned = stats.pruned,
                        content_pruned = stats.content_pruned,
                        compacted = stats.compacted,
                        closets_updated = stats.closets_updated,
                        semantically_consolidated = stats.semantically_consolidated,
                        semantic_llm_calls = stats.semantic_llm_calls,
                        duration_ms = stats.duration_ms,
                        drawers_before = stats.drawers_before,
                        drawers_after = stats.drawers_after,
                        compression_ratio = stats.compression_ratio,
                        "dream cycle complete"
                    ),
                    Err(e) => tracing::warn!(palace = %handle.id, "dream cycle failed: {e:#}"),
                }
            }
        })
    }

    /// Spawn the background dream loop with a cooperative shutdown signal.
    ///
    /// Why: A long-running daemon needs to stop its background workers cleanly
    /// on SIGTERM / Ctrl-C; otherwise the process can block on shutdown waiting
    /// for an in-flight cycle, or worse, terminate mid-cycle and leave on-disk
    /// state inconsistent. A `tokio::sync::watch` channel is the cheapest way
    /// to fan out a single cancel signal to every spawned task.
    /// What: Spawns a tokio task that races the inter-cycle sleep against the
    /// shutdown signal. When `shutdown` flips to `true`, the loop logs and
    /// exits cleanly. When the shutdown sender is dropped, the loop also
    /// exits (treated as a cancel).
    /// Test: `dreamer_shutdown_terminates_loop` — spawn the loop, flip the
    /// shutdown flag, await the join handle.
    pub fn start_with_shutdown(
        self: Arc<Self>,
        handle: Arc<PalaceHandle>,
        mut shutdown: tokio::sync::watch::Receiver<bool>,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let interval = Duration::from_secs(self.config.idle_secs.max(1));
            loop {
                tokio::select! {
                    _ = tokio::time::sleep(interval) => {}
                    res = shutdown.changed() => {
                        // Sender closed (`Err`) or value changed to true: shut down.
                        if res.is_err() || *shutdown.borrow() {
                            tracing::info!(palace = %handle.id, "dreamer shutting down");
                            return;
                        }
                    }
                }
                if *shutdown.borrow() {
                    tracing::info!(palace = %handle.id, "dreamer shutting down");
                    return;
                }
                if !self.is_idle() {
                    continue;
                }
                match self.dream_cycle(&handle).await {
                    Ok(stats) => tracing::info!(
                        palace = %handle.id,
                        merged = stats.merged,
                        pruned = stats.pruned,
                        content_pruned = stats.content_pruned,
                        compacted = stats.compacted,
                        closets_updated = stats.closets_updated,
                        semantically_consolidated = stats.semantically_consolidated,
                        semantic_llm_calls = stats.semantic_llm_calls,
                        duration_ms = stats.duration_ms,
                        drawers_before = stats.drawers_before,
                        drawers_after = stats.drawers_after,
                        compression_ratio = stats.compression_ratio,
                        "dream cycle complete"
                    ),
                    Err(e) => tracing::warn!(palace = %handle.id, "dream cycle failed: {e:#}"),
                }
            }
        })
    }

    /// Run one synchronous dream cycle: dedup, prune, closet refresh, flush,
    /// and optional inference-backed semantic consolidation.
    ///
    /// Why: Consolidation must happen as a single, bounded unit so we can
    /// schedule it conservatively and report telemetry to the operator.
    /// What:
    ///   1. Content-prune: drop noise drawers matching the blocklist or below
    ///      the minimum word count.
    ///   2. Dedup near-duplicates by L3-searching each drawer; if the top
    ///      neighbor's score >= `dedup_threshold`, merge into the higher-
    ///      importance survivor and `forget` the loser.
    ///   3. Prune drawers whose effective importance falls below
    ///      `prune_importance` AND whose age exceeds 30 days.
    ///   4. Compact orphaned vectors from the HNSW index.
    ///   5. Rebuild the closet index (keyword -> drawer ids).
    ///   6. (Optional) Semantic consolidation: when an inference backend is
    ///      available, cluster near-duplicate drawers and canonicalize them via
    ///      LLM. Original drawers are preserved; canonical drawers are added
    ///      with a `superseded_by` link in the KG. Gracefully skipped when no
    ///      inference backend is configured.
    ///   7. Flush the L1 snapshot.
    ///
    /// Test: `dream_cycle_merges_duplicates`, `dream_cycle_prunes_low_importance`,
    /// `closet_refresh_builds_index`, `dream_cycle_semantic_consolidation_with_mock`,
    /// `dream_cycle_semantic_consolidation_no_inference`.
    pub async fn dream_cycle(&self, handle: &Arc<PalaceHandle>) -> Result<DreamStats> {
        let started = std::time::Instant::now();
        let budget = Duration::from_millis(self.config.max_cycle_ms);
        // Mark the palace as compacting for the entirety of this cycle so the
        // operator dashboard can render the dreaming spinner. The guard clears
        // the flag on drop, which keeps it correct on early-return errors and
        // panics alike.
        let _compaction_guard = CompactionGuard::new(handle.is_compacting.clone());

        // ── Effectiveness metric: pre-cycle snapshot (issue #1530) ────────────
        // Count drawers before any pass so we can compute the compression ratio.
        let drawers_before = handle.drawers.read().len() as u64;

        // Recall benchmark before consolidation — skip silently on failure.
        let recall_score_before = run_benchmark(handle).await;

        let content_pruned = if self.config.content_prune_enabled {
            content_prune_pass(handle, started, budget, self.config.content_prune_min_words)
                .await
                .context("dream content prune pass")?
        } else {
            0
        };
        let merged = dedup_pass(handle, started, budget, self.config.dedup_threshold)
            .await
            .context("dream dedup pass")?;
        let pruned = prune_pass(handle, started, budget, self.config.prune_importance)
            .await
            .context("dream prune pass")?;
        let compacted = compact_pass(handle, started, budget)
            .await
            .context("dream compact pass")?;
        let closets_updated = refresh_closets(handle);

        // ── Phase: Semantic consolidation (optional, inference-gated) ──────────
        let (semantically_consolidated, semantic_llm_calls, semantic_cache_hits) =
            semantic_consolidation_pass(handle, &self.config, self.consolidator.clone()).await;

        // Persist the trimmed L1 snapshot so a restart sees the consolidated state.
        if let Err(e) = handle.flush() {
            tracing::warn!("dream flush failed: {e:#}");
        }

        // ── Effectiveness metric: post-cycle snapshot (issue #1530) ───────────
        let drawers_after = handle.drawers.read().len() as u64;

        // Recall benchmark after consolidation — skip silently on failure.
        let recall_score_after = run_benchmark(handle).await;

        let mut stats = DreamStats {
            merged,
            pruned,
            closets_updated,
            compacted,
            content_pruned,
            semantically_consolidated,
            semantic_llm_calls,
            semantic_cache_hits,
            duration_ms: started.elapsed().as_millis() as u64,
            drawers_before,
            drawers_after,
            compression_ratio: 0.0, // populated below
            recall_score_before,
            recall_score_after,
        };
        stats.update_compression_ratio();

        // WAL checkpoint — PASSIVE mode is non-blocking. Issue #36: without
        // periodic checkpointing the SQLite WAL grows unbounded over a
        // long-running daemon's lifetime.
        match handle.kg.checkpoint() {
            Ok((wal, done)) => {
                tracing::debug!(
                    palace = %handle.id,
                    wal_pages = wal,
                    checkpointed = done,
                    "WAL checkpoint complete"
                );
            }
            Err(e) => {
                tracing::warn!(
                    palace = %handle.id,
                    error = %e,
                    "WAL checkpoint failed (non-fatal)"
                );
            }
        }

        // Snapshot the run for the admin dashboard. Failures here are
        // non-fatal — the cycle itself succeeded, we just couldn't record it.
        if let Some(data_dir) = handle.data_dir.as_ref() {
            use super::config::PersistedDreamStats;
            let persisted = PersistedDreamStats {
                last_run_at: chrono::Utc::now(),
                stats: stats.clone(),
            };
            if let Err(e) = persisted.save(data_dir) {
                tracing::warn!(palace = %handle.id, "persist dream_stats.json failed: {e:#}");
            }
        }

        Ok(stats)
    }
}
