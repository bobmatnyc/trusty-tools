//! Graph points a search cannot find, and the exact scan that answers for
//! them (#9174).
//!
//! Why: above [`super::EXHAUSTIVE_SCAN_MAX_POINTS`] the vector lane missed live
//! drawers queried with their own vector. Measured on a copy of the
//! trusty-tools palace (6,931 live drawers, 384-dim), with every live drawer
//! queried by its own vector; a "tie" is a drawer whose exact vector more than
//! 10 drawers share, which no top 10 can rank:
//!
//! | graph build | 1 | 2 |
//! |---|---|---|
//! | points no traversal from the entry point reaches | 1,433 | 1,529 |
//! | non-tie drawers missing from their own top 10 | 8 | 7 |
//! | ...of which unreachable | 8 | 7 |
//!
//! A search with `ef` equal to the point count did not find those either, so
//! neither the `ef` budget nor the exhaustive threshold is the cause.
//! `hnsw_rs` 0.3.4 drops a candidate neighbour that is no farther from an
//! already-chosen neighbour than from the new point (`hnsw.rs:1361-1363`), and
//! a full neighbour list evicts its farthest entry when a reverse edge arrives
//! (`hnsw.rs:1260-1271`). A point next to many copies of one vector — that
//! palace holds 1,721 identical "say hi" turns — keeps one out-edge, and loses
//! its only in-edge when that copy's list fills with copies at distance 0.
//! Building one point per distinct vector still stranded 3–5 points.
//!
//! Scanning only the unreachable points then left 0, 1 and 1 misses in three
//! builds. Each was reachable: an outlier whose nearest other drawer sat at
//! cosine distance 0.34–0.49, which `ef = 1024` missed and `ef` = point count
//! found. So the test of a stranded point is the search itself, not the walk.
//! What: [`stranded_points`] searches for every point's own vector after a
//! replay; [`newest_is_reachable`] does the same for each upserted point;
//! [`merge_stranded`] scores the points those searches missed exactly, on
//! every graph-arm query. Points are grouped by vector, so the scan costs one
//! distance per distinct vector; most were copies of a few vectors. A point
//! that a later upsert strands, by evicting its last in-edge, stays unscanned
//! until the next open.
//! Test: `search_finds_drawers_the_graph_cannot_reach`.

use std::collections::{HashMap, HashSet};

use hnsw_rs::prelude::{DistCosine, Distance, Hnsw};

/// Stranded points that share one vector, as `(vector, vector_ids)`.
pub(super) type StrandedGroup = (Vec<f32>, Vec<u64>);

/// `ef` of the search that decides whether a point is stranded.
///
/// Why (#9174): a point this search misses is scanned exactly, which costs one
/// distance per query; a point it finds is reachable by the far wider
/// `graph_arm` search. A false alarm is therefore cheap and a narrow search
/// is safe.
const SELF_SEARCH_EF: usize = 64;

/// Points per `parallel_search` batch, bounding the vector copies it needs.
const SELF_SEARCH_BATCH: usize = 4096;

/// Every point that a search for its own vector does not return, grouped by
/// vector.
///
/// Why (#9174): see the module header.
/// What: copies the layer-0 points' vectors in batches, runs `hnsw_rs`'s
/// `parallel_search` for each with `k = 1` and [`SELF_SEARCH_EF`], and groups
/// the points missing from their own answer by the bits of their vector. Ids
/// ascend within a group, and groups are ordered by their first id.
/// Test: `search_finds_drawers_the_graph_cannot_reach`,
/// `stranded_points_are_grouped_in_id_order`.
pub(super) fn stranded_points(index: &Hnsw<'static, f32, DistCosine>) -> Vec<StrandedGroup> {
    let points: Vec<_> = index.get_point_indexation().get_layer_iterator(0).collect();
    let mut groups: HashMap<Vec<u32>, StrandedGroup> = HashMap::new();
    for batch in points.chunks(SELF_SEARCH_BATCH) {
        let vectors: Vec<Vec<f32>> = batch.iter().map(|p| p.get_v().to_vec()).collect();
        let answers = index.parallel_search(&vectors, 1, SELF_SEARCH_EF);
        for ((point, vector), answer) in batch.iter().zip(vectors).zip(answers) {
            let id = point.get_origin_id();
            if answer.iter().any(|hit| hit.d_id == id) {
                continue;
            }
            groups
                .entry(vector.iter().map(|x| x.to_bits()).collect())
                .or_insert_with(|| (vector, Vec::new()))
                .1
                .push(id as u64);
        }
    }
    // #9280: `HashMap` order and the parallel replay's point order both vary
    // per open; sort so the stranded set is the same structure every time.
    let mut groups: Vec<StrandedGroup> = groups.into_values().collect();
    for (_, ids) in &mut groups {
        ids.sort_unstable();
    }
    groups.sort_unstable_by_key(|(_, ids)| ids.first().copied());
    groups
}

/// Whether a search for the vector just inserted under `id` returns it.
///
/// Why (#9174): the test [`stranded_points`] applies at open, for one point.
/// What: `Hnsw::search(vector, 1, SELF_SEARCH_EF)`.
/// Test: `search_finds_drawers_the_graph_cannot_reach`.
pub(super) fn newest_is_reachable(
    index: &Hnsw<'static, f32, DistCosine>,
    vector: &[f32],
    id: u64,
) -> bool {
    index
        .search(vector, 1, SELF_SEARCH_EF)
        .iter()
        .any(|hit| hit.d_id as u64 == id)
}

/// Record `id` as stranded, in the group of its vector if one exists.
/// Test: `search_finds_drawers_the_graph_cannot_reach`.
pub(super) fn add_stranded(groups: &mut Vec<StrandedGroup>, vector: &[f32], id: u64) {
    let bits = |v: &[f32]| v.iter().map(|x| x.to_bits()).collect::<Vec<u32>>();
    let key = bits(vector);
    match groups.iter_mut().find(|g| bits(&g.0) == key) {
        Some(group) => group.1.push(id),
        None => groups.push((vector.to_vec(), vec![id])),
    }
}

/// Add every live stranded point to a graph arm's candidates, scored exactly.
///
/// Why (#9174): the graph search does not return these points.
/// What: one distance per group, then `(vector_id, distance)` for each id not
/// in `tombstoned`; re-sorts `hits` by ascending distance, then id (#9280), so
/// equal distances rank the same whatever order the groups were found in.
/// Test: `search_finds_drawers_the_graph_cannot_reach`,
/// `merge_stranded_orders_equal_distances_by_id`.
pub(super) fn merge_stranded(
    hits: &mut Vec<(u64, f32)>,
    stranded: &[StrandedGroup],
    query: &[f32],
    tombstoned: &HashSet<u64>,
) {
    if stranded.is_empty() {
        return;
    }
    for (vector, ids) in stranded {
        let distance = DistCosine.eval(query, vector);
        hits.extend(
            ids.iter()
                .filter(|id| !tombstoned.contains(id))
                .map(|id| (*id, distance)),
        );
    }
    // #9280: id tiebreak — a total order over the merged candidates.
    hits.sort_by(|a, b| a.1.total_cmp(&b.1).then(a.0.cmp(&b.0)));
}
