/// Context embedding tests for the reindex pipeline (issue #112).
use super::*;

#[tokio::test]
async fn context_embedding_populated_after_reindex() {
    use crate::core::embed::{Embedder, MockEmbedder};
    use crate::core::store::{UsearchStore, VectorStore};

    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().to_path_buf();
    fs::write(root.join("lib.rs"), "fn hello() {}\n").unwrap();
    fs::write(
        root.join("README.md"),
        "# proj\n\nA test project for #112.\n",
    )
    .unwrap();

    let dim = 32;
    let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder::new(dim));
    let store: Arc<dyn VectorStore> = Arc::new(UsearchStore::new(dim).expect("usearch new"));
    let indexer = CodeIndexer::new("ctx-test", root.clone()).with_components(embedder, store);

    let handle = Arc::new(IndexHandle::bare(
        IndexId::new("ctx-test"),
        Arc::new(tokio::sync::RwLock::new(indexer)),
        root.clone(),
    ));
    let progress = Arc::new(ReindexProgress::new());
    spawn_reindex(handle.clone(), progress.clone(), false);

    for _ in 0..100 {
        if progress.status.load() == ReindexStatus::Complete {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert_eq!(progress.status.load(), ReindexStatus::Complete);

    let ctx = handle.context_embedding.read().await.clone();
    assert!(
        ctx.is_some(),
        "context_embedding must be populated when metadata is present and embedder is wired"
    );
    assert_eq!(ctx.unwrap().len(), dim, "embedding must have embedder dim");

    let summary = handle.context_summary.read().await.clone();
    assert!(summary.is_some(), "context_summary must be populated");
    let s = summary.unwrap();
    assert!(s.contains("proj") || s.contains("README"));
}

#[tokio::test]
async fn context_embedding_none_when_no_metadata() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().to_path_buf();
    fs::write(root.join("lib.rs"), "fn hello() {}\n").unwrap();

    let indexer = CodeIndexer::new("no-meta", root.clone());
    let handle = Arc::new(IndexHandle::bare(
        IndexId::new("no-meta"),
        Arc::new(tokio::sync::RwLock::new(indexer)),
        root.clone(),
    ));
    let progress = Arc::new(ReindexProgress::new());
    spawn_reindex(handle.clone(), progress.clone(), false);

    for _ in 0..100 {
        if progress.status.load() == ReindexStatus::Complete {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert_eq!(progress.status.load(), ReindexStatus::Complete);
    assert!(handle.context_embedding.read().await.is_none());
    assert!(handle.context_summary.read().await.is_none());
}
