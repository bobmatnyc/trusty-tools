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

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::Instant;

use rand::rngs::StdRng;
use rand::seq::SliceRandom;
use rand::{Rng, SeedableRng};
use usearch::{Index, ScalarKind};

use super::clustered_vectors::{clustered_points, Blobs};
use super::usearch_relink::{find_stranded, relink_stranded};
use super::usearch_requant::{add_parallel, new_rebuild_index, no_fault};

const BLOBS: Blobs = Blobs {
    dim: 32,
    clusters: 30,
    sigma: 0.6,
};

/// Fraction of the corpus removed, in rounds, and the replacements per removal.
const REMOVE_PCT: usize = 70;
const ROUNDS: usize = 10;
const REPLACEMENTS: usize = 3;

/// An empty f16 index sized for `n` keys, and the vector pool: the first `n`
/// are the keys' starting vectors, the rest feed the swaps.
fn fresh_index(n: usize, seed: u64) -> (Index, Vec<Vec<f32>>) {
    let pool = clustered_points(n * (2 + REPLACEMENTS * 2), BLOBS, seed);
    let index = new_rebuild_index(BLOBS.dim, ScalarKind::F16, n, "relink test").expect("index");
    (index, pool)
}

/// Churn keys `0..m` of `index` the way #9450 describes: `REMOVE_PCT` percent
/// removed over `ROUNDS` rounds, each removal followed by `REPLACEMENTS`
/// remove-and-re-add swaps of random survivors (which reuse the freed slots).
/// usearch repairs none of the links that pointed at a removed node.
/// `vectors` holds each key's current vector and `fresh` feeds the swaps.
/// Returns the surviving and the removed keys, both sorted.
fn churn(
    index: &Index,
    vectors: &mut [Vec<f32>],
    fresh: &[Vec<f32>],
    m: usize,
    seed: u64,
) -> (Vec<u64>, Vec<u64>) {
    let mut fresh = fresh.iter();
    let mut rng = StdRng::seed_from_u64(seed ^ 0x9450);
    let mut live: Vec<u64> = (0..m as u64).collect();
    live.shuffle(&mut rng);
    let doomed_total = m * REMOVE_PCT / 100;
    let mut doomed: Vec<u64> = live.drain(..doomed_total).collect();
    let mut removed = doomed.clone();
    removed.sort_unstable();
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
    (live, removed)
}

/// An f16 index of `n` keys added in order on one thread, then churned.
/// Returns the index, the vector each key now holds, and the surviving keys.
fn damaged_index(n: usize, seed: u64) -> (Index, Vec<Vec<f32>>, Vec<u64>) {
    let (index, pool) = fresh_index(n, seed);
    let mut vectors: Vec<Vec<f32>> = pool[..n].to_vec();
    for (key, v) in vectors.iter().enumerate() {
        index.add(key as u64, v).expect("add");
    }
    let (live, _) = churn(&index, &mut vectors, &pool[n..], n, seed);
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

/// Keys in the wiring test: under 10K, so `add_parallel` adds on one thread
/// and the churn below lands at a fixed point of the build.
const WIRING_KEYS: usize = 8_000;

/// #9450 wiring: `add_parallel` itself sweeps. The source's read hook, called
/// for the last item just before its add, churns the index built so far; the
/// churn strands survivors (counted in the hook), and the sweep inside
/// `add_parallel` must leave none. With the sweep call removed the final
/// assertion fails, which is how the call is proven wired.
#[test]
fn add_parallel_sweeps_what_the_build_strands() {
    let n = WIRING_KEYS;
    let (index, pool) = fresh_index(n, 9450);
    let vectors = Mutex::new(pool[..n].to_vec());
    let removed: Mutex<HashSet<u64>> = Mutex::new(HashSet::new());
    let churned = AtomicBool::new(false);
    let stranded_by_churn = AtomicUsize::new(0);
    let read = |i: usize, buf: &mut [f32]| {
        if i == n - 1 && !churned.swap(true, Ordering::AcqRel) {
            // Churn keys 0..n-1; the last key is added after the damage.
            let mut v = vectors.lock().expect("vectors");
            let (live, gone) = churn(&index, &mut v, &pool[n..], n - 1, 9450);
            removed.lock().expect("removed").extend(gone);
            let check = |j: usize, b: &mut [f32]| {
                b.copy_from_slice(&v[live[j] as usize]);
                Ok(Some(live[j]))
            };
            let hit = find_stranded(&index, live.len(), BLOBS.dim, 1, &check).expect("find");
            stranded_by_churn.store(hit.len(), Ordering::Release);
        }
        if removed.lock().expect("removed").contains(&(i as u64)) {
            return Ok(None);
        }
        buf.copy_from_slice(&vectors.lock().expect("vectors")[i]);
        Ok(Some(i as u64))
    };
    let missing =
        add_parallel(&index, n, BLOBS.dim, "wiring test", &no_fault(), &read).expect("build");
    assert_eq!(missing, 0, "every item was present during the add phase");
    let by_churn = stranded_by_churn.load(Ordering::Acquire);
    eprintln!(
        "#9450: the churn stranded {by_churn} of the first {} keys",
        n - 1
    );
    assert!(by_churn > 0, "the churn must strand at least one node");
    let after = find_stranded(&index, n, BLOBS.dim, 1, &read).expect("find");
    assert!(
        after.is_empty(),
        "{} nodes still stranded after add_parallel: {:?}",
        after.len(),
        &after[..after.len().min(10)]
    );
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
        for threads in [1usize, 4] {
            let started = Instant::now();
            let stranded = find_stranded(&index, n, BLOBS.dim, threads, &read).expect("find");
            eprintln!(
                "#9450 cost: n={n} stranded={} sweep={} ms ({threads} threads)",
                stranded.len(),
                started.elapsed().as_millis()
            );
        }
    }
}
