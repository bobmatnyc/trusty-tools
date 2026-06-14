//! Dream cycle passes: content-prune, dedup, prune, compact, closet refresh,
//! and semantic consolidation.
//!
//! Why: Extracted from dream.rs to keep each file under the 500-SLOC cap
//! (#607). Each pass is a focused async function called by `Dreamer::dream_cycle`.
//! What: `content_prune_pass`, `dedup_pass`, `prune_pass`, `compact_pass`,
//! `refresh_closets`, `semantic_consolidation_pass`.
//! Test: `dream_cycle_merges_duplicates`, `dream_cycle_prunes_low_importance`,
//! `closet_refresh_builds_index`, `dream_cycle_semantic_consolidation_with_mock`.

use super::config::DreamConfig;
use super::helpers::{
    build_closet_index, is_low_quality_content, merge_into, rebuild_index_from_drawers,
};
use crate::memory_core::decay::DecayConfig;
use crate::memory_core::palace::{Drawer, RoomType};
use crate::memory_core::retrieval::{PalaceHandle, shared_embedder};
use crate::memory_core::semantic_consolidation::{SemanticConsolidator, inference_available};
use crate::memory_core::store::vector::VectorStore;
use anyhow::Result;
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;
use uuid::Uuid;

/// Drop drawers whose content is recognisably noise.
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
pub(super) async fn content_prune_pass(
    handle: &Arc<PalaceHandle>,
    started: std::time::Instant,
    budget: Duration,
    min_words: usize,
) -> Result<usize> {
    let snapshot: Vec<Drawer> = handle.drawers.read().clone();
    let mut victims: Vec<Uuid> = Vec::new();

    for drawer in snapshot.iter() {
        if started.elapsed() >= budget {
            break;
        }
        if is_low_quality_content(&drawer.content, min_words) {
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

/// Remove orphaned vectors from the HNSW index whose drawer row no longer exists.
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
pub(super) async fn compact_pass(
    handle: &Arc<PalaceHandle>,
    started: std::time::Instant,
    budget: Duration,
) -> Result<usize> {
    let drawer_ids: HashSet<Uuid> = handle.drawers.read().iter().map(|d| d.id).collect();

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
            .map_err(|e| e.context("dream compact rebuild"))?;
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
pub(super) async fn dedup_pass(
    handle: &Arc<PalaceHandle>,
    started: std::time::Instant,
    budget: Duration,
    dedup_threshold: f32,
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
        .map_err(|e| e.context("acquire shared embedder for dream dedup"))?;

    let contents: Vec<String> = snapshot.iter().map(|d| d.content.clone()).collect();
    let vectors = embedder
        .embed_batch(&contents)
        .await
        .map_err(|e| e.context("batch embed drawers for dream dedup"))?;

    if vectors.len() != snapshot.len() {
        // Defensive: embedder must return one vector per input.
        anyhow::bail!(
            "embedder returned {} vectors for {} drawers",
            vectors.len(),
            snapshot.len()
        );
    }

    let mut merges: usize = 0;
    let mut already_removed: HashSet<Uuid> = HashSet::new();

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
            if hit.score < dedup_threshold {
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
pub(super) async fn prune_pass(
    handle: &Arc<PalaceHandle>,
    started: std::time::Instant,
    budget: Duration,
    prune_importance: f32,
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
        if eff <= prune_importance && age > MIN_AGE_DAYS {
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
pub(super) fn refresh_closets(handle: &Arc<PalaceHandle>) -> usize {
    let snapshot: Vec<Drawer> = handle.drawers.read().clone();
    let new_index = build_closet_index(&snapshot);
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
pub(super) async fn semantic_consolidation_pass(
    handle: &Arc<PalaceHandle>,
    config: &DreamConfig,
    injected: Option<Arc<SemanticConsolidator>>,
) -> (usize, usize, usize) {
    if !config.semantic.enabled {
        tracing::debug!(
            palace = %handle.id,
            "skipping semantic consolidation: disabled in config"
        );
        return (0, 0, 0);
    }

    // Use the injected consolidator (test path) or build one from config.
    let consolidator: Arc<SemanticConsolidator> = if let Some(c) = injected {
        c
    } else {
        // Production path: gate on inference availability.
        let api_key = if !config.openrouter_api_key.is_empty() {
            config.openrouter_api_key.clone()
        } else {
            std::env::var("OPENROUTER_API_KEY").unwrap_or_default()
        };

        if !inference_available(&api_key, config.local_model_enabled) {
            tracing::debug!(
                palace = %handle.id,
                "skipping semantic consolidation: inference unavailable \
                 (set OPENROUTER_API_KEY or enable local_model)"
            );
            return (0, 0, 0);
        }

        // Build the inference backend: prefer local model (free),
        // fall back to OpenRouter.
        use crate::memory_core::semantic_consolidation::{OllamaInference, OpenRouterInference};
        let backend: Arc<dyn crate::memory_core::semantic_consolidation::Inference> =
            if config.local_model_enabled && api_key.is_empty() {
                Arc::new(OllamaInference::new(
                    "http://localhost:11434",
                    &config.semantic.model,
                ))
            } else {
                Arc::new(OpenRouterInference::new(api_key, &config.semantic.model))
            };

        Arc::new(SemanticConsolidator::new(backend, config.semantic.clone()))
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
