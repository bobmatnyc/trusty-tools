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
//! every point the graph holds and keeps one entry per `vector_id`, so its
//! distances are identical to the ones the traversal would have reported for
//! the same points. [`resolve_shadowed`] then re-scores any drawer that has
//! more than one embedding in the graph against the one `VECTORS` holds. The
//! scan deliberately hands back its FULL ranking: a shadowed drawer's
//! provisional distance is optimistic, so any cut taken before re-scoring can
//! drop a true neighbour — see [`exhaustive_nearest`].
//! Test: `exhaustive_scan_returns_every_point_the_graph_holds`,
//! `search_returns_the_exact_top_k_below_the_exhaustive_threshold`,
//! `search_scores_a_re_upserted_drawer_by_its_current_vector`.

use std::collections::{HashMap, HashSet};

use hnsw_rs::prelude::{DistCosine, Distance, Hnsw};
use redb::ReadableTable;

/// Live-drawer ceiling at or below which [`super::HnswStore::search`] scans
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
/// What: compared against the number of LIVE drawers — `VECTOR_KEYS` rows, the
/// `reverse` map `search` builds two statements earlier. Not
/// `Hnsw::get_nb_point()`, which also counts tombstoned points and re-upsert
/// shadows: those accumulate within a session, so a palace of 200 real drawers
/// could drift past 256 graph points mid-session and silently revert to the
/// path this module exists to replace. Scan cost does follow the graph count,
/// but the overhang is bounded by churn since the last open — one extra point
/// per delete or re-upsert, and `HnswStore::open` rebuilds one point per live
/// vector — so a bulk `palace_reembed` at most doubles it.
/// Test: `search_uses_the_graph_above_the_exhaustive_threshold`,
/// `deleting_drawers_does_not_push_a_small_palace_off_the_exhaustive_path`.
pub(super) const EXHAUSTIVE_SCAN_MAX_POINTS: usize = 256;

/// Every live point in `index`, ranked by exact distance to `query`, best
/// first — the whole ranking, not a prefix of it.
///
/// Why: see the module header — below [`EXHAUSTIVE_SCAN_MAX_POINTS`] the graph
/// traversal can silently omit a true nearest neighbour, and a full scan cannot.
/// What: walks the point indexation (which visits layer 0 upward and yields
/// every stored point exactly once), skips `tombstoned` ids, evaluates the
/// index's own distance function, keeps one entry per `vector_id` so a
/// re-upsert's shadow copy cannot occupy two result slots, and returns every
/// surviving `(vector_id, distance)` pair sorted ascending by distance.
///
/// Tombstones are excluded here rather than by the caller because `delete`
/// never removes a point from the graph, so a palace that has deleted most of
/// its drawers would otherwise spend a distance evaluation on every dead point
/// and rank it against live ones.
///
/// Why this returns everything rather than a `want`-sized prefix (#5171): the
/// distance held for a shadowed id is PROVISIONAL — it is whichever of the
/// drawer's copies is nearer, and `min` is optimistic. Cutting the list on that
/// provisional key lets a drawer whose SUPERSEDED vector sits near the query
/// evict a drawer whose CURRENT vector is a true neighbour, before
/// [`resolve_shadowed`] can correct either score. That is the same
/// missing-true-neighbour defect #5171 exists to close, reached through
/// re-embedding instead of through graph reachability, and `palace_reembed`
/// marks every drawer shadowed. Any bounding of the result must therefore
/// happen AFTER re-scoring; [`super::HnswStore::search`] does it with the
/// `out.len() >= k` break on the already-corrected ranking. Cost is bounded by
/// the gate that selects this path: at most one entry per live drawer, so at
/// most [`EXHAUSTIVE_SCAN_MAX_POINTS`] plus whatever dead-but-untombstoned
/// overhang the graph carries.
/// Test: `exhaustive_scan_returns_every_point_the_graph_holds`,
/// `search_scores_a_re_upserted_drawer_by_its_current_vector`,
/// `search_keeps_a_true_neighbour_a_shadowed_decoy_would_evict`.
pub(super) fn exhaustive_nearest(
    index: &Hnsw<'static, f32, DistCosine>,
    query: &[f32],
    tombstoned: &HashSet<u64>,
) -> Vec<(u64, f32)> {
    // `hnsw_rs`'s point iterator unwraps the entry point (`hnsw.rs:662`), which
    // is `None` until the first insert — iterating an empty index panics.
    if index.get_nb_point() == 0 {
        return Vec::new();
    }
    let dist = index.get_distance();
    let mut best: HashMap<u64, f32> = HashMap::new();
    for point in index.get_point_indexation() {
        let id = point.get_origin_id() as u64;
        if tombstoned.contains(&id) {
            continue;
        }
        let d = dist.eval(query, point.get_v());
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
    ranked
}

/// Re-score every candidate whose `vector_id` has more than one point in the
/// graph, against the vector `VECTORS` currently holds for it.
///
/// Why (#5171): `upsert` on an already-mapped uuid inserts a second point under
/// the same `vector_id` — `hnsw_rs` cannot remove the old one — so until the
/// next `HnswStore::open` rebuilds the graph, one drawer has two embeddings in
/// it. Ranking that drawer by whichever copy is nearer means a query against
/// text the drawer NO LONGER HOLDS scores it as an exact match:
/// `VectorStore::search` maps distance 0.0 to score 1.0, so a re-embedded
/// drawer would clear a 0.99 near-duplicate threshold against superseded
/// content. `palace_reembed` creates these in bulk, so it is not a rare state.
/// The `VECTORS` row is the single authority on what a drawer currently holds,
/// and it is the only thing the caller may be scored against.
/// What: for each candidate id in `shadowed`, decodes its `VECTORS` row and
/// replaces the provisional distance with `DistCosine` against that vector; a
/// candidate whose row has gone (compacted between the search and this read)
/// is dropped rather than reported at a distance nothing backs. Re-sorts, since
/// re-scoring can reorder. Costs one point read per shadowed candidate and
/// nothing at all when no re-upsert has happened since the palace was opened,
/// which is the steady state. On the scan path `candidates` is the full live
/// ranking rather than a small over-fetch, because a cut taken before this
/// function runs is taken on the wrong key (see [`exhaustive_nearest`]); after
/// a bulk `palace_reembed` that is one point read per live drawer, bounded by
/// [`EXHAUSTIVE_SCAN_MAX_POINTS`], on a path that already evaluates a distance
/// against every point.
/// Test: `search_scores_a_re_upserted_drawer_by_its_current_vector`,
/// `search_drops_a_shadowed_candidate_whose_vector_row_is_gone`,
/// `search_keeps_a_true_neighbour_a_shadowed_decoy_would_evict`.
pub(super) fn resolve_shadowed<T: ReadableTable<u64, &'static [u8]>>(
    candidates: &mut Vec<(u64, f32)>,
    shadowed: &HashSet<u64>,
    query: &[f32],
    vectors: &T,
) -> super::Result<()> {
    let mut touched = false;
    for slot in candidates.iter_mut() {
        if !shadowed.contains(&slot.0) {
            continue;
        }
        touched = true;
        match vectors.get(slot.0)? {
            Some(row) => {
                let current: Vec<f32> = postcard::from_bytes(row.value())?;
                slot.1 = DistCosine.eval(query, &current);
            }
            // NaN marks the slot for removal below. `DistCosine::eval` clamps
            // at 0 and never returns NaN, so it cannot collide with a score.
            None => slot.1 = f32::NAN,
        }
    }
    if !touched {
        return Ok(());
    }
    candidates.retain(|(_, d)| !d.is_nan());
    candidates.sort_by(|a, b| a.1.total_cmp(&b.1).then(a.0.cmp(&b.0)));
    Ok(())
}
