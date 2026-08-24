//! Graph-traversal candidate retrieval for collections above
//! [`super::EXHAUSTIVE_SCAN_MAX_POINTS`] (#5179).
//!
//! Why: this arm had two budgets and both were constants that ignored the size
//! and the state of the collection.
//!
//! `ef_search` was pinned at 64 whatever the point count, so the layer-0
//! candidate heap covered a shrinking fraction of the index as a palace grew
//! and recall decayed with no signal to the caller — measured on this repo's
//! own palace embeddings, 17.4% of queries lost a true top-10 neighbour at 512
//! points and 24.5% at 1024.
//!
//! The candidate count was pinned at `2k`, and `HnswStore::search` dropped
//! tombstoned ids only AFTER `Hnsw::search` had already truncated to it. That
//! bounds the result in GRAPH points while the caller asked for `k` LIVE
//! drawers, and the two diverge whenever a palace has deleted since its last
//! open: `hnsw_rs` cannot remove a point, so every delete leaves one behind. A
//! palace whose nearest neighbours have all been deleted returned fewer than
//! `k` hits, or none, with live neighbours still in the graph. This is the
//! sibling of the cut #5178 removed from the exhaustive arm, which returns its
//! full ranking for the same reason.
//! What: [`graph_ef_search`] scales `ef_search` with the live-drawer count and
//! clamps the scaled term at [`HNSW_MAX_EF_SEARCH`]. [`graph_nearest`] filters
//! tombstones out of the candidate pool BEFORE asking whether the pool is deep
//! enough, and widens it until `k` live drawers survive or the graph is
//! exhausted — so filtering can no longer happen after the slot count is fixed.
//! Test: `graph_ef_search_scales_with_live_count_and_stops_at_the_cap`,
//! `graph_candidates_inflate_by_the_graphs_dead_weight`,
//! `search_above_the_threshold_fills_k_despite_tombstoned_nearest_neighbours`.

use std::collections::HashSet;

use hnsw_rs::prelude::{DistCosine, Hnsw};

/// Baseline `ef_search` — the floor the live-scaled term is measured against.
///
/// Why: a small multiple of `top_k` is the standard recommendation, and 64
/// keeps search quality stable for the small `top_k` most callers ask for. It
/// is the FLOOR rather than the value since #5179: as a fixed value it made the
/// candidate heap a shrinking fraction of a growing collection.
const HNSW_DEFAULT_EF_SEARCH: usize = 64;

/// Divisor turning the live-drawer count into an `ef_search` budget.
///
/// Why (#5179): recall on this path is set by how much of the collection the
/// layer-0 candidate heap can hold, so the budget has to grow with the
/// collection rather than sit at a constant.
const HNSW_EF_LIVE_DIVISOR: usize = 4;

/// Ceiling on the live-scaled `ef_search` term.
///
/// Why (#5179): `live / 4` is unbounded, so without a ceiling a 100k-drawer
/// palace would ask for `ef = 25_000`, and this budget is what a graph query
/// costs. The number is chosen on measured latency: against real palace
/// embeddings on an M-series laptop, a 384-dim query over ~2900 drawers takes
/// about 0.5ms at `ef = 64` and about 2.2ms at `ef = 719`, and 1024 puts the
/// ceiling at roughly 3ms — the most this store is willing to spend answering
/// one recall. It is not a cost equivalence with a 1024-point scan: an
/// `ef = 1024` traversal costs several times what scanning 1024 points costs,
/// because each unit of `ef` carries heap and visited-set work a flat sequential
/// scan does not. What the cap does buy is that per-query cost stops growing
/// with the palace, which is the whole reason this arm exists.
///
/// The cap binds from `live = 4096` upward, and since #5179 raised
/// [`super::EXHAUSTIVE_SCAN_MAX_POINTS`] to 4096 that is every collection this
/// arm sees: the `live / 4` ramp now lies entirely inside the exhaustive
/// regime, so in practice `ef` here is this constant. The ramp is kept because
/// it is what the ceiling would fall back onto if the scan's cost ever forces
/// the ceiling down again, and because the divisor is the thing that would have
/// to change for the cap to bind later rather than immediately.
///
/// The recall it buys is real but partial: with the scaled `ef` in place, real
/// embeddings still lost a true top-10 neighbour on 14.9% of queries at 2048
/// live drawers and 20.5% at 2879 — sizes now answered by the scan. Closing the
/// gap that remains above 4096 needs a different index structure or a rerank
/// pass, not a larger number here.
/// What: clamps the SCALED term only. A caller asking for a large `top_k` still
/// gets `2k`, because returning fewer results than asked for is a different
/// defect from spending too long finding them.
/// Test: `graph_ef_search_scales_with_live_count_and_stops_at_the_cap`.
pub(super) const HNSW_MAX_EF_SEARCH: usize = 1024;

/// `ef_search` for a graph-arm query over `live` drawers returning `k` hits.
///
/// Why (#5179): see [`HNSW_EF_LIVE_DIVISOR`] and [`HNSW_MAX_EF_SEARCH`] — a
/// constant `ef` means recall decays as the palace grows, silently.
/// What: `max(HNSW_DEFAULT_EF_SEARCH, 2k, min(live / 4, HNSW_MAX_EF_SEARCH))`.
/// Test: `graph_ef_search_scales_with_live_count_and_stops_at_the_cap`.
pub(super) fn graph_ef_search(k: usize, live: usize) -> usize {
    let scaled = (live / HNSW_EF_LIVE_DIVISOR).min(HNSW_MAX_EF_SEARCH);
    HNSW_DEFAULT_EF_SEARCH.max(k.saturating_mul(2)).max(scaled)
}

/// First-guess candidate count for a graph with `graph_points` points backing
/// `live` drawers.
///
/// Why (#5179): the `2k` over-fetch is stated in graph points but spent in live
/// drawers, and a graph carries one extra point per delete and per re-upsert
/// since the last `HnswStore::open`. Scaling the first guess by that overhang
/// means an ordinarily-churned palace answers in one traversal instead of
/// entering [`graph_nearest`]'s widening loop.
/// What: `2k * ceil(graph_points / live)`, clamped at `graph_points` — no query
/// can return more points than the graph holds. In the steady state the ratio
/// is 1 and this is exactly the old `2k`, so a palace that has not churned pays
/// nothing.
/// Test: `graph_candidates_inflate_by_the_graphs_dead_weight`.
pub(super) fn graph_candidates(k: usize, live: usize, graph_points: usize) -> usize {
    let want = k.saturating_mul(2).max(k);
    let inflation = graph_points.div_ceil(live.max(1)).max(1);
    want.saturating_mul(inflation).min(graph_points.max(1))
}

/// Candidates for `query` from the graph, with tombstoned ids already removed
/// and the pool widened until `k` distinct live ids survive.
///
/// Why (#5179): `Hnsw::search` truncates to the count it is given, so a filter
/// applied to its output is a filter applied after the slot count is fixed.
/// Deciding how deep to go on the UNFILTERED count is what let a palace with
/// deleted nearest neighbours return fewer than `k` live hits — and an
/// over-fetch scaled to the average dead fraction does not repair it either,
/// because the dead points that matter are the ones nearest the query, not a
/// random sample of them. The only sound bound is the one measured on live
/// candidates.
/// What: filters tombstones out of each traversal's output, and while fewer than
/// `k` DISTINCT live ids survive, doubles the requested candidate count and
/// re-traverses. Terminates because the count strictly increases and stops at
/// `graph_points`. Distinct rather than raw count because a re-upsert leaves two
/// points under one `vector_id` and `HnswStore::search` emits each id once.
/// Returns `(vector_id, distance)` pairs in the traversal's ascending-distance
/// order.
///
/// What this does NOT repair: a live drawer the traversal cannot REACH from its
/// descent pivot along pruned layer-0 neighbour lists. Widening the candidate
/// budget cannot reach a point no path leads to, and at `want = graph_points`
/// the traversal has still only seen the reachable component. That is #5171's
/// limit, and the exhaustive threshold — not this function — is what answers it.
///
/// Cost: one traversal in the steady state, since [`graph_candidates`] already
/// covers an ordinarily-churned palace. The loop is reached only when deleted
/// points genuinely crowd out the query's live neighbours, and the doublings
/// sum to about twice the final traversal — bounded above by one full-graph
/// traversal, which is the honest price of not handing the caller an empty
/// result while its neighbours sit in the index.
/// Test: `search_above_the_threshold_fills_k_despite_tombstoned_nearest_neighbours`,
/// `search_above_the_threshold_stops_widening_when_the_graph_is_exhausted`.
pub(super) fn graph_nearest(
    index: &Hnsw<'static, f32, DistCosine>,
    query: &[f32],
    tombstoned: &HashSet<u64>,
    k: usize,
    live: usize,
) -> Vec<(u64, f32)> {
    let graph_points = index.get_nb_point();
    if graph_points == 0 || k == 0 {
        return Vec::new();
    }
    let ef = graph_ef_search(k, live);
    let mut want = graph_candidates(k, live, graph_points);
    loop {
        // `hnsw_rs` raises `ef` to the requested neighbour count internally
        // (`hnsw.rs:1519`); passing the max explicitly keeps the widening this
        // loop performs visible here rather than hidden in the library.
        let hits: Vec<(u64, f32)> = index
            .search(query, want, ef.max(want))
            .into_iter()
            .map(|hit| (hit.d_id as u64, hit.distance))
            .filter(|(id, _)| !tombstoned.contains(id))
            .collect();
        let distinct = hits
            .iter()
            .map(|(id, _)| *id)
            .collect::<HashSet<u64>>()
            .len();
        if distinct >= k || want >= graph_points {
            return hits;
        }
        want = want.saturating_mul(2).min(graph_points);
    }
}
