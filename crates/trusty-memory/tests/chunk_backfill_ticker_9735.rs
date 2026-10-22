//! #9735 PR 2: the daemon's background maintenance re-embeds a drawer stored
//! before chunking, with no operator command.
//!
//! Why: before PR 2, `reembed_slice` had no caller in the binary, so a palace
//! written before #9275 kept one truncated vector per long drawer and a fact
//! late in that drawer stayed unfindable by vector recall.
//! What: its own process, so the shared embedder is seeded with a chunking
//! mock (`MockEmbedder::with_chunking(32)`: 30 words per chunk, bag-of-words
//! vectors truncated at the window). A long drawer is reduced to the
//! pre-chunking shape (one row holding the truncated vector), then the same
//! `spawn_background_maintenance` the daemon calls at startup runs until the
//! drawer has one row per chunk and its tail is the top vector hit.
//! Test: this file IS the tests.

use std::sync::Arc;
use std::time::{Duration, Instant};

use trusty_common::embedder::{Embedder, MockEmbedder, chunk_text};
use trusty_memory::AppState;
use trusty_memory::dream_scheduler::{
    DREAM_DISABLED_ENV, make_shutdown_watch, spawn_background_maintenance,
};
use trusty_memory::engine::palace::{Palace, PalaceId, RoomType};
use trusty_memory::engine::retrieval::{
    PalaceHandle, RememberOptions, seed_shared_embedder_with, shared_embedder,
};
use trusty_memory::engine::store::VectorStore;
use uuid::Uuid;

const WINDOW: usize = 32;

fn needle() -> Vec<String> {
    (0..20).map(|i| format!("zq{i}needle")).collect()
}

/// 990 filler words (33 whole chunks), then the needle sentence.
fn long_text() -> String {
    let mut words: Vec<String> = (0..990).map(|i| format!("filler{i}")).collect();
    words.extend(needle());
    words.join(" ")
}

/// 30 words sharing four needle words, so each outranks a vector with none.
fn distractor(n: usize) -> String {
    let needle = needle();
    let mut words: Vec<String> = (0..4).map(|i| needle[(n + i * 5) % 20].clone()).collect();
    words.extend((0..26).map(|k| format!("d{n}w{k}")));
    words.join(" ")
}

async fn remember(handle: &PalaceHandle, text: &str) -> Uuid {
    let mut opts = RememberOptions::default();
    opts.force = true;
    opts.enforce_min_tokens = false;
    handle
        .remember_with_options(text.to_string(), RoomType::General, vec![], 0.5, opts)
        .await
        .expect("remember")
}

fn rows(handle: &PalaceHandle, id: Uuid) -> usize {
    let shapes = handle.vector_store.chunk_shapes().expect("chunk shapes");
    shapes.get(&id).map_or(0, |s| s.rows)
}

/// The `state` field of the palace's chunk marker, if it is written.
fn marker_state(path: &std::path::Path) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    let value: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    value["state"].as_str().map(str::to_string)
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn background_maintenance_reembeds_pre_chunking_drawers() {
    let mock: Arc<dyn Embedder + Send + Sync> =
        Arc::new(MockEmbedder::new(384).with_chunking(WINDOW));
    seed_shared_embedder_with(mock);
    // SAFETY: this binary is its own process and holds one test.
    unsafe {
        std::env::set_var(DREAM_DISABLED_ENV, "1");
        std::env::set_var("TRUSTY_CHUNK_BACKFILL_TICK_MS", "20");
    }
    let root = tempfile::tempdir().expect("tempdir");
    let state = AppState::new(root.path().to_path_buf()).with_writer_intent();
    let palace = Palace::new(
        PalaceId::new("legacy"),
        "legacy",
        root.path().join("legacy"),
    );
    let handle = state
        .registry
        .create_palace(root.path(), palace)
        .expect("create palace");
    for n in 0..20 {
        remember(&handle, &distractor(n)).await;
    }
    let text = long_text();
    let long = remember(&handle, &text).await;
    // The pre-chunking shape: one row, the window-truncated vector.
    let embedder = shared_embedder().await.expect("embedder");
    let truncated = embedder
        .embed_batch(std::slice::from_ref(&text))
        .await
        .expect("embed");
    handle
        .vector_store
        .upsert_chunks(long, truncated)
        .await
        .expect("legacy row");
    let query = needle().join(" ");
    assert_eq!(rows(&handle, long), 1, "precondition: one truncated row");
    assert_ne!(
        vector_top1(&handle, &query).await,
        long,
        "precondition: the tail is unfindable before the backfill"
    );

    let (tx, rx) = make_shutdown_watch();
    spawn_background_maintenance(&state.registry, rx, tx, None);

    let tok = MockEmbedder::new(384)
        .with_chunking(WINDOW)
        .chunking_tokenizer()
        .expect("load")
        .expect("tokenizer");
    let expected = chunk_text(&tok, &text, WINDOW).expect("chunk").len();
    // The marker is rewritten after the slice ends, so a `complete` marker
    // means the upsert finished; the row count alone can be read mid-upsert.
    let marker = root.path().join("legacy").join("chunk_backfill.json");
    let deadline = Instant::now() + Duration::from_secs(30);
    while marker_state(&marker).as_deref() != Some("complete") {
        assert!(
            Instant::now() < deadline,
            "no background task re-embedded the pre-chunking drawer: {} of {expected} rows, \
             marker {:?}",
            rows(&handle, long),
            marker_state(&marker)
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(rows(&handle, long), expected, "one row per chunk");
    assert_eq!(
        vector_top1(&handle, &query).await,
        long,
        "the tail is findable after the backfill"
    );
}
