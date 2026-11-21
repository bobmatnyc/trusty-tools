//! #10682: the parallel open-time replay can no longer deadlock in `hnsw_rs`.
//!
//! Why: `hnsw_rs` 0.3.4 `reverse_update_neighborhood_simple` held one point's
//! neighbour read lock while it took another's write lock, so two parallel
//! inserts that are mutual neighbours could block each other forever. The
//! workspace patches it (`vendor/hnsw_rs/README.trusty-tools.md`); the replay stays
//! parallel (#9141).
//! What: replays dense mutual-neighbour clusters on a many-thread pool under a
//! hard timeout, so a hang fails the test instead of stalling the suite, and
//! checks every row is inserted with neighbours. A parallel build's graph
//! depends on thread scheduling, so no test compares graphs across runs.
//! Test: this file is the test.

use super::*;

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

/// Run `replay` on a pool of `threads`; fail if it does not finish in `bound`
/// (#10682: a deadlocked replay never returns), or if any row is missing or
/// has no layer-0 neighbours.
fn replay_within(rows: Vec<(Vec<f32>, usize)>, threads: usize, bound: std::time::Duration) {
    let n = rows.len();
    let (done, finished) = std::sync::mpsc::channel();
    // Detached on purpose: a deadlocked replay thread cannot be joined.
    std::thread::spawn(move || {
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .build()
            .expect("replay pool");
        let index = replay::new_index();
        let result = pool.install(|| replay::replay(&index, &rows));
        let _ = done.send(result.map(|()| {
            (
                index.get_nb_point(),
                points_without_neighbours(&index).len(),
            )
        }));
    });
    match finished.recv_timeout(bound) {
        Ok(Ok((points, orphans))) => {
            assert_eq!(points, n, "replay inserted {points} of {n} rows");
            assert_eq!(orphans, 0, "{orphans} points have no layer-0 neighbours");
        }
        Ok(Err(e)) => panic!("replay failed: {e}"),
        Err(_) => panic!("#10682: replay did not finish within {bound:?}; deadlocked"),
    }
}

/// Why (#10682): mutual-neighbour inserts are the deadlocking shape.
/// What: 20 parallel replays of 200 clusters of 16 near-duplicate points, on
/// a 16-thread pool, each bounded at 60 s, each checked for full insertion
/// and no orphan points.
/// Test: this test.
#[test]
fn parallel_replay_of_mutual_neighbours_finishes_within_bound() {
    for _ in 0..20 {
        replay_within(
            mutual_neighbour_rows(16, 200, 16),
            16,
            std::time::Duration::from_secs(60),
        );
    }
}

/// Why (#10682): option (b) keeps the parallel replay for its open speed;
/// this measures that against a serial insert of the same rows.
/// What: prints wall time of `replay` on a 16-thread pool and of a serial
/// `insert_slice` loop over 20,000 rows. Opt-in: run with `--ignored`.
/// Test: this test.
#[test]
#[ignore = "timing probe; run on demand"]
fn replay_parallel_vs_serial_timing() {
    let dim = 384;
    let rows: Vec<(Vec<f32>, usize)> = (0..20_000)
        .map(|i| (spread_vec(dim, 33_000 + i as u64), i))
        .collect();
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(16)
        .build()
        .expect("pool");
    let t = std::time::Instant::now();
    let par = replay::new_index();
    pool.install(|| replay::replay(&par, &rows))
        .expect("replay");
    let par_s = t.elapsed();
    let t = std::time::Instant::now();
    let ser = replay::new_index();
    for (v, id) in &rows {
        ser.insert_slice((v.as_slice(), *id));
    }
    let ser_s = t.elapsed();
    assert_eq!(par.get_nb_point(), rows.len());
    assert_eq!(ser.get_nb_point(), rows.len());
    eprintln!("#10682 timing: parallel {par_s:?}, serial {ser_s:?}");
}
