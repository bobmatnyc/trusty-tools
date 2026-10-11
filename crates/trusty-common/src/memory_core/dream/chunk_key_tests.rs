//! #9275 PR B: the dream compact pass over `{uuid}#n` chunk rows.
//!
//! Why: the compact pass rebuilds the whole index when it holds more than
//! `2 * drawers + 1` entries. Counting chunk ROWS there makes every healthy
//! chunked palace look orphan-ridden, and the rebuild re-embeds one vector per
//! drawer, dropping every chunk.
//! What: palaces with the mock embedder; chunk rows added with the raw
//! `HnswStore::upsert` primitive; `compact_pass` driven directly.
//! Test: this file IS the tests.

use super::cycle::compact_pass;
use crate::memory_core::palace::{Palace, PalaceId, RoomType};
use crate::memory_core::retrieval::{PalaceHandle, seed_shared_embedder_with_mock};
use crate::memory_core::store::kg_store::{VECTOR_KEYS, VECTORS};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tracing_subscriber::fmt::MakeWriter;
use uuid::Uuid;

fn open_handle(dir: &std::path::Path, name: &str) -> Arc<PalaceHandle> {
    seed_shared_embedder_with_mock();
    let palace = Palace {
        id: PalaceId::new(name),
        name: name.into(),
        description: None,
        created_at: chrono::Utc::now(),
        data_dir: dir.join(name),
    };
    std::fs::create_dir_all(&palace.data_dir).unwrap();
    PalaceHandle::open(&palace).unwrap()
}

async fn remember(handle: &PalaceHandle, text: &str) -> Uuid {
    handle
        .remember(text.into(), RoomType::General, vec![], 0.5)
        .await
        .unwrap()
}

fn add_chunk_rows(handle: &PalaceHandle, id: Uuid, extra: usize) {
    let dim = handle.vector_store.dim();
    for n in 1..=extra {
        let mut v = vec![0.0f32; dim];
        v[n % dim] = 1.0;
        handle
            .vector_store
            .hnsw_for_test()
            .upsert(&format!("{id}#{n}"), &v)
            .expect("raw chunk row");
    }
}

fn keys_of(handle: &PalaceHandle, id: Uuid) -> usize {
    let prefix = id.to_string();
    handle
        .vector_store
        .hnsw_for_test()
        .all_keys()
        .expect("all_keys")
        .iter()
        .filter(|k| k.starts_with(&prefix))
        .count()
}

/// AC-B14: three drawers of four rows each is a healthy palace. The compact
/// pass removes nothing and does not reset and rebuild the index.
#[tokio::test]
async fn drawer_count_ignores_chunk_rows_and_dream_does_not_rebuild_a_chunked_palace() {
    let dir = tempfile::tempdir().unwrap();
    let handle = open_handle(dir.path(), "chunk-dream");
    let mut ids = Vec::new();
    for text in [
        "Sloths digest leaves slowly",
        "Ravens solve multi-step puzzles",
        "Mantis shrimp see polarised light",
    ] {
        let id = remember(&handle, text).await;
        add_chunk_rows(&handle, id, 3);
        ids.push(id);
    }
    assert_eq!(handle.vector_store.index_size(), 12);

    let removed = compact_pass(&handle, Instant::now(), Duration::from_secs(30))
        .await
        .unwrap();
    assert_eq!(removed, 0, "nothing in a healthy palace is an orphan");
    assert_eq!(
        handle.vector_store.index_size(),
        12,
        "a rebuild would have re-embedded one row per drawer"
    );
    for id in ids {
        assert_eq!(keys_of(&handle, id), 4, "every chunk row survives");
    }
}

/// Fail-open check 2 (`cycle.rs` compact pass, #9275): when removing an orphan
/// drawer's vectors fails, the pass counts no removal, drops no row, and the
/// orphan's every chunk row stays reclaimable by `compact_vector_orphans`.
#[tokio::test]
async fn dream_compact_counts_only_successful_removals_of_a_chunked_orphan() {
    let dir = tempfile::tempdir().unwrap();
    let handle = open_handle(dir.path(), "chunk-dream-fail");
    let kept = remember(&handle, "Elephants mourn their dead").await;
    let orphan = remember(&handle, "Bees dance to give directions").await;
    add_chunk_rows(&handle, orphan, 2);
    handle.drawers.write().retain(|d| d.id != orphan);
    let _ = handle.kg.delete_drawer(orphan).await;
    // Every bounded vector call fails from here on.
    handle.vector_store.op_breaker().trip_for_test();

    let removed = compact_pass(&handle, Instant::now(), Duration::from_secs(30))
        .await
        .unwrap();
    assert_eq!(removed, 0, "a failed removal is not counted as one");
    assert_eq!(keys_of(&handle, orphan), 3, "no row is dropped on failure");
    assert_eq!(keys_of(&handle, kept), 1);

    let result = handle.compact_vector_orphans().await.unwrap();
    assert_eq!(result.orphans_removed, 3);
    assert_eq!(keys_of(&handle, orphan), 0, "every chunk row is reclaimed");
    assert_eq!(keys_of(&handle, kept), 1);
}

/// Collects formatted `warn` output from this thread.
#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Capture {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for Capture {
    type Writer = Capture;
    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

/// Write rows straight into a palace's vector redb before it opens: each
/// `Some(key)` row is keyed, each `None` row has a vector and no key.
fn seed_vector_rows(data_dir: &std::path::Path, rows: &[(u64, Option<String>)]) {
    std::fs::create_dir_all(data_dir).unwrap();
    let db = redb::Database::create(data_dir.join("index.usearch.redb")).unwrap();
    let wtx = db.begin_write().unwrap();
    {
        let mut vectors = wtx.open_table(VECTORS).unwrap();
        let mut keys = wtx.open_table(VECTOR_KEYS).unwrap();
        for (id, key) in rows {
            let mut v = vec![0.0f32; 384];
            v[(*id as usize) % 384] = 1.0;
            let encoded = postcard::to_allocvec(&v).unwrap();
            vectors.insert(*id, encoded.as_slice()).unwrap();
            if let Some(key) = key {
                keys.insert(key.as_str(), *id).unwrap();
            }
        }
    }
    wtx.commit().unwrap();
}

/// AC-B7 (#9275): rows the addressable pass cannot enumerate — keys that name
/// no drawer, and vectors with no key — still count toward the open-time
/// orphan warning and the dream rebuild trigger, exactly as on main.
#[tokio::test]
async fn unparseable_and_keyless_rows_still_trip_the_orphan_warning_and_rebuild() {
    let dir = tempfile::tempdir().unwrap();
    let name = "legacy-orphans";
    let ghost = Uuid::new_v4();
    seed_vector_rows(
        &dir.path().join(name),
        &[
            (900_001, Some("not-a-drawer".into())),
            (900_002, Some(format!("{ghost}#0"))),
            (900_003, Some(format!("{ghost}#01"))),
            (900_004, None),
            (900_005, None),
            (900_006, None),
        ],
    );

    // Six entries against zero drawers: over the `drawers + 5` warning bar.
    let cap = Capture::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(cap.clone())
        .with_ansi(false)
        .with_max_level(tracing::Level::WARN)
        .finish();
    let guard = tracing::subscriber::set_default(subscriber);
    let handle = open_handle(dir.path(), name);
    drop(guard);
    let log = String::from_utf8_lossy(&cap.0.lock().unwrap()).into_owned();
    assert!(
        log.contains("orphaned entries"),
        "the open-time orphan warning fires: {log}"
    );

    // Seven entries against one drawer: over the `2 * drawers + 1` trigger.
    remember(&handle, "Octopuses taste with their arms").await;
    assert_eq!(handle.vector_store.index_size(), 7);
    let removed = compact_pass(&handle, Instant::now(), Duration::from_secs(30))
        .await
        .unwrap();
    assert_eq!(
        handle.vector_store.index_size(),
        1,
        "the rebuild reset the index and re-embedded the one drawer"
    );
    assert_eq!(removed, 6, "the rebuild drops every unenumerable entry");
}

// ── #9275 PR C: the rebuild re-embeds every chunk ────────────────────────────

/// Window of the chunking mock below: ten words per chunk.
const WINDOW: usize = 12;

async fn remember_forced(handle: &PalaceHandle, text: &str) -> Uuid {
    handle
        .remember_with_options(
            text.into(),
            RoomType::General,
            vec![],
            0.5,
            crate::memory_core::retrieval::RememberOptions::forced(),
        )
        .await
        .unwrap()
}

fn words(n: usize, tag: &str) -> String {
    (0..n)
        .map(|i| format!("{tag}{i}"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn chunking_mock(handle: &PalaceHandle) -> crate::embedder::MockEmbedder {
    crate::embedder::MockEmbedder::new(handle.vector_store.dim()).with_chunking(WINDOW)
}

/// Chunk count of `text` at [`WINDOW`], from `chunk_text` directly.
fn chunks_at(handle: &PalaceHandle, text: &str) -> usize {
    use crate::memory_core::embed::Embedder;
    let tok = chunking_mock(handle).chunking_tokenizer().unwrap().unwrap();
    crate::embedder::chunk_text(&tok, text, WINDOW)
        .unwrap()
        .len()
}

fn shape(handle: &PalaceHandle, id: Uuid) -> Option<(usize, u32)> {
    let shapes = handle.vector_store.chunk_shapes().unwrap();
    shapes.get(&id).map(|s| (s.rows, s.max_index))
}

/// Embedder whose second call sleeps past the rebuild budget.
struct SlowSecondCall {
    inner: crate::embedder::MockEmbedder,
    calls: std::sync::atomic::AtomicUsize,
    stall: Duration,
}

#[async_trait::async_trait]
impl crate::memory_core::embed::Embedder for SlowSecondCall {
    async fn embed_batch(&self, texts: &[String]) -> anyhow::Result<Vec<Vec<f32>>> {
        let n = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if n == 1 {
            tokio::time::sleep(self.stall).await;
        }
        self.inner.embed_batch(texts).await
    }
    fn dimension(&self) -> usize {
        self.inner.dimension()
    }
    fn input_window(&self) -> Option<usize> {
        self.inner.input_window()
    }
    fn chunking_tokenizer(
        &self,
    ) -> Result<Option<Arc<crate::embedder::ChunkTokenizer>>, crate::embedder::ChunkError> {
        self.inner.chunking_tokenizer()
    }
}

/// The Architect's PR B ruling: the dream rebuild must keep every chunk.
#[tokio::test]
async fn rebuild_index_keeps_every_chunk_of_a_long_drawer() {
    let dir = tempfile::tempdir().unwrap();
    let handle = open_handle(dir.path(), "rebuild-chunks");
    let (long_text, short_text) = (words(100, "w"), words(5, "s"));
    let long = remember_forced(&handle, &long_text).await;
    let short = remember_forced(&handle, &short_text).await;
    let k = chunks_at(&handle, &long_text);
    assert!(k > 1, "{k} chunks");
    add_chunk_rows(&handle, long, k - 1);
    let before = (shape(&handle, long), shape(&handle, short));
    assert_eq!(before, (Some((k, (k - 1) as u32)), Some((1, 0))));

    let e: Arc<dyn crate::memory_core::embed::Embedder + Send + Sync> =
        Arc::new(chunking_mock(&handle));
    let rebuilt = super::helpers::rebuild_index_with(
        &handle,
        Some(&e),
        Instant::now(),
        Duration::from_secs(60),
    )
    .await
    .unwrap();
    assert_eq!(rebuilt, 2);
    assert_eq!((shape(&handle, long), shape(&handle, short)), before);
}

#[tokio::test]
async fn rebuild_stopping_at_the_budget_leaves_only_complete_groups() {
    let dir = tempfile::tempdir().unwrap();
    let handle = open_handle(dir.path(), "rebuild-budget");
    let mut ids = Vec::new();
    for tag in ["a", "b", "c", "d"] {
        ids.push(remember_forced(&handle, &words(100, tag)).await);
    }
    let k = chunks_at(&handle, &words(100, "a"));
    let e: Arc<dyn crate::memory_core::embed::Embedder + Send + Sync> = Arc::new(SlowSecondCall {
        inner: chunking_mock(&handle),
        calls: std::sync::atomic::AtomicUsize::new(0),
        stall: Duration::from_secs(2),
    });
    // The second drawer's embed outlasts the budget: two drawers land whole,
    // the loop stops before the third.
    super::helpers::rebuild_index_with(&handle, Some(&e), Instant::now(), Duration::from_secs(1))
        .await
        .unwrap();
    let shapes: Vec<_> = ids.iter().map(|id| shape(&handle, *id)).collect();
    let whole = Some((k, (k - 1) as u32));
    assert_eq!(shapes, vec![whole, whole, None, None]);
}

// ── #9275 fix round: dedup merge and rebuild keep full chunk groups ──────────

/// Drawer `text`, `age_days` old, in redb and the table with its full chunk
/// group under `e`.
async fn put_embedded(
    handle: &Arc<PalaceHandle>,
    text: &str,
    age_days: i64,
    e: &crate::embedder::MockEmbedder,
) -> Uuid {
    let mut d = crate::memory_core::palace::Drawer::new(Uuid::new_v4(), text);
    d.created_at = chrono::Utc::now() - chrono::Duration::days(age_days);
    let id = d.id;
    handle.kg.upsert_drawer(&d).await.unwrap();
    let vecs = crate::memory_core::retrieval::embed_chunks(e, text)
        .await
        .unwrap();
    handle.vector_store.upsert_chunks(id, vecs).await.unwrap();
    handle.add_drawer(d);
    id
}

/// Drawer `text` in redb and the table, with no vector.
async fn put_bare(handle: &Arc<PalaceHandle>, text: &str) -> Uuid {
    let d = crate::memory_core::palace::Drawer::new(Uuid::new_v4(), text);
    let id = d.id;
    handle.kg.upsert_drawer(&d).await.unwrap();
    handle.add_drawer(d);
    id
}

/// #9275 fix round (Fix 1): the merge appended the loser's text but kept the
/// survivor's old vectors, so the survivor read short and its tail was not
/// findable by vector search.
#[tokio::test]
async fn a_dedup_merge_reembeds_the_survivor_as_a_full_chunk_group() {
    use crate::memory_core::embed::Embedder;
    use crate::memory_core::store::vector::VectorStore;
    let dir = tempfile::tempdir().unwrap();
    let handle = open_handle(dir.path(), "merge-chunks");
    let e = chunking_mock(&handle);
    let tail = words(8, "t");
    let survivor = put_embedded(&handle, &words(8, "s"), 0, &e).await;
    let loser = put_embedded(&handle, &tail, 1, &e).await;

    // Threshold -1: the only two drawers always merge; the newer survives.
    let outcome = super::cycle::dedup_pass_with_embedder(
        &handle,
        Instant::now(),
        Duration::from_secs(60),
        -1.0,
        &e,
        Duration::from_secs(60),
    )
    .await
    .unwrap();
    assert_eq!((outcome.merged, outcome.failed), (1, 0));
    let merged = handle.drawers.read()[0].content().to_string();
    assert_eq!(handle.drawers.read().len(), 1);
    assert!(handle.drawers.read().iter().all(|d| d.id != loser));
    let k = chunks_at(&handle, &merged);
    assert!(k >= 2, "fixture: the merged text needs {k} chunks");

    let health = handle.embed_health_with(Some(&e as &dyn Embedder));
    assert!(
        health.short_chunk_ids.is_empty(),
        "short after merge: {:?}",
        health.short_chunk_ids
    );
    assert_eq!(shape(&handle, survivor), Some((k, (k - 1) as u32)));
    // A query naming only the loser's text finds the survivor's tail chunk.
    let q = e.embed_batch(std::slice::from_ref(&tail)).await.unwrap();
    let hits = handle.vector_store.search(&q[0], 1).await.unwrap();
    assert_eq!(hits[0].drawer_id, survivor);
    assert!(hits[0].score > 0.5, "tail not recallable: {:?}", hits[0]);
}

/// A chunking window with a tokenizer that will not load.
struct BrokenTokenizer(crate::embedder::MockEmbedder);

#[async_trait::async_trait]
impl crate::memory_core::embed::Embedder for BrokenTokenizer {
    async fn embed_batch(&self, texts: &[String]) -> anyhow::Result<Vec<Vec<f32>>> {
        self.0.embed_batch(texts).await
    }
    fn dimension(&self) -> usize {
        self.0.dimension()
    }
    fn input_window(&self) -> Option<usize> {
        Some(WINDOW)
    }
    fn chunking_tokenizer(
        &self,
    ) -> Result<Option<Arc<crate::embedder::ChunkTokenizer>>, crate::embedder::ChunkError> {
        Err(crate::embedder::ChunkError::TokenizerMissing {
            path: "/nonexistent/tokenizer.json".into(),
        })
    }
}

/// #9275 fix round (Fix 2): a drawer whose text will not split stopped the
/// rebuild after `reset()`, so every later drawer lost its vectors.
#[tokio::test]
async fn a_rebuild_skips_a_drawer_that_will_not_split_and_rebuilds_the_rest() {
    use crate::memory_core::store::vector_key::MAX_CHUNKS_PER_DRAWER;
    let dir = tempfile::tempdir().unwrap();
    let handle = open_handle(dir.path(), "rebuild-skip");
    // Ten words per chunk: one chunk past the per-drawer cap.
    let bad = put_bare(
        &handle,
        &words((WINDOW - 2) * (MAX_CHUNKS_PER_DRAWER + 1), "x"),
    )
    .await;
    let mut good = Vec::new();
    for tag in ["a", "b"] {
        good.push(remember_forced(&handle, &words(30, tag)).await);
    }
    assert_ne!(handle.drawers.read().last().map(|d| d.id), Some(bad));

    let e: Arc<dyn crate::memory_core::embed::Embedder + Send + Sync> =
        Arc::new(chunking_mock(&handle));
    let result = super::helpers::rebuild_index_with(
        &handle,
        Some(&e),
        Instant::now(),
        Duration::from_secs(60),
    )
    .await;
    assert!(result.is_ok(), "{result:?}");
    let k = chunks_at(&handle, &words(30, "a"));
    for id in good {
        assert_eq!(shape(&handle, id), Some((k, (k - 1) as u32)));
    }
    let health = handle.embed_health_with(Some(e.as_ref()));
    assert_eq!(health.missing_vector_ids, vec![bad]);
}

/// #9275 fix round (Fix 2): no planner means no rebuild, and the index stays.
#[tokio::test]
async fn a_rebuild_whose_planner_cannot_be_built_leaves_the_index_untouched() {
    let dir = tempfile::tempdir().unwrap();
    let handle = open_handle(dir.path(), "rebuild-no-planner");
    let mut ids = Vec::new();
    for tag in ["a", "b"] {
        ids.push(remember_forced(&handle, &words(5, tag)).await);
    }
    let before: Vec<_> = ids.iter().map(|id| shape(&handle, *id)).collect();
    assert!(before.iter().all(Option::is_some));

    let e: Arc<dyn crate::memory_core::embed::Embedder + Send + Sync> =
        Arc::new(BrokenTokenizer(chunking_mock(&handle)));
    let err = super::helpers::rebuild_index_with(
        &handle,
        Some(&e),
        Instant::now(),
        Duration::from_secs(60),
    )
    .await
    .expect_err("no planner, no rebuild");
    assert!(format!("{err:#}").contains("tokenizer"), "{err:#}");
    let after: Vec<_> = ids.iter().map(|id| shape(&handle, *id)).collect();
    assert_eq!(after, before, "the index is untouched");
}
