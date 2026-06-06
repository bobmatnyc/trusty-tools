//! Dreaming — background idle-time memory consolidation with optional
//! inference-backed semantic consolidation.
//!
//! Why: Long-running palaces accumulate near-duplicate drawers, low-importance
//! noise, and stale closet indexes. Periodic consolidation during idle windows
//! keeps retrieval fast and the L1 cache focused on what matters. When an LLM
//! inference backend is available the optional `SemanticConsolidate` phase also
//! canonicalizes paraphrases and aliases that the NLP-only passes miss.
//! What: `DreamConfig` (tunables), `DreamStats` (per-cycle telemetry), and
//! `Dreamer` (idle clock + `dream_cycle` doing content-prune, dedup, prune,
//! compaction, closet refresh, and optional semantic consolidation).
//! Test: `cargo test -p trusty-memory-core dream::tests::` exercises every
//! moving part — defaults, idle clock, merge, prune, closet refresh, and the
//! semantic-consolidation integration tests.

use crate::memory_core::decay::DecayConfig;
use crate::memory_core::palace::{Drawer, RoomType};
use crate::memory_core::retrieval::{PalaceHandle, shared_embedder};
use crate::memory_core::semantic_consolidation::{
    SemanticConsolidationConfig, SemanticConsolidator, inference_available,
};
use crate::memory_core::store::vector::VectorStore;
use anyhow::{Context, Result};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use uuid::Uuid;

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
        }
    }
}

/// Substring patterns whose presence in a drawer's content marks it as
/// low-value auto-capture noise that retroactive dreaming should drop.
///
/// Why: PR #221 introduced an identical blocklist at the write path
/// (`trusty-memory/src/tools.rs`) so new writes never land. But drawers
/// captured before that gate shipped — `Tool use: Bash`, `Claude Code session
/// ended: <uuid>`, etc. — already pollute existing palaces. The dream cycle
/// is the right place to retroactively enforce the same policy without
/// requiring an admin migration script.
/// What: Substring patterns (not regexes) checked via `str::contains` after
/// `str::trim_start`. Mirrors the write-path list exactly so both gates stay
/// in lock-step. Patterns are matched case-sensitively because the
/// auto-capture hooks always emit the exact English prefix.
/// Test: `dream_content_prune_drops_blocklist_drawer`.
const CONTENT_BLOCKLIST: &[&str] = &[
    "Tool use: ",          // Claude Code tool-use captures
    "Claude Code session", // Session lifecycle events
];

/// Per-cycle dream telemetry.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
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

/// RAII guard that toggles a palace's `is_compacting` flag for the lifetime
/// of a dream cycle.
///
/// Why: A plain `flag.store(true)` at the top of `dream_cycle` and
/// `flag.store(false)` at the bottom leaks `true` if any pass returns an
/// error or panics, leaving the dashboard stuck on "dreaming". A Drop guard
/// guarantees the flag clears on every exit path.
/// What: Stores `true` in the supplied `AtomicBool` on construction and
/// `false` on drop, both with `Relaxed` ordering (the dashboard read path
/// uses the same ordering — exact happens-before semantics across tasks are
/// not required for a UI indicator).
/// Test: `dream::tests::dream_cycle_toggles_is_compacting`.
struct CompactionGuard {
    flag: Arc<AtomicBool>,
}

impl CompactionGuard {
    /// Why: Centralises the "set flag, then return guard" pattern so callers
    /// can't forget the drop side.
    /// What: Stores `true` and returns the guard.
    /// Test: `dream::tests::dream_cycle_toggles_is_compacting`.
    fn new(flag: Arc<AtomicBool>) -> Self {
        flag.store(true, Ordering::Relaxed);
        Self { flag }
    }
}

impl Drop for CompactionGuard {
    fn drop(&mut self) {
        self.flag.store(false, Ordering::Relaxed);
    }
}

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
    last_activity: Arc<AtomicU64>,
    /// Injected semantic consolidator (used in tests via `with_consolidator`).
    /// When `None`, `semantic_consolidation_pass` builds the consolidator from
    /// `config` at runtime.
    consolidator: Option<Arc<SemanticConsolidator>>,
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

        let content_pruned = if self.config.content_prune_enabled {
            self.content_prune_pass(handle, started, budget)
                .await
                .context("dream content prune pass")?
        } else {
            0
        };
        let merged = self
            .dedup_pass(handle, started, budget)
            .await
            .context("dream dedup pass")?;
        let pruned = self
            .prune_pass(handle, started, budget)
            .await
            .context("dream prune pass")?;
        let compacted = self
            .compact_pass(handle, started, budget)
            .await
            .context("dream compact pass")?;
        let closets_updated = self.refresh_closets(handle);

        // ── Phase: Semantic consolidation (optional, inference-gated) ──────────
        let (semantically_consolidated, semantic_llm_calls, semantic_cache_hits) =
            self.semantic_consolidation_pass(handle).await;

        // Persist the trimmed L1 snapshot so a restart sees the consolidated state.
        if let Err(e) = handle.flush() {
            tracing::warn!("dream flush failed: {e:#}");
        }

        let stats = DreamStats {
            merged,
            pruned,
            closets_updated,
            compacted,
            content_pruned,
            semantically_consolidated,
            semantic_llm_calls,
            semantic_cache_hits,
            duration_ms: started.elapsed().as_millis() as u64,
        };

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

    /// Drop drawers whose content is recognisably noise: matches the
    /// `CONTENT_BLOCKLIST` substrings or contains fewer than
    /// `config.content_prune_min_words` whitespace-delimited words. Returns
    /// the number of drawers dropped.
    ///
    /// Why: The write-path blocklist (PR #221) only gates new writes. Pre-
    /// existing drawers that slipped through before the gate need periodic
    /// cleanup; the dream cycle is the right place for retroactive quality
    /// enforcement so palaces self-heal without admin migrations.
    /// What: Snapshots the in-memory drawer table, applies the same content
    /// rule the write path uses (trim leading whitespace, substring-check
    /// against `CONTENT_BLOCKLIST`) plus a word-count floor, and forgets each
    /// matching drawer via `PalaceHandle::forget`. Respects the per-cycle
    /// wall-clock `budget` deadline.
    /// Test: `dream_content_prune_drops_blocklist_drawer`,
    /// `dream_content_prune_drops_short_drawer`,
    /// `dream_content_prune_keeps_good_drawer`.
    async fn content_prune_pass(
        &self,
        handle: &Arc<PalaceHandle>,
        started: std::time::Instant,
        budget: Duration,
    ) -> Result<usize> {
        let snapshot: Vec<Drawer> = handle.drawers.read().clone();
        let mut victims: Vec<Uuid> = Vec::new();

        for drawer in snapshot.iter() {
            if started.elapsed() >= budget {
                break;
            }
            if is_low_quality_content(&drawer.content, self.config.content_prune_min_words) {
                victims.push(drawer.id);
            }
        }

        let count = victims.len();
        for id in victims {
            if started.elapsed() >= budget {
                break;
            }
            if let Err(e) = handle.forget(id).await {
                tracing::warn!(?id, "dream content prune: forget failed: {e:#}");
            }
        }
        Ok(count)
    }

    /// Remove orphaned vectors from the HNSW index whose drawer row no longer
    /// exists. Returns the number of vectors removed.
    ///
    /// Why: Dedup and prune remove drawers via `handle.forget`, which removes
    /// the matching vector. But over a palace's lifetime, vectors can also be
    /// orphaned by partial writes, schema migrations, or pre-fix bugs that
    /// dropped drawer rows without removing the corresponding vector. This
    /// pass closes the gap and clears the `index_vectors >> drawer_records`
    /// cold-start warning (issue #33).
    /// What: Snapshots drawer ids into a `HashSet`, asks the vector store for
    /// every id it currently tracks, and removes any vector whose id is not
    /// in the drawer set. Respects the per-cycle wall-clock budget. Returns 0
    /// silently when the vector store can't enumerate ids (e.g. cold reload
    /// before any upsert this session).
    /// Test: `dream_cycle_compacts_orphaned_vectors`.
    async fn compact_pass(
        &self,
        handle: &Arc<PalaceHandle>,
        started: std::time::Instant,
        budget: Duration,
    ) -> Result<usize> {
        let drawer_ids: std::collections::HashSet<Uuid> =
            handle.drawers.read().iter().map(|d| d.id).collect();

        // Addressable pass: walk every id our key_map knows about and drop
        // anything missing from the drawer table.
        let vector_ids = handle.vector_store.all_ids();
        let mut removed: usize = 0;
        for vid in vector_ids {
            if started.elapsed() >= budget {
                break;
            }
            if drawer_ids.contains(&vid) {
                continue;
            }
            match handle.vector_store.remove(vid).await {
                Ok(()) => removed += 1,
                Err(e) => tracing::warn!(?vid, "dream compact: vector remove failed: {e:#}"),
            }
        }

        // Fallback rebuild: if the index still reports significantly more
        // vectors than the drawer table holds (e.g. pre-fix orphans we can't
        // enumerate via key_map), reset the index and re-upsert every drawer
        // from scratch. Costly but bounded — only runs when the divergence is
        // material, and re-embedding 100s of drawers takes <1s on the local
        // ONNX model.
        let drawer_count = drawer_ids.len();
        let index_size_after = handle.vector_store.index_size();
        // Only rebuild when we have drawers to re-embed AND the index has at
        // least 1 + 2*drawer_count entries (well past noise). Avoids tight
        // rebuild loops on a healthy small palace.
        if drawer_count > 0 && index_size_after > drawer_count.saturating_mul(2) + 1 {
            let rebuilt = rebuild_index_from_drawers(handle, started, budget)
                .await
                .context("dream compact rebuild")?;
            // `rebuilt` counts every drawer we re-upserted; the number of
            // orphans removed via rebuild is `index_size_before - drawer_count`.
            // Surface a conservative `removed` increment by counting the
            // delta as orphans dropped from the index.
            let delta = index_size_after.saturating_sub(rebuilt);
            removed = removed.saturating_add(delta);
        }

        Ok(removed)
    }

    /// Find near-duplicates and merge survivors; returns the merge count.
    ///
    /// Why: The previous implementation initialised `FastEmbedder` once but
    /// then called `recall_deep` per drawer — each call does a fresh embed
    /// (50–100ms on the local ONNX model) plus an L3 search. On a palace with
    /// ~100 drawers that's >5s, which exceeded the per-cycle budget (issue
    /// #55). Batch-embedding all drawer contents upfront turns the inner loop
    /// into pure vector arithmetic via `vector_store.search`, which is
    /// sub-millisecond per query.
    /// What: Snapshots drawers, batch-embeds every drawer's content in one
    /// `embed_batch` call, then iterates each drawer and uses its pre-computed
    /// vector to search the HNSW index for near-duplicates. `vector_store
    /// .search` returns pure cosine similarity (1 - distance), so no
    /// importance-renormalisation is required. Survivors are picked by raw
    /// `importance`; losers are merged in and forgotten.
    async fn dedup_pass(
        &self,
        handle: &Arc<PalaceHandle>,
        started: std::time::Instant,
        budget: Duration,
    ) -> Result<usize> {
        let snapshot: Vec<Drawer> = handle.drawers.read().clone();
        if snapshot.len() < 2 {
            return Ok(0);
        }

        // Reuse the process-wide shared embedder instead of constructing a
        // fresh ONNX session for every dream cycle (issue #57). The previous
        // per-cycle construction multiplied the daemon's memory footprint by
        // the number of palaces.
        let embedder = shared_embedder()
            .await
            .context("acquire shared embedder for dream dedup")?;

        let contents: Vec<String> = snapshot.iter().map(|d| d.content.clone()).collect();
        let vectors = embedder
            .embed_batch(&contents)
            .await
            .context("batch embed drawers for dream dedup")?;

        if vectors.len() != snapshot.len() {
            // Defensive: embedder must return one vector per input.
            anyhow::bail!(
                "embedder returned {} vectors for {} drawers",
                vectors.len(),
                snapshot.len()
            );
        }

        let mut merges: usize = 0;
        let mut already_removed: std::collections::HashSet<Uuid> = std::collections::HashSet::new();

        for (drawer, query_vec) in snapshot.iter().zip(vectors.iter()) {
            if started.elapsed() >= budget {
                break;
            }
            if already_removed.contains(&drawer.id) {
                continue;
            }
            // Top-3 keeps the dedup pass cheap; the first neighbor is `drawer`
            // itself (score ~1.0) so we look at index 1+. `vector_store.search`
            // returns pure cosine similarity — no importance weighting baked
            // in, so we can compare directly to `dedup_threshold`.
            let hits = handle.vector_store.search(query_vec, 3).await?;
            for hit in hits.into_iter() {
                if hit.drawer_id == drawer.id || already_removed.contains(&hit.drawer_id) {
                    continue;
                }
                if hit.score < self.config.dedup_threshold {
                    continue;
                }
                // Resolve the loser's drawer record from the snapshot. If it's
                // not in the snapshot (e.g. orphan vector), skip — the compact
                // pass will clean it up.
                let Some(hit_drawer) = snapshot.iter().find(|d| d.id == hit.drawer_id) else {
                    continue;
                };

                // Pick survivor (higher importance wins; ties keep `drawer`).
                let (survivor, loser) = if drawer.importance >= hit_drawer.importance {
                    (drawer.clone(), hit_drawer.clone())
                } else {
                    (hit_drawer.clone(), drawer.clone())
                };
                merge_into(handle, &survivor, &loser);
                let _ = handle.forget(loser.id).await;
                already_removed.insert(loser.id);
                merges += 1;
                // Only one merge per source to keep behavior predictable.
                break;
            }
        }
        Ok(merges)
    }

    /// Drop drawers whose effective importance is below `prune_importance`
    /// AND that are older than 30 days. Returns the prune count.
    async fn prune_pass(
        &self,
        handle: &Arc<PalaceHandle>,
        started: std::time::Instant,
        budget: Duration,
    ) -> Result<usize> {
        const MIN_AGE_DAYS: f32 = 30.0;
        let snapshot: Vec<Drawer> = handle.drawers.read().clone();
        let mut victims: Vec<Uuid> = Vec::new();

        for drawer in snapshot.iter() {
            if started.elapsed() >= budget {
                break;
            }
            let age = DecayConfig::age_days(drawer.created_at);
            let boost = drawer.accumulated_boost(&handle.decay_config);
            let eff = handle
                .decay_config
                .effective_importance(drawer.importance, age, boost);
            // `<=` (not `<`): once a drawer's effective importance decays to
            // the floor — meaning it's old and unimportant enough that the
            // decay clamp kicked in — it becomes prunable. Using strict `<`
            // here created the floor-collision bug (#55): with the default
            // `floor = prune_importance = 0.05`, the condition `eff < 0.05`
            // was unsatisfiable, so nothing was ever pruned.
            if eff <= self.config.prune_importance && age > MIN_AGE_DAYS {
                victims.push(drawer.id);
            }
        }

        let count = victims.len();
        for id in victims {
            let _ = handle.forget(id).await;
        }
        Ok(count)
    }

    /// Rebuild closets: simple whitespace tokenization, stop-word filter,
    /// keyword -> drawer ids. Returns the number of keywords indexed.
    fn refresh_closets(&self, handle: &Arc<PalaceHandle>) -> usize {
        let snapshot: Vec<Drawer> = handle.drawers.read().clone();
        let mut new_index: HashMap<String, Vec<Uuid>> = HashMap::new();
        for drawer in snapshot.iter() {
            for kw in extract_keywords(&drawer.content) {
                new_index.entry(kw).or_default().push(drawer.id);
            }
        }
        let count = new_index.len();
        let mut closets = handle.closets.write();
        *closets = new_index;
        count
    }

    /// Optional inference-backed semantic consolidation pass.
    ///
    /// Why: the NLP-only passes miss semantic equivalence (aliases, paraphrases,
    /// near-duplicate triples expressed differently). This phase delegates
    /// canonicalization to a cheap LLM, preserving original drawers and adding
    /// canonical replacements with `superseded_by` links in the KG.
    /// What: gates on `inference_available`; when false logs at DEBUG and
    /// returns `(0, 0, 0)` immediately. When true (or when a consolidator is
    /// injected via `Dreamer::with_consolidator`), runs consolidation on all
    /// current drawers, writes each canonical drawer via `handle.remember`,
    /// and records the `superseded_by` KG triple so the original drawers are
    /// traceable. Returns `(canonical_count, llm_calls, cache_hits)`.
    /// Test: `dream_cycle_semantic_consolidation_with_mock` (injected
    /// consolidator); `dream_cycle_semantic_consolidation_no_inference`.
    async fn semantic_consolidation_pass(
        &self,
        handle: &Arc<PalaceHandle>,
    ) -> (usize, usize, usize) {
        if !self.config.semantic.enabled {
            tracing::debug!(
                palace = %handle.id,
                "skipping semantic consolidation: disabled in config"
            );
            return (0, 0, 0);
        }

        // Use the injected consolidator (test path) or build one from config.
        let consolidator: Arc<SemanticConsolidator> = if let Some(c) = self.consolidator.clone() {
            c
        } else {
            // Production path: gate on inference availability.
            let api_key = if !self.config.openrouter_api_key.is_empty() {
                self.config.openrouter_api_key.clone()
            } else {
                std::env::var("OPENROUTER_API_KEY").unwrap_or_default()
            };

            if !inference_available(&api_key, self.config.local_model_enabled) {
                tracing::debug!(
                    palace = %handle.id,
                    "skipping semantic consolidation: inference unavailable \
                     (set OPENROUTER_API_KEY or enable local_model)"
                );
                return (0, 0, 0);
            }

            // Build the inference backend: prefer local model (free),
            // fall back to OpenRouter.
            use crate::memory_core::semantic_consolidation::{
                OllamaInference, OpenRouterInference,
            };
            let backend: Arc<dyn crate::memory_core::semantic_consolidation::Inference> =
                if self.config.local_model_enabled && api_key.is_empty() {
                    Arc::new(OllamaInference::new(
                        "http://localhost:11434",
                        &self.config.semantic.model,
                    ))
                } else {
                    Arc::new(OpenRouterInference::new(
                        api_key,
                        &self.config.semantic.model,
                    ))
                };

            Arc::new(SemanticConsolidator::new(
                backend,
                self.config.semantic.clone(),
            ))
        };

        let snapshot: Vec<Drawer> = handle.drawers.read().clone();
        if snapshot.is_empty() {
            return (0, 0, 0);
        }

        let consolidation_result = consolidator.consolidate(&snapshot).await;

        // Apply results: add canonical drawers, mark superseded ids in KG.
        let mut canonical_count = 0usize;

        for canonical in &consolidation_result.canonical_drawers {
            // Add the canonical drawer to the palace.
            let room_type = RoomType::General;
            match handle
                .remember(
                    canonical.content.clone(),
                    room_type,
                    canonical.tags.clone(),
                    canonical.importance,
                )
                .await
            {
                Ok(canonical_id) => {
                    canonical_count += 1;
                    // Record `superseded_by` triples in the KG for every
                    // original drawer so the provenance chain is preserved.
                    for &orig_id in &canonical.canonical_for {
                        let triple_subject = format!("drawer:{orig_id}");
                        let triple_object = format!("drawer:{canonical_id}");
                        let triple = crate::memory_core::store::kg::Triple {
                            subject: triple_subject,
                            predicate: "superseded_by".to_string(),
                            object: triple_object,
                            valid_from: chrono::Utc::now(),
                            valid_to: None,
                            confidence: 1.0,
                            provenance: Some("dream:semantic_consolidation".to_string()),
                        };
                        if let Err(e) = handle.kg.assert(triple).await {
                            tracing::warn!(
                                orig = %orig_id,
                                canonical = %canonical_id,
                                "failed to write superseded_by triple: {e:#}"
                            );
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        content = &canonical.content[..canonical.content.len().min(80)],
                        "dream semantic: failed to add canonical drawer: {e:#}"
                    );
                }
            }
        }

        // Store aliases as KG triples.
        for (from, to) in &consolidation_result.aliases {
            let triple = crate::memory_core::store::kg::Triple {
                subject: from.clone(),
                predicate: "alias_of".to_string(),
                object: to.clone(),
                valid_from: chrono::Utc::now(),
                valid_to: None,
                confidence: 1.0,
                provenance: Some("dream:semantic_consolidation".to_string()),
            };
            if let Err(e) = handle.kg.assert(triple).await {
                tracing::warn!(
                    from,
                    to,
                    "dream semantic: failed to write alias triple: {e:#}"
                );
            }
        }

        // Log flagged contradictions (no auto-resolution).
        for (id, reason) in &consolidation_result.flagged_ids {
            tracing::info!(
                palace = %handle.id,
                drawer_id = %id,
                reason,
                "dream semantic: flagged drawer for human review (contradiction)"
            );
        }

        tracing::debug!(
            palace = %handle.id,
            canonical_added = canonical_count,
            aliases = consolidation_result.aliases.len(),
            flagged = consolidation_result.flagged_ids.len(),
            llm_calls = consolidation_result.llm_calls,
            cache_hits = consolidation_result.cache_hits,
            "semantic consolidation phase complete"
        );

        (
            canonical_count,
            consolidation_result.llm_calls,
            consolidation_result.cache_hits,
        )
    }
}

/// Reset the vector index and re-upsert every drawer from the in-memory
/// drawer table. Returns the number of drawers re-embedded.
///
/// Why: When the HNSW index accumulates orphans we can't address through
/// `key_map` (pre-fix data, partial writes, schema migrations), the cheapest
/// correct fix is to throw away the index and rebuild from the authoritative
/// drawer table.
/// What: Snapshots drawers, calls `UsearchStore::reset` to truncate the
/// index, then re-embeds and re-upserts each drawer. Respects the budget by
/// stopping early — incomplete rebuilds are still safe (the next cycle picks
/// up where this one left off).
async fn rebuild_index_from_drawers(
    handle: &Arc<PalaceHandle>,
    started: std::time::Instant,
    budget: Duration,
) -> Result<usize> {
    let snapshot: Vec<Drawer> = handle.drawers.read().clone();
    handle
        .vector_store
        .reset()
        .context("reset vector index for rebuild")?;

    if snapshot.is_empty() {
        return Ok(0);
    }

    let embedder = shared_embedder()
        .await
        .context("acquire shared embedder for dream rebuild")?;

    let mut rebuilt: usize = 0;
    for drawer in snapshot.iter() {
        if started.elapsed() >= budget {
            break;
        }
        let vecs = embedder
            .embed_batch(std::slice::from_ref(&drawer.content))
            .await
            .with_context(|| format!("re-embed drawer {}", drawer.id))?;
        if let Some(v) = vecs.into_iter().next() {
            handle
                .vector_store
                .upsert(drawer.id, v)
                .await
                .with_context(|| format!("re-upsert drawer {}", drawer.id))?;
            rebuilt += 1;
        }
    }
    Ok(rebuilt)
}

/// Merge `loser` content into `survivor` (in-memory drawer table only).
///
/// Why: Dreaming consolidates duplicates without losing information; we
/// concatenate the loser's content into the survivor (capped) and union tags.
/// What: Updates the in-memory drawer entry for `survivor.id`. The vector
/// store entry remains keyed to the survivor; the loser's vector is removed
/// by the caller via `handle.forget`.
fn merge_into(handle: &Arc<PalaceHandle>, survivor: &Drawer, loser: &Drawer) {
    let mut drawers = handle.drawers.write();
    if let Some(target) = drawers.iter_mut().find(|d| d.id == survivor.id) {
        let mut combined = target.content.clone();
        combined.push_str("\n\nAlso: ");
        combined.push_str(&loser.content);
        if combined.len() > 500 {
            combined.truncate(500);
        }
        target.content = combined;
        target.importance = target.importance.max(loser.importance);
        for tag in &loser.tags {
            if !target.tags.contains(tag) {
                target.tags.push(tag.clone());
            }
        }
    }
}

/// Stop-word filter for closet keyword extraction.
const STOP_WORDS: &[&str] = &[
    "the", "a", "an", "is", "are", "was", "were", "be", "been", "being", "of", "in", "on", "at",
    "to", "for", "with", "and", "or", "but", "not", "no", "yes", "i", "you", "he", "she", "it",
    "we", "they", "this", "that", "these", "those", "as", "by", "from", "into", "over", "under",
    "if", "then", "than", "so", "do", "does", "did", "have", "has", "had", "will", "would",
    "shall", "should", "can", "could", "may", "might", "must", "about", "any", "all", "some",
    "more", "most", "such",
];

/// Extract keyword tokens from a drawer's content.
///
/// Why: Closets are a lightweight pre-computed index; we want stable, deduped
/// keyword tokens so the dream cycle's index is reproducible.
/// What: Lowercases, strips non-alphanumeric chars, drops stop-words and
/// tokens shorter than 3 chars, and dedups within a single drawer.
/// Test: Indirectly via `closet_refresh_builds_index`.
pub fn extract_keywords(content: &str) -> Vec<String> {
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut out: Vec<String> = Vec::new();
    for raw in content.split_whitespace() {
        let token: String = raw
            .chars()
            .filter(|c| c.is_alphanumeric())
            .flat_map(|c| c.to_lowercase())
            .collect();
        if token.len() < 3 {
            continue;
        }
        if STOP_WORDS.iter().any(|s| *s == token) {
            continue;
        }
        if seen.insert(token.clone()) {
            out.push(token);
        }
    }
    out
}

/// Returns true when `content` should be dropped by the content-quality
/// prune pass.
///
/// Why: Centralises the "is this drawer noise?" decision so the prune pass
/// and its tests share one rule. The rule mirrors the write-path gate
/// (`trusty-memory::tools::blocklist_gate` plus a minimum word-count
/// floor) so a drawer that wouldn't be written today is also a drawer
/// that should not survive the next dream cycle.
/// What: Trims leading whitespace, then returns true iff the trimmed content
/// contains any `CONTENT_BLOCKLIST` substring, OR the whitespace-delimited
/// word count is strictly less than `min_words`. An empty `content` (zero
/// words) is always low-quality whenever `min_words >= 1`.
/// Test: `dream_content_prune_drops_blocklist_drawer`,
/// `dream_content_prune_drops_short_drawer`,
/// `dream_content_prune_keeps_good_drawer`.
fn is_low_quality_content(content: &str, min_words: usize) -> bool {
    let trimmed = content.trim_start();
    if CONTENT_BLOCKLIST.iter().any(|pat| trimmed.contains(pat)) {
        return true;
    }
    let word_count = content.split_whitespace().count();
    word_count < min_words
}

/// Current unix timestamp in seconds. Saturates to 0 on clock errors.
fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// Quiet a dead-code warning for the legacy import re-export when the type is
// only used through `Arc<PalaceHandle>` in this module.
#[allow(dead_code)]
type _PalaceHandleRef = RwLock<()>;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory_core::palace::{Palace, PalaceId, RoomType};
    use crate::memory_core::retrieval::{PalaceHandle, seed_shared_embedder_with_mock};
    use chrono::{Duration as ChronoDuration, Utc};
    use tempfile::tempdir;

    /// Why: Lock the default config values so accidental changes are caught.
    #[test]
    fn dream_config_defaults() {
        let cfg = DreamConfig::default();
        assert_eq!(cfg.idle_secs, 300);
        assert!((cfg.dedup_threshold - 0.95).abs() < 1e-6);
        assert!((cfg.prune_importance - 0.05).abs() < 1e-6);
        assert_eq!(cfg.max_cycle_ms, 60_000);
        assert!(
            cfg.content_prune_enabled,
            "content-quality pruning is on by default"
        );
        assert_eq!(cfg.content_prune_min_words, 4);
    }

    /// Why: `touch` must reset the idle clock; with `idle_secs=0` `is_idle`
    /// flips to `true` immediately, and `touch` must NOT make it stay false
    /// for >= idle_secs of zero. We use idle_secs=2 and assert the transition.
    #[test]
    fn dreamer_touch_resets_idle() {
        let dreamer = Dreamer::new(DreamConfig {
            idle_secs: 2,
            ..DreamConfig::default()
        });
        // Just-constructed: last_activity = now, so idle_secs has not elapsed.
        assert!(!dreamer.is_idle(), "fresh dreamer should not be idle yet");

        // Force the idle clock far into the past.
        dreamer
            .last_activity
            .store(now_secs().saturating_sub(10), Ordering::Relaxed);
        assert!(dreamer.is_idle(), "should be idle after 10s simulated wait");

        // Touch resets it.
        dreamer.touch();
        assert!(!dreamer.is_idle(), "touch should reset idle clock");
    }

    async fn open_test_handle(name: &str) -> Arc<PalaceHandle> {
        // Pre-seed the process-wide embedder with MockEmbedder so no HuggingFace
        // download is attempted. Safe to call multiple times — OnceCell semantics
        // make subsequent calls a no-op. Issue #850.
        seed_shared_embedder_with_mock();
        let dir = tempdir().unwrap();
        let palace = Palace {
            id: PalaceId::new(name),
            name: name.into(),
            description: None,
            created_at: Utc::now(),
            data_dir: dir.path().join(name),
        };
        std::fs::create_dir_all(&palace.data_dir).unwrap();
        let handle = PalaceHandle::open(&palace).unwrap();
        // Keep the tempdir alive by leaking it for the duration of the test —
        // tests are short and tempdir cleanup at process exit is fine.
        std::mem::forget(dir);
        handle
    }

    /// Why: Two near-identical drawers should collapse to one after a dream
    /// cycle so the L1 cache isn't filled with duplicates.
    /// What: Insert two drawers with the same content (verbatim — embeddings
    /// will land identically), run a dream cycle with default config, and
    /// assert the count drops from 2 to 1.
    /// Test: This test itself.
    #[tokio::test]
    async fn dream_cycle_merges_duplicates() {
        let handle = open_test_handle("dream-merge").await;
        handle
            .remember(
                "Rust uses HNSW for vector search".into(),
                RoomType::Backend,
                vec!["rust".into()],
                0.7,
            )
            .await
            .unwrap();
        handle
            .remember(
                "Rust uses HNSW for vector search".into(),
                RoomType::Backend,
                vec!["rust".into()],
                0.6,
            )
            .await
            .unwrap();
        assert_eq!(handle.drawers.read().len(), 2);

        let dreamer = Dreamer::new(DreamConfig::default());
        let stats = dreamer.dream_cycle(&handle).await.unwrap();

        assert_eq!(stats.merged, 1, "expected exactly one merge");
        assert_eq!(handle.drawers.read().len(), 1, "expected dedup to 1 drawer");
    }

    /// Why: Old, low-importance drawers must be pruned so storage doesn't
    /// grow without bound.
    /// What: Insert one drawer with importance=0.01 and back-date its
    /// `created_at` to 60 days ago (older than the 30-day prune floor); run
    /// dream_cycle and assert it's gone.
    /// Test: This test itself.
    #[tokio::test]
    async fn dream_cycle_prunes_low_importance() {
        let handle = open_test_handle("dream-prune").await;
        handle
            .remember(
                "very stale fact nobody cares about".into(),
                RoomType::General,
                vec![],
                0.01,
            )
            .await
            .unwrap();
        // Back-date this drawer to satisfy the >30 days requirement.
        {
            let mut drawers = handle.drawers.write();
            for d in drawers.iter_mut() {
                d.created_at = Utc::now() - ChronoDuration::days(60);
            }
        }
        assert_eq!(handle.drawers.read().len(), 1);

        let dreamer = Dreamer::new(DreamConfig::default());
        let stats = dreamer.dream_cycle(&handle).await.unwrap();

        assert_eq!(stats.pruned, 1, "expected exactly one prune");
        assert!(
            handle.drawers.read().is_empty(),
            "low-importance aged drawer should be removed"
        );
    }

    /// Why: Regression for issue #55. With the previous strict `<` condition
    /// and `prune_importance == DecayConfig::floor == 0.05`, a drawer whose
    /// `effective_importance` decayed to the floor was clamped at exactly
    /// `0.05`, making `eff < 0.05` unsatisfiable — nothing was ever pruned.
    /// The `<=` fix means a drawer at the floor (old, unimportant) is now
    /// correctly eligible for pruning.
    /// What: Insert one drawer with `importance == prune_importance == floor`,
    /// age it past 30 days so the decay floor clamps `eff`, run a cycle, and
    /// assert it gets pruned.
    /// Test: This test itself.
    #[tokio::test]
    async fn dream_cycle_prunes_at_floor_importance() {
        let handle = open_test_handle("dream-prune-floor").await;
        // Importance exactly at the prune threshold (and decay floor default).
        handle
            .remember(
                "drawer that decays to the floor".into(),
                RoomType::General,
                vec![],
                0.05,
            )
            .await
            .unwrap();
        {
            let mut drawers = handle.drawers.write();
            for d in drawers.iter_mut() {
                // 60 days ago — well past the 30-day prune-age floor and
                // enough decay time to push `eff` down to `floor`.
                d.created_at = Utc::now() - ChronoDuration::days(60);
            }
        }
        assert_eq!(handle.drawers.read().len(), 1);

        let dreamer = Dreamer::new(DreamConfig::default());
        let stats = dreamer.dream_cycle(&handle).await.unwrap();

        assert_eq!(
            stats.pruned, 1,
            "drawer at floor importance + aged > 30d must be prunable (was unsatisfiable under strict `<`)"
        );
        assert!(handle.drawers.read().is_empty());
    }

    /// Why: The serve daemon must be able to terminate the dream loop on
    /// SIGTERM/Ctrl-C; verify the watch-channel shutdown path actually causes
    /// the spawned task to exit instead of looping forever.
    /// What: Spawn `start_with_shutdown` with `idle_secs=10` (so it would
    /// otherwise sleep), flip the shutdown flag, and assert the join handle
    /// completes within a short bounded timeout.
    /// Test: This test itself.
    #[tokio::test]
    async fn dreamer_shutdown_terminates_loop() {
        let handle = open_test_handle("dream-shutdown").await;
        let dreamer = Arc::new(Dreamer::new(DreamConfig {
            idle_secs: 10,
            ..DreamConfig::default()
        }));
        let (tx, rx) = tokio::sync::watch::channel(false);
        let join = dreamer.clone().start_with_shutdown(handle, rx);

        // Yield once so the task is scheduled.
        tokio::task::yield_now().await;
        tx.send(true).expect("send shutdown signal");

        // The task should exit promptly — bound the wait to keep the test fast.
        let outcome = tokio::time::timeout(Duration::from_secs(2), join).await;
        assert!(
            outcome.is_ok(),
            "dream loop did not exit within 2s of shutdown"
        );
        outcome.unwrap().expect("join handle clean exit");
    }

    /// Why: When drawer rows disappear without their matching vector being
    /// removed (partial write, schema migration, pre-fix bug), the HNSW index
    /// fills with orphans and the cold-start warning fires. The compact pass
    /// must clean these up so `index_vectors == drawer_records` again.
    /// What: Remember three drawers, then directly remove two from the drawer
    /// table (bypassing `forget`, so the vectors stay in the HNSW index),
    /// then run a dream cycle and assert exactly two vectors were compacted.
    /// Test: This test itself.
    #[tokio::test]
    async fn dream_cycle_compacts_orphaned_vectors() {
        let handle = open_test_handle("dream-compact").await;
        let id_keep = handle
            .remember(
                "alpha drawer about HNSW".into(),
                RoomType::Backend,
                vec![],
                0.7,
            )
            .await
            .unwrap();
        let id_orphan_a = handle
            .remember(
                "beta drawer about something else".into(),
                RoomType::General,
                vec![],
                0.5,
            )
            .await
            .unwrap();
        let id_orphan_b = handle
            .remember(
                "gamma drawer about yet another topic".into(),
                RoomType::General,
                vec![],
                0.5,
            )
            .await
            .unwrap();

        assert_eq!(handle.drawers.read().len(), 3);
        let before_idx = handle.vector_store.index_size();
        let before_ids = handle.vector_store.all_ids().len();
        assert_eq!(before_ids, 3, "key_map should track all three upserts");

        // Manually orphan two: drop them from the drawer table (and the SQLite
        // mirror) but leave their vectors in the HNSW index. This mirrors the
        // pre-fix bug pattern that produced 720 index vectors against 129
        // drawer rows.
        {
            let mut drawers = handle.drawers.write();
            drawers.retain(|d| d.id == id_keep);
        }
        let _ = handle.kg.delete_drawer(id_orphan_a).await;
        let _ = handle.kg.delete_drawer(id_orphan_b).await;

        // Dedup threshold high enough that the surviving drawer's L3 hits
        // don't trigger an accidental merge against the orphan vectors.
        let dreamer = Dreamer::new(DreamConfig {
            dedup_threshold: 0.999,
            ..DreamConfig::default()
        });
        let stats = dreamer.dream_cycle(&handle).await.unwrap();

        assert_eq!(
            stats.compacted, 2,
            "expected exactly two orphan vectors removed; got stats={stats:?}"
        );
        let after_ids = handle.vector_store.all_ids().len();
        assert_eq!(
            after_ids, 1,
            "key_map should only track the surviving drawer (before={before_ids}, before_idx={before_idx})"
        );
        // The surviving drawer's id must still be present.
        assert!(
            handle.vector_store.all_ids().contains(&id_keep),
            "compaction must not remove the live drawer's vector"
        );
    }

    /// Why: The admin dashboard reads `dream_stats.json` to surface the last
    /// run's outcome and a "last ran X ago" timestamp; the dream cycle must
    /// snapshot itself to that file after every run so the file is current.
    /// What: Run a dream cycle on a palace, then load the persisted snapshot
    /// from disk and assert the timestamp is recent + stats match.
    /// Test: This test itself.
    #[tokio::test]
    async fn dream_stats_persisted_after_cycle() {
        let handle = open_test_handle("dream-persist").await;
        // One harmless drawer so the cycle has something to scan.
        handle
            .remember(
                "non-duplicate baseline drawer".into(),
                RoomType::General,
                vec![],
                0.5,
            )
            .await
            .unwrap();

        let dreamer = Dreamer::new(DreamConfig::default());
        let stats = dreamer.dream_cycle(&handle).await.unwrap();

        let data_dir = handle.data_dir.clone().expect("data_dir set");
        let loaded = PersistedDreamStats::load(&data_dir)
            .unwrap()
            .expect("dream_stats.json should exist after a cycle");

        assert_eq!(
            loaded.stats, stats,
            "persisted stats must match cycle output"
        );
        let age = chrono::Utc::now().signed_duration_since(loaded.last_run_at);
        assert!(
            age.num_seconds().abs() < 5,
            "last_run_at must be within a few seconds of now; got {age}"
        );
    }

    /// Why: After a dream cycle, the closet index should map keywords from
    /// drawer content back to that drawer's id so L2 can use it as a cheap
    /// pre-filter.
    /// What: Insert a drawer with a distinctive keyword, run the cycle, and
    /// assert the closets map contains that keyword pointing to the drawer.
    /// Test: This test itself.
    #[tokio::test]
    async fn closet_refresh_builds_index() {
        let handle = open_test_handle("dream-closets").await;
        let id = handle
            .remember(
                "Quokkas are the happiest marsupials in Australia".into(),
                RoomType::General,
                vec![],
                0.5,
            )
            .await
            .unwrap();

        let dreamer = Dreamer::new(DreamConfig::default());
        let stats = dreamer.dream_cycle(&handle).await.unwrap();
        assert!(
            stats.closets_updated > 0,
            "closet index should be non-empty"
        );

        let closets = handle.closets.read();
        let entry = closets.get("quokkas").expect("expected `quokkas` keyword");
        assert!(
            entry.contains(&id),
            "closet entry must reference the source drawer"
        );
    }

    /// Why: The operator dashboard depends on `is_compacting()` flipping to
    /// `true` while a dream cycle runs and back to `false` once it's done;
    /// otherwise the dreaming spinner would either never appear or never
    /// clear.
    /// What: Confirms the flag starts cleared, then runs a dream cycle and
    /// asserts the flag is cleared again after completion. (Catching the
    /// `true` window requires racy mid-cycle inspection; the drop-guard
    /// semantics are also covered by direct construction below.)
    /// Test: This test itself.
    #[tokio::test]
    async fn dream_cycle_toggles_is_compacting() {
        let handle = open_test_handle("dream-compacting-flag").await;
        assert!(!handle.is_compacting(), "flag must start cleared");

        // Direct guard exercise — the in-flight `true` window.
        {
            let _g = CompactionGuard::new(handle.is_compacting.clone());
            assert!(handle.is_compacting(), "guard must set the flag");
        }
        assert!(!handle.is_compacting(), "guard must clear on drop");

        // Full cycle still clears the flag on exit.
        let dreamer = Dreamer::new(DreamConfig::default());
        let _stats = dreamer.dream_cycle(&handle).await.unwrap();
        assert!(
            !handle.is_compacting(),
            "flag must be cleared after dream_cycle returns"
        );
    }

    /// Why: Drawers captured before the write-path blocklist landed (PR #221)
    /// still pollute existing palaces with `Tool use: Bash`-style noise. The
    /// dream cycle's content-prune pass must drop them retroactively so the
    /// palace self-heals on the next idle window.
    /// What: Insert a drawer whose content matches the blocklist prefix and a
    /// second sentence-length drawer that should survive, run a dream cycle,
    /// and assert only the noise drawer was content-pruned.
    /// Test: This test itself.
    #[tokio::test]
    async fn dream_content_prune_drops_blocklist_drawer() {
        let handle = open_test_handle("dream-content-blocklist").await;
        // `force=true` bypasses the write-path filter so we can plant a
        // pre-blocklist-era noise drawer that the dream pass must clean up.
        handle
            .remember_with_options(
                "Tool use: Bash".into(),
                RoomType::General,
                vec![],
                0.5,
                crate::memory_core::retrieval::RememberOptions::forced(),
            )
            .await
            .unwrap();
        let keep_id = handle
            .remember(
                "Refactor the dream loop to add a content-quality prune pass.".into(),
                RoomType::Backend,
                vec!["dream".into()],
                0.7,
            )
            .await
            .unwrap();
        assert_eq!(handle.drawers.read().len(), 2);

        let dreamer = Dreamer::new(DreamConfig::default());
        let stats = dreamer.dream_cycle(&handle).await.unwrap();

        assert_eq!(
            stats.content_pruned, 1,
            "expected exactly one blocklist-pruned drawer; got stats={stats:?}"
        );
        let surviving: Vec<Uuid> = handle.drawers.read().iter().map(|d| d.id).collect();
        assert_eq!(surviving, vec![keep_id], "noise drawer must be gone");
    }

    /// Why: Three-word one-liners (and shorter) carry no semantic value but
    /// burn L1 budget and recall slots; the content-prune pass must drop
    /// anything under `content_prune_min_words`.
    /// What: Insert one 2-word drawer and one comfortably long drawer, run
    /// the cycle, and assert only the short one was pruned.
    /// Test: This test itself.
    #[tokio::test]
    async fn dream_content_prune_drops_short_drawer() {
        let handle = open_test_handle("dream-content-short").await;
        // `force=true` bypasses the write-path token-count gate so we can
        // plant a too-short drawer for the dream pass to clean up.
        handle
            .remember_with_options(
                "hello world".into(),
                RoomType::General,
                vec![],
                0.5,
                crate::memory_core::retrieval::RememberOptions::forced(),
            )
            .await
            .unwrap();
        let keep_id = handle
            .remember(
                "This drawer has more than four words and should survive.".into(),
                RoomType::General,
                vec![],
                0.6,
            )
            .await
            .unwrap();
        assert_eq!(handle.drawers.read().len(), 2);

        let dreamer = Dreamer::new(DreamConfig::default());
        let stats = dreamer.dream_cycle(&handle).await.unwrap();

        assert_eq!(
            stats.content_pruned, 1,
            "expected exactly one short drawer pruned; got stats={stats:?}"
        );
        let surviving: Vec<Uuid> = handle.drawers.read().iter().map(|d| d.id).collect();
        assert_eq!(surviving, vec![keep_id], "short drawer must be gone");
    }

    /// Why: The prune pass must not be over-eager — normal multi-sentence
    /// drawers should survive untouched even when the cycle runs with default
    /// config. Without this regression test a future tightening of the
    /// blocklist or min-word floor could silently delete useful memories.
    /// What: Insert a single multi-sentence drawer, run the cycle, and assert
    /// `content_pruned == 0` and the drawer is still present.
    /// Test: This test itself.
    #[tokio::test]
    async fn dream_content_prune_keeps_good_drawer() {
        let handle = open_test_handle("dream-content-keep").await;
        let keep_id = handle
            .remember(
                "Dreaming runs a content-quality prune pass before dedup. \
                 It enforces the same rule the write path uses."
                    .into(),
                RoomType::Backend,
                vec!["dream".into()],
                0.7,
            )
            .await
            .unwrap();
        assert_eq!(handle.drawers.read().len(), 1);

        let dreamer = Dreamer::new(DreamConfig::default());
        let stats = dreamer.dream_cycle(&handle).await.unwrap();

        assert_eq!(
            stats.content_pruned, 0,
            "well-formed drawer must not be content-pruned; got stats={stats:?}"
        );
        let surviving: Vec<Uuid> = handle.drawers.read().iter().map(|d| d.id).collect();
        assert_eq!(surviving, vec![keep_id], "good drawer must survive");
    }

    // ─── Semantic consolidation integration tests ────────────────────────────

    /// Why: The dream cycle's semantic phase must add canonical drawers and
    /// preserve the originals when a MockInference returns a Merge action.
    /// What: Injects a MockInference that merges two drawers into one canonical
    /// summary; runs dream_cycle; asserts the canonical drawer is added and the
    /// originals are still present (additive-only).
    /// Test: This test itself.
    #[tokio::test]
    async fn dream_cycle_semantic_consolidation_with_mock() {
        use crate::memory_core::semantic_consolidation::{
            ConsolidationAction, MockInference, SemanticConsolidationConfig, SemanticConsolidator,
        };

        let handle = open_test_handle("dream-semantic-mock").await;

        // Plant two drawers with distinct content (so NLP dedup doesn't remove one).
        let id1 = handle
            .remember(
                "ts is the search tool used for code navigation".into(),
                RoomType::Backend,
                vec!["ts".into()],
                0.7,
            )
            .await
            .unwrap();
        let id2 = handle
            .remember(
                "trusty-search provides hybrid BM25 and vector retrieval".into(),
                RoomType::Backend,
                vec!["trusty-search".into()],
                0.6,
            )
            .await
            .unwrap();
        assert_eq!(handle.drawers.read().len(), 2);

        // Configure the mock to merge both into one canonical summary.
        let canonical_text = "trusty-search (alias: ts) provides hybrid BM25 + vector code search";
        let actions = vec![ConsolidationAction::Merge {
            canonical_content: canonical_text.to_string(),
            superseded_ids: vec![id1, id2],
        }];
        let mock = std::sync::Arc::new(MockInference::new(actions));
        let call_count = mock.call_count.clone();
        let cfg = SemanticConsolidationConfig {
            enabled: true,
            max_batch_size: 8,
            max_calls_per_cycle: 20,
            ..Default::default()
        };
        let consolidator = std::sync::Arc::new(SemanticConsolidator::new(mock, cfg));

        let dreamer = Dreamer::with_consolidator(
            DreamConfig {
                // High dedup threshold so NLP pass doesn't remove the drawers.
                dedup_threshold: 0.999,
                semantic: SemanticConsolidationConfig {
                    enabled: true,
                    ..Default::default()
                },
                ..DreamConfig::default()
            },
            consolidator,
        );

        let stats = dreamer.dream_cycle(&handle).await.unwrap();

        // One canonical drawer added.
        assert_eq!(
            stats.semantically_consolidated, 1,
            "expected one canonical drawer; got stats={stats:?}"
        );
        assert_eq!(
            call_count.load(std::sync::atomic::Ordering::Relaxed),
            1,
            "expected exactly one LLM call"
        );
        assert_eq!(stats.semantic_llm_calls, 1);

        // Original drawers still present (additive-only).
        let drawer_ids: Vec<Uuid> = handle.drawers.read().iter().map(|d| d.id).collect();
        assert!(
            drawer_ids.contains(&id1),
            "original drawer 1 must be preserved"
        );
        assert!(
            drawer_ids.contains(&id2),
            "original drawer 2 must be preserved"
        );

        // Canonical drawer was added.
        let has_canonical = handle
            .drawers
            .read()
            .iter()
            .any(|d| d.content == canonical_text);
        assert!(has_canonical, "canonical drawer must be present");
    }

    /// Why: When no inference backend is configured, the semantic phase must
    /// silently skip without error and the dream cycle must complete normally
    /// with the same behavior as pre-#87.
    /// What: Run dream_cycle with default config (no env var, local_model_enabled=false);
    /// assert semantically_consolidated == 0 and the cycle succeeds.
    /// Test: This test itself.
    #[tokio::test]
    async fn dream_cycle_semantic_consolidation_no_inference() {
        // Ensure no env key is set for this test.
        let _guard = EnvVarGuard::remove("OPENROUTER_API_KEY");

        let handle = open_test_handle("dream-semantic-no-inference").await;
        handle
            .remember(
                "some memory that should not be semantically consolidated".into(),
                RoomType::General,
                vec![],
                0.5,
            )
            .await
            .unwrap();

        let dreamer = Dreamer::new(DreamConfig {
            semantic: crate::memory_core::semantic_consolidation::SemanticConsolidationConfig {
                enabled: true,
                ..Default::default()
            },
            local_model_enabled: false,
            openrouter_api_key: String::new(),
            ..DreamConfig::default()
        });

        let stats = dreamer.dream_cycle(&handle).await.unwrap();

        assert_eq!(
            stats.semantically_consolidated, 0,
            "no inference available → semantic phase must be no-op"
        );
        assert_eq!(
            stats.semantic_llm_calls, 0,
            "no LLM calls when inference unavailable"
        );
        // Palace must be intact.
        assert_eq!(
            handle.drawers.read().len(),
            1,
            "drawer must survive untouched"
        );
    }

    /// Why: When `semantic.enabled = false`, the phase must be skipped even
    /// if an inference backend is configured, so operators can opt out cheaply.
    /// What: Supply a consolidator but set enabled=false; assert the consolidator's
    /// call_count stays at zero after a dream cycle.
    /// Test: This test itself.
    #[tokio::test]
    async fn dream_cycle_semantic_consolidation_disabled_by_config() {
        use crate::memory_core::semantic_consolidation::{
            MockInference, SemanticConsolidationConfig, SemanticConsolidator,
        };

        let handle = open_test_handle("dream-semantic-disabled").await;
        handle
            .remember(
                "this drawer should not be touched by semantic phase".into(),
                RoomType::General,
                vec![],
                0.5,
            )
            .await
            .unwrap();

        let mock = std::sync::Arc::new(MockInference::no_op());
        let call_count = mock.call_count.clone();
        let consolidator = std::sync::Arc::new(SemanticConsolidator::new(
            mock,
            SemanticConsolidationConfig::default(),
        ));

        let dreamer = Dreamer::with_consolidator(
            DreamConfig {
                semantic: SemanticConsolidationConfig {
                    enabled: false, // ← disabled
                    ..Default::default()
                },
                ..DreamConfig::default()
            },
            consolidator,
        );

        let stats = dreamer.dream_cycle(&handle).await.unwrap();

        assert_eq!(stats.semantically_consolidated, 0);
        assert_eq!(
            call_count.load(std::sync::atomic::Ordering::Relaxed),
            0,
            "mock must not be called when semantic phase is disabled"
        );
    }

    // ─── RAII env-var guard for tests ────────────────────────────────────────
    //
    // Safety: test-only; the tokio::test macro with default settings uses the
    // current-thread runtime so env-var mutation is single-threaded.

    struct EnvVarGuard {
        key: &'static str,
        previous: Option<String>,
    }

    impl EnvVarGuard {
        fn remove(key: &'static str) -> Self {
            let previous = std::env::var(key).ok();
            // Safety: test-only; single-threaded test execution.
            unsafe { std::env::remove_var(key) };
            Self { key, previous }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            // Safety: test-only; single-threaded test execution.
            match &self.previous {
                Some(v) => unsafe { std::env::set_var(self.key, v) },
                None => unsafe { std::env::remove_var(self.key) },
            }
        }
    }
}
