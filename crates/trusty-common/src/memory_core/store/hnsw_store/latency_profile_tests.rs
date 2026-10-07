//! Per-query latency of the exact scan against the graph arm (#9280).
//!
//! Why: the exact scan ranks independently of graph shape, so raising
//! [`exhaustive::EXHAUSTIVE_SCAN_MAX_POINTS`] extends the range where two
//! opens of one palace rank every query identically. The Architect's budget
//! for that raise is p95 recall latency at or under the graph arm's p95 plus
//! 5 ms at 20k rows. This profile produces both numbers from one process.
//! What: for each size, seeds a clustered 384-dim palace, reopens it (the
//! parallel replay), and times 300 queries three ways: `HnswStore::search`
//! (whichever arm the threshold selects), the exact scan alone, and the graph
//! arm alone (`graph_nearest` plus the stranded merge). Prints p50/p95 per
//! row, and how many queries' top 10 differ between two opens. #9174: also
//! times `upsert` and, alone, the evicted-point re-check it now runs. A no-op unless `TRUSTY_HNSW_LATENCY_PROFILE` is set; run it in release:
//! `TRUSTY_HNSW_LATENCY_PROFILE=1 cargo test -p trusty-common --release --lib
//! hnsw_latency_profile -- --nocapture`. `TRUSTY_HNSW_LATENCY_SIZES` overrides
//! the size list (comma-separated row counts).
//! Test: this file is the profile; it asserts nothing about timing.

use std::time::{Duration, Instant};

use super::*;

/// Opt-in switch; the profile is minutes of work in a debug build.
const PROFILE_ENV: &str = "TRUSTY_HNSW_LATENCY_PROFILE";

/// Comma-separated override for [`DEFAULT_SIZES`].
const SIZES_ENV: &str = "TRUSTY_HNSW_LATENCY_SIZES";

const DEFAULT_SIZES: &[usize] = &[4_096, 8_192, 12_288, 16_384, 20_000, 32_768, 50_000];

/// Embedding width of the production embedder (all-MiniLM-L6-v2).
const DIM: usize = 384;

/// Candidates `retrieve_l2` asks the store for at `top_k = 10` (3x overfetch).
const K: usize = 30;

const QUERIES: u64 = 300;

/// A unit vector near one of 48 seeded centres: cosine to its centre ~0.7,
/// the spread of one topic in a real palace.
pub(super) fn clustered_vec(seed: u64) -> Vec<f32> {
    let centre = spread_vec(DIM, 1_000 + seed % 48);
    let noise = spread_vec(DIM, 5_000_000 + seed);
    let raw: Vec<f32> = centre.iter().zip(&noise).map(|(c, n)| c + n).collect();
    let norm: f32 = raw.iter().map(|x| x * x).sum::<f32>().sqrt();
    raw.into_iter().map(|x| x / norm).collect()
}

/// `(p50, p95)` of `samples`, in microseconds.
fn percentiles(samples: &mut [Duration]) -> (u128, u128) {
    samples.sort();
    let at = |q: f64| samples[((samples.len() as f64 * q).ceil() as usize).saturating_sub(1)];
    (at(0.50).as_micros(), at(0.95).as_micros())
}

/// Times `f` once per query, after a 20-query warm-up.
fn time_queries(queries: &[Vec<f32>], mut f: impl FnMut(&[f32])) -> (u128, u128) {
    for q in queries.iter().take(20) {
        f(q);
    }
    let mut samples: Vec<Duration> = queries
        .iter()
        .map(|q| {
            let start = Instant::now();
            f(q);
            start.elapsed()
        })
        .collect();
    percentiles(&mut samples)
}

fn load_average() -> String {
    std::process::Command::new("sysctl")
        .args(["-n", "vm.loadavg"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|e| format!("unavailable: {e}"))
}

/// See the module header.
#[test]
fn hnsw_latency_profile() {
    if std::env::var_os(PROFILE_ENV).is_none() {
        return;
    }
    let sizes: Vec<usize> = std::env::var(SIZES_ENV)
        .ok()
        .map(|s| s.split(',').filter_map(|n| n.trim().parse().ok()).collect())
        .unwrap_or_else(|| DEFAULT_SIZES.to_vec());
    let queries: Vec<Vec<f32>> = (0..QUERIES).map(|q| clustered_vec(9_000_000 + q)).collect();
    println!(
        "threshold={} dim={DIM} k={K} queries={QUERIES} load_before=[{}]",
        exhaustive::EXHAUSTIVE_SCAN_MAX_POINTS,
        load_average()
    );
    println!(
        "rows | open ms | stranded groups | search p50/p95 us | exact p50/p95 us | \
         graph p50/p95 us | top-10 differs between two opens | upsert p50/p95 us | \
         re-check p50/p95 us"
    );
    for n in sizes {
        let pool: Vec<Vec<f32>> = (0..n as u64).map(clustered_vec).collect();
        let (_dir, seed) = open_store(DIM);
        seed_rows(&seed.db, &pool);
        let start = Instant::now();
        let store = HnswStore::open(Arc::clone(&seed.db), DIM).expect("reopen");
        let open_ms = start.elapsed().as_millis();
        let none = std::collections::HashSet::new();
        let search = time_queries(&queries, |q| {
            std::hint::black_box(store.search(q, K).expect("search"));
        });
        let index = store.index.read();
        let stranded = store.stranded.read();
        let exact = time_queries(&queries, |q| {
            let mut all = exhaustive::exhaustive_nearest(&index, q, &none);
            all.truncate(K);
            std::hint::black_box(all);
        });
        let graph = time_queries(&queries, |q| {
            let mut hits = graph_arm::graph_nearest(&index, q, &none, K, n);
            stranded::merge_stranded(&mut hits, &stranded, q, &none);
            std::hint::black_box(hits);
        });
        drop((index, stranded));
        // #9280: above the threshold determinism is measured, not guaranteed.
        let second = HnswStore::open(Arc::clone(&seed.db), DIM).expect("second open");
        let top10 = |s: &HnswStore, q: &[f32]| -> Vec<(String, u32)> {
            let hits = s.search(q, 10).expect("search");
            hits.into_iter().map(|(u, d)| (u, d.to_bits())).collect()
        };
        let differ = queries
            .iter()
            .filter(|q| top10(&store, q) != top10(&second, q))
            .count();
        // #9174: the re-check's snapshot and diff, without an insert between.
        let index = store.index.read();
        let recheck = time_queries(&queries[..100], |q| {
            let before = stranded::neighbourhoods_before_insert(&index, q);
            std::hint::black_box(stranded::restrand_evicted(
                &index,
                before,
                u64::MAX,
                &store.stranded,
            ));
        });
        drop(index);
        let mut upserted = 0u64;
        let upsert = time_queries(&queries[..100], |q| {
            upserted += 1;
            store
                .upsert(&format!("profile-{upserted}"), q)
                .expect("upsert");
        });
        println!(
            "{n} | {open_ms} | {} | {}/{} | {}/{} | {}/{} | {differ}/{QUERIES} | {}/{} | {}/{}",
            store.stranded.read().len(),
            search.0,
            search.1,
            exact.0,
            exact.1,
            graph.0,
            graph.1,
            upsert.0,
            upsert.1,
            recheck.0,
            recheck.1
        );
    }
    println!("load_after=[{}]", load_average());
}
