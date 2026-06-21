//! DreamConfig, DreamStats, and PersistedDreamStats types.
//!
//! Why: Extracted from dream.rs to keep each file under the 500-SLOC cap
//! (#607). These are the tunables and telemetry types for the dream loop.
//! What: `DreamConfig` (loop tunables), `DreamStats` (per-cycle telemetry),
//! `PersistedDreamStats` (on-disk snapshot with timestamp).
//! Test: `dream::tests::dream_config_defaults` and
//! `dream::tests::dream_stats_persisted_after_cycle`.

use crate::memory_core::semantic_consolidation::SemanticConsolidationConfig;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Tunables for the dream loop.
///
/// Why: The defaults bias toward conservative consolidation (rare cycles, only
/// merge near-identical drawers, only prune truly forgotten ones). The
/// semantic consolidation sub-config is separate so it can be independently
/// tuned or disabled.
/// What: Plain values, all overridable. `semantic` holds the optional
/// inference-backed phase config.
/// Test: `dream_config_defaults`.
#[derive(Debug, Clone)]
pub struct DreamConfig {
    /// Seconds of inactivity before a dream cycle is allowed to run.
    pub idle_secs: u64,
    /// Cosine similarity above which two drawers are treated as duplicates.
    pub dedup_threshold: f32,
    /// Effective importance below which old drawers are pruned.
    pub prune_importance: f32,
    /// Wall-clock budget for one dream cycle.
    pub max_cycle_ms: u64,
    /// Whether to drop low-quality drawers by content inspection during dreaming.
    pub content_prune_enabled: bool,
    /// Drawers with fewer than this many whitespace-delimited words are dropped.
    pub content_prune_min_words: usize,
    /// Config for the optional inference-backed semantic consolidation phase.
    /// The phase only fires when both `semantic.enabled` and a configured LLM
    /// backend is available; it is silently skipped otherwise.
    pub semantic: SemanticConsolidationConfig,
    /// OpenRouter API key for the semantic consolidation phase. When non-empty,
    /// takes precedence over the `OPENROUTER_API_KEY` environment variable.
    pub openrouter_api_key: String,
    /// Whether the local Ollama (or compatible) model server is enabled.
    /// When `true` and no OpenRouter key is available, the semantic phase uses
    /// the local model at `http://localhost:11434`.
    pub local_model_enabled: bool,
    /// Whether to run the recall benchmark before and after each dream cycle.
    ///
    /// Why: The benchmark performs two full embed+search passes per cycle. On
    /// resource-constrained deployments or very frequent dream cycles, this
    /// overhead may be undesirable. Setting to `false` skips both passes and
    /// leaves `recall_score_before`/`recall_score_after` as `None`.
    /// What: When `false`, `dream_cycle` skips `run_benchmark` entirely; both
    /// recall score fields in `DreamStats` are `None`. Defaults to `true` so
    /// existing configs and behavior are unchanged.
    /// Test: `dream_cycle_recall_benchmark_disabled` asserts that when
    /// `recall_benchmark_enabled = false`, the cycle completes and both recall
    /// scores are `None`.
    pub recall_benchmark_enabled: bool,
}

impl Default for DreamConfig {
    fn default() -> Self {
        Self {
            idle_secs: 300,
            dedup_threshold: 0.95,
            prune_importance: 0.05,
            // 60s gives the dedup pass room to embed several hundred drawers
            // in one batch + run pairwise comparisons even on cold-start
            // embedder loads. The previous 5s budget was exhausted before the
            // pass could finish on palaces with ~100+ drawers (issue #55).
            max_cycle_ms: 60_000,
            content_prune_enabled: true,
            content_prune_min_words: 4,
            semantic: SemanticConsolidationConfig::default(),
            openrouter_api_key: String::new(),
            local_model_enabled: true,
            recall_benchmark_enabled: true,
        }
    }
}

/// Per-cycle dream telemetry.
///
/// Why: Operators need to see whether dreaming actually helps — raw action
/// counts (merged, pruned) are necessary but not sufficient. The compression
/// ratio captures structural change; the recall scores before/after capture
/// whether retrieval quality improved or degraded after consolidation.
/// What: Bundles counters for each dream phase plus effectiveness metrics
/// (drawer compression ratio and mean recall benchmark scores). All new fields
/// use `#[serde(default)]` for backward-compat with existing dream_stats.json
/// files that predate this struct extension.
/// Test: `dream_stats_serde_roundtrip_new_fields`, `dream_stats_backward_compat`,
/// `dream_compression_ratio_math`, `dream_compression_ratio_zero_drawers`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DreamStats {
    pub merged: usize,
    pub pruned: usize,
    pub closets_updated: usize,
    /// Orphaned vectors removed from the HNSW index because no surviving
    /// drawer row references them (issue #33).
    pub compacted: usize,
    /// Drawers dropped by the content-quality prune pass (issue #222):
    /// matches the blocklist or has fewer than `content_prune_min_words`
    /// words. Defaults to zero when the pass is disabled.
    #[serde(default)]
    pub content_pruned: usize,
    /// Number of canonical drawers added by the semantic consolidation phase
    /// (issue #87). Zero when the phase is disabled or no inference backend
    /// is configured.
    #[serde(default)]
    pub semantically_consolidated: usize,
    /// Number of LLM calls made during the semantic consolidation phase.
    #[serde(default)]
    pub semantic_llm_calls: usize,
    /// Number of LLM response cache hits in the semantic consolidation phase.
    #[serde(default)]
    pub semantic_cache_hits: usize,
    pub duration_ms: u64,

    // ── Effectiveness metrics (issue #1530) ──────────────────────────────────
    /// Total drawer count at the start of the dream cycle (before any passes).
    ///
    /// Why: Together with `drawers_after`, this gives the compression ratio —
    /// how many drawers were eliminated relative to what existed before.
    /// What: Snapshot of `handle.drawers.read().len()` taken at cycle entry.
    /// Test: `dream_cycle_records_drawer_counts` asserts this > 0 after seeding.
    #[serde(default)]
    pub drawers_before: u64,

    /// Total drawer count at the end of the dream cycle (after all passes).
    ///
    /// Why: Compared with `drawers_before` to compute `compression_ratio`.
    /// What: Snapshot of `handle.drawers.read().len()` taken after all passes.
    /// Test: `dream_cycle_records_drawer_counts`.
    #[serde(default)]
    pub drawers_after: u64,

    /// Fraction of drawers eliminated: `(before - after) / before`.
    ///
    /// Why: A single number that summarises structural consolidation for the
    /// admin dashboard. Serialised directly so `dream_stats.json` shows it
    /// without requiring clients to do arithmetic.
    /// What: `0.0` when `drawers_before == 0` (guard against divide-by-zero).
    /// Otherwise `(drawers_before - drawers_after) / drawers_before`. In
    /// `[0.0, 1.0]`; 0.0 means no net shrinkage OR net growth (growth is
    /// clamped to 0.0 via `saturating_sub`). Net growth can occur when the
    /// semantic consolidation phase adds canonical drawers.
    /// Test: `dream_compression_ratio_math`, `dream_compression_ratio_zero_drawers`.
    #[serde(default)]
    pub compression_ratio: f64,

    /// Mean top-3 retrieval score across the fixed benchmark query set,
    /// measured *before* the dream cycle ran any consolidation passes.
    ///
    /// Why: Establishes a quality baseline so we can compare with
    /// `recall_score_after`. A decrease post-dream signals the cycle
    /// accidentally discarded high-signal drawers.
    /// What: `None` when the palace is empty or the embedder is unavailable
    /// (graceful skip). Serialised as a JSON `null` in that case.
    /// Test: `dream_recall_benchmark_empty_palace_returns_none`,
    /// `dream_recall_benchmark_returns_score_with_drawers`.
    #[serde(default)]
    pub recall_score_before: Option<f64>,

    /// Mean top-3 retrieval score across the fixed benchmark query set,
    /// measured *after* all dream consolidation passes completed.
    ///
    /// Why: Pair with `recall_score_before` to compute delta. If post-dream
    /// score ≥ pre-dream score, the cycle improved or maintained quality.
    /// What: Same semantics as `recall_score_before`. `None` on skip.
    /// Test: `dream_recall_benchmark_returns_score_with_drawers`.
    #[serde(default)]
    pub recall_score_after: Option<f64>,
}

impl DreamStats {
    /// Compute and set the compression ratio from `drawers_before` and
    /// `drawers_after`.
    ///
    /// Why: Callers that update `drawers_before`/`drawers_after` independently
    /// need a single place to sync the derived `compression_ratio` field. This
    /// avoids duplicating the divide-by-zero guard.
    /// What: Sets `self.compression_ratio` to
    /// `(drawers_before - drawers_after) / drawers_before`, or `0.0` when
    /// `drawers_before == 0`. When `drawers_after > drawers_before` (net
    /// growth), the ratio is clamped to `0.0` via `saturating_sub` and a
    /// `tracing::warn!` is emitted so the growth is observable.
    /// Test: `dream_compression_ratio_math`, `dream_compression_ratio_zero_drawers`,
    /// `dream_compression_ratio_net_growth`.
    pub fn update_compression_ratio(&mut self) {
        self.compression_ratio = if self.drawers_before == 0 {
            0.0
        } else {
            if self.drawers_after > self.drawers_before {
                tracing::warn!(
                    drawers_before = self.drawers_before,
                    drawers_after = self.drawers_after,
                    "dream cycle: net palace growth detected (more drawers after than before); \
                     compression_ratio clamped to 0.0"
                );
            }
            let eliminated = self.drawers_before.saturating_sub(self.drawers_after);
            eliminated as f64 / self.drawers_before as f64
        };
    }
}

/// Persisted dream stats including the wall-clock timestamp of the run.
///
/// Why: The admin dashboard needs to display "last ran X minutes ago" so
/// operators can detect a stuck dream loop. The per-cycle stats alone don't
/// carry that signal; we wrap them with the run timestamp and snapshot to disk.
/// What: `DreamStats` + `last_run_at` (UTC). Persisted as JSON at
/// `<palace_data_dir>/dream_stats.json` after every cycle.
/// Test: `dream_stats_persisted_after_cycle`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistedDreamStats {
    pub last_run_at: chrono::DateTime<chrono::Utc>,
    #[serde(flatten)]
    pub stats: DreamStats,
}

impl PersistedDreamStats {
    /// File name used for the per-palace dream stats snapshot.
    pub const FILE_NAME: &'static str = "dream_stats.json";

    /// Read the persisted snapshot from `<data_dir>/dream_stats.json`, if any.
    ///
    /// Why: The dashboard reads this file directly via the web API; centralizing
    /// the path + parsing keeps every reader in sync.
    /// What: Returns `Ok(None)` when the file is missing; surfaces I/O and JSON
    /// errors as `Err`.
    /// Test: `dream_stats_persisted_after_cycle` reads back the snapshot.
    pub fn load(data_dir: &Path) -> Result<Option<Self>> {
        let path = data_dir.join(Self::FILE_NAME);
        if !path.exists() {
            return Ok(None);
        }
        let raw =
            std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
        let parsed: Self =
            serde_json::from_str(&raw).with_context(|| format!("parse {}", path.display()))?;
        Ok(Some(parsed))
    }

    /// Write the snapshot to `<data_dir>/dream_stats.json`.
    pub fn save(&self, data_dir: &Path) -> Result<()> {
        let path = data_dir.join(Self::FILE_NAME);
        let raw = serde_json::to_string_pretty(self).context("serialize dream stats")?;
        std::fs::write(&path, raw).with_context(|| format!("write {}", path.display()))?;
        Ok(())
    }
}
