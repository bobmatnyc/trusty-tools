//! DreamConfig, DreamStats, and PersistedDreamStats types.
//!
//! Why: Extracted from dream.rs to keep each file under the 500-SLOC cap
//! (#607). These are the tunables and telemetry types for the dream loop.
//! What: `DreamConfig` (loop tunables), `DreamStats` (per-cycle telemetry),
//! `PersistedDreamStats` (on-disk snapshot with timestamp).
//! Test: `dream::tests::dream_config_defaults` and
//! `dream::tests::dream_stats_persisted_after_cycle`.

use super::fading::{FadingMemory, FadingParams};
use crate::memory_core::semantic_consolidation::SemanticConsolidationConfig;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Floor for [`DreamConfig::prune_history_after_days`] (#6652).
///
/// Why: history rows are the only record that a fact ever changed. A config
/// that pruned them after a day would delete the evidence an operator reaches
/// for while a contradiction is still fresh. Seven days is the shortest window
/// in which someone plausibly notices and investigates.
pub const MIN_PRUNE_HISTORY_DAYS: i64 = 7;

/// Reclaimable share of the file below which a rewrite is not worth its cost.
///
/// Why: the rewrite reads and writes the whole file and takes a same-size
/// backup. Paying that to recover 2% is worse than leaving the slack alone.
pub const COMPACT_MIN_RECLAIM_PERCENT: u64 = 10;

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
    /// Off unless a config key turns it on (#5188), and even then it only
    /// fires when `semantic.model` resolves to a usable provider.
    pub semantic: SemanticConsolidationConfig,
    /// OpenRouter API key for the semantic consolidation phase. When non-empty,
    /// takes precedence over the `OPENROUTER_API_KEY` environment variable.
    pub openrouter_api_key: String,
    /// Whether a local model server may be used at all. Defaults to `false`
    /// (#5188) and only permits — it never selects. `semantic.model` must
    /// carry an explicit `ollama/` or `local/` prefix for the local backend to
    /// be chosen; this flag is the operator's veto over that choice.
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

    /// Whether the dream cycle rewrites `kg.redb` to reclaim disk (#6652).
    ///
    /// Why: redb never shrinks a live file, so a palace's KG store only grows —
    /// 342 MB on `trusty-tools` for 2,425 drawers. Nothing in the cycle
    /// reclaimed a byte of it before this flag existed; the step that looked
    /// like it might (`kg.checkpoint()`) is a documented no-op.
    /// What: `true` runs the prune-and-compact phase after the vector compact
    /// pass, subject to [`Self::compact_min_bytes`] and the reclaimable-ratio
    /// gate. `false` skips it entirely. Config key `dream.compact`.
    /// Test: `dream_config_defaults`, `kg_compaction_is_skipped_below_the_size_gate`.
    pub compact: bool,

    /// Age in days after which a closed `hist:` triple row is prunable (#6652).
    ///
    /// Why: every retraction and every functional-predicate overwrite leaves a
    /// permanent history row that no live query reads — only `dump_all_triples`
    /// and the export paths built on it. Deleting them unconditionally would
    /// destroy the audit trail an operator debugging a contradiction actually
    /// wants, so the prune is gated on age rather than on the rows being
    /// unread. 90 days is three times the existing `prune_pass` floor, because
    /// a history delete has no undo and a low-importance drawer prune
    /// effectively does.
    /// What: rows whose `valid_to` is older than this are skipped by the
    /// rewrite. Clamped to a floor of [`MIN_PRUNE_HISTORY_DAYS`] — a
    /// configuration that would delete last week's history is a mistake, not a
    /// preference. Config key `dream.prune_history_after_days`.
    /// Test: `history_prune_days_never_goes_below_the_floor`.
    pub prune_history_after_days: i64,

    /// File size below which the rewrite is not worth running (#6652).
    ///
    /// Why: a compaction costs a full read+write pass over the file plus a
    /// same-size backup, and it frees nothing a small palace misses. Running it
    /// on every idle tick for a 2 MB store is pure I/O.
    /// What: `kg.redb` must be at least this large AND its reclaimable estimate
    /// must be at least [`COMPACT_MIN_RECLAIM_PERCENT`] of the file before the
    /// phase runs. Config key `dream.compact_min_bytes`.
    /// Test: `kg_compaction_is_skipped_below_the_size_gate`.
    pub compact_min_bytes: u64,

    /// Whether to keep `kg.redb.pre-compact.bak` until the next run (#6652).
    ///
    /// Why: the rewrite replaces the palace's whole knowledge graph. A verified
    /// copy of the pre-rewrite bytes is the only recovery path if the new file
    /// turns out wrong in a way the row-count verification did not catch.
    /// What: `true` writes and size-verifies the backup before the copy starts;
    /// a backup that cannot be written aborts the compaction. Exactly one
    /// generation is kept — the previous backup is removed before the new one
    /// is written, so the safety net never becomes the bloat.
    /// Test: `a_backup_write_failure_aborts_before_the_copy_starts`.
    pub compact_keep_backup: bool,

    /// Tunables for the fading-memories resurface pass (issue #2352).
    ///
    /// Why: The resurface thresholds must be configurable per deployment (and,
    /// through this config, per palace) without recompiling. Embedded here the
    /// same way `semantic` is, so the dream loop keeps a single config surface.
    /// What: See [`FadingParams`]. Defaults resurface memories with base
    /// importance >= 0.7 whose effective importance has fallen below 0.3.
    /// Test: `dream_config_defaults`.
    pub fading: FadingParams,
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
            // #5188: a local model server is opt-in, never a fallback.
            local_model_enabled: false,
            recall_benchmark_enabled: true,
            // #6652: on by default — a palace that only grows is the bug.
            compact: true,
            prune_history_after_days: 90,
            compact_min_bytes: 64 * 1024 * 1024,
            compact_keep_backup: true,
            fading: FadingParams::default(),
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

    /// Bytes `kg.redb` shed in this cycle's copy-then-swap rewrite (#6652).
    ///
    /// Why: the pre-#6652 cycle reported `compression_ratio`, which counts
    /// DRAWERS and says nothing about file bytes — a palace could log zero net
    /// growth every cycle while `kg.redb` only grew, which is exactly what
    /// `trusty-tools` did. This is the byte-level twin.
    /// What: `bytes_before - bytes_after`, or `0` when the phase was gated off
    /// or skipped. `#[serde(default)]` keeps older `dream_stats.json` readable.
    /// Test: `dream_cycle_records_kg_compaction_stats`.
    #[serde(default)]
    pub kg_bytes_reclaimed: u64,

    /// `kg.redb` size after this cycle, for the doctor's growth check (#6652).
    ///
    /// Why: `trusty-memory doctor` warns when the store has grown sharply since
    /// the last cycle, and the only durable record of "last cycle" is this file.
    /// What: `metadata(kg.redb).len()` at the end of the phase; `0` when the
    /// palace has no on-disk store.
    /// Test: `dream_cycle_records_kg_compaction_stats`.
    #[serde(default)]
    pub kg_bytes_after: u64,

    /// Stale `hist:` triple rows the rewrite dropped this cycle (#6652).
    ///
    /// Test: `dream_cycle_records_kg_compaction_stats`.
    #[serde(default)]
    pub kg_history_rows_pruned: u64,

    /// Fading high-value memories detected this cycle (issue #2352).
    ///
    /// Why: Ranked list of originally-important memories whose effective
    /// importance has decayed below the resurface threshold. The cycle does
    /// NOT auto-boost them (that would defeat the forgetting curve); it
    /// surfaces them here so operators/agents can touch (recall re-boosts) or
    /// `memory_forget`. Persisted into `dream_stats.json` so the list survives
    /// until the next cycle. `#[serde(default)]` keeps older snapshots readable.
    /// What: Ranked by base importance desc then age desc, capped at
    /// `DreamConfig::fading.resurface_top_n`.
    /// Test: `dream::fading::tests` (detection/ranking) and the
    /// `palace_dream_response_includes_fading` MCP test in trusty-memory.
    #[serde(default)]
    pub fading: Vec<FadingMemory>,
}

impl DreamConfig {
    /// The history-prune age, clamped to [`MIN_PRUNE_HISTORY_DAYS`].
    ///
    /// Why: the clamp belongs at the read, not at the write. A value parsed
    /// from `~/.trusty-memory/config.toml` never passes through a constructor
    /// that could validate it, so a caller that read the field directly would
    /// bypass the floor.
    /// What: `max(prune_history_after_days, MIN_PRUNE_HISTORY_DAYS)`.
    /// Test: `history_prune_days_never_goes_below_the_floor`.
    pub fn effective_prune_history_days(&self) -> i64 {
        self.prune_history_after_days.max(MIN_PRUNE_HISTORY_DAYS)
    }
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
