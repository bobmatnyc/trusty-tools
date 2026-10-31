//! Issue #9450 — the post-rebuild reachability sweep, proven on forced strandings.
//!
//! Why: the sweep only matters when a node is stranded, which a parallel
//! rebuild does rarely and at random. These tests strand nodes on purpose, with
//! the mechanism #9450 names (usearch `remove` unlinks a node and repairs none
//! of its neighbours' links), on a single-threaded build whose layout is fixed
//! by the seed, so the sweep is proven without waiting for a lucky run.
//! What: a seeded index loses most of its keys; the survivors that no longer
//! find themselves are the forced strandings. `relink_stranded` must leave none.
//! Test: this module. Run with `cargo test -p trusty-search --lib relink_9450`.

use std::time::Instant;

use rand::rngs::StdRng;
use rand::seq::SliceRandom;
use rand::{Rng, SeedableRng};
use usearch::{Index, ScalarKind};

use super::clustered_vectors::{clustered_points, Blobs};
use super::usearch_relink::{find_stranded, relink_stranded};
use super::usearch_requant::new_rebuild_index;

const BLOBS: Blobs = Blobs {
    dim: 32,
    clusters: 30,
    sigma: 0.6,
};

/// Fraction of the corpus removed, in rounds, and the replacements per removal.
const REMOVE_PCT: usize = 70;
const ROUNDS: usize = 10;
const REPLACEMENTS: usize = 3;

/// An f16 index of `n` keys added in order on one thread, then churned the way
/// #9450 describes: `REMOVE_PCT` percent of the keys removed over `ROUNDS`
/// rounds, each removal followed by `REPLACEMENTS` remove-and-re-add swaps of
/// random survivors (which reuse the freed slots). usearch repairs none of the
/// links that pointed at a removed node. Returns the index, the vector each
/// key now holds, and the surviving keys.
fn damaged_index(n: usize, seed: u64) -> (Index, Vec<Vec<f32>>, Vec<u64>) {
    let pool = clustered_points(n * (2 + REPLACEMENTS * 2), BLOBS, seed);
    let mut vectors: Vec<Vec<f32>> = pool[..n].to_vec();
    let mut fresh = pool[n..].iter();
    let index = new_rebuild_index(BLOBS.dim, ScalarKind::F16, n, "relink test").expect("index");
    for (key, v) in vectors.iter().enumerate() {
        index.add(key as u64, v).expect("add");
    }
    let mut rng = StdRng::seed_from_u64(seed ^ 0x9450);
    let mut live: Vec<u64> = (0..n as u64).collect();
    live.shuffle(&mut rng);
    let doomed_total = n * REMOVE_PCT / 100;
    let mut doomed: Vec<u64> = live.drain(..doomed_total).collect();
    let per_round = doomed_total.div_ceil(ROUNDS);
    while !doomed.is_empty() {
        for key in doomed.drain(..per_round.min(doomed.len())) {
            index.remove(key).expect("remove");
            for _ in 0..REPLACEMENTS {
                let target = live[rng.gen_range(0..live.len())];
                let v = fresh.next().expect("fresh vector");
                index.remove(target).expect("remove for swap");
                index.add(target, v).expect("re-add");
                vectors[target as usize] = v.clone();
            }
        }
    }
    live.sort_unstable();
    (index, vectors, live)
}

/// #9450: a damaged index has survivors that cannot find themselves; the sweep
/// re-links every one, loses no key, and a second sweep finds nothing.
/// Without the sweep the `before` precondition holds and the `after`
/// assertion fails, which is the pre-fix behaviour.
#[test]
fn the_sweep_relinks_forced_strandings() {
    let (index, vectors, survivors) = damaged_index(20_000, 9450);
    let read = |i: usize, buf: &mut [f32]| {
        buf.copy_from_slice(&vectors[survivors[i] as usize]);
        Ok(Some(survivors[i]))
    };
    let n = survivors.len();
    let before = find_stranded(&index, n, BLOBS.dim, 4, &read).expect("find");
    eprintln!(
        "#9450: {} of {n} survivors stranded before the sweep",
        before.len()
    );
    assert!(
        !before.is_empty(),
        "the damage must strand at least one node"
    );

    let relinked = relink_stranded(&index, n, BLOBS.dim, 4, "relink test", &read).expect("relink");
    let after = find_stranded(&index, n, BLOBS.dim, 4, &read).expect("find");
    assert!(
        after.is_empty(),
        "{} of {n} survivors still stranded after {relinked} re-links: {:?}",
        after.len(),
        &after[..after.len().min(10)]
    );
    assert!(relinked >= before.len());
    assert_eq!(index.size(), n, "re-linking must not add or lose a key");
}

/// #9450: sweep cost, printed for the PR. Run with `--ignored --nocapture`.
#[test]
#[ignore = "timing probe, run by hand"]
fn sweep_cost_on_a_clean_index() {
    for n in [20_000usize, 100_000] {
        let vectors = clustered_points(n, BLOBS, 1);
        let index = new_rebuild_index(BLOBS.dim, ScalarKind::F16, n, "cost").expect("index");
        for (key, v) in vectors.iter().enumerate() {
            index.add(key as u64, v).expect("add");
        }
        let read = |i: usize, buf: &mut [f32]| {
            buf.copy_from_slice(&vectors[i]);
            Ok(Some(i as u64))
        };
        let started = Instant::now();
        let stranded = find_stranded(&index, n, BLOBS.dim, 4, &read).expect("find");
        eprintln!(
            "#9450 cost: n={n} stranded={} sweep={} ms (4 threads)",
            stranded.len(),
            started.elapsed().as_millis()
        );
    }
}
