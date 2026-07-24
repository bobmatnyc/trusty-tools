//! Retrieval layer functions: L0+L1, L2, L3, recall, recall_deep, cross-palace.
//!
//! Why: Extracted from retrieval/mod.rs to keep each file under the 500-SLOC
//! cap (#607). All pure query functions live here; mutation lives in handle.rs.
//! What: `retrieve_l0_l1`, `rescore_l1_by_similarity`, `retrieve_l2`,
//! `retrieve_l3`, `expand_query`, `recall`, `recall_deep`,
//! `recall_with_default_embedder`, `recall_deep_with_default_embedder`,
//! `recall_across_palaces`, `recall_across_palaces_with_default_embedder`,
//! `room_to_uuid`, `uuid_prefix_eq`, `dedup_extend`.
//! Test: `recall_ranks_by_similarity_over_importance`, `l0_l1_always_present`,
//! `l2_returns_relevant_drawer`, `l2_room_filter_excludes_other_rooms`,
//! `recall_across_palaces_merges_results`.

use super::embedder::shared_embedder;
use super::handle::PalaceHandle;
use super::types::{CrossPalaceResult, RecallResult};
use crate::memory_core::decay::DecayConfig;
use crate::memory_core::dream::extract_keywords;
use crate::memory_core::embed::Embedder;
use crate::memory_core::palace::{Drawer, DrawerType, RoomType};
use crate::memory_core::store::vector::VectorStore;
use anyhow::{Context, Result};
use std::collections::HashMap;
use std::sync::Arc;
use uuid::Uuid;

/// Scaling factor applied to L1 importance when no vector-similarity score
/// is available for a drawer (i.e. the HNSW search did not return it).
///
/// Why: L1 drawers that were not in the vector search results have unknown
/// similarity to the query.  Assigning them their raw importance
/// (e.g. 1.0) made them dominate the ranked output even when they were
/// completely off-topic (issue #633). Multiplying by this floor coefficient
/// reduces their effective score below typical in-topic L2 hits, turning
/// importance into a mild tiebreaker rather than the primary ranking signal.
/// What: `0.15` — chosen so a maximum-importance L1 drawer without a
/// similarity score (0.15) is outranked by a mediocre-similarity L2 hit
/// (e.g. importance=0.5 * similarity=0.4 = 0.20).
/// Test: `recall_ranks_by_similarity_over_importance` in the tests below.
pub(super) const L1_NO_SIMILARITY_PENALTY: f32 = 0.15;

/// Hash a `RoomType` to a deterministic `Uuid` so the room signal survives
/// through the in-memory drawer table without a real `Room` row.
///
/// Why: `Drawer.room_id` is a `Uuid`; until we wire a Room table, callers need
/// a stable mapping from `RoomType` to id so `list_drawers` can filter by room.
/// What: FNV-1a-like hash of the `Debug` repr, packed into 16 bytes.
/// Test: Indirectly via `cli_list_filters_by_room`.
pub fn room_to_uuid(room: &RoomType) -> Uuid {
    let label = format!("{room:?}");
    let mut bytes = [0u8; 16];
    // Fold each byte into the buffer with a simple xor-rot hash; collisions
    // here are fine — this only needs to be stable per-process.
    for (i, b) in label.bytes().enumerate() {
        bytes[i % 16] ^= b.wrapping_add(i as u8);
    }
    Uuid::from_bytes(bytes)
}

/// Compare two UUIDs by their first 8 bytes.
///
/// Why: The vector store keys vectors by the first 8 bytes of a UUID, so
/// search results carry a `Uuid` whose last 8 bytes are zero. Matching these
/// back to drawers must therefore compare prefixes only.
/// What: Returns true if `a` and `b` agree on bytes `0..8`.
/// Test: Implicitly exercised by `l2_returns_relevant_drawer`.
pub(super) fn uuid_prefix_eq(a: Uuid, b: Uuid) -> bool {
    a.as_bytes()[..8] == b.as_bytes()[..8]
}

/// Build the always-on L0 + L1 portion of a recall.
///
/// Why: Every retrieval flow includes L0+L1; centralizing the construction
/// keeps `recall` and `recall_deep` short and makes L0/L1 layering testable
/// in isolation.
/// What: Emits one `RecallResult { layer: 0, score: 1.0 }` for the identity
/// (only when non-empty), followed by one result per cached L1 drawer with
/// `score = drawer.importance` and `layer: 1`. The L0 result reuses the
/// identity text inside a synthetic `Drawer` so callers can render every
/// layer uniformly.
///
/// Note: the returned scores are importance-only.  Callers that have
/// vector-similarity data (i.e. `recall` / `recall_deep`) should call
/// `rescore_l1_by_similarity` afterward so the final merged list ranks by
/// relevance, not importance (issue #633).
/// Test: `l0_l1_always_present` asserts both layers appear.
pub fn retrieve_l0_l1(handle: &PalaceHandle) -> Vec<RecallResult> {
    let mut out: Vec<RecallResult> = Vec::with_capacity(1 + handle.l1_drawers.len());

    if !handle.identity.is_empty() {
        // Synthesize a Drawer for the identity so RecallResult stays uniform.
        let identity_drawer = Drawer {
            id: Uuid::nil(),
            room_id: Uuid::nil(),
            content: handle.identity.clone(),
            importance: 1.0,
            source_file: None,
            created_at: chrono::Utc::now(),
            tags: Vec::new(),
            last_accessed_at: None,
            access_count: 0,
            drawer_type: DrawerType::UserFact,
            expires_at: None,
            completed_at: None,
        };
        out.push(RecallResult {
            drawer: identity_drawer,
            score: 1.0,
            layer: 0,
        });
    }

    for d in &handle.l1_drawers {
        out.push(RecallResult {
            drawer: d.clone(),
            score: d.importance,
            layer: 1,
        });
    }
    out
}

/// Re-score L1 entries using vector-similarity data from the L2/L3 results.
///
/// Why: Issue #633 — L1 scores are raw importance values (up to 1.0), which
/// made high-importance-but-irrelevant bulk-imported drawers dominate every
/// recall result.  After L2/L3 runs, we have true cosine-similarity scores
/// for many drawers.  This function patches each L1 entry's score with the
/// corresponding L2/L3 score when available, or applies a small penalty
/// coefficient (`L1_NO_SIMILARITY_PENALTY`) when the HNSW search did not
/// return the drawer (indicating low query relevance).  The L0 identity row
/// is left untouched (`layer == 0`).
///
/// What: For every entry in `results` with `layer == 1`, looks up the
/// drawer's id in `similarity_scores` (a map from drawer id to the score
/// produced by the vector search).  If found, replaces the L1 score with
/// the similarity score.  If not found, sets the score to
/// `importance * L1_NO_SIMILARITY_PENALTY` — a mild floor that keeps
/// importance as a tiebreaker without letting it override on-topic hits.
///
/// Test: `recall_ranks_by_similarity_over_importance` inserts one
/// high-importance-but-irrelevant drawer and one low-importance-but-on-topic
/// drawer, then asserts the on-topic drawer ranks first after a query.
pub fn rescore_l1_by_similarity(
    results: &mut [RecallResult],
    similarity_scores: &HashMap<Uuid, f32>,
) {
    for r in results.iter_mut() {
        if r.layer == 1 {
            let id = r.drawer.id;
            r.score = match similarity_scores.get(&id) {
                // Similarity score from the vector search is authoritative.
                Some(&sim) => sim,
                // Drawer was not in the HNSW results — likely off-topic.
                // Apply penalty so importance alone can't dominate ranking.
                None => r.drawer.importance * L1_NO_SIMILARITY_PENALTY,
            };
        }
    }
}

/// L2 retrieval: metadata-filtered HNSW search.
///
/// Why: Most queries don't need a full deep search — a topic-scoped vector
/// search returns relevant drawers cheaply. Filtering by `RoomType` lets
/// callers narrow into a domain (e.g. only Backend rooms) when intent is
/// known.
/// What: Embeds the query, searches the vector store with `top_k * 3` to
/// leave room for filtering, maps each hit back to a drawer via UUID-prefix
/// match, applies the optional room filter by comparing `drawer.room_id`
/// against `room_to_uuid(&filter)` (issue #3274 — the same deterministic
/// hash `list_drawers` already uses, since there is no real Room table yet),
/// scores as `drawer.importance * hit.score`, and returns the top `top_k`
/// drawers tagged with `layer: 2`.
/// Test: `l2_returns_relevant_drawer` upserts a Rust-themed drawer and
/// asserts a Rust-themed query retrieves it at rank 0.
/// `l2_room_filter_excludes_other_rooms` (issue #3274) asserts a drawer from
/// a non-matching room is dropped rather than silently included.
pub async fn retrieve_l2(
    handle: &PalaceHandle,
    embedder: &dyn Embedder,
    query: &str,
    room_filter: Option<RoomType>,
    top_k: usize,
) -> Result<Vec<RecallResult>> {
    if top_k == 0 {
        return Ok(Vec::new());
    }
    let embeddings = embedder.embed_batch(&[query.to_string()]).await?;
    let Some(query_vec) = embeddings.into_iter().next() else {
        return Ok(Vec::new());
    };

    let overfetch = top_k.saturating_mul(3).max(top_k);
    let hits = handle.vector_store.search(&query_vec, overfetch).await?;

    let drawers = handle.drawers.read();
    let closets = handle.closets.read();
    let query_tokens: Vec<String> = extract_keywords(query);
    let mut results: Vec<RecallResult> = Vec::with_capacity(hits.len());

    // Issue #3274: this used to be a silent no-op (empty if-body) — a
    // caller-supplied `room_filter` was accepted but never applied, so
    // results silently included every room. There is no real Room table yet,
    // but `room_to_uuid` deterministically hashes a `RoomType` into the
    // `Uuid` stamped on `Drawer::room_id` at creation time (see
    // `PalaceHandle::upsert`), which is the exact mechanism `list_drawers`
    // already uses to filter by room — so the filter IS enforceable now.
    let target_room_id = room_filter.as_ref().map(room_to_uuid);

    for hit in hits {
        let Some(drawer) = drawers.iter().find(|d| uuid_prefix_eq(d.id, hit.drawer_id)) else {
            // Vector hit refers to a drawer we no longer have metadata for;
            // skip silently — this can happen during partial loads.
            continue;
        };

        if let Some(rid) = target_room_id
            && drawer.room_id != rid
        {
            continue;
        }

        let age_days = DecayConfig::age_days(drawer.created_at);
        let boost = drawer.accumulated_boost(&handle.decay_config);
        let eff_importance =
            handle
                .decay_config
                .effective_importance(drawer.importance, age_days, boost);
        let effective_score = eff_importance * hit.score;

        // Closet tag boost: if any query token matches a closet keyword that
        // contains this drawer, add a 0.15 bump (capped at 1.0) so topical
        // hits outrank generic semantic neighbors.
        let drawer_id = drawer.id;
        let in_closet = query_tokens
            .iter()
            .any(|tok| closets.get(tok).is_some_and(|ids| ids.contains(&drawer_id)));
        let tag_boost = if in_closet { 0.15_f32 } else { 0.0 };
        let final_score = (effective_score + tag_boost).min(1.0);

        results.push(RecallResult {
            drawer: drawer.clone(),
            score: final_score,
            layer: 2,
        });
    }
    drop(closets);
    drop(drawers);

    results.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    results.truncate(top_k);
    Ok(results)
}

/// L3 retrieval: full HNSW deep search across the palace.
///
/// Why: For deep / exploratory queries the agent wants the broadest possible
/// recall; L3 skips the overfetch+filter dance and just returns the top-k
/// nearest neighbors with `layer: 3`.
/// What: Embeds the query, searches with exactly `top_k`, joins each hit to
/// its drawer via UUID-prefix match, scores as `importance * hit.score`,
/// sorts descending, and returns at most `top_k` `RecallResult`s.
/// Test: Symmetric with `l2_returns_relevant_drawer`; same join logic.
pub async fn retrieve_l3(
    handle: &PalaceHandle,
    embedder: &dyn Embedder,
    query: &str,
    top_k: usize,
) -> Result<Vec<RecallResult>> {
    if top_k == 0 {
        return Ok(Vec::new());
    }
    let embeddings = embedder.embed_batch(&[query.to_string()]).await?;
    let Some(query_vec) = embeddings.into_iter().next() else {
        return Ok(Vec::new());
    };

    let hits = handle.vector_store.search(&query_vec, top_k).await?;

    let drawers = handle.drawers.read();
    let closets = handle.closets.read();
    let query_tokens: Vec<String> = extract_keywords(query);
    let mut results: Vec<RecallResult> = Vec::with_capacity(hits.len());
    for hit in hits {
        let Some(drawer) = drawers.iter().find(|d| uuid_prefix_eq(d.id, hit.drawer_id)) else {
            continue;
        };
        let age_days = DecayConfig::age_days(drawer.created_at);
        let boost = drawer.accumulated_boost(&handle.decay_config);
        let eff_importance =
            handle
                .decay_config
                .effective_importance(drawer.importance, age_days, boost);
        let effective_score = eff_importance * hit.score;

        let drawer_id = drawer.id;
        let in_closet = query_tokens
            .iter()
            .any(|tok| closets.get(tok).is_some_and(|ids| ids.contains(&drawer_id)));
        let tag_boost = if in_closet { 0.15_f32 } else { 0.0 };
        let final_score = (effective_score + tag_boost).min(1.0);

        results.push(RecallResult {
            drawer: drawer.clone(),
            score: final_score,
            layer: 3,
        });
    }
    drop(closets);
    drop(drawers);

    results.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    results.truncate(top_k);
    Ok(results)
}

/// Expand a user query with domain synonyms before embedding.
///
/// Why: There's a vocabulary gap between casual user queries ("how fast is X?")
/// and technical memory content ("HNSW provides O(log N) latency"). Appending
/// related terms steers the embedded query vector toward both the original
/// intent and the technical phrasing — boosting recall on speed/performance,
/// vector-search, memory-safety, and concurrency questions.
/// What: Lowercase-scans the query for trigger phrases and appends a list of
/// related domain terms. No-op when no triggers match.
/// Test: `expand_query_adds_synonyms`, `expand_query_noop_for_unmatched`.
pub fn expand_query(query: &str) -> String {
    let q = query.to_lowercase();
    let mut extra: Vec<&str> = Vec::new();

    if q.contains("fast")
        || q.contains("speed")
        || q.contains("latency")
        || q.contains("performance")
    {
        extra.push("latency performance speed throughput");
    }
    if q.contains("vector search")
        || q.contains("semantic search")
        || q.contains("nearest neighbor")
    {
        extra.push("HNSW ANN approximate nearest neighbor usearch vector index");
    }
    if q.contains("memory safe") || q.contains("borrow") || q.contains("ownership") {
        extra.push("borrow checker lifetime ownership Rust memory safety");
    }
    if q.contains("concurren") || q.contains("thread") || q.contains("parallel") {
        extra.push("concurrent async tokio DashMap RwLock mutex thread-safe");
    }

    if extra.is_empty() {
        query.to_string()
    } else {
        format!("{} {}", query, extra.join(" "))
    }
}

/// Standard recall = L0 + L1 + L2, deduplicated and ranked by similarity.
///
/// Why: This is the default path for "hey memory, what do you know about X?"
/// — always-on identity + essentials, plus the cheapest topic search.
/// What: Runs `retrieve_l2` to obtain vector-similarity scores, builds a
/// score map from those results, applies `rescore_l1_by_similarity` to patch
/// L1 entries so importance alone can't dominate relevance-first ranking
/// (issue #633), deduplicates by drawer id, sorts the merged list by score
/// descending, and finally **truncates to `top_k`** (issue #877) so the
/// caller always receives at most `top_k` results regardless of how many
/// L0/L1 entries the palace has.  Applies `expand_query` before embedding
/// to bridge the user-vocabulary / technical-vocabulary gap.
/// Test: `recall_ranks_by_similarity_over_importance` verifies that a
/// low-importance but on-topic drawer outranks a high-importance but
/// off-topic drawer after this function returns.
/// `recall_top_k_caps_result_count` (issue #877) verifies the length cap.
pub async fn recall(
    handle: &PalaceHandle,
    embedder: &dyn Embedder,
    query: &str,
    top_k: usize,
) -> Result<Vec<RecallResult>> {
    // Idle-to-disk: a recall is a genuine user access — reset the idle clock
    // so the idle-evict ticker does not drop an actively-queried palace.
    handle.touch();
    let expanded = expand_query(query);
    let mut combined = retrieve_l0_l1(handle);
    let l2 = retrieve_l2(handle, embedder, &expanded, None, top_k).await?;

    // Build similarity-score map from L2 results (drawer_id -> score) before
    // consuming the vec. This lets us re-score L1 entries that happen to be
    // in the vector search results with their true cosine-similarity score.
    let sim_scores: HashMap<Uuid, f32> = l2.iter().map(|r| (r.drawer.id, r.score)).collect();

    // Patch L1 entries: replace importance-only scores with similarity scores
    // where available; apply the penalty coefficient elsewhere (issue #633).
    rescore_l1_by_similarity(&mut combined, &sim_scores);

    dedup_extend(&mut combined, l2);

    // Re-rank the full merged list by score descending so relevance (not
    // layer number or raw importance) determines which results surface first.
    combined.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    // Issue #877: enforce the top_k contract. L0+L1 can add up to
    // L1_CAP+1 entries before any L2 hits are merged; without this truncation
    // the caller receives more than top_k results whenever the palace has a
    // non-empty identity string plus several high-importance drawers.
    combined.truncate(top_k);

    handle.log_recall(query, &combined);
    Ok(combined)
}

/// Deep recall = L0 + L1 + L3, deduplicated and ranked by similarity.
///
/// Why: When the user explicitly asks for deep search, fall through to L3
/// instead of the metadata-filtered L2.
/// What: Same as `recall` but uses `retrieve_l3` for the heavy layer.
/// L1 entries are still re-scored via `rescore_l1_by_similarity` so the
/// final ranking is similarity-first (issue #633). The merged list is
/// **truncated to `top_k`** (issue #877) before returning so the caller
/// always receives at most `top_k` results.
/// Test: Symmetric with `recall`; covered indirectly.
pub async fn recall_deep(
    handle: &PalaceHandle,
    embedder: &dyn Embedder,
    query: &str,
    top_k: usize,
) -> Result<Vec<RecallResult>> {
    // Idle-to-disk: deep recall is also a genuine user access.
    handle.touch();
    let expanded = expand_query(query);
    let mut combined = retrieve_l0_l1(handle);
    let l3 = retrieve_l3(handle, embedder, &expanded, top_k).await?;

    // Build similarity-score map from L3 results, then re-score L1 entries
    // so high-importance-but-irrelevant drawers don't dominate (issue #633).
    let sim_scores: HashMap<Uuid, f32> = l3.iter().map(|r| (r.drawer.id, r.score)).collect();
    rescore_l1_by_similarity(&mut combined, &sim_scores);

    dedup_extend(&mut combined, l3);

    // Re-rank full list by score descending (relevance-first).
    combined.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    // Issue #877: enforce the top_k contract (same as `recall`).
    combined.truncate(top_k);

    handle.log_recall(query, &combined);
    Ok(combined)
}

/// Recall via the L0+L1+L2 path with the per-call `FastEmbedder`.
///
/// Why: CLI/MCP often want a one-shot "recall" without managing an embedder
/// handle; this convenience binds the embedder lifecycle to the call.
/// What: Initializes a `FastEmbedder` (which warms on first run), then
/// delegates to `recall`.
/// Test: `cli_remember_and_recall` integration test.
pub async fn recall_with_default_embedder(
    handle: &PalaceHandle,
    query: &str,
    top_k: usize,
) -> Result<Vec<RecallResult>> {
    let embedder = shared_embedder()
        .await
        .context("acquire shared embedder for recall")?;
    recall(handle, embedder.as_ref(), query, top_k).await
}

/// Deep recall with the shared `FastEmbedder` (issue #57).
pub async fn recall_deep_with_default_embedder(
    handle: &PalaceHandle,
    query: &str,
    top_k: usize,
) -> Result<Vec<RecallResult>> {
    let embedder = shared_embedder()
        .await
        .context("acquire shared embedder for recall_deep")?;
    recall_deep(handle, embedder.as_ref(), query, top_k).await
}

/// Fan out a recall across every palace handle and merge the results.
///
/// Why: Agents often want the most relevant memories regardless of which palace
/// they are stored in. This function fans out a single query across every open
/// palace handle, merges the results, deduplicates by drawer id, and re-ranks
/// by score descending.
/// What: For each palace handle in `handles`, runs `recall` (L0+L1+L2) or
/// `recall_deep` (L0+L1+L3) depending on `deep`, concurrently via
/// `futures::future::join_all`. Errors from individual palaces are logged via
/// `tracing::warn!` and skipped (not fatal). The merged list is deduplicated
/// by `result.drawer.id` (highest score wins on collision), sorted by
/// `result.score` descending, then truncated to `top_k`.
/// Test: `recall_across_palaces_merges_results` verifies results from two
/// palaces appear in the combined output.
pub async fn recall_across_palaces(
    handles: &[Arc<PalaceHandle>],
    embedder: &Arc<dyn Embedder + Send + Sync>,
    query: &str,
    top_k: usize,
    deep: bool,
) -> Result<Vec<CrossPalaceResult>> {
    if handles.is_empty() || top_k == 0 {
        return Ok(Vec::new());
    }

    // Fan out concurrently. Each future returns (palace_id, Result<Vec<...>>);
    // we keep the palace id alongside the result so failures can be logged
    // with the right context.
    let mut futures = Vec::with_capacity(handles.len());
    for handle in handles {
        let palace_id = handle.id.as_str().to_string();
        let handle = handle.clone();
        let embedder = embedder.clone();
        let query = query.to_string();
        futures.push(async move {
            let result = if deep {
                recall_deep(&handle, embedder.as_ref(), &query, top_k).await
            } else {
                recall(&handle, embedder.as_ref(), &query, top_k).await
            };
            (palace_id, result)
        });
    }

    let outcomes = futures::future::join_all(futures).await;

    // Deduplicate by drawer id — keep the highest-scoring occurrence. We index
    // into `merged` via a parallel `HashMap<Uuid, usize>` so we can mutate the
    // chosen entry in place when a higher-scoring duplicate arrives.
    let mut merged: Vec<CrossPalaceResult> = Vec::new();
    let mut by_drawer: HashMap<Uuid, usize> = HashMap::new();

    for (palace_id, outcome) in outcomes {
        match outcome {
            Ok(hits) => {
                for r in hits {
                    let drawer_id = r.drawer.id;
                    let candidate = CrossPalaceResult {
                        palace_id: palace_id.clone(),
                        result: r,
                    };
                    match by_drawer.get(&drawer_id).copied() {
                        Some(idx) if merged[idx].result.score >= candidate.result.score => {
                            // Existing entry wins; drop the candidate.
                        }
                        Some(idx) => {
                            merged[idx] = candidate;
                        }
                        None => {
                            by_drawer.insert(drawer_id, merged.len());
                            merged.push(candidate);
                        }
                    }
                }
            }
            Err(e) => {
                tracing::warn!(palace = %palace_id, "recall_across_palaces: skipping palace: {e:#}");
            }
        }
    }

    merged.sort_by(|a, b| {
        b.result
            .score
            .partial_cmp(&a.result.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    merged.truncate(top_k);
    Ok(merged)
}

/// Convenience wrapper for `recall_across_palaces` using the process-wide
/// shared `FastEmbedder`.
///
/// Why: CLI / MCP / HTTP entry points should not have to thread an embedder
/// through every call; the shared singleton (issue #57) is the right default
/// for cross-palace fan-out too.
/// What: Resolves `shared_embedder()`, erases it to `Arc<dyn Embedder + Send +
/// Sync>`, and delegates to `recall_across_palaces`.
/// Test: Indirectly exercised via the MCP / HTTP / CLI integration paths;
/// `recall_across_palaces_merges_results` covers the core merge logic.
pub async fn recall_across_palaces_with_default_embedder(
    handles: &[Arc<PalaceHandle>],
    query: &str,
    top_k: usize,
    deep: bool,
) -> Result<Vec<CrossPalaceResult>> {
    let embedder = shared_embedder()
        .await
        .context("acquire shared embedder for recall_across_palaces")?;
    recall_across_palaces(handles, &embedder, query, top_k, deep).await
}

/// Extend `base` with entries from `extra` whose drawer id isn't already in
/// `base`. L0/L1 priority is implied by call ordering: pass L0/L1 first.
pub(super) fn dedup_extend(base: &mut Vec<RecallResult>, extra: Vec<RecallResult>) {
    for r in extra {
        if !base.iter().any(|b| b.drawer.id == r.drawer.id) {
            base.push(r);
        }
    }
}
