//! Recall handlers for the trusty-memory MCP surface.
//!
//! Why: `memory_ops.rs` held the whole `memory_*` family and crossed the
//! 500-SLOC production cap when the recall projection landed (owner ruling
//! 2026-09-14). Recall is the natural cut: the three read handlers
//! (`memory_recall`, `memory_recall_deep`, `memory_recall_all`) and the lane
//! helpers only they use form a closed group, leaving `memory_ops.rs` with the
//! write/list/forget family.
//! What: `handle_memory_recall`, `handle_memory_recall_deep`,
//! `handle_memory_recall_all`, and their private helpers, moved verbatim except
//! for `recall_scope`, which widened to `pub(crate)` because `memory_list`
//! shares it. Response shaping lives next door in
//! [`super::recall_projection`]. #8246/#9143: every handler fetches a
//! [`ranking_window`] of candidates, demotes stale snapshots
//! ([`super::recall_rank`]) and only then cuts to `top_k`; the single-palace
//! handlers' ready path also merges user-scope rulings
//! ([`super::recall_rulings`]) and reports failed rulings palaces.
//! Test: `dispatch_recall_room_filter_scopes_results` in `tools::tests`;
//! `stdio_serve_recall_all_bounded`; `tests/recall_query_discrimination.rs`.

use crate::{AppState, DaemonReadiness};
use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};
use trusty_common::memory_core::palace::RoomType;
use trusty_common::memory_core::retrieval::{
    recall_across_palaces_reporting, recall_deep_scoped, recall_scoped, scope_admits,
    CrossPalaceRecall, PalaceHandle, RecallResult, RecallScope,
};

use crate::service::recall_stream::{recall_all_scoped, RecallAllScope};

use super::bm25::{bm25_hits_to_recall_results, bm25_search_optional, fuse_bm25_into_recall};
use super::helpers::open_palace_handle;
// #6318: the read tools below fall back to a palace index instead of erroring
// when the caller names no palace and the server has no default.
use super::palace_index::{resolve_palace_or_index, PalaceScope};
// #8246 / #9143: typed temporal ranking and the user-scope rulings leg.
use super::recall_rank::{demote_stale_snapshots, demote_stale_snapshots_across, ranking_window};
use super::recall_rulings::{fetch_user_rulings, fold_rulings, RulingsFold};
use super::recall_rulings_floor::apply_rulings_floor;
// #9421: drawers superseded through a `superseded_by` edge rank below their
// replacements on every recall path.
use super::recall_supersede::{supersessions_across, supersessions_for, Supersessions};
// Owner ruling 2026-09-14: the recall projection — creator-tag hiding and the
// optional `min_score` floor — applies to every recall response this file emits.
use super::recall_projection::{
    apply_score_floor, include_creator_tags_arg, min_score_arg, project_tags, serialize_recall,
    RecallProjection,
};
use super::wing_ops::resolve_wing_arg;

/// Whether a recall may use the vector lane, or must take the degraded
/// L0/L1 + BM25 fallback.
///
/// Why (#4836): the degraded fallback ignores the query except through an
/// optional BM25 lane, so taking it when the embedder is in fact live makes
/// `memory_recall` answer every query with the same drawers — which is what it
/// did for the whole life of any daemon whose startup warm-up failed once. The
/// readiness latch alone cannot be trusted for this decision: a recall that
/// degrades never calls [`AppState::embedder`], so it can never observe the
/// recovery that would clear the latch. Asking the embedder cell directly
/// breaks that deadlock, and costs one atomic load.
/// What: `true` when readiness is `Ready`, or when the shared embedder is
/// already initialised regardless of the latch. Never triggers a cold init, so
/// issue #1970's "never block a recall on embedder warm-up" guarantee holds.
/// Test: `different_queries_return_different_drawers`,
/// `deep_recall_also_discriminates_by_query` (tests/recall_query_discrimination.rs)
/// — both drive recall with a live embedder behind a stranded `Warming` latch.
fn vector_lane_available(state: &AppState) -> bool {
    state.readiness() != DaemonReadiness::Warming
        || trusty_common::memory_core::retrieval::shared_embedder_initialized()
}

/// Degraded-embedder recall fallback (issue #1970): L0/L1 + BM25 only.
///
/// Why: mirrors trusty-search's staged-pipeline degradation — rather than
/// blocking/erroring while the shared embedder cold-inits, every recall
/// variant should return identity + essential drawers plus whatever the
/// BM25 lexical lane can resolve, with the semantic (vector) layer simply
/// omitted until the embedder warms up.
/// What: seeds the result set with `retrieve_l0_l1`, joins the optional BM25
/// lane (query runs regardless of embedder state — BM25 has no embedder
/// dependency), hydrates any BM25-only hits via `bm25_hits_to_recall_results`,
/// merges without duplicating drawers already present, re-sorts by score
/// descending, and truncates to `top_k`.
/// ADR-0027 T7: `room` scopes the lexical lane here for the same reason it
/// scopes L2/L3 — a caller who narrowed to one room must never be handed
/// another room's drawer just because the daemon happened to be warming. L0/L1
/// are deliberately left unfiltered, matching `retrieval::recall_in_room`.
/// #5036: the palace is read off `handle`, never passed in. This is the one
/// path where the lexical lane is the ONLY lane, so keying it on a caller's
/// slug — which `open_palace` may have redirected through an alias — searched a
/// socket the backfill never writes and left the caller with L0/L1 alone.
/// Taking no `palace` parameter makes that disagreement unrepresentable.
/// Test: `recall_degrades_to_l0_l1_when_the_embedder_is_genuinely_cold`,
/// `recall_deep_degrades_to_l0_l1_when_the_embedder_is_genuinely_cold`,
/// `bm25_alias_recall.rs::an_aliased_recall_reads_the_corpus_the_backfill_wrote`.
async fn recall_without_embedder(
    state: &AppState,
    handle: &trusty_common::memory_core::retrieval::PalaceHandle,
    query: &str,
    scope: &RecallScope,
    top_k: usize,
) -> Vec<trusty_common::memory_core::retrieval::RecallResult> {
    let palace = handle.id.as_str();
    let mut results = trusty_common::memory_core::retrieval::retrieve_l0_l1(handle);
    // ADR-0027 T7 filtered this lane by room; T9 (#4809) widens it to the whole
    // scope so a WING-scoped recall issued while the embedder is warming is
    // filtered too. Without that, the warming path would return unscoped
    // results — a scope boundary failing open, which is a leak, not a
    // degraded result.
    let allowed = scope.allowed_room_ids(&handle.kg);
    if let Some(bm25_hits) = bm25_search_optional(state, palace, query, top_k).await {
        for hydrated in bm25_hits_to_recall_results(handle, &bm25_hits) {
            if !scope_admits(&allowed, hydrated.drawer.room_id) {
                continue;
            }
            if !results.iter().any(|r| r.drawer.id == hydrated.drawer.id) {
                results.push(hydrated);
            }
        }
    }
    results.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    results.truncate(top_k);
    results
}

/// The optional `room` scope on a recall call.
///
/// Why (ADR-0027 T7 / #4806): `retrieve_l2` has enforced a room filter since
/// #3274, but the MCP recall schema never carried the argument, so room-scoped
/// recall was reachable only through `memory_list` or the HTTP `/recall` route.
/// Parsing it in one helper keeps `memory_recall` and `memory_recall_deep`
/// from drifting into two spellings of the same option (ADR-0027 D4.1).
/// What: `RoomType::parse` over `args["room"]`; `None` means every room.
/// Test: `dispatch_recall_room_filter_scopes_results`.
fn recall_room_filter(args: &Value) -> Option<RoomType> {
    args.get("room")
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
        .map(RoomType::parse)
}

/// Combine the optional `room` (T7) and `wing` (T9) arguments into one scope.
///
/// Why (ADR-0027 #4806 + #4809): a room is a topic and a wing is an owner, so
/// the two arguments are different axes and every read path must agree on what
/// they mean together. Resolving them in ONE helper is what keeps
/// `memory_recall`, `memory_recall_deep`, and `memory_list` from drifting into
/// three spellings of the same option (ADR-0027 D4.1).
/// What: `RecallScope::Wing` when `wing` is present, `RecallScope::Room` when
/// only `room` is, `RecallScope::All` when neither. Supplying BOTH is an error
/// — "that topic inside that scope" is a real query this ticket does not
/// implement, and honouring one while dropping the other is exactly the
/// invisible failure ADR-0027 exists to remove. An unknown wing errors here
/// too, via `resolve_wing_arg`.
/// Test: `recall_rejects_wing_and_room_together`,
/// `recall_rejects_an_unknown_wing`.
pub(crate) fn recall_scope(handle: &PalaceHandle, args: &Value, tool: &str) -> Result<RecallScope> {
    let room = recall_room_filter(args);
    match resolve_wing_arg(handle, args, tool)? {
        Some(wing_id) => {
            if room.is_some() {
                return Err(anyhow!(
                    "{tool}: 'wing' and 'room' together are not supported yet — \
                     pass one or the other"
                ));
            }
            Ok(RecallScope::Wing(wing_id))
        }
        None => Ok(RecallScope::from_room_filter(room)),
    }
}

pub(crate) async fn handle_memory_recall(state: &AppState, args: Value) -> Result<Value> {
    // #6318: no palace and no default is answered with an index, not an error.
    let palace = match resolve_palace_or_index(state, &args, "memory_recall").await? {
        PalaceScope::Palace(p) => p,
        PalaceScope::Index(index) => return Ok(index),
    };
    let query = args
        .get("query")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("memory_recall: missing 'query'"))?;
    let top_k = args.get("top_k").and_then(|v| v.as_u64()).unwrap_or(10) as usize;
    // Owner ruling 2026-09-14: both projection arguments are read once, here,
    // so the warming short-circuit below and the fused path answer identically.
    let include_creator_tags = include_creator_tags_arg(&args);
    let min_score = min_score_arg(&args, "memory_recall")?;
    // A floored recall asks the lanes for a wider candidate set, so hits the
    // floor removes are backfilled instead of leaving the caller short.
    // #8246: and every recall fetches about 2 * top_k, so demotion can lift a
    // hit from just past the cut.
    let fetch_k = ranking_window(top_k, min_score);

    let handle = open_palace_handle(state, &palace)?;
    // ADR-0027 T7 + T9: resolved BEFORE the warming short-circuit below, so a
    // wing- or room-scoped recall is filtered on every path. Resolving it after
    // that early return would let a scoped recall issued during embedder warmup
    // come back unfiltered.
    let scope = recall_scope(&handle, &args, "memory_recall")?;

    // Issue #1970: while the embedder is still warming up, return BM25 +
    // L0/L1 results immediately instead of blocking/erroring on embedder
    // state.
    // #4836: gate on the embedder's real state, not the readiness latch alone —
    // this fallback ignores the query, so entering it while the embedder is live
    // makes every query return the same drawers.
    if !vector_lane_available(state) {
        let results = recall_without_embedder(state, &handle, query, &scope, fetch_k).await;
        // #8246: stale snapshots rank below current facts on every path. No
        // rulings leg here: it needs the embedder this path is waiting for.
        let sup = supersessions_for(&handle, &results).await;
        let cut = RecallCut::new(top_k, min_score, include_creator_tags);
        return Ok(cut.rank_and_serialize(&palace, query, results, RulingsFold::default(), sup));
    }

    let embedder = state.embedder().await?;
    // Issue #156: when the BM25 lane is enabled, run it in parallel
    // with the vector recall and RRF-fuse the two ranked lists.
    // When the daemon is unavailable or the env var is unset, the
    // helper returns `None` and we return the vector-only results
    // verbatim — zero behavioural change for existing deployments.
    // ADR-0027 T7: the BM25 lane needs no scope filter of its own here —
    // `fuse_bm25_into_recall` only *boosts* drawers already present in the
    // vector list (it never appends BM25-only hits), and that list is already
    // scoped, so no out-of-scope drawer can enter through the lexical
    // lane. The warming path above is different: it hydrates BM25-only hits,
    // which is why it filters explicitly.
    let vector_fut = recall_scoped(&handle, embedder.as_ref(), query, &scope, fetch_k);
    // #5036: key the lexical lane on the RESOLVED palace id. `open_palace`
    // follows aliases, so the requested slug can address a different palace
    // than the vector lane just searched.
    let bm25_fut = bm25_search_optional(state, handle.id.as_str(), query, fetch_k);
    // #9143: the rulings leg runs beside the project lanes, bounded.
    let rulings_fut = fetch_user_rulings(state, &handle.id, embedder.clone(), query, &scope, top_k);
    let (vector_res, bm25_res, rulings) = tokio::join!(vector_fut, bm25_fut, rulings_fut);
    let mut results = vector_res.context("recall")?;
    if let Some(bm25_hits) = bm25_res {
        // `fetch_k`, not `top_k`: this truncation feeds the floor, so cutting to
        // the caller's count here would undo the widened window.
        fuse_bm25_into_recall(&mut results, &bm25_hits, fetch_k);
    }
    // #9143: user-scope rulings join at L1, after fusion so they compete on
    // the fused scale; failed rulings palaces are reported, never fatal.
    let fold = fold_rulings(&mut results, rulings, query, top_k, min_score);
    let sup = supersessions_for(&handle, &results).await;
    // Owner ruling 2026-09-14: the floor runs AFTER fusion — the RRF bonus is
    // part of the score the caller set a bar against, so filtering before it
    // would judge a hit on a number the response never shows.
    let cut = RecallCut::new(top_k, min_score, include_creator_tags);
    Ok(cut.rank_and_serialize(&palace, query, results, fold, sup))
}

pub(crate) async fn handle_memory_recall_deep(state: &AppState, args: Value) -> Result<Value> {
    // #6318: no palace and no default is answered with an index, not an error.
    let palace = match resolve_palace_or_index(state, &args, "memory_recall_deep").await? {
        PalaceScope::Palace(p) => p,
        PalaceScope::Index(index) => return Ok(index),
    };
    let query = args
        .get("query")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("memory_recall_deep: missing 'query'"))?;
    let top_k = args.get("top_k").and_then(|v| v.as_u64()).unwrap_or(10) as usize;
    // Owner ruling 2026-09-14: deep recall takes the same two projection
    // arguments as `memory_recall`, for the reason ADR-0027 D4.1 gives about
    // `room`/`wing` — two spellings of one option is how these two drift.
    let include_creator_tags = include_creator_tags_arg(&args);
    let min_score = min_score_arg(&args, "memory_recall_deep")?;
    // #8246: same widened window as `memory_recall`.
    let fetch_k = ranking_window(top_k, min_score);

    let handle = open_palace_handle(state, &palace)?;
    // ADR-0027 T7 + T9: deep recall is no longer the odd one out — it takes the
    // same `room`/`wing` scope as `memory_recall`, resolved before the warming
    // short-circuit so no path can return unscoped results.
    let scope = recall_scope(&handle, &args, "memory_recall_deep")?;

    // Issue #1970: same warming-fallback posture as memory_recall.
    // #4836: and the same embedder-state gate, for the same reason.
    if !vector_lane_available(state) {
        let results = recall_without_embedder(state, &handle, query, &scope, fetch_k).await;
        // #8246: stale snapshots rank below current facts on every path. No
        // rulings leg here: it needs the embedder this path is waiting for.
        let sup = supersessions_for(&handle, &results).await;
        let cut = RecallCut::new(top_k, min_score, include_creator_tags);
        return Ok(cut.rank_and_serialize(&palace, query, results, RulingsFold::default(), sup));
    }

    let embedder = state.embedder().await?;
    // #5036: deep recall runs the lexical lane alongside the vector lane, the
    // same as its `memory_recall` sibling. It was the one dispatch path that
    // had the fusion available and did not call it, so a deep recall answered
    // by vector centroid alone.
    let vector_fut = recall_deep_scoped(&handle, embedder.as_ref(), query, &scope, fetch_k);
    let bm25_fut = bm25_search_optional(state, handle.id.as_str(), query, fetch_k);
    // #9143 / #8246: same rulings leg and demotion as `memory_recall`.
    let rulings_fut = fetch_user_rulings(state, &handle.id, embedder.clone(), query, &scope, top_k);
    let (vector_res, bm25_res, rulings) = tokio::join!(vector_fut, bm25_fut, rulings_fut);
    let mut results = vector_res.context("recall_deep")?;
    // ADR-0027 T7: no scope filter needed on the lexical side — the fusion only
    // boosts drawers already in the vector list, which is already scoped.
    if let Some(bm25_hits) = bm25_res {
        // `fetch_k`, not `top_k` — see the `memory_recall` sibling.
        fuse_bm25_into_recall(&mut results, &bm25_hits, fetch_k);
    }
    let fold = fold_rulings(&mut results, rulings, query, top_k, min_score);
    let sup = supersessions_for(&handle, &results).await;
    // Owner ruling 2026-09-14: after fusion, same as `memory_recall`.
    let cut = RecallCut::new(top_k, min_score, include_creator_tags);
    Ok(cut.rank_and_serialize(&palace, query, results, fold, sup))
}

/// The caller's cut for a single-palace recall: count, floor and tag view.
///
/// Why: both single-palace handlers, on both their paths, end the same way;
/// one tail keeps demotion ahead of the floor and the cut on all four.
/// Test: `demotion_applies_on_every_recall_surface`.
struct RecallCut {
    top_k: usize,
    min_score: Option<f32>,
    include_creator_tags: bool,
}

impl RecallCut {
    fn new(top_k: usize, min_score: Option<f32>, include_creator_tags: bool) -> Self {
        Self {
            top_k,
            min_score,
            include_creator_tags,
        }
    }

    /// Demote stale snapshots (#8246) and superseded drawers (#9421), lift
    /// answering rulings into reserved slots (#9143), apply the floor, cut to
    /// `top_k`, and serialize with any failed rulings palaces (#9143).
    ///
    /// `sup` holds the project palace's edges; the rulings palaces' own edges
    /// arrive in `rulings.superseded` and join it here (#9421). A superseded
    /// ruling keeps its demoted score rank but loses its reserved slot.
    /// Test: `the_rulings_floor_never_lifts_a_superseded_ruling_above_its_replacement`,
    /// `a_superseded_ruling_from_a_rulings_palace_ranks_below_its_replacement`.
    fn rank_and_serialize(
        &self,
        palace: &str,
        query: &str,
        mut results: Vec<RecallResult>,
        mut rulings: RulingsFold,
        mut sup: Supersessions,
    ) -> Value {
        // #9421: a ruling superseded inside its rulings palace demotes too.
        sup.extend(std::mem::take(&mut rulings.superseded));
        // #9421: a superseded ruling is no longer the standing rule, so the
        // floor never lifts it (it could land above its replacement).
        rulings.floored.retain(|id| !sup.contains_key(id));
        demote_stale_snapshots(&mut results, chrono::Utc::now(), &sup);
        // #9143 AC2: after the score sort, before the floor and the cut, so
        // the reserved slots sit inside `top_k`.
        apply_rulings_floor(&mut results, &rulings.floored, self.top_k);
        let dropped_below_floor = apply_score_floor(&mut results, self.min_score, self.top_k);
        serialize_recall(
            palace,
            query,
            results,
            &RecallProjection {
                include_creator_tags: self.include_creator_tags,
                dropped_below_floor,
                rulings_degraded: rulings.degraded,
            },
        )
    }
}

/// Cross-palace counterpart of `recall_without_embedder` (issue #1970).
///
/// Why: `memory_recall_all` must degrade the same way the single-palace
/// recall handlers do — BM25 + L0/L1 per palace, no vector lane — while the
/// embedder is warming up.
/// What: runs `recall_without_embedder` against every handle, tags each hit
/// with its source palace id, then merges/re-sorts/truncates exactly like
/// `recall_across_palaces` does for the vector-backed path.
/// Test: `recall_degrades_to_l0_l1_when_the_embedder_is_genuinely_cold` and
/// `recall_deep_degrades_to_l0_l1_when_the_embedder_is_genuinely_cold` cover the
/// single-palace `recall_without_embedder` this fans out; no test drives the
/// cross-palace fan-out through a cold embedder yet.
async fn recall_all_without_embedder(
    state: &AppState,
    handles: &[std::sync::Arc<trusty_common::memory_core::retrieval::PalaceHandle>],
    query: &str,
    top_k: usize,
) -> Vec<trusty_common::memory_core::retrieval::CrossPalaceResult> {
    let mut merged = Vec::new();
    for handle in handles {
        let palace_id = handle.id.as_str().to_string();
        let hits = recall_without_embedder(state, handle, query, &RecallScope::All, top_k).await;
        merged.extend(hits.into_iter().map(|result| {
            trusty_common::memory_core::retrieval::CrossPalaceResult {
                palace_id: palace_id.clone(),
                result,
            }
        }));
    }
    merged.sort_by(|a, b| {
        b.result
            .score
            .partial_cmp(&a.result.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    merged.truncate(top_k);
    merged
}

/// MCP `memory_recall_all`: cross-palace recall with a scope and a coverage
/// report (ADR-0071, #9299).
///
/// Why: the default searched every palace on disk by cold-opening it, and the
/// response could not say what it had missed.
/// What: parses `q`, `top_k`, `deep` and `scope` (`resident` by default,
/// `all` for the streamed walk; anything else is an error), runs
/// `recall_all_scoped`, demotes stale snapshots and superseded drawers
/// (#9421) in either scope, cuts to `top_k`, and returns
/// `query`, `results` and the ADR-0071 D4 coverage fields.
/// Test: `default_scope_opens_no_palace_and_keeps_the_resident_set`,
/// `open_failure_is_reported_on_every_surface`,
/// `recall_all_rejects_an_unknown_scope`,
/// `both_scopes_demote_a_superseded_drawer`.
pub(crate) async fn handle_memory_recall_all(state: &AppState, args: Value) -> Result<Value> {
    let query = args
        .get("q")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("memory_recall_all: missing 'q'"))?;
    let top_k = args.get("top_k").and_then(|v| v.as_u64()).unwrap_or(10) as usize;
    let deep = args.get("deep").and_then(|v| v.as_bool()).unwrap_or(false);
    // #9299 (ADR-0071): resident palaces by default; `scope: "all"` opens.
    let scope =
        RecallAllScope::parse(args.get("scope")).context("memory_recall_all: invalid 'scope'")?;
    // Owner ruling 2026-09-14: the fan-out builds its own payload (it carries a
    // `palace_id` the single-palace envelope has no field for), but the tag
    // projection is the same one — a cross-palace hit's provenance is no more
    // useful to a caller than a single-palace hit's. No `min_score` here: this
    // tool merges across palaces, where one floor over heterogeneous corpora
    // would mean different things per palace.
    let include_creator_tags = include_creator_tags_arg(&args);
    // #8246: fetch about 2 * top_k so demotion can lift a hit past the cut.
    let window = ranking_window(top_k, None);

    // #9299: `recall_all_scoped` lists the palaces, skips provably empty ones
    // (#9141), and either searches the resident set or streams the estate in
    // bounded batches (#7125). A palace that fails to open is reported in the
    // coverage, not just logged.
    // Issue #1970: BM25 + L0/L1 fallback across every palace while warming.
    // #4836: gated on the embedder's real state, as the per-palace paths are.
    // #9421: each batch reads its own palaces' `superseded_by` edges while its
    // handles are open; the merged list is demoted once, below. #9299: in
    // either scope, so the resident default demotes as `scope: "all"` does.
    let sup = std::sync::Arc::new(parking_lot::Mutex::new(Supersessions::new()));
    let outcome = if !vector_lane_available(state) {
        recall_all_scoped(state, scope, "memory_recall_all", window, |handles| {
            let sup = sup.clone();
            async move {
                let hits = recall_all_without_embedder(state, &handles, query, window).await;
                let found = supersessions_across(&handles, &hits).await;
                sup.lock().extend(found);
                Ok(CrossPalaceRecall {
                    results: hits,
                    failed: Vec::new(),
                })
            }
        })
        .await?
    } else {
        // #4836: `embedder()` now yields the type-erased shared embedder
        // directly, so the local re-erasure this used to need is gone.
        let embedder = state.embedder().await?;
        recall_all_scoped(state, scope, "memory_recall_all", window, |handles| {
            let embedder = embedder.clone();
            let sup = sup.clone();
            async move {
                // #9299: the reporting variant names palaces whose recall failed.
                let recall =
                    recall_across_palaces_reporting(&handles, &embedder, query, window, deep)
                        .await?;
                let found = supersessions_across(&handles, &recall.results).await;
                sup.lock().extend(found);
                Ok(recall)
            }
        })
        .await
        .context("recall_across_palaces")?
    };
    let mut results = outcome.results;
    // #8246: the merged list gets the same snapshot demotion, then the cut.
    let sup = std::mem::take(&mut *sup.lock());
    demote_stale_snapshots_across(&mut results, chrono::Utc::now(), &sup);
    results.truncate(top_k);

    let payload: Vec<Value> = results
        .iter()
        .map(|r| {
            json!({
                "palace_id":  r.palace_id,
                "drawer_id":  r.result.drawer.id.to_string(),
                "content":    r.result.drawer.content(),
                "importance": r.result.drawer.importance,
                "tags":       project_tags(&r.result.drawer.tags, include_creator_tags),
                "score":      r.result.score,
                "layer":      r.result.layer,
                "drawer_type": r.result.drawer.drawer_type.as_str(),
            })
        })
        .collect();
    let mut out = serde_json::Map::new();
    out.insert("query".into(), json!(query));
    out.insert("results".into(), json!(payload));
    // #9299 (ADR-0071 D4): scope and coverage on every response.
    outcome.coverage.insert_into(scope, &mut out);
    Ok(Value::Object(out))
}

#[cfg(test)]
#[path = "recall_ops_tests.rs"]
mod recall_ops_tests;
