use super::*;
use crate::core::registry::IndexId;
use crate::core::store::UsearchStore;
use crate::core::{CodeIndexer, Embedder};
use std::sync::atomic::{AtomicUsize, Ordering};

struct RejectingEmbedder(Arc<AtomicUsize>);

#[async_trait::async_trait]
impl Embedder for RejectingEmbedder {
    async fn embed(&self, _: &str) -> anyhow::Result<Vec<f32>> {
        self.0.fetch_add(1, Ordering::SeqCst);
        anyhow::bail!("unexpected context embedding")
    }
    async fn embed_batch(&self, _: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
        self.0.fetch_add(1, Ordering::SeqCst);
        anyhow::bail!("unexpected warmup embedding")
    }
    fn dimension(&self) -> usize {
        32
    }
}

#[tokio::test]
async fn experiment_reindex_warmup_and_context_refresh_never_embed() {
    let fixture = tempfile::tempdir().unwrap();
    std::fs::write(
        fixture.path().join("README.md"),
        "# Fixture\nIndexes source code with lexical search and a knowledge graph.\n",
    )
    .unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let mut indexer = CodeIndexer::new("experiment-stages", fixture.path().to_path_buf())
        .with_components(
            Arc::new(RejectingEmbedder(calls.clone())),
            Arc::new(UsearchStore::new(32).unwrap()),
        );
    indexer.skip_vector = true;
    let mut handle = IndexHandle::bare(
        IndexId::new("experiment-stages"),
        Arc::new(tokio::sync::RwLock::new(indexer)),
        fixture.path().to_path_buf(),
    );
    handle.skip_vector = true;
    let handle = Arc::new(handle);
    // Exercise the actual two helpers reached outside the batch ingestion phase.
    handle.indexer.read().await.warm_embedder().await;
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "warmup must respect skip_vector"
    );
    refresh_context_embedding(&handle).await;
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "context refresh must respect skip_vector"
    );
    assert!(handle.context_embedding.read().await.is_none());
    assert!(
        handle
            .context_summary
            .read()
            .await
            .as_ref()
            .is_some_and(|summary| summary.contains("Fixture")),
        "metadata is still extracted even with vectors disabled"
    );
    // Control: vector-enabled indexes still call both entrypoints.
    handle.indexer.write().await.skip_vector = false;
    handle.indexer.read().await.warm_embedder().await;
    refresh_context_embedding(&handle).await;
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}
