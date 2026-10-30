//! #10481: a full reindex on an inline-embed index splits new files at the
//! embedder window, and a file whose content is unchanged is not embedded again.
//!
//! Why: the split sits on the parse path, so the hash skip that spares an
//! unchanged file must still spare it from the embedder and from a second split.
//! Test: this file.

use super::*;
use crate::core::embed::{Embedder, MockEmbedder};
use crate::core::indexer::CodeIndexer;
use crate::core::registry::{IndexHandle, IndexId};
use crate::core::store::{UsearchStore, VectorStore};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

const DIM: usize = 32;
const WINDOW: usize = 20;

/// A chunking mock that counts `embed_batch` calls.
struct Counting {
    inner: MockEmbedder,
    calls: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl Embedder for Counting {
    async fn embed(&self, text: &str) -> anyhow::Result<Vec<f32>> {
        Embedder::embed(&self.inner, text).await
    }
    async fn embed_batch(&self, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
        // The reindex's own pre-warm sends `["warm"]`; it embeds no file.
        if texts != ["warm"] {
            self.calls.fetch_add(1, Ordering::SeqCst);
        }
        Embedder::embed_batch(&self.inner, texts).await
    }
    fn dimension(&self) -> usize {
        DIM
    }
    fn input_window(&self) -> Option<usize> {
        Embedder::input_window(&self.inner)
    }
    fn chunking_tokenizer(
        &self,
    ) -> Result<
        Option<Arc<trusty_common::embedder::ChunkTokenizer>>,
        trusty_common::embedder::ChunkError,
    > {
        Embedder::chunking_tokenizer(&self.inner)
    }
}

/// A long file splits on the first reindex; an unchanged one is skipped on the
/// second, with no embed call and no new window.
///
/// Test: this test.
#[tokio::test]
#[serial_test::parallel]
async fn an_unchanged_file_is_not_embedded_again() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().to_path_buf();
    let body: Vec<String> = (0..40).map(|i| format!("    let v{i} = {i};")).collect();
    std::fs::write(
        root.join("long.rs"),
        format!("fn long_one() {{\n{}\n}}\n", body.join("\n")),
    )
    .unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let embedder: Arc<dyn Embedder> = Arc::new(Counting {
        inner: MockEmbedder::new(DIM).with_chunking(WINDOW),
        calls: Arc::clone(&calls),
    });
    let store: Arc<dyn VectorStore> = Arc::new(UsearchStore::new(DIM).expect("usearch"));
    let id = IndexId::new("window-split-10481-reindex");
    let indexer = CodeIndexer::new(id.0.clone(), root.clone()).with_components(embedder, store);
    let mut handle = IndexHandle::bare(
        id,
        Arc::new(tokio::sync::RwLock::new(indexer)),
        root.clone(),
    );
    handle.defer_embed = false;
    let handle = Arc::new(handle);

    let first = Arc::new(ReindexProgress::new());
    spawn_reindex_awaitable(handle.clone(), first.clone(), false)
        .await
        .expect("reindex task must not panic");
    let (after_first, split_first) = {
        let idx = handle.indexer.read().await;
        (
            idx.raw_chunks_snapshot().await.expect("chunks"),
            idx.embed_window_stats().chunks_split(),
        )
    };
    assert_eq!(
        split_first, 1,
        "the long function split on the first reindex"
    );
    assert!(after_first.len() > 1, "the windows are the corpus rows");
    let embeds = calls.load(Ordering::SeqCst);
    assert!(embeds > 0, "the first reindex embedded");

    let second = Arc::new(ReindexProgress::new());
    spawn_reindex_awaitable(handle.clone(), second.clone(), false)
        .await
        .expect("reindex task must not panic");
    assert_eq!(second.skipped.load(Ordering::Acquire), 1, "hash-skipped");
    assert_eq!(calls.load(Ordering::SeqCst), embeds, "no second embed call");
    let idx = handle.indexer.read().await;
    assert_eq!(
        idx.embed_window_stats().chunks_split(),
        1,
        "no second split"
    );
    let mut ids_before: Vec<_> = after_first.iter().map(|c| c.id.clone()).collect();
    let mut ids_after: Vec<_> = idx
        .raw_chunks_snapshot()
        .await
        .expect("chunks")
        .into_iter()
        .map(|c| c.id)
        .collect();
    ids_before.sort();
    ids_after.sort();
    assert_eq!(ids_before, ids_after, "the corpus rows are unchanged");
}
