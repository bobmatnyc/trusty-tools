//! #9275 PR C: the chunked embed+store primitive, with explicit embedders.
//!
//! Why: the shared embedder is a process-wide cell this binary seeds with a
//! non-chunking mock, so each test passes its own embedder instead.
//! What: a counting wrapper around `MockEmbedder::with_chunking` and a store
//! on disk; each test reads the store's row shape after the call.
//! Test: this file IS the tests.

use super::deferred_embed::{EmbedLoss, RetryPolicy, embed_and_store};
use super::embed_store::{
    chunked_embed_timeout, clamp_chunked_timeout, embed_chunks, embed_chunks_within,
    expected_chunk_count, plan_chunks,
};
use super::handle::PalaceHandle;
use trusty_common::embedder::{ChunkError, ChunkTokenizer, MockEmbedder, chunk_text};
use crate::engine::embed::Embedder;
use crate::engine::palace::{Drawer, PalaceId};
use crate::engine::store::{kg::KnowledgeGraph, vector::UsearchStore};
use anyhow::Result;
use async_trait::async_trait;
use parking_lot::Mutex;
use std::sync::Arc;
use uuid::Uuid;

/// Dimension of every test vector.
pub(super) const DIM: usize = 16;

/// `n` distinct words, space-separated.
pub(super) fn words(n: usize) -> String {
    (0..n)
        .map(|i| format!("w{i}"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Chunks `text` makes at `window` under the chunking mock's tokenizer —
/// computed with `chunk_text` directly, not through the code under test.
pub(super) fn chunks_at(window: usize, text: &str) -> usize {
    let mock = MockEmbedder::new(DIM).with_chunking(window);
    let tok = mock.chunking_tokenizer().unwrap().expect("tokenizer");
    chunk_text(&tok, text, window).expect("chunk").len()
}

/// Embedder wrapper that records each `embed_batch` input count and can drop
/// the last vector or fail every call.
pub(super) struct Counting {
    pub inner: MockEmbedder,
    pub calls: Mutex<Vec<usize>>,
    pub drop_one: bool,
    pub fail: bool,
}

impl Counting {
    pub(super) fn chunking(window: usize) -> Self {
        Self {
            inner: MockEmbedder::new(DIM).with_chunking(window),
            calls: Mutex::new(Vec::new()),
            drop_one: false,
            fail: false,
        }
    }
}

#[async_trait]
impl Embedder for Counting {
    async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        self.calls.lock().push(texts.len());
        if self.fail {
            anyhow::bail!("simulated embedder failure");
        }
        let mut v = self.inner.embed_batch(texts).await?;
        if self.drop_one {
            v.pop();
        }
        Ok(v)
    }
    fn dimension(&self) -> usize {
        DIM
    }
    fn input_window(&self) -> Option<usize> {
        self.inner.input_window()
    }
    fn chunking_tokenizer(&self) -> Result<Option<Arc<ChunkTokenizer>>, ChunkError> {
        self.inner.chunking_tokenizer()
    }
}

/// A window with a tokenizer that will not load.
pub(super) struct NoTokenizer;

#[async_trait]
impl Embedder for NoTokenizer {
    async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        Ok(texts.iter().map(|_| vec![1.0; DIM]).collect())
    }
    fn dimension(&self) -> usize {
        DIM
    }
    fn input_window(&self) -> Option<usize> {
        Some(32)
    }
    fn chunking_tokenizer(&self) -> Result<Option<Arc<ChunkTokenizer>>, ChunkError> {
        Err(ChunkError::TokenizerMissing {
            path: "/nonexistent/tokenizer.json".into(),
        })
    }
}

/// A disk palace with no drawers, its vector store at [`DIM`].
pub(super) fn handle(dir: &std::path::Path) -> PalaceHandle {
    let vs = UsearchStore::new(dir.join("idx.usearch"), DIM).unwrap();
    let kg = KnowledgeGraph::open(&dir.join("kg.db")).unwrap();
    let mut handle = PalaceHandle::new(PalaceId::new("chunk-store"), String::new(), vs, kg);
    handle.data_dir = Some(dir.to_path_buf());
    handle
}

/// Add a drawer with `text` and no vector.
pub(super) fn add(handle: &PalaceHandle, text: &str) -> Uuid {
    let drawer = Drawer::new(Uuid::new_v4(), text);
    let id = drawer.id;
    handle.add_drawer(drawer);
    id
}

/// `(rows, max_index)` of drawer `id`, or `None` with no row.
pub(super) fn shape(handle: &PalaceHandle, id: Uuid) -> Option<(usize, u32)> {
    let shapes = handle.vector_store.chunk_shapes().unwrap();
    shapes.get(&id).map(|s| (s.rows, s.max_index))
}

fn arc(e: impl Embedder + 'static) -> Arc<dyn Embedder + Send + Sync> {
    Arc::new(e)
}

#[tokio::test]
async fn short_text_is_one_chunk_and_one_embed_input() {
    let e = Counting::chunking(32);
    assert_eq!(plan_chunks(&e, "a few words").unwrap(), vec!["a few words"]);
    let v = embed_chunks(&e, "a few words").await.unwrap();
    assert_eq!(v.len(), 1);
    assert_eq!(*e.calls.lock(), vec![1]);
}

#[tokio::test]
async fn embedder_without_a_window_never_chunks() {
    let text = words(500);
    let plain = MockEmbedder::new(DIM);
    assert_eq!(plan_chunks(&plain, &text).unwrap().len(), 1);
    let window_only = MockEmbedder::new(DIM).with_input_window(12);
    assert_eq!(plan_chunks(&window_only, &text).unwrap().len(), 1);
    assert_eq!(embed_chunks(&plain, &text).await.unwrap().len(), 1);
}

#[tokio::test]
async fn long_text_embeds_every_chunk_in_one_batch() {
    let e = Counting::chunking(12);
    let text = words(100);
    let k = chunks_at(12, &text);
    assert!(k > 1, "{k} chunks");
    let v = embed_chunks(&e, &text).await.unwrap();
    assert_eq!(v.len(), k);
    assert_eq!(*e.calls.lock(), vec![k], "one embed_batch with every chunk");
}

#[tokio::test]
async fn chunk_count_above_the_cap_is_a_hard_error_before_any_embed() {
    let dir = tempfile::tempdir().unwrap();
    let h = handle(dir.path());
    let id = Uuid::new_v4();
    h.vector_store
        .upsert_chunks(id, vec![vec![1.0; DIM]])
        .await
        .unwrap();
    // Window 3 leaves one word per chunk: 4097 words is one past the cap.
    let counting = Counting::chunking(3);
    let text = words(4097);
    assert!(plan_chunks(&counting, &text).is_err());
    let e = arc(counting);
    let loss = embed_and_store(&e, &h.vector_store, id, &text, &RetryPolicy::instant(1))
        .await
        .expect_err("over the cap");
    assert!(matches!(loss, EmbedLoss::Embed { .. }), "{loss:?}");
    assert_eq!(shape(&h, id), Some((1, 0)), "the old row is untouched");
}

#[tokio::test]
async fn embed_batch_returning_the_wrong_count_is_an_error_and_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let h = handle(dir.path());
    let id = Uuid::new_v4();
    let mut short_return = Counting::chunking(12);
    short_return.drop_one = true;
    assert!(embed_chunks(&short_return, &words(50)).await.is_err());
    let e = arc(short_return);
    let loss = embed_and_store(&e, &h.vector_store, id, "one", &RetryPolicy::instant(2))
        .await
        .expect_err("a short return is an error");
    assert!(matches!(loss, EmbedLoss::Embed { .. }), "{loss:?}");
    assert_eq!(shape(&h, id), None, "no partial group");
}

#[tokio::test]
async fn unavailable_tokenizer_with_a_window_is_reported_not_swallowed() {
    let err = embed_chunks(&NoTokenizer, "a short text")
        .await
        .expect_err("a known window with no tokenizer must not embed");
    assert!(format!("{err:#}").contains("tokenizer"), "{err:#}");

    let dir = tempfile::tempdir().unwrap();
    let h = handle(dir.path());
    let id = add(&h, "a drawer with a vector");
    h.vector_store
        .upsert_chunks(id, vec![vec![1.0; DIM]])
        .await
        .unwrap();
    let health = h.embed_health_with(Some(&NoTokenizer as &dyn Embedder));
    assert!(
        health.chunk_check.unavailable_reason().is_some(),
        "{:?}",
        health.chunk_check
    );
    assert!(!health.is_healthy());
}

#[tokio::test]
async fn embed_and_store_writes_a_chunk_group_and_a_shorter_rewrite_shrinks_it() {
    let dir = tempfile::tempdir().unwrap();
    let h = handle(dir.path());
    let id = Uuid::new_v4();
    let e = arc(Counting::chunking(12));
    let text = words(100);
    let k = chunks_at(12, &text);
    let policy = RetryPolicy::instant(1);
    embed_and_store(&e, &h.vector_store, id, &text, &policy)
        .await
        .unwrap();
    assert_eq!(shape(&h, id), Some((k, (k - 1) as u32)));
    embed_and_store(&e, &h.vector_store, id, "now short", &policy)
        .await
        .unwrap();
    assert_eq!(shape(&h, id), Some((1, 0)));
}

#[tokio::test]
async fn expected_chunk_count_equals_rows_the_pipeline_writes() {
    let dir = tempfile::tempdir().unwrap();
    let h = handle(dir.path());
    let counting = Counting::chunking(12);
    let policy = RetryPolicy::instant(1);
    let e = arc(Counting::chunking(12));
    for n in [0, 1, 9, 10, 11, 19, 28, 37, 100, 251] {
        let text = words(n);
        let id = Uuid::new_v4();
        embed_and_store(&e, &h.vector_store, id, &text, &policy)
            .await
            .unwrap();
        let expected = expected_chunk_count(&counting, &text).unwrap();
        assert_eq!(expected, chunks_at(12, &text).max(1), "{n} words");
        assert_eq!(shape(&h, id).map(|s| s.0), Some(expected), "{n} words");
    }
}

/// Embedder that spends `per_input` on every text of each `embed_batch` call.
struct PerInputLatency {
    inner: MockEmbedder,
    per_input: std::time::Duration,
}

#[async_trait]
impl Embedder for PerInputLatency {
    async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        let n = u32::try_from(texts.len()).unwrap_or(u32::MAX);
        tokio::time::sleep(self.per_input.saturating_mul(n)).await;
        self.inner.embed_batch(texts).await
    }
    fn dimension(&self) -> usize {
        DIM
    }
    fn input_window(&self) -> Option<usize> {
        self.inner.input_window()
    }
    fn chunking_tokenizer(&self) -> Result<Option<Arc<ChunkTokenizer>>, ChunkError> {
        self.inner.chunking_tokenizer()
    }
}

fn latency(per_input: std::time::Duration) -> PerInputLatency {
    PerInputLatency {
        inner: MockEmbedder::new(DIM).with_chunking(12),
        per_input,
    }
}

/// #9275 fix round (Fix 3): a ceiling sized for one input timed out a drawer
/// whose chunks together outlast it, so the drawer got no vector at all.
#[tokio::test]
async fn a_multi_chunk_drawer_inside_the_scaled_embed_timeout_gets_every_vector() {
    let per_chunk = std::time::Duration::from_millis(300);
    let e = latency(per_chunk / 2);
    let text = words(40);
    let n = plan_chunks(&e, &text).unwrap().len();
    assert!(n >= 4, "fixture: {n} chunks");
    // n x 150 ms outlasts one 300 ms ceiling and fits n x 300 ms.
    let vecs = embed_chunks_within(&e, &text, per_chunk)
        .await
        .expect("the ceiling covers every chunk");
    assert_eq!(vecs.len(), n);
}

/// #9275 fix round (Fix 3): the scaled ceiling is still a bound.
#[tokio::test]
async fn a_wedged_chunked_embed_ends_at_the_scaled_bound() {
    let per_chunk = std::time::Duration::from_millis(50);
    let e = latency(std::time::Duration::from_secs(3600));
    let text = words(40);
    let n = u32::try_from(plan_chunks(&e, &text).unwrap().len()).unwrap();
    let started = std::time::Instant::now();
    let err = embed_chunks_within(&e, &text, per_chunk)
        .await
        .expect_err("a wedged embedder must time out");
    let took = started.elapsed();
    assert!(format!("{err:#}").contains("timed out"), "{err:#}");
    assert!(
        took >= per_chunk * n && took < std::time::Duration::from_secs(30),
        "{took:?} for {n} chunks"
    );
}

/// #9735 Q3: 4096 chunks at 30 s each was a 34 h ceiling.
#[test]
fn chunked_embed_timeout_is_clamped_at_the_cap() {
    let cap = std::time::Duration::from_secs(
        crate::engine::timeouts::DEFAULT_EMBED_CHUNKED_CAP_SECS,
    );
    let got = clamp_chunked_timeout(std::time::Duration::from_secs(30), 4096, cap);
    assert_eq!(got, std::time::Duration::from_secs(600));
    // Below the cap the scaled bound is unchanged.
    let small = clamp_chunked_timeout(std::time::Duration::from_secs(30), 4, cap);
    assert_eq!(small, std::time::Duration::from_secs(120));
}

/// #9735 Q3: the operator's `TRUSTY_EMBED_BATCH_TIMEOUT_SECS` is the floor.
#[test]
fn chunked_embed_timeout_never_drops_below_the_operators_per_chunk() {
    let cap = std::time::Duration::from_secs(600);
    let per_chunk = std::time::Duration::from_secs(900);
    assert_eq!(clamp_chunked_timeout(per_chunk, 4096, cap), per_chunk);
    assert_eq!(clamp_chunked_timeout(per_chunk, 1, cap), per_chunk);
}

/// #9735 Q3: the cap is `TRUSTY_EMBED_CHUNKED_CAP_SECS`, not a constant.
#[test]
fn chunked_embed_timeout_honours_the_cap_env() {
    let _env = trusty_common::data_dir::ENV_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    // SAFETY: serialised by `ENV_LOCK`, held above for the whole test.
    unsafe { std::env::set_var("TRUSTY_EMBED_CHUNKED_CAP_SECS", "60") };
    let got = chunked_embed_timeout(std::time::Duration::from_secs(30), 4096);
    unsafe { std::env::remove_var("TRUSTY_EMBED_CHUNKED_CAP_SECS") };
    assert_eq!(got, std::time::Duration::from_secs(60));
}
