//! #10682: the open-time replay can no longer deadlock inside `hnsw_rs`.
//!
//! Why: `hnsw_rs` 0.3.4 `reverse_update_neighborhood_simple` holds one
//! point's neighbour read lock while it takes another's write lock, so two
//! parallel inserts that are mutual neighbours can block each other forever.
//! What: one test pins that the replay is serial (a parallel build's
//! neighbour lists depend on thread scheduling, a serial one's never do); the
//! other replays dense mutual-neighbour clusters on a many-thread pool under a
//! hard timeout, so a hang fails the test instead of stalling the suite.
//! Test: this file is the test.

use super::*;

/// Layer-0 neighbour origin ids of every point, keyed by origin id.
fn neighbour_lists(index: &Hnsw<'static, f32, DistCosine>) -> Vec<(usize, Vec<usize>)> {
    let mut out: Vec<(usize, Vec<usize>)> = index
        .get_point_indexation()
        .into_iter()
        .map(|p| {
            let mut n: Vec<usize> = p.get_neighborhood_id()[0]
                .iter()
                .map(|nb| nb.get_origin_id())
                .collect();
            n.sort_unstable();
            (p.get_origin_id(), n)
        })
        .collect();
    out.sort_unstable();
    out
}

/// Rows in clusters of near-duplicate vectors, so every point's nearest
/// neighbours are its cluster mates and each is a neighbour of the others.
fn mutual_neighbour_rows(dim: usize, clusters: usize, per: usize) -> Vec<(Vec<f32>, usize)> {
    let mut rows = Vec::with_capacity(clusters * per);
    for c in 0..clusters {
        let base = spread_vec(dim, 91_000 + c as u64);
        for j in 0..per {
            let mut v = base.clone();
            v[j % dim] += 1e-4 * (j as f32 + 1.0);
            rows.push((v, c * per + j));
        }
    }
    rows
}

/// Run `replay` on a pool of `threads`, failing if it does not finish in
/// `bound` (#10682: a deadlocked replay never returns).
fn replay_within(rows: Vec<(Vec<f32>, usize)>, threads: usize, bound: std::time::Duration) {
    let (done, finished) = std::sync::mpsc::channel();
    // Detached on purpose: a deadlocked replay thread cannot be joined.
    std::thread::spawn(move || {
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .build()
            .expect("replay pool");
        let index = replay::new_index();
        let result = pool.install(|| replay::replay(&index, &rows));
        let _ = done.send(result.map(|()| index.get_nb_point()));
    });
    match finished.recv_timeout(bound) {
        Ok(Ok(_)) => {}
        Ok(Err(e)) => panic!("replay failed: {e}"),
        Err(_) => panic!("#10682: replay did not finish within {bound:?}; deadlocked"),
    }
}

/// Why (#10682): a serial replay is the fix; a parallel one is the bug.
/// What: two replays of the same 3,000 rows on an 8-thread pool must produce
/// identical layer-0 neighbour lists. A parallel `hnsw_rs` build reorders its
/// inserts by thread scheduling, so its lists differ between builds.
/// Test: this test.
#[test]
fn replay_builds_the_same_graph_every_time() {
    let dim = 16;
    let rows: Vec<(Vec<f32>, usize)> = (0..3_000)
        .map(|i| (spread_vec(dim, 77_000 + i as u64), i))
        .collect();
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(8)
        .build()
        .expect("pool");
    let build = || {
        let index = replay::new_index();
        pool.install(|| replay::replay(&index, &rows))
            .expect("replay");
        neighbour_lists(&index)
    };
    let (a, b) = (build(), build());
    assert_eq!(a.len(), 3_000);
    assert!(
        a == b,
        "two replays of the same rows built different graphs"
    );
}

/// Why (#10682): mutual-neighbour inserts are the deadlocking shape.
/// What: 20 replays of 200 clusters of 16 near-duplicate points, on a
/// 16-thread pool, each bounded at 60 s. The deadlock needs two inserts to
/// interleave inside `hnsw_rs` within a few instructions, so the pre-fix
/// parallel replay is not guaranteed to hang here; this is the strongest
/// bounded check, and the determinism test above pins the serial replay.
/// Test: this test.
#[test]
fn replay_of_mutual_neighbours_finishes_within_bound() {
    for _ in 0..20 {
        replay_within(
            mutual_neighbour_rows(16, 200, 16),
            16,
            std::time::Duration::from_secs(60),
        );
    }
}
