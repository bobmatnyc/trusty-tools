//! Exact nearest-neighbour scan for collections small enough that HNSW's
//! approximation is pure downside (#5171).
//!
//! Why: `hnsw_rs` seeds its level-assignment RNG from OS entropy, so
//! `HnswStore::open` builds a different random graph on every palace open. A
//! layer-0 search returns only what is reachable from the descent pivot along
//! neighbour lists that Navarro's heuristic has pruned, and that reachable set
//! is not the whole index. Below `ef_search` points the candidate budget
//! already covers every point in the collection, so the traversal is spending a
//! full scan's worth of distance evaluations and still returning a subset —
//! measured on real 384-dim palace embeddings, 2–4% of queries lost a true
//! top-5 neighbour at 6–16 points. Scanning every point instead is exact and,
//! at these sizes, no more expensive. 93% of this machine's 94 real palaces
//! hold 64 vectors or fewer.
//! What: [`exhaustive_nearest`] evaluates the index's own `DistCosine` against
//! every point the graph holds and keeps the closest `want` by
//! `vector_id`, so its distances are identical to the ones the traversal would
//! have reported for the same points.
//! Test: `exhaustive_scan_returns_every_point_the_graph_holds`,
//! `search_returns_the_exact_top_k_below_the_exhaustive_threshold`,
//! `search_reports_a_re_upserted_drawer_once_at_its_current_vector`.

use std::collections::HashMap;

use hnsw_rs::prelude::{DistCosine, Distance, Hnsw};

/// Live-point ceiling at or below which [`super::HnswStore::search`] scans
/// exhaustively instead of traversing the graph.
///
/// Why (#5171): the recall loss this scan removes is not confined to tiny
/// collections — on real embeddings it was still 3.2% of queries at 256 points
/// — but the scan has to stay cheap enough that it never becomes the reason a
/// recall is slow. 256 is where the two curves meet on this workload: an
/// in-memory scan of 256 384-dim vectors costs about the same as the
/// `VECTOR_KEYS` reverse-map build `search` already performs on every call, and
/// it covers 99% of this machine's real palaces. Above it the graph traversal's
/// sublinear cost is worth its approximation, which is the trade HNSW exists to
/// make.
/// What: compared against `Hnsw::get_nb_point()`, which counts inserted points
/// including any shadow copies left by a re-upsert.
/// Test: `search_uses_the_graph_above_the_exhaustive_threshold`.
pub(super) const EXHAUSTIVE_SCAN_MAX_POINTS: usize = 256;

/// Every point in `index`, ranked by exact distance to `query`, best first.
///
/// Why: see the module header — below [`EXHAUSTIVE_SCAN_MAX_POINTS`] the graph
/// traversal can silently omit a true nearest neighbour, and a full scan cannot.
/// What: walks the point indexation (which visits layer 0 upward and yields
/// every stored point exactly once), evaluates the index's own distance
/// function, keeps the smallest distance per `vector_id` so a re-upsert's
/// shadow copy cannot occupy two result slots, and returns at most `want`
/// `(vector_id, distance)` pairs sorted ascending by distance.
/// Test: `exhaustive_scan_returns_every_point_the_graph_holds`,
/// `search_reports_a_re_upserted_drawer_once_at_its_current_vector`.
pub(super) fn exhaustive_nearest(
    index: &Hnsw<'static, f32, DistCosine>,
    query: &[f32],
    want: usize,
) -> Vec<(u64, f32)> {
    // `hnsw_rs`'s point iterator unwraps the entry point (`hnsw.rs:662`), which
    // is `None` until the first insert — iterating an empty index panics.
    if index.get_nb_point() == 0 {
        return Vec::new();
    }
    let dist = index.get_distance();
    let mut best: HashMap<u64, f32> = HashMap::new();
    for point in index.get_point_indexation() {
        let d = dist.eval(query, point.get_v());
        let id = point.get_origin_id() as u64;
        best.entry(id)
            .and_modify(|cur| {
                if d < *cur {
                    *cur = d;
                }
            })
            .or_insert(d);
    }

    let mut ranked: Vec<(u64, f32)> = best.into_iter().collect();
    // Ties broken by id so a rebuilt graph ranks an unchanged collection
    // identically — `f32::total_cmp` alone leaves equal distances in HashMap
    // iteration order, which is not stable across runs.
    ranked.sort_by(|a, b| a.1.total_cmp(&b.1).then(a.0.cmp(&b.0)));
    ranked.truncate(want);
    ranked
}
