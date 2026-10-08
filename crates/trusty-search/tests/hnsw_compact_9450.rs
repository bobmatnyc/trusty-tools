//! Issue #9450 — what one graph compaction costs at production scale.
//!
//! Why: a compaction rebuilds the whole HNSW graph and holds a copy of the
//! vectors plus a second graph until the swap. Writers wait on the store's
//! `save_lock` only while the vectors are copied and while the graph is
//! swapped. The threshold and the load heal are only acceptable if these
//! costs are known at the index size #9414 reported.
//! What: builds a 150K-vector, 384-dim store at the default precision (f16)
//! and default tuning. Run 1 compacts while a writer upserts every 10 ms and
//! a reader searches every 10 ms; a write lands during the build, so the
//! compaction is abandoned, and the run reports the longest and median write
//! wait. Run 2 compacts with the reader only and reports the rebuild time.
//! Both report process RSS before, at peak during, and after. A measurement,
//! not a gate: it asserts only the outcomes and that no vector was lost.
//! Run (release, the shipping profile):
//! `cargo test -p trusty-search --release --test integration hnsw_compact_9450 -- --ignored --nocapture`.
//! Test: this file.

use crate::clustered_vectors;

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use trusty_search::core::memguard::current_rss_mb;
use trusty_search::core::store::{CompactMode, CompactReport, UsearchStore, VectorStore};

const VECTORS: usize = 150_000;
const DIM: usize = 384;
const BATCH: usize = 10_000;
const BLOBS: clustered_vectors::Blobs = clustered_vectors::Blobs {
    dim: DIM,
    clusters: 100,
    sigma: 0.6,
};

/// Sample process RSS every 25 ms into `peak` until `stop` is set.
fn spawn_rss_sampler(stop: Arc<AtomicBool>, peak: Arc<AtomicU64>) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        while !stop.load(Ordering::Acquire) {
            if let Some(mb) = current_rss_mb() {
                peak.fetch_max(mb, Ordering::AcqRel);
            }
            std::thread::sleep(Duration::from_millis(25));
        }
    })
}

/// #9450 measurement: write-block time, rebuild time, and peak RSS of
/// compacting a 150K x 384 f16 index, with and without a racing writer.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "builds a 150K x 384 HNSW index and compacts it; run in release with --ignored"]
async fn compaction_cost_at_150k_384d_f16() {
    let store = Arc::new(UsearchStore::new(DIM).expect("store init"));
    assert_eq!(store.vector_quant_label().await, Some("f16"));
    let build = Instant::now();
    for b in 0..VECTORS / BATCH {
        let items: Vec<(String, Vec<f32>)> =
            clustered_vectors::clustered_points(BATCH, BLOBS, 9450 + b as u64)
                .into_iter()
                .enumerate()
                .map(|(i, v)| (format!("c{}", b * BATCH + i), v))
                .collect();
        store.upsert_batch(&items).await.expect("upsert batch");
    }
    assert_eq!(store.len().await.expect("len"), VECTORS);
    let build_secs = build.elapsed().as_secs_f64();
    let writes = clustered_vectors::clustered_points(4_000, BLOBS, 1);
    let probes = clustered_vectors::clustered_points(500, BLOBS, 2);

    // Run 1: a writer races the compaction.
    let contended = measure(&store, &probes, Some(&writes)).await;
    let err = contended
        .outcome
        .as_ref()
        .expect_err("a write during the build abandons the compaction");
    assert!(err.to_string().contains("abandoned"), "{err}");
    // Run 2: readers only, so the compaction swaps.
    let quiet = measure(&store, &probes, None).await;
    let report = *quiet
        .outcome
        .as_ref()
        .expect("compaction")
        .as_ref()
        .expect("report");

    let mut waits = contended.waits.clone();
    waits.sort();
    let longest_wait = waits.last().copied().unwrap_or_default();
    let median_wait = waits.get(waits.len() / 2).copied().unwrap_or_default();
    eprintln!(
        "#9450 measure: {VECTORS} x {DIM} f16, built in {build_secs:.1} s\n\
         #9450 measure: run 1 (writer every 10 ms): abandoned after {} ms; writes {}; \
         longest wait {} ms; median wait {} ms; searches {}, longest {} ms; \
         RSS before {} MB, peak {} MB (+{} MB), after {} MB\n\
         #9450 measure: run 2 (readers only): compaction {} ms ({} -> {} vectors, {} \
         unreadable); searches {}, longest {} ms; RSS before {} MB, peak {} MB (+{} MB), \
         after {} MB",
        contended.elapsed.as_millis(),
        waits.len(),
        longest_wait.as_millis(),
        median_wait.as_millis(),
        contended.searches,
        contended.longest_search.as_millis(),
        contended.rss_before,
        contended.rss_peak,
        contended.rss_peak.saturating_sub(contended.rss_before),
        contended.rss_after,
        report.elapsed_ms,
        report.vectors_before,
        report.vectors_after,
        report.missing,
        quiet.searches,
        quiet.longest_search.as_millis(),
        quiet.rss_before,
        quiet.rss_peak,
        quiet.rss_peak.saturating_sub(quiet.rss_before),
        quiet.rss_after,
    );
    assert_eq!(report.missing, 0);
    assert_eq!(report.vectors_after, report.vectors_before);
    assert_eq!(
        store.len().await.expect("len"),
        VECTORS + waits.len(),
        "no vector lost"
    );
}

/// What one measured compaction run saw.
struct Run {
    outcome: anyhow::Result<Option<CompactReport>>,
    elapsed: Duration,
    waits: Vec<Duration>,
    searches: usize,
    longest_search: Duration,
    rss_before: u64,
    rss_peak: u64,
    rss_after: u64,
}

/// One `CompactMode::Always` compaction with a reader searching every 10 ms
/// and, when `writes` is given, a writer upserting every 10 ms.
async fn measure(
    store: &Arc<UsearchStore>,
    probes: &[Vec<f32>],
    writes: Option<&[Vec<f32>]>,
) -> Run {
    let rss_before = current_rss_mb().unwrap_or(0);
    let stop = Arc::new(AtomicBool::new(false));
    let peak = Arc::new(AtomicU64::new(rss_before));
    let sampler = spawn_rss_sampler(stop.clone(), peak.clone());
    let started = Instant::now();
    let compaction = tokio::spawn({
        let store = store.clone();
        async move { store.compact_graph_now(CompactMode::Always).await }
    });
    let reader = tokio::spawn({
        let store = store.clone();
        let stop = stop.clone();
        let probes = probes.to_vec();
        async move {
            let mut longest = Duration::ZERO;
            let mut i = 0usize;
            while !stop.load(Ordering::Acquire) {
                let t = Instant::now();
                store
                    .search(&probes[i % probes.len()], 10)
                    .await
                    .expect("search");
                longest = longest.max(t.elapsed());
                i += 1;
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            (i, longest)
        }
    });
    let mut waits: Vec<Duration> = Vec::new();
    while !compaction.is_finished() {
        let Some(writes) = writes else {
            tokio::time::sleep(Duration::from_millis(10)).await;
            continue;
        };
        let i = waits.len();
        let t = Instant::now();
        store
            .upsert(&format!("w{i}"), writes[i % writes.len()].clone())
            .await
            .expect("concurrent upsert");
        waits.push(t.elapsed());
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let outcome = compaction.await.expect("compaction task");
    let elapsed = started.elapsed();
    stop.store(true, Ordering::Release);
    sampler.join().expect("sampler");
    let (searches, longest_search) = reader.await.expect("reader");
    Run {
        outcome,
        elapsed,
        waits,
        searches,
        longest_search,
        rss_before,
        rss_peak: peak.load(Ordering::Acquire),
        rss_after: current_rss_mb().unwrap_or(0),
    }
}
