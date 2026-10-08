//! Seeded, clustered test vectors shared by the HNSW recall tests.
//!
//! Why: #9414's recall test and #9450's churn test need the same corpus
//! shape — Gaussian blobs whose members are close enough that a damaged graph
//! loses them. One copy keeps the two tests measuring the same thing.
//! What: test-only generators. The unit tests mount this file as a module;
//! `tests/integration.rs` mounts it through `#[path]`, so it depends on
//! `rand` alone and on nothing else in this crate.
//! Test: `tests_9450`, `tests/hnsw_recall_9414.rs`, `tests/hnsw_compact_9450.rs`.

use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};

/// One standard-normal sample (Box-Muller).
pub fn gaussian(rng: &mut StdRng) -> f32 {
    let u1: f32 = rng.gen_range(f32::EPSILON..1.0);
    let u2: f32 = rng.gen_range(0.0..1.0);
    (-2.0 * u1.ln()).sqrt() * (std::f32::consts::TAU * u2).cos()
}

/// Normalise to unit length, then round every component to a multiple of
/// 2^-10. Such values are exact in f16 (the default store precision), so the
/// stored vector, the query cast and an f32 brute force all see one vector.
pub fn quantize_unit(mut v: Vec<f32>) -> Vec<f32> {
    let norm = v
        .iter()
        .map(|x| x * x)
        .sum::<f32>()
        .sqrt()
        .max(f32::EPSILON);
    for x in &mut v {
        *x = ((*x / norm) * 1024.0).round() / 1024.0;
    }
    if v.iter().all(|x| *x == 0.0) {
        v[0] = 1.0;
    }
    v
}

/// The shape of a clustered corpus: dimensionality, blob count, and the
/// per-axis spread of each blob.
#[derive(Debug, Clone, Copy)]
pub struct Blobs {
    pub dim: usize,
    pub clusters: usize,
    pub sigma: f32,
}

/// `n` points drawn from `blobs.clusters` Gaussian blobs, fixed by `seed`.
pub fn clustered_points(n: usize, blobs: Blobs, seed: u64) -> Vec<Vec<f32>> {
    let mut rng = StdRng::seed_from_u64(seed);
    let centers: Vec<Vec<f32>> = (0..blobs.clusters)
        .map(|_| (0..blobs.dim).map(|_| gaussian(&mut rng)).collect())
        .collect();
    (0..n)
        .map(|_| {
            let c = &centers[rng.gen_range(0..blobs.clusters)];
            quantize_unit(
                c.iter()
                    .map(|x| x + blobs.sigma * gaussian(&mut rng))
                    .collect(),
            )
        })
        .collect()
}
