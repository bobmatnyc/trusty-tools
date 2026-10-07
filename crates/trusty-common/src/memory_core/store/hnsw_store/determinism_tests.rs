//! #9280: two opens of one palace rank every query identically at or below
//! the exact-scan threshold, ties included.
//!
//! Why: the parallel replay (#9141) gives each open different neighbour
//! lists, so above [`exhaustive::EXHAUSTIVE_SCAN_MAX_POINTS`] the graph arm
//! can return a different candidate set, and equal distances came back in
//! whatever order `hnsw_rs` and the stranded-group `HashMap` produced.
//! What: a reopen test at the threshold (clustered vectors and bit-identical
//! copies), plus unit tests that pin the id tiebreak at the graph arm and the
//! stranded merge.
//! Test: this file is the test.

use super::latency_profile_tests::clustered_vec;
use super::*;

/// `(uuid, distance bits)` per hit, so a reorder or a changed score both show.
fn bits(hits: &[(String, f32)]) -> Vec<(String, u32)> {
    hits.iter().map(|(u, d)| (u.clone(), d.to_bits())).collect()
}

/// Why (#9280): the issue's acceptance criterion — two reopenings give the
/// same order for the same query under a parallel rebuild — at the largest
/// palace the exact scan answers. Bit-identical vectors tie exactly, and no
/// graph can rank a tie; only an id tiebreak over the full candidate set can.
/// Real palaces hold such clumps (1,721 identical "say hi" turns in one, see
/// `stranded`).
/// What: a clustered 32-dim palace of exactly the threshold count in which
/// every 50th row is a copy of one anchor vector, opened three times. On every
/// open, 30 clustered queries return the same top 10 (ids, order, distance
/// bits) as the first open, and the anchor query returns the ten lowest-id
/// copies in id order.
/// Test: this test.
#[test]
fn reopening_a_palace_at_the_exact_scan_threshold_ranks_identically() {
    let dim = 32;
    let n = exhaustive::EXHAUSTIVE_SCAN_MAX_POINTS;
    let anchor = spread_vec(dim, 31_337);
    let is_copy = |i: usize| i % 50 == 7;
    let pool: Vec<Vec<f32>> = (0..n)
        .map(|i| {
            if is_copy(i) {
                anchor.clone()
            } else {
                clustered_vec(i as u64)[..dim].to_vec()
            }
        })
        .collect();
    let lowest_copies: Vec<String> = (0..n)
        .filter(|i| is_copy(*i))
        .take(10)
        .map(|i| format!("drawer-{i:05}"))
        .collect();
    let queries: Vec<Vec<f32>> = (0..30u64)
        .map(|q| clustered_vec(8_000_000 + q)[..dim].to_vec())
        .collect();
    let (_dir, store) = open_store(dim);
    seed_rows(&store.db, &pool);

    let mut first: Option<Vec<Vec<(String, u32)>>> = None;
    for open in 1..=3 {
        let reopened = HnswStore::open(Arc::clone(&store.db), dim).expect("reopen");
        let ties: Vec<String> = reopened
            .search(&anchor, 10)
            .expect("search")
            .into_iter()
            .map(|(u, _)| u)
            .collect();
        assert_eq!(
            ties, lowest_copies,
            "open {open}: tied copies must rank as the ten lowest ids"
        );
        let answers: Vec<Vec<(String, u32)>> = queries
            .iter()
            .map(|q| bits(&reopened.search(q, 10).expect("search")))
            .collect();
        match &first {
            None => first = Some(answers),
            Some(first) => {
                for (q, (a, b)) in first.iter().zip(&answers).enumerate() {
                    assert_eq!(a.len(), 10, "query {q}: expected a full top 10");
                    assert_eq!(a, b, "open {open}, query {q}: two opens ranked differently");
                }
            }
        }
    }
}

/// Why (#9280): `hnsw_rs` returns equal distances in heap order, which differs
/// between graphs built from the same rows.
/// What: 60 copies of one vector inserted under shuffled ids; `graph_nearest`
/// for that vector must return its hits ordered by (distance, id).
/// Test: this test.
#[test]
fn graph_nearest_orders_equal_distances_by_id() {
    let dim = 16;
    let index = replay::new_index();
    let anchor = spread_vec(dim, 4_242);
    for i in 0..300usize {
        let v = if i % 5 == 0 {
            anchor.clone()
        } else {
            spread_vec(dim, 70_000 + i as u64)
        };
        // A stride coprime to 300 scatters ids against insertion order.
        index.insert((v.as_slice(), (i * 7) % 300));
    }
    let hits = graph_arm::graph_nearest(&index, &anchor, &Default::default(), 30, 300);
    assert!(
        hits.windows(2)
            .all(|w| w[0].1.total_cmp(&w[1].1).then(w[0].0.cmp(&w[1].0)).is_lt()),
        "graph hits must be ordered by (distance, id): {hits:?}"
    );
}

/// Why (#9280): stranded points were merged with a distance-only sort, so
/// tied stranded ids kept the group order a `HashMap` produced at open.
/// What: two stranded groups at the same distance from the query, listed with
/// their ids out of order, merged into graph hits at that distance too. The
/// merged list must be ordered by (distance, id).
/// Test: this test.
#[test]
fn merge_stranded_orders_equal_distances_by_id() {
    let v = planar_vec(4, 0.0);
    let mirrored = vec![v[0], -v[1], v[2], v[3]];
    let groups: Vec<stranded::StrandedGroup> = vec![(v.clone(), vec![9, 3]), (mirrored, vec![7])];
    let query = planar_vec(4, 0.0);
    let mut hits = vec![(5u64, 0.0f32)];
    stranded::merge_stranded(&mut hits, &groups, &query, &Default::default());
    let ids: Vec<u64> = hits.iter().map(|(id, _)| *id).collect();
    assert_eq!(ids, vec![3, 5, 7, 9], "ties must rank by id: {hits:?}");
}

/// Why (#9280): the open-time stranded scan grouped points through a
/// `HashMap`, so group order, and id order within a group, varied per open.
/// What: a graph whose points are all copies of two vectors (heavy stranding,
/// see `stranded`); the scan's groups must come back sorted, ids ascending
/// within each group and groups by their first id.
/// Test: this test.
#[test]
fn stranded_points_are_grouped_in_id_order() {
    let dim = 8;
    let index = replay::new_index();
    let (a, b) = (spread_vec(dim, 1), spread_vec(dim, 2));
    let live: Vec<(Vec<f32>, usize)> = (0..400usize)
        .map(|i| {
            (
                if i % 2 == 0 { a.clone() } else { b.clone() },
                (i * 7) % 400,
            )
        })
        .collect();
    replay::replay(&index, &live).expect("replay");
    let groups = stranded::stranded_points(&index);
    for (_, ids) in &groups {
        assert!(
            ids.windows(2).all(|w| w[0] < w[1]),
            "ids out of order: {ids:?}"
        );
    }
    assert!(
        groups.windows(2).all(|w| w[0].1[0] < w[1].1[0]),
        "groups out of order: {:?}",
        groups.iter().map(|g| g.1[0]).collect::<Vec<_>>()
    );
}
