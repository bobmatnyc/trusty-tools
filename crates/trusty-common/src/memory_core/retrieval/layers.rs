//! Retrieval layer functions: L0+L1, L2, L3, recall, recall_deep, cross-palace.
//!
//! Why: Extracted from retrieval/mod.rs to keep each file under the 500-SLOC
//! cap (#607). All pure query functions live here; mutation lives in handle.rs.
//! What: `retrieve_l0_l1`, `rescore_l1_by_similarity`, `rank_score`,
//! `retrieve_l2`, `retrieve_l3`, `expand_query`, `recall`, `recall_deep`,
//! `recall_with_default_embedder`, `recall_deep_with_default_embedder`,
//! `recall_across_palaces`, `recall_across_palaces_with_default_embedder`,
//! `uuid_prefix_eq`, `dedup_extend`.
//! Test: `recall_ranks_by_similarity_over_importance`, `l0_l1_always_present`,
//! `l2_returns_relevant_drawer`, `l2_room_filter_excludes_other_rooms`,
//! `recall_across_palaces_merges_results`,
//! `l2_ranks_similar_low_importance_above_less_similar_high_importance`.
//!
//! # Read-time expiry (#4885, ADR-0028 D4)
//!
//! Every layer here drops drawers whose `expires_at` has passed, judged by the
//! one shared predicate `Drawer::is_expired_at`. Before this, expiry was
//! consulted only when a palace was opened — and the daemon opens once and
//! holds the handle for the process lifetime, so a drawer that expired
//! mid-session kept being served until a restart.
//!
//! These paths FILTER; they do not delete. Three reasons, in order of weight:
//!
//! 1. Deleting needs the write lock on `handle.drawers` plus async I/O to the
//!    KG and the vector index. Retrieval holds a read guard over the same
//!    `RwLock` while it scans, so a delete here means either upgrading a lock
//!    mid-scan or doing I/O under it — the second stalls every writer on the
//!    palace behind one recall.
//! 2. A read must not fail because a cleanup failed. Filtering cannot fail
//!    open: an expired drawer is unreachable by recall whether or not its row
//!    is ever reclaimed. That is the fail-closed property ADR-0028 D4 is
//!    after, and a deleting read path would trade it for a write that can
//!    error.
//! 3. Reclamation already has an owner — `PalaceHandle::purge_expired` and the
//!    open-time sweep. Correctness and reclamation stay separable; a
//!    reclamation bug can waste space but can no longer serve a false fact.
//!
//! Cost of the choice: an expired drawer keeps its row and its vector slot
//! until a sweep runs. That is storage, not correctness.

use super::embedder::shared_embedder;
use super::handle::PalaceHandle;
use super::scope::{RecallScope, scope_admits};
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
/// similarity score (0.15) is outranked by a mediocre-similarity L2 hit. #4904
/// widened that margin rather than narrowing it: an L2 hit now scores close to
/// its raw similarity (see [`rank_score`]) instead of similarity times a
/// ~0.46-median `eff_importance`, so the same 0.15 ceiling sits further below a
/// real hit than it did.
/// Test: `recall_ranks_by_similarity_over_importance` in the tests below.
pub(super) const L1_NO_SIMILARITY_PENALTY: f32 = 0.15;

/// Share of an L2/L3 candidate's score that effective importance may move.
///
/// Why (#4904): L2 and L3 used to score a candidate `eff_importance *
/// similarity`. Measured over the trusty-tools palace (1,272 drawers) and 200
/// real hook queries, the two factors are not comparable in scale: inside one
/// query's 15-candidate pool, similarity spans 0.067 on average while
/// `eff_importance` spans 0.436 — 6.5× more. Multiplying by it therefore does
/// not weight the ranking, it *replaces* it: the final top-5 matched the
/// similarity top-5 in 1 of those 200 queries. On a 400-drawer self-retrieval
/// probe (each drawer queried with its own leading text, so the right answer is
/// known) the product scored recall@1 of 102/400 where similarity alone scored
/// 291/400 — the multiplier was discarding two thirds of the correct top hits.
/// What: `0.05` — a candidate's score is similarity scaled by
/// `0.95 + 0.05 * eff_importance`, so importance can move a score by at most 5%
/// and acts as the tiebreaker its own docs always described, not the primary
/// signal. The same sweep read 350/400 recall@5 at this weight, matching
/// similarity-only (350/400) while keeping importance live; the shipped
/// multiplier scored 295/400.
/// Test: `rank_score_keeps_importance_a_tiebreaker`,
/// `l2_ranks_similar_low_importance_above_less_similar_high_importance`.
pub(super) const IMPORTANCE_TILT: f32 = 0.05;

/// Combine the three L2/L3 ranking inputs into one comparable score.
///
/// Why: L2 and L3 scored candidates with the same three-term expression written
/// out twice, so a change to one silently left the other on the old formula —
/// exactly the drift that made `retrieve_l3` the odd one out for room filtering
/// in #3274. One function is also the only place a reader has to look to see
/// what "score" means.
/// What: tilts `similarity` by at most [`IMPORTANCE_TILT`] according to
/// `eff_importance`, adds the closet `tag_boost`, and clamps to `1.0`.
/// Test: `rank_score_keeps_importance_a_tiebreaker`.
pub(super) fn rank_score(similarity: f32, eff_importance: f32, tag_boost: f32) -> f32 {
    let tilted = similarity * ((1.0 - IMPORTANCE_TILT) + IMPORTANCE_TILT * eff_importance);
    (tilted + tag_boost).min(1.0)
}

/// Tracing target for per-candidate L2 ranking traces (#4904).
///
/// Why: a missed fact looks identical whether it never entered the candidate
/// set, entered but ranked below the cutoff, or was never queried for. Only the
/// pre-truncation candidate list with its score components tells them apart, and
/// nothing logged it. Its own target keeps `RUST_LOG=trusty_common=debug` from
/// drowning in one line per candidate per recall while still letting an operator
/// ask for exactly this with `RUST_LOG=memory_recall_rank=debug`.
/// What: the `target:` string on the `tracing::debug!` inside
/// [`retrieve_l2_scoped`], emitted once per surviving candidate before the
/// `top_k` truncation.
/// Test: `l2_rank_trace_emits_one_event_per_candidate`.
pub const RANK_TRACE_TARGET: &str = "memory_recall_rank";

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
///
/// #4885: L1 entries past their `expires_at` are dropped (see the module
/// header). This layer is where read-time expiry matters most: `l1_drawers` is
/// filled from the L1 cache snapshot at open and the open-time sweep only
/// retains over the full drawer table, so an expired drawer in that snapshot
/// survived even a reopen. The L0 identity row is synthetic and never expires.
/// Test: `l0_l1_always_present` asserts both layers appear;
/// `expired_l1_drawer_is_excluded_without_reopen` covers the filter.
pub fn retrieve_l0_l1(handle: &PalaceHandle) -> Vec<RecallResult> {
    let mut out: Vec<RecallResult> = Vec::with_capacity(1 + handle.l1_drawers.len());
    let now = chrono::Utc::now();

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
            fact_key: None,
        };
        out.push(RecallResult {
            drawer: identity_drawer,
            score: 1.0,
            layer: 0,
        });
    }

    for d in &handle.l1_drawers {
        // #4885: an expired drawer is a false fact, not a low-ranked one —
        // skip it rather than let `rescore_l1_by_similarity` demote it, which
        // would still leave it eligible for the injection budget.
        if d.is_expired_at(now) {
            continue;
        }
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
                // #5037: demoted is not excluded — this branch still yields a
                // score that competes for a `top_k` slot. See
                // `super::relevance::DEFAULT_RELEVANCE_FLOOR`.
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
/// against the id the palace's `ROOMS` registry holds for that room
/// (issue #3274 for the filter itself; ADR-0027 D1.3 for resolving the id
/// through the table instead of re-hashing it), scores each candidate with
/// [`rank_score`], and returns the top `top_k` drawers tagged with `layer: 2`.
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
    // ADR-0027 T9: the room filter is one case of the general scope. Keeping
    // this signature means no pre-wing call site changes — the "a caller who
    // never mentions a wing behaves identically" guarantee is structural here,
    // not a promise, because `RecallScope::All` / `Room` reach exactly the code
    // that ran before.
    retrieve_l2_scoped(
        handle,
        embedder,
        query,
        &RecallScope::from_room_filter(room_filter),
        top_k,
    )
    .await
}

/// L2 retrieval under an explicit [`RecallScope`] (ADR-0027 T9).
///
/// Why: a wing cannot be expressed as an `Option<RoomType>`, and duplicating
/// the L2 body to add one would leave two scoring paths to drift apart. This is
/// the single implementation; [`retrieve_l2`] is a thin wrapper over it.
/// What: as `retrieve_l2`, but the drawer filter compares `room_id` against the
/// scope's resolved room-id set. `RecallScope::All` resolves to no filter at
/// all, which is distinct from a scope that resolves to an empty set.
/// Test: `wing_scope_returns_only_that_wings_drawers`,
/// `l2_room_filter_excludes_other_rooms` (unchanged, via the wrapper).
pub async fn retrieve_l2_scoped(
    handle: &PalaceHandle,
    embedder: &dyn Embedder,
    query: &str,
    scope: &RecallScope,
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

    // Issue #3274: this used to be a silent no-op (empty if-body) — a
    // caller-supplied `room_filter` was accepted but never applied, so
    // results silently included every room.
    // ADR-0027 D1.3: resolve the id through the palace's ROOMS registry rather
    // than re-hashing the label. Re-hashing would silently return nothing for
    // every room minted after ADR-0027 (their ids are UUIDv5, which no fold can
    // reproduce); `resolve_room_filter_id` falls back to the legacy fold when
    // the room has no row, so an un-backfilled palace filters exactly as before.
    // ADR-0027 T9: a wing scope resolves fail-CLOSED (see `scope`), because a
    // scope boundary that fails open is a leak, not a wider result set.
    // Resolved BEFORE the guards below are taken: it is a redb read
    // transaction, and holding the drawer + closet locks across I/O would stall
    // every writer on this palace for its duration.
    let allowed = scope.allowed_room_ids(&handle.kg);

    let drawers = handle.drawers.read();
    let closets = handle.closets.read();
    let query_tokens: Vec<String> = extract_keywords(query);
    let now = chrono::Utc::now();
    let mut results: Vec<RecallResult> = Vec::with_capacity(hits.len());

    for hit in hits {
        let Some(drawer) = drawers.iter().find(|d| uuid_prefix_eq(d.id, hit.drawer_id)) else {
            // Vector hit refers to a drawer we no longer have metadata for;
            // skip silently — this can happen during partial loads.
            continue;
        };

        if !scope_admits(&allowed, drawer.room_id) {
            continue;
        }

        // #4885: the vector index still holds an expired drawer's embedding
        // until a sweep compacts it, so a semantic hit can land on a drawer
        // whose TTL passed after this handle was opened. Drop it here.
        if drawer.is_expired_at(now) {
            continue;
        }

        let age_days = DecayConfig::age_days(drawer.created_at);
        let boost = drawer.accumulated_boost(&handle.decay_config);
        let eff_importance =
            handle
                .decay_config
                .effective_importance(drawer.importance, age_days, boost);

        // Closet tag boost: if any query token matches a closet keyword that
        // contains this drawer, add a 0.15 bump (capped at 1.0) so topical
        // hits outrank generic semantic neighbors.
        let drawer_id = drawer.id;
        let in_closet = query_tokens
            .iter()
            .any(|tok| closets.get(tok).is_some_and(|ids| ids.contains(&drawer_id)));
        let tag_boost = if in_closet { 0.15_f32 } else { 0.0 };
        // #4904: importance tilts the similarity score, it no longer multiplies
        // it — see `IMPORTANCE_TILT`.
        let final_score = rank_score(hit.score, eff_importance, tag_boost);

        // #4904: the three candidate explanations for a missed fact — absent
        // from the candidate set, present but ranked below the cutoff, or never
        // queried for — are indistinguishable from the outside, because the only
        // observable is the truncated top-k. Emitting every candidate WITH its
        // score components before `truncate` is what separates them.
        tracing::debug!(
            target: RANK_TRACE_TARGET,
            drawer_id = %drawer.id,
            similarity = hit.score,
            importance = drawer.importance,
            eff_importance,
            tag_boost,
            score = final_score,
            "l2 candidate"
        );

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
/// ADR-0027 T7: L3 takes the same optional `room_filter` L2 has taken since
/// #3274. Without it, `memory_recall_deep` was the one recall path a room
/// scope could not reach — a caller narrowing to `Planning` silently got every
/// room back, which is the invisible-failure class ADR-0027 D4.4 rejects.
/// What: Embeds the query, searches with `top_k` (over-fetched to `top_k * 3`
/// when a room filter is active, since filtered-out hits would otherwise eat
/// the budget), joins each hit to its drawer via UUID-prefix match, drops
/// drawers outside the requested room, scores each candidate with
/// [`rank_score`], sorts descending, and returns at most `top_k`
/// `RecallResult`s.
/// Test: Symmetric with `l2_returns_relevant_drawer`; same join logic.
/// `l3_room_filter_excludes_other_rooms` covers the filter.
pub async fn retrieve_l3(
    handle: &PalaceHandle,
    embedder: &dyn Embedder,
    query: &str,
    room_filter: Option<RoomType>,
    top_k: usize,
) -> Result<Vec<RecallResult>> {
    retrieve_l3_scoped(
        handle,
        embedder,
        query,
        &RecallScope::from_room_filter(room_filter),
        top_k,
    )
    .await
}

/// L3 retrieval under an explicit [`RecallScope`] (ADR-0027 T9).
///
/// Why: a wing cannot be expressed as an `Option<RoomType>`, and leaving L3
/// room-only would make `memory_recall_deep` silently ignore a `wing` argument
/// — the invisible-failure class ADR-0027 D4.4 rejects. This is the single
/// implementation; [`retrieve_l3`] is a thin wrapper over it.
/// What: as `retrieve_l3`, but the drawer filter compares `room_id` against the
/// scope's resolved room-id set.
/// Test: `l3_room_filter_excludes_other_rooms`,
/// `wing_scoped_deep_recall_returns_only_that_wing`.
pub async fn retrieve_l3_scoped(
    handle: &PalaceHandle,
    embedder: &dyn Embedder,
    query: &str,
    scope: &RecallScope,
    top_k: usize,
) -> Result<Vec<RecallResult>> {
    if top_k == 0 {
        return Ok(Vec::new());
    }
    let embeddings = embedder.embed_batch(&[query.to_string()]).await?;
    let Some(query_vec) = embeddings.into_iter().next() else {
        return Ok(Vec::new());
    };

    // Resolved BEFORE the drawer/closet guards are taken: it is a redb read
    // transaction, and holding those locks across I/O would stall every writer
    // on this palace for its duration (same ordering rule as `retrieve_l2`).
    let allowed = scope.allowed_room_ids(&handle.kg);
    let fetch = if allowed.is_some() {
        top_k.saturating_mul(3).max(top_k)
    } else {
        top_k
    };
    let hits = handle.vector_store.search(&query_vec, fetch).await?;

    let drawers = handle.drawers.read();
    let closets = handle.closets.read();
    let query_tokens: Vec<String> = extract_keywords(query);
    let now = chrono::Utc::now();
    let mut results: Vec<RecallResult> = Vec::with_capacity(hits.len());
    for hit in hits {
        let Some(drawer) = drawers.iter().find(|d| uuid_prefix_eq(d.id, hit.drawer_id)) else {
            continue;
        };
        if !scope_admits(&allowed, drawer.room_id) {
            continue;
        }
        // #4885: same read-time expiry gate as L2 — a deep search must not be
        // the one path that still serves a drawer past its TTL.
        if drawer.is_expired_at(now) {
            continue;
        }
        let age_days = DecayConfig::age_days(drawer.created_at);
        let boost = drawer.accumulated_boost(&handle.decay_config);
        let eff_importance =
            handle
                .decay_config
                .effective_importance(drawer.importance, age_days, boost);

        let drawer_id = drawer.id;
        let in_closet = query_tokens
            .iter()
            .any(|tok| closets.get(tok).is_some_and(|ids| ids.contains(&drawer_id)));
        let tag_boost = if in_closet { 0.15_f32 } else { 0.0 };
        // #4904: shares the one `rank_score` L2 uses, so deep recall cannot be
        // left ranking on the old importance-multiplier formula.
        let final_score = rank_score(hit.score, eff_importance, tag_boost);

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
    // ADR-0027 T9: unscoped recall is `RecallScope::All`, which reaches exactly
    // the code path that ran before rooms or wings could scope it.
    recall_scoped(handle, embedder, query, &RecallScope::All, top_k).await
}

/// Room-scoped `recall` — L0 + L1 + a `room`-filtered L2.
///
/// Why (ADR-0027 D6 / ticket T7): room filtering has worked in `retrieve_l2`
/// since #3274, but no recall entry point exposed it, so the only way to read
/// one room was `memory_list` or the HTTP `/recall` route. This is the door.
/// What: [`recall_scoped`] with the room lifted into a [`RecallScope::Room`].
/// Kept as its own entry point because a room is the axis most callers want and
/// `Option<RoomType>` is the shape they already hold.
/// Test: `recall_in_room_scopes_l2_hits`.
pub async fn recall_in_room(
    handle: &PalaceHandle,
    embedder: &dyn Embedder,
    query: &str,
    room: Option<RoomType>,
    top_k: usize,
) -> Result<Vec<RecallResult>> {
    recall_scoped(
        handle,
        embedder,
        query,
        &RecallScope::from_room_filter(room),
        top_k,
    )
    .await
}

/// Standard recall restricted to a [`RecallScope`] (ADR-0027 T9).
///
/// Why: "recall everything the `engineer` wing has learned" is the access
/// pattern a Wing exists to enable (ADR-0027 D2, pattern 1) — one query, with
/// no requirement that the caller already know that scope's complete topic set.
/// This is the one implementation [`recall`] and [`recall_in_room`] both run.
/// What: L0 + L1 (never scoped) merged with an L2 leg run under `scope`.
///
/// L0/L1 are deliberately NOT scoped: they are the palace's identity and
/// essential drawers — the always-on grounding every recall carries — and
/// dropping them for a scoped query would silently change what "recall" means
/// rather than narrowing it.
/// Test: `wing_scoped_recall_returns_only_that_wing`,
/// `recall_in_room_scopes_l2_hits`.
pub async fn recall_scoped(
    handle: &PalaceHandle,
    embedder: &dyn Embedder,
    query: &str,
    scope: &RecallScope,
    top_k: usize,
) -> Result<Vec<RecallResult>> {
    // Idle-to-disk: a recall is a genuine user access — reset the idle clock
    // so the idle-evict ticker does not drop an actively-queried palace.
    handle.touch();
    let expanded = expand_query(query);
    let mut combined = retrieve_l0_l1(handle);
    let l2 = retrieve_l2_scoped(handle, embedder, &expanded, scope, top_k).await?;

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
    // #5037: this stays a length cap, not a quality gate — callers that must
    // not show a weak match apply `super::relevance::apply_relevance_floor` to
    // what comes back. Gating here would silently change every MCP and CLI
    // recall caller's contract, which #5037's ruling does not ask for.
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
    recall_deep_in_room(handle, embedder, query, None, top_k).await
}

/// Room-scoped `recall_deep` — L0 + L1 + a `room`-filtered L3.
///
/// Why (ADR-0027 D6 / ticket T7): so deep recall is not the odd one out. Same
/// motivation and same L0/L1 policy as [`recall_in_room`].
/// What: identical to [`recall_deep`] except the room filter reaches L3.
/// Test: `recall_deep_in_room_scopes_l3_hits`.
pub async fn recall_deep_in_room(
    handle: &PalaceHandle,
    embedder: &dyn Embedder,
    query: &str,
    room: Option<RoomType>,
    top_k: usize,
) -> Result<Vec<RecallResult>> {
    recall_deep_scoped(
        handle,
        embedder,
        query,
        &RecallScope::from_room_filter(room),
        top_k,
    )
    .await
}

/// Deep recall restricted to a [`RecallScope`] (ADR-0027 T9).
///
/// Why/What: the L3 counterpart of [`recall_scoped`], so `memory_recall_deep`
/// honours a wing exactly as `memory_recall` does. L0/L1 stay unscoped for the
/// same reason given there.
/// Test: `wing_scoped_deep_recall_returns_only_that_wing`.
pub async fn recall_deep_scoped(
    handle: &PalaceHandle,
    embedder: &dyn Embedder,
    query: &str,
    scope: &RecallScope,
    top_k: usize,
) -> Result<Vec<RecallResult>> {
    // Idle-to-disk: deep recall is also a genuine user access.
    handle.touch();
    let expanded = expand_query(query);
    let mut combined = retrieve_l0_l1(handle);
    let l3 = retrieve_l3_scoped(handle, embedder, &expanded, scope, top_k).await?;

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
