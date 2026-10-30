//! #10504: a full reindex on a default (deferred-embed) index splits new files
//! at the embedder window, embeds nothing twice, and replaces a changed file's
//! windows.
//!
//! Why: the split sits on the deferred parse, so the hash skip and the
//! remove-then-commit of a changed file must still hold over windows.
//! Test: this file.

use super::window_split_10481_tests::{Counting, DIM, WINDOW};
use super::*;
use crate::core::embed::{Embedder, MockEmbedder};
use crate::core::indexer::CodeIndexer;
use crate::core::registry::{IndexHandle, IndexId, StageStatus};
use crate::core::store::{UsearchStore, VectorStore};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

fn long_source(marker: &str) -> String {
    let body: Vec<String> = (0..40).map(|i| format!("    let v{i} = {i};")).collect();
    format!(
        "fn long_one() {{\n{}\n    // {marker}\n}}\n",
        body.join("\n")
    )
}

/// A deferred handle (the default) over a counting chunking embedder.
fn handle(
    name: &str,
    root: &std::path::Path,
) -> (Arc<IndexHandle>, Arc<AtomicUsize>, Arc<dyn VectorStore>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let embedder: Arc<dyn Embedder> = Arc::new(Counting {
        inner: MockEmbedder::new(DIM).with_chunking(WINDOW),
        calls: Arc::clone(&calls),
    });
    let store: Arc<dyn VectorStore> = Arc::new(UsearchStore::new(DIM).expect("usearch"));
    let id = IndexId::new(name);
    let indexer = CodeIndexer::new(id.0.clone(), root.to_path_buf())
        .with_components(embedder, Arc::clone(&store));
    let handle = IndexHandle::bare(
        id,
        Arc::new(tokio::sync::RwLock::new(indexer)),
        root.to_path_buf(),
    );
    assert!(handle.defer_embed, "precondition: the default is deferred");
    (Arc::new(handle), calls, store)
}

/// Reindex, then wait for the detached catch-up to make the semantic stage ready.
async fn reindex_and_settle(handle: &Arc<IndexHandle>) -> Arc<ReindexProgress> {
    let progress = Arc::new(ReindexProgress::new());
    spawn_reindex_awaitable(handle.clone(), progress.clone(), false)
        .await
        .expect("reindex task must not panic");
    for _ in 0..200 {
        if handle.stages.read().await.semantic.status == StageStatus::Ready {
            return progress;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    panic!("the catch-up never made the semantic stage ready");
}

/// A long file splits once on the first reindex; the second skips it, with no
/// embed call and no new window.
#[tokio::test]
#[serial_test::parallel]
async fn an_unchanged_file_is_not_embedded_or_split_again() {
    let tmp = tempfile::tempdir().expect("tempdir");
    std::fs::write(tmp.path().join("long.rs"), long_source("first")).unwrap();
    let (handle, calls, store) = handle("w10504-unchanged", tmp.path());

    reindex_and_settle(&handle).await;
    let (chunks, split) = {
        let idx = handle.indexer.read().await;
        (
            idx.raw_chunks_snapshot().await.expect("chunks"),
            idx.embed_window_stats().chunks_split(),
        )
    };
    assert_eq!(split, 1, "the long function split at parse time");
    assert!(chunks.len() > 1, "the windows are the corpus rows");
    let ids: Vec<String> = chunks.iter().map(|c| c.id.clone()).collect();
    assert!(store.contains_many(&ids).await.iter().all(|&b| b));
    let embeds = calls.load(Ordering::SeqCst);
    assert!(embeds > 0);

    let second = reindex_and_settle(&handle).await;
    assert_eq!(second.skipped.load(Ordering::Acquire), 1, "hash-skipped");
    assert_eq!(calls.load(Ordering::SeqCst), embeds, "no second embed call");
    let idx = handle.indexer.read().await;
    assert_eq!(
        idx.embed_window_stats().chunks_split(),
        1,
        "no second split"
    );
    assert_eq!(
        idx.raw_chunks_snapshot().await.expect("chunks").len(),
        ids.len()
    );
}

/// A changed file's old windows are replaced, not left beside the new ones.
#[tokio::test]
#[serial_test::parallel]
async fn a_changed_file_supersedes_its_old_windows_on_the_deferred_path() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("long.rs");
    std::fs::write(&path, long_source("firstmarker")).unwrap();
    let (handle, _calls, store) = handle("w10504-changed", tmp.path());
    reindex_and_settle(&handle).await;

    std::fs::write(&path, long_source("secondmarker")).unwrap();
    reindex_and_settle(&handle).await;

    let idx = handle.indexer.read().await;
    let chunks = idx.raw_chunks_snapshot().await.expect("chunks");
    assert!(chunks.len() > 1);
    assert!(chunks.iter().all(|c| !c.content.contains("firstmarker")));
    assert!(chunks.iter().any(|c| c.content.contains("secondmarker")));
    let ids: Vec<String> = chunks.iter().map(|c| c.id.clone()).collect();
    assert!(store.contains_many(&ids).await.iter().all(|&b| b));
    assert_eq!(
        store.len().await.expect("len"),
        ids.len(),
        "no stale vector"
    );
}
