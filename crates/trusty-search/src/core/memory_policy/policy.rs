//! `MemoryPolicy` struct and construction logic.
//!
//! Why: single source of truth for all resolved memory caps at daemon startup.
//! What: [`MemoryPolicy`] holds the final set of caps after tier selection,
//! proportional computation, and env-var overrides. [`MemoryPolicy::detect`]
//! drives the full detection pipeline; [`MemoryPolicy::from_total_ram_mb`]
//! accepts a caller-supplied RAM value for tests.
//! Test: see `super::tests` — `test_tier_selection`, `test_env_override`,
//! `test_batch_size_env_override_clamped_by_hard_cap`,
//! `test_batch_size_explicit_flag_bypasses_clamp`.

use super::compute::{
    compute_index_memory_limit_mb, compute_max_batch_size, compute_max_chunks,
    compute_memory_limit_mb, resolve_coreml_batch_size,
};
use super::constants::*;
use super::detect::detect_total_ram_mb;
use super::tier::MemoryTier;

/// Resolved memory caps for this daemon process. Constructed by
/// [`MemoryPolicy::detect`].
#[derive(Debug, Clone, Copy)]
pub struct MemoryPolicy {
    pub total_ram_mb: u64,
    pub tier: MemoryTier,
    pub memory_limit_mb: usize,
    /// Separate soft cap applied *only* while the indexing pipeline is running
    /// (see [`compute_index_memory_limit_mb`] and `core::memguard`).
    /// Always at least as large as `memory_limit_mb`. Lets the reindex
    /// orchestrator absorb CoreML unified-memory spikes without OOM-aborting
    /// under the global daemon ceiling.
    pub index_memory_limit_mb: usize,
    pub max_chunks: usize,
    pub embedding_cache: usize,
    pub max_batch_size: usize,
    /// Provider-specific batch-size cap applied **only** when the CoreML
    /// execution provider is active. See [`DEFAULT_COREML_BATCH_SIZE`] for
    /// motivation. The reindex pipeline reads this and uses it in place of
    /// `max_batch_size` whenever the live embedder reports `CoreML`.
    pub coreml_batch_size: usize,
    pub bm25_corpus_cap: usize,
    pub max_kg_nodes: usize,
}

impl MemoryPolicy {
    /// Detect total system RAM, pick a tier, apply env-var overrides, and
    /// return the resolved policy.
    ///
    /// Why: single source of truth for memory caps at daemon startup.
    /// What: runs platform RAM detection, selects a [`MemoryTier`], starts
    /// from the tier's defaults, then overrides any field whose corresponding
    /// `TRUSTY_*` env var is set to a parseable value. As a transitional
    /// measure (so existing scattered env-var readers Just Work) it also
    /// writes every resolved field back into the process environment.
    /// Test: see `test_tier_selection`, `test_env_override`, and
    /// `test_ram_detection_returns_nonzero`.
    pub fn detect() -> Self {
        let total_ram_mb = detect_total_ram_mb().unwrap_or_else(|| {
            tracing::warn!(
                "memory_policy: could not detect total system RAM — \
                 falling back to {FALLBACK_RAM_MB} MB (Medium tier defaults)"
            );
            FALLBACK_RAM_MB
        });
        Self::from_total_ram_mb(total_ram_mb)
    }

    /// Like [`Self::detect`] but with a caller-supplied RAM value. Useful for
    /// tests and for callers that have already measured RAM.
    pub fn from_total_ram_mb(total_ram_mb: u64) -> Self {
        let tier = MemoryTier::from_total_ram_mb(total_ram_mb);
        // Compute the proportional memory limit (25% of system RAM, clamped)
        // BEFORE selecting tier-keyed defaults so max_chunks / max_batch_size
        // scale with the actual host RAM rather than a fixed tier bucket.
        // See issue #120 (104 GB reindex on a 128 GB host).
        let proportional_limit_mb = compute_memory_limit_mb(total_ram_mb);
        let proportional_index_limit_mb = compute_index_memory_limit_mb(total_ram_mb);
        let d = tier.defaults(proportional_limit_mb, proportional_index_limit_mb);

        // Resolve memory limit first so the derived max_batch_size default
        // tracks any TRUSTY_MEMORY_LIMIT_MB override. TRUSTY_MAX_BATCH_SIZE
        // still wins when explicitly set. See issue #95.
        let memory_limit_mb = env_override_usize("TRUSTY_MEMORY_LIMIT_MB", d.memory_limit_mb);
        // Resolve the indexing-pipeline limit. When operator overrides the
        // global limit without overriding the indexing limit, we want the
        // indexing limit to remain >= the global limit (the global limit is
        // the steady-state floor; the indexing limit is the transient peak).
        // If neither is overridden, both come from the proportional defaults.
        let index_memory_limit_mb = {
            let raw = env_override_usize("TRUSTY_INDEX_MEMORY_LIMIT_MB", d.index_memory_limit_mb);
            // Guarantee `index_memory_limit_mb >= memory_limit_mb` so an
            // accidental low override doesn't leave the pipeline tighter than
            // the daemon floor. Operators who explicitly want a smaller index
            // limit than the global limit are blocked here — this is by
            // design; a smaller-than-global index limit is almost always a
            // misconfiguration.
            raw.max(memory_limit_mb)
        };
        // Batch size derivation now follows the *indexing* limit because the
        // ORT transient arena cost is incurred by the pipeline, not the idle
        // daemon. The TRUSTY_MAX_BATCH_SIZE env override still wins below.
        let derived_batch_size = if index_memory_limit_mb == d.index_memory_limit_mb {
            d.max_batch_size
        } else {
            compute_max_batch_size(index_memory_limit_mb)
        };
        // Recompute max_chunks if the operator overrode the memory limit so
        // chunk capacity tracks the actual configured limit (env var for
        // max_chunks still wins below via env_override_usize).
        let derived_max_chunks = if memory_limit_mb == d.memory_limit_mb {
            d.max_chunks
        } else {
            compute_max_chunks(memory_limit_mb)
        };

        // Apply tier-based hard cap on max_batch_size (issue #89). The cap
        // only constrains env-var overrides; auto-derived defaults are already
        // safely below it.
        //
        // Escape hatch: `TRUSTY_MAX_BATCH_SIZE_EXPLICIT=1` opts the operator
        // out of the tier hard cap, honoring `TRUSTY_MAX_BATCH_SIZE` verbatim.
        // Why: the GPU tuning path (commands::start::tune_batch_size_for_provider)
        // needs to set 512 to feed CUDA efficiently, and other power users may
        // have measured their own per-slot cost on specific workloads. This
        // mirrors the GPU path's existing semantics and makes the flag apply
        // universally (CPU + GPU), not just at GPU init. See the 94 GB reindex
        // incident report.
        let explicit = std::env::var("TRUSTY_MAX_BATCH_SIZE_EXPLICIT")
            .map(|v| v == "1")
            .unwrap_or(false);
        let env_set = std::env::var("TRUSTY_MAX_BATCH_SIZE").is_ok();
        let raw_batch_size = env_override_usize("TRUSTY_MAX_BATCH_SIZE", derived_batch_size);
        let batch_cap = tier.batch_size_hard_cap();
        let max_batch_size = if explicit && env_set {
            tracing::warn!(
                "memory_policy: TRUSTY_MAX_BATCH_SIZE_EXPLICIT=1 — honoring \
                 TRUSTY_MAX_BATCH_SIZE={} verbatim and bypassing tier {} hard cap of {}. \
                 Ensure you have measured the actual ORT transient-allocation cost per slot \
                 on your workload (defaults assume 32 MB/slot with arena disabled).",
                raw_batch_size,
                tier,
                batch_cap,
            );
            raw_batch_size
        } else if raw_batch_size > batch_cap {
            tracing::warn!(
                "memory_policy: TRUSTY_MAX_BATCH_SIZE={} exceeds tier {} hard cap of {}; \
                 clamping to protect against ORT transient-arena spike (issue #89). \
                 Set TRUSTY_MAX_BATCH_SIZE_EXPLICIT=1 to bypass this clamp.",
                raw_batch_size,
                tier,
                batch_cap,
            );
            batch_cap
        } else {
            raw_batch_size
        };

        let policy = MemoryPolicy {
            total_ram_mb,
            tier,
            memory_limit_mb,
            index_memory_limit_mb,
            max_chunks: env_override_usize("TRUSTY_MAX_CHUNKS", derived_max_chunks),
            embedding_cache: env_override_usize("TRUSTY_EMBEDDING_CACHE", d.embedding_cache),
            max_batch_size,
            coreml_batch_size: resolve_coreml_batch_size(),
            bm25_corpus_cap: env_override_usize("TRUSTY_BM25_CORPUS_CAP", d.bm25_corpus_cap),
            max_kg_nodes: env_override_usize("TRUSTY_MAX_KG_NODES", d.max_kg_nodes),
        };

        // Transitional: stamp resolved values back into the env so existing
        // module-level readers (in indexer.rs, bm25.rs, symbol_graph.rs,
        // memguard.rs, store.rs) pick up the auto-tuned defaults without
        // each having to learn about MemoryPolicy.
        policy.apply_to_env();
        policy
    }

    /// Write every field back into the process environment. Idempotent; safe
    /// to call before any worker thread has read its env-cached cap.
    ///
    /// SAFETY: This must run before the daemon spawns any threads that read
    /// these env vars (e.g. before tokio workers start indexing). Calling
    /// `std::env::set_var` from a multi-threaded context is unsound on some
    /// platforms (see the std docs); `MemoryPolicy::detect()` is intended to
    /// be invoked once in `main` before the runtime is built.
    pub fn apply_to_env(&self) {
        // SAFETY: see doc comment — caller must invoke before threading begins.
        unsafe {
            std::env::set_var("TRUSTY_MEMORY_LIMIT_MB", self.memory_limit_mb.to_string());
            std::env::set_var(
                "TRUSTY_INDEX_MEMORY_LIMIT_MB",
                self.index_memory_limit_mb.to_string(),
            );
            std::env::set_var("TRUSTY_MAX_CHUNKS", self.max_chunks.to_string());
            std::env::set_var("TRUSTY_EMBEDDING_CACHE", self.embedding_cache.to_string());
            std::env::set_var("TRUSTY_MAX_BATCH_SIZE", self.max_batch_size.to_string());
            std::env::set_var(
                "TRUSTY_COREML_BATCH_SIZE",
                self.coreml_batch_size.to_string(),
            );
            std::env::set_var("TRUSTY_BM25_CORPUS_CAP", self.bm25_corpus_cap.to_string());
            std::env::set_var("TRUSTY_MAX_KG_NODES", self.max_kg_nodes.to_string());
        }
    }

    /// Pretty-print the resolved policy in two compact log lines suitable for
    /// `tracing::info!` at daemon startup.
    pub fn log_summary(&self) {
        let gb = self.total_ram_mb / 1024;
        let proportional = compute_memory_limit_mb(self.total_ram_mb);
        let proportional_index = compute_index_memory_limit_mb(self.total_ram_mb);
        tracing::info!(
            "trusty-search: detected {} GB RAM → tier={} \
             (daemon memory_limit_mb={}, 25% of RAM clamped to [{}, {}]; \
              index memory_limit_mb={}, 75% of RAM clamped to [{}, {}])",
            gb,
            self.tier,
            proportional,
            MEMORY_LIMIT_FLOOR_MB,
            MEMORY_LIMIT_CEIL_MB,
            proportional_index,
            INDEX_MEMORY_LIMIT_FLOOR_MB,
            INDEX_MEMORY_LIMIT_CEIL_MB,
        );
        tracing::info!(
            "  MEMORY_LIMIT_MB={}  INDEX_MEMORY_LIMIT_MB={}  MAX_CHUNKS={}  \
             EMBEDDING_CACHE={}  MAX_BATCH_SIZE={}  COREML_BATCH_SIZE={}  \
             BM25_CORPUS_CAP={}  MAX_KG_NODES={}",
            self.memory_limit_mb,
            self.index_memory_limit_mb,
            self.max_chunks,
            self.embedding_cache,
            self.max_batch_size,
            self.coreml_batch_size,
            self.bm25_corpus_cap,
            self.max_kg_nodes,
        );
    }
}

/// Read a `TRUSTY_*` env var as `usize`; fall back to `default` when unset
/// or unparseable. A warning is logged on parse failure to surface typos.
pub(super) fn env_override_usize(name: &str, default: usize) -> usize {
    match std::env::var(name) {
        Ok(v) => match v.parse::<usize>() {
            Ok(n) => n,
            Err(_) => {
                tracing::warn!(
                    "memory_policy: {name}={v:?} is not a valid usize; \
                     using tier default ({default})"
                );
                default
            }
        },
        Err(_) => default,
    }
}

// Suppress unused-import lint when DEFAULT_COREML_TRIPWIRE_MB is referenced
// only in doc comments from this module.
#[allow(unused_imports)]
use super::compute::DEFAULT_COREML_TRIPWIRE_MB as _;
