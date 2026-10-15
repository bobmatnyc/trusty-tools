//! #9735 PR 1: measure the slice defaults and recall-embed contention with
//! the real ONNX embedder.
//!
//! Why: ruling 8 and the clearance require the 64-chunk slice cap and the
//! 120 s per-drawer embed bound to be measured, not assumed, and gap 14 asks
//! for the query-embed p95 at idle and under a running slice embed.
//! What: one `#[ignore]` test that prints `MEASURE` lines: the wall time of a
//! 64-chunk embed, the chunks that fit in 120 s, slice wall times on a palace
//! of legacy drawers, and query-embed p50/p95/max in both phases. It asserts
//! only that the real slice repairs every drawer; the numbers are a report.
//! Run alone, so the shared embedder is the real one:
//! `cargo test -p trusty-memory --lib -- --ignored
//! --exact memory_core::retrieval::chunk_backfill_measure_tests::slice_defaults_and_recall_contention_measured
//! --nocapture`.
//! Test: this file IS the test.

use super::chunk_backfill::SliceBounds;
use super::embed_store::{embed_planned, plan_chunks};
use super::embedder::shared_embedder;
use super::handle::PalaceHandle;
use crate::engine::embed::Embedder;
use crate::engine::palace::{Drawer, PalaceId};
use crate::engine::store::{kg::KnowledgeGraph, vector::UsearchStore};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use uuid::Uuid;

type Emb = Arc<dyn Embedder + Send + Sync>;

/// Query embeds sampled per phase.
const QUERIES: usize = 200;
/// Drawers in the slice palace; each splits into about `DRAWER_CHUNKS`.
const DRAWERS: usize = 16;
const DRAWER_CHUNKS: usize = 8;

/// `n` distinct prose sentences tagged with `tag`.
fn prose(tag: &str, n: usize) -> String {
    (0..n)
        .map(|i| {
            format!(
                "Entry {tag}-{i} records the harbour ledger tally for crate {i} and its route. "
            )
        })
        .collect()
}

/// Prose that splits into at least `target` chunks under `e`.
fn prose_with_chunks(e: &dyn Embedder, tag: &str, target: usize) -> String {
    let mut n = 8;
    loop {
        let text = prose(tag, n);
        if plan_chunks(e, &text).expect("split").len() >= target {
            return text;
        }
        n += (n / 4).max(8);
    }
}

/// Exactly 64 full-window chunks of prose tagged `tag` (the slice's chunk cap).
fn cap_chunks(e: &dyn Embedder, tag: &str) -> Vec<String> {
    let text = prose_with_chunks(e, tag, 64);
    let chunks = plan_chunks(e, &text).expect("split");
    chunks[..64].iter().map(|s| s.to_string()).collect()
}

/// The `p` quantile (0..=1) of `samples`, nearest-rank.
fn quantile(samples: &mut [Duration], p: f64) -> Duration {
    samples.sort_unstable();
    let rank = ((p * samples.len() as f64).ceil() as usize).clamp(1, samples.len());
    samples[rank - 1]
}

/// Embed `QUERIES` single-input queries 10 ms apart, as recall does.
async fn query_latencies(e: &Emb, phase: &str) -> Vec<Duration> {
    let mut out = Vec::with_capacity(QUERIES);
    for i in 0..QUERIES {
        let q = vec![format!(
            "which harbour ledger tally lists crate {i} {phase}"
        )];
        let t = Instant::now();
        e.embed_batch(&q).await.expect("query embed");
        out.push(t.elapsed());
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    out
}

fn report(phase: &str, mut s: Vec<Duration>) -> Duration {
    let (p50, p95) = (quantile(&mut s, 0.50), quantile(&mut s, 0.95));
    let max = *s.last().expect("samples");
    println!(
        "MEASURE query_embed phase={phase} n={} p50={p50:?} p95={p95:?} max={max:?}",
        s.len()
    );
    p95
}

/// A disk palace whose vector store has the real embedder's dimension.
fn palace(dir: &std::path::Path, dim: usize) -> PalaceHandle {
    let vs = UsearchStore::new(dir.join("idx.usearch"), dim).unwrap();
    let kg = KnowledgeGraph::open(&dir.join("kg.db")).unwrap();
    let mut h = PalaceHandle::new(PalaceId::new("measure"), String::new(), vs, kg);
    h.data_dir = Some(dir.to_path_buf());
    h
}

#[ignore = "measurement: needs the real ONNX embedder in its own process; not a gate"]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn slice_defaults_and_recall_contention_measured() {
    let e: Emb = shared_embedder().await.expect("real embedder");
    println!(
        "MEASURE embedder dim={} window={:?}",
        e.dimension(),
        e.input_window()
    );
    e.embed_batch(&["warm up".to_string()]).await.unwrap();

    // The slice's chunk cap: a 64-chunk embed, three times on fresh text,
    // because the embedder's LRU cache answers a repeated input in microseconds.
    let mut runs = Vec::new();
    for r in 0..3 {
        let cap = cap_chunks(e.as_ref(), &format!("cap{r}"));
        let t = Instant::now();
        assert_eq!(embed_planned(e.as_ref(), &cap).await.unwrap().len(), 64);
        runs.push(t.elapsed());
    }
    let best = *runs.iter().min().unwrap();
    let per_chunk = best / 64;
    let fit = Duration::from_secs(120).as_secs_f64() / per_chunk.as_secs_f64();
    println!(
        "MEASURE embed_64_chunks runs={runs:?} per_chunk={per_chunk:?} chunks_in_120s={fit:.0}"
    );

    // Real slices on legacy drawers, default bounds bar min_age.
    let dir = tempfile::tempdir().unwrap();
    let h = palace(dir.path(), e.dimension());
    let mut ids = Vec::new();
    for k in 0..DRAWERS {
        let text = prose_with_chunks(e.as_ref(), &format!("d{k}"), DRAWER_CHUNKS);
        // `Drawer::new` takes a room id and mints the drawer id.
        let drawer = Drawer::new(Uuid::new_v4(), text);
        let id = drawer.id;
        h.add_drawer(drawer);
        h.vector_store
            .upsert_chunks(id, vec![vec![0.1; e.dimension()]])
            .await
            .unwrap();
        ids.push(id);
    }
    let bounds = SliceBounds {
        min_age: Duration::ZERO,
        ..SliceBounds::default()
    };
    for n in 0..10 {
        let t = Instant::now();
        let r = h.reembed_slice(&e, &bounds).await;
        println!(
            "MEASURE slice n={n} wall={:?} chunks={} repaired={} failed={} stop={:?}",
            t.elapsed(),
            r.chunks_embedded,
            r.repaired,
            r.failed,
            r.stop
        );
        if r.attempted == 0 {
            break;
        }
    }
    let shapes = h.vector_store.chunk_shapes().unwrap();
    let rows: Vec<Option<usize>> = ids
        .iter()
        .map(|id| shapes.get(id).map(|s| s.rows))
        .collect();
    println!("MEASURE groups keys={} rows={rows:?}", shapes.len());
    let full = rows.iter().filter(|r| r.is_some_and(|r| r > 1)).count();
    assert_eq!(
        full, DRAWERS,
        "every legacy drawer must end with its full group"
    );

    // Gap 14: query-embed latency at idle, then beside a looping 64-chunk embed.
    let idle = report("idle", query_latencies(&e, "idle").await);
    let stop = Arc::new(AtomicBool::new(false));
    let bg = {
        let (e, stop) = (e.clone(), stop.clone());
        tokio::spawn(async move {
            let mut batches = 0usize;
            while !stop.load(Ordering::Relaxed) {
                let cap = cap_chunks(e.as_ref(), &format!("bg{batches}"));
                embed_planned(e.as_ref(), &cap).await.unwrap();
                batches += 1;
            }
            batches
        })
    };
    let busy = report("contention", query_latencies(&e, "busy").await);
    stop.store(true, Ordering::Relaxed);
    let batches = bg.await.unwrap();
    let ratio = busy.as_secs_f64() / idle.as_secs_f64();
    let flag = if ratio > 2.0 { "FLAG" } else { "ok" };
    println!("MEASURE contention_p95_over_idle_p95={ratio:.2} bg_batches={batches} verdict={flag}");
}
