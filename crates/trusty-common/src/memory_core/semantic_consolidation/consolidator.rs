//! `SemanticConsolidator` — batches drawers and calls the inference backend.
//!
//! Why: Isolates the consolidation orchestration logic from the types and
//! inference implementations so each file stays under the 500-SLOC cap.
//! What: `SemanticConsolidator::new` + `consolidate` with cache and budget
//! enforcement.
//! Test: `consolidator_merges_cluster`, `consolidator_caches_repeated_batches`,
//! `consolidator_respects_call_budget`.

use std::collections::HashMap;
use std::sync::Arc;

use crate::memory_core::palace::Drawer;

use super::inference::Inference;
use super::types::{
    ConsolidationResult, SemanticConsolidationConfig, apply_actions_to_result, batch_cache_key,
};
use super::types::ConsolidationAction;

/// Content-hash-keyed cache for consolidation results.
///
/// Why: the dream cycle may run frequently; re-consolidating the same batch
/// costs money and time. Keying by a hash of sorted drawer content lets us
/// skip batches we already processed.
/// What: `HashMap<String, Vec<ConsolidationAction>>` where the key is the
/// SHA-256 of the batch's sorted content strings (hex-encoded first 16 bytes
/// for compactness).
/// Test: cache hits verified via `MockInference::call_count`.
type ConsolidationCache = HashMap<String, Vec<ConsolidationAction>>;

/// Clusters semantically similar drawers and canonicalizes them via LLM.
///
/// Why: wraps the `Inference` trait + similarity threshold + call budget
/// behind one struct so `dream.rs` has a single callsite.
/// What: given a slice of drawers (already filtered to candidates via the
/// embedding-similarity threshold), batches them and calls `Inference::consolidate`;
/// applies the response cache to skip already-seen batches; returns a
/// `ConsolidationResult` for the caller to apply.
/// Test: `consolidator_merges_cluster`, `consolidator_caches_repeated_batches`,
/// `consolidator_respects_call_budget`.
pub struct SemanticConsolidator {
    inference: Arc<dyn Inference>,
    pub config: SemanticConsolidationConfig,
    cache: parking_lot::Mutex<ConsolidationCache>,
}

impl SemanticConsolidator {
    /// Create a new consolidator.
    ///
    /// Why: constructor that bundles the inference backend with the config.
    /// What: stores both; cache starts empty.
    /// Test: exercised by all `SemanticConsolidator` tests.
    pub fn new(inference: Arc<dyn Inference>, config: SemanticConsolidationConfig) -> Self {
        Self {
            inference,
            config,
            cache: parking_lot::Mutex::new(HashMap::new()),
        }
    }

    /// Run consolidation over a list of candidate drawers.
    ///
    /// Why: top-level entry point for the dream cycle; hides batching,
    /// caching, and budget enforcement.
    /// What: splits `drawers` into batches of `config.max_batch_size`, checks
    /// the response cache for each batch, calls `self.inference.consolidate` on
    /// cache misses, and stops once `config.max_calls_per_cycle` LLM calls have
    /// been made. Returns a `ConsolidationResult` accumulating all actions.
    /// Test: `consolidator_merges_cluster`, `consolidator_respects_call_budget`.
    pub async fn consolidate(&self, drawers: &[Drawer]) -> ConsolidationResult {
        let mut result = ConsolidationResult::default();
        let mut calls_made = 0usize;

        for batch in drawers.chunks(self.config.max_batch_size) {
            if calls_made >= self.config.max_calls_per_cycle {
                tracing::debug!(
                    budget = self.config.max_calls_per_cycle,
                    "semantic consolidation call budget exhausted"
                );
                break;
            }

            let cache_key = batch_cache_key(batch);

            // Check cache first.
            let cached = {
                let guard = self.cache.lock();
                guard.get(&cache_key).cloned()
            };

            let actions = if let Some(actions) = cached {
                result.cache_hits += 1;
                tracing::debug!(key = %cache_key, "semantic consolidation cache hit");
                actions
            } else {
                match self.inference.consolidate(batch).await {
                    Ok(actions) => {
                        calls_made += 1;
                        result.llm_calls += 1;
                        // Store in cache.
                        self.cache.lock().insert(cache_key, actions.clone());
                        actions
                    }
                    Err(e) => {
                        tracing::warn!(
                            error = %e,
                            "semantic consolidation LLM call failed; skipping batch"
                        );
                        calls_made += 1; // count as consumed to avoid infinite retry
                        vec![]
                    }
                }
            };

            apply_actions_to_result(actions, batch, &mut result);
        }

        result
    }
}
