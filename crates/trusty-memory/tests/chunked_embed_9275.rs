//! #9275 PR C: a long drawer is embedded as one vector per window-sized chunk.
//!
//! Why: an embedder truncates at its input window, so a fact late in a long
//! drawer never reached its vector and vector recall could not find it. This
//! binary drives the real write, recall, health, repair and forget paths.
//! What: its own process, so the shared embedder is seeded with a chunking
//! mock (`MockEmbedder::with_chunking(32)`: 30 words per chunk, bag-of-words
//! vectors truncated at the window) before any palace opens. Disk palaces, no
//! model.
//! Test: this file IS the tests.

use std::sync::Arc;
use tempfile::tempdir;
use trusty_common::embedder::Embedder;
use trusty_common::embedder::{MockEmbedder, chunk_text};
use trusty_memory::engine::palace::{Palace, PalaceId, RoomType};
use trusty_memory::engine::retrieval::{
    ChunkCheck, PalaceHandle, RememberOptions, VectorBackfillOptions, embed_chunks,
    recall_with_default_embedder, seed_shared_embedder_with, shared_embedder,
};
use trusty_memory::engine::store::VectorStore;
use uuid::Uuid;

/// Window of the seeded mock: 30 words per chunk after `[CLS]`/`[SEP]`.
const WINDOW: usize = 32;

fn seed() {
    let mock: Arc<dyn Embedder + Send + Sync> =
        Arc::new(MockEmbedder::new(384).with_chunking(WINDOW));
    seed_shared_embedder_with(mock);
}

fn open_palace(name: &str) -> Arc<PalaceHandle> {
    seed();
    let dir = tempdir().expect("tempdir");
    let data_dir = dir.path().join(name);
    std::fs::create_dir_all(&data_dir).expect("mkdir");
    let palace = Palace::new(PalaceId::new(name), name, data_dir);
    let handle = PalaceHandle::open(&palace).expect("open palace");
    std::mem::forget(dir);
    handle
}

// #9278: default-then-assign; A2b's #[non_exhaustive] forbids the `..Default::default()` literal.
async fn remember(handle: &PalaceHandle, text: &str) -> Uuid {
    let mut opts = RememberOptions::default();
    opts.force = true;
    opts.enforce_min_tokens = false;
    handle
        .remember_with_options(text.to_string(), RoomType::General, vec![], 0.5, opts)
        .await
        .expect("remember")
}

/// The unique 20-word sentence at the end of the long drawer.
fn needle() -> Vec<String> {
    (0..20).map(|i| format!("zq{i}needle")).collect()
}

/// 5000 words: 4980 distinct filler words, then the needle sentence.
fn long_text() -> String {
    let mut words: Vec<String> = (0..4980).map(|i| format!("filler{i}")).collect();
    words.extend(needle());
    words.join(" ")
}

/// 30 words sharing four needle words, so each outranks a vector that holds
/// none of the needle.
fn distractor(n: usize) -> String {
    let needle = needle();
    let mut words: Vec<String> = (0..4).map(|i| needle[(n + i * 5) % 20].clone()).collect();
    words.extend((0..26).map(|k| format!("d{n}w{k}")));
    words.join(" ")
}

/// Rows of drawer `id` in the vector index, as `(row count, highest index)`.
fn shape(handle: &PalaceHandle, id: Uuid) -> Option<(usize, u32)> {
    let shapes = handle.vector_store.chunk_shapes().expect("chunk shapes");
    shapes.get(&id).map(|s| (s.rows, s.max_index))
}

/// Chunk count of `text` at [`WINDOW`], from `chunk_text` directly rather
/// than through the pipeline under test.
fn chunk_count(text: &str) -> usize {
    let mock = MockEmbedder::new(384).with_chunking(WINDOW);
    let tok = mock.chunking_tokenizer().expect("load").expect("tokenizer");
    chunk_text(&tok, text, WINDOW).expect("chunk").len()
}

async fn vector_top1(handle: &PalaceHandle, query: &str) -> Uuid {
    let embedder = shared_embedder().await.expect("embedder");
    let q = embedder
        .embed_batch(&[query.to_string()])
        .await
        .expect("embed query")
        .remove(0);
    let hits = handle.vector_store.search(&q, 1).await.expect("search");
    hits.first().expect("one hit").drawer_id
}

#[tokio::test]
async fn needle_in_the_last_chunk_is_recalled_at_rank_1() {
    let handle = open_palace("needle");
    for n in 0..20 {
        remember(&handle, &distractor(n)).await;
    }
    let long = remember(&handle, &long_text()).await;
    let query = needle().join(" ");

    assert_eq!(
        vector_top1(&handle, &query).await,
        long,
        "the tail sentence must be in some chunk vector of the long drawer"
    );
    let results = recall_with_default_embedder(&handle, &query, 10)
        .await
        .expect("recall");
    assert_eq!(results.first().map(|r| r.drawer.id), Some(long), "rank 1");
    let hits = results.iter().filter(|r| r.drawer.id == long).count();
    assert_eq!(hits, 1, "a chunked drawer is listed once");
}

#[tokio::test]
async fn remember_writes_one_row_per_chunk_and_none_for_a_short_drawer() {
    let handle = open_palace("rows");
    let text = long_text();
    let long = remember(&handle, &text).await;
    let short = remember(
        &handle,
        "ten words of text that fit inside one chunk easily",
    )
    .await;

    let k = chunk_count(&text);
    assert!(k > 100, "5000 words at 30 per chunk: {k} chunks");
    assert_eq!(shape(&handle, long), Some((k, (k - 1) as u32)));
    assert_eq!(
        shape(&handle, short),
        Some((1, 0)),
        "a short drawer keeps one bare row"
    );
}

#[tokio::test]
async fn rewriting_a_drawer_shorter_drops_its_stale_chunk_rows() {
    let handle = open_palace("rewrite");
    let long = remember(&handle, &long_text()).await;
    assert!(
        shape(&handle, long).expect("rows").0 > 1,
        "long drawer chunked"
    );

    let embedder = shared_embedder().await.expect("embedder");
    let vectors = embed_chunks(embedder.as_ref(), "now a short text")
        .await
        .expect("embed");
    handle
        .vector_store
        .upsert_chunks(long, vectors)
        .await
        .expect("rewrite");
    assert_eq!(
        shape(&handle, long),
        Some((1, 0)),
        "no stale #n row survives"
    );
}

#[tokio::test]
// #9278: default-then-assign; A2b's #[non_exhaustive] forbids the `..Default::default()` literal.
async fn legacy_palace_recalls_unchanged_then_reembed_adds_the_tail() {
    let handle = open_palace("legacy");
    for n in 0..20 {
        remember(&handle, &distractor(n)).await;
    }
    let text = long_text();
    let long = remember(&handle, &text).await;
    // What 0.33.x wrote: one bare vector of the truncated text.
    let embedder = shared_embedder().await.expect("embedder");
    let truncated = embedder
        .embed_batch(std::slice::from_ref(&text))
        .await
        .expect("embed")
        .remove(0);
    handle
        .vector_store
        .upsert_chunks(long, vec![truncated])
        .await
        .expect("legacy row");
    let query = needle().join(" ");

    let health = handle.embed_health();
    assert_eq!(health.chunk_check, ChunkCheck::Ran);
    assert_eq!(
        health.short_chunk_ids,
        vec![long],
        "only the long drawer is short"
    );
    assert!(health.missing_vector_ids.is_empty());
    assert!(!health.is_healthy());
    assert_ne!(
        vector_top1(&handle, &query).await,
        long,
        "tail not yet embedded"
    );

    let mut repair = VectorBackfillOptions::default();
    repair.dry_run = false;
    let report = handle
        .backfill_missing_vectors(repair)
        .await
        .expect("backfill");
    assert_eq!((report.short, report.repaired), (1, 1));
    assert!(report.still_short_ids.is_empty());
    assert_eq!(
        vector_top1(&handle, &query).await,
        long,
        "tail now findable"
    );

    let again = handle
        .backfill_missing_vectors(repair)
        .await
        .expect("backfill");
    assert_eq!((again.short, again.repaired, again.attempted), (0, 0, 0));
    assert!(handle.embed_health().is_healthy());
}

#[tokio::test]
async fn forget_removes_every_chunk_row_written_by_the_pipeline() {
    let handle = open_palace("forget");
    let long = remember(&handle, &long_text()).await;
    assert!(
        shape(&handle, long).expect("rows").0 > 1,
        "long drawer chunked"
    );

    handle.forget(long).await.expect("forget");
    let prefix = long.to_string();
    let keys = handle
        .vector_store
        .hnsw_for_test()
        .all_keys()
        .expect("keys");
    assert!(
        keys.iter().all(|k| !k.starts_with(&prefix)),
        "no row of the forgotten drawer survives: {keys:?}"
    );
}
