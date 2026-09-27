//! A glob-narrowed grep over an evicted index answers from the durable corpus
//! without rehydrating it (#8266).
//!
//! Why: grep took its file set from `raw_chunks_snapshot`, which refills the
//! whole in-memory chunk map before the glob filter runs — 117,773 chunks
//! loaded for a one-file lookup.
//! What: indexes two files into a real redb corpus, evicts the in-memory
//! caches, greps one file by glob, and counts the chunks the in-memory map
//! holds afterwards. Before the fix that count is the whole corpus; after it
//! the map stays empty.
//! Test: this file.

use super::*;
use axum::Json;

/// Measures how many chunks a cold, one-file grep materializes in memory:
/// zero, while still returning exactly the matches in the selected file.
#[tokio::test]
#[serial_test::serial]
async fn a_cold_narrow_grep_does_not_materialize_the_corpus() {
    use crate::core::corpus::CorpusStore;
    use crate::core::indexer::CodeIndexer;
    use crate::core::registry::{IndexHandle, IndexId, IndexRegistry, StageStatus};

    let tmp = tempfile::tempdir().expect("tempdir");
    let files = [
        ("src/a.rs", "fn authenticate_user() {}\n"),
        ("src/b.rs", "fn authenticate_user_again() {}\n"),
    ];
    std::fs::create_dir_all(tmp.path().join("src")).expect("create src dir");
    for (rel, body) in files {
        std::fs::write(tmp.path().join(rel), body).expect("write source file");
    }
    let corpus = Arc::new(CorpusStore::open(&tmp.path().join("index.redb")).expect("open corpus"));
    let mut indexer = CodeIndexer::new("grep-cold-8266", tmp.path());
    indexer.set_corpus_store(Arc::clone(&corpus));
    let batch: Vec<(String, String)> = files
        .iter()
        .map(|(rel, body)| ((*rel).to_string(), (*body).to_string()))
        .collect();
    indexer
        .index_files_batch(&batch)
        .await
        .expect("index batch");

    let registry = IndexRegistry::new();
    let handle = IndexHandle::bare(
        IndexId::new("grep-cold-8266"),
        Arc::new(tokio::sync::RwLock::new(indexer)),
        tmp.path().to_path_buf(),
    );
    let indexer = Arc::clone(&handle.indexer);
    let stages = Arc::clone(&handle.stages);
    registry.register(handle);
    stages.write().await.lexical.status = StageStatus::Ready;
    let state = Arc::new(SearchAppState::new(registry));

    let resident = indexer.read().await.chunk_count();
    assert!(resident > 0, "the corpus is resident before eviction");
    let evicted = indexer.read().await.reclaim_memory_now().await;
    assert!(evicted > 0, "eviction dropped the in-memory caches");
    assert_eq!(indexer.read().await.chunk_count(), 0);

    let req: crate::service::grep::GrepRequest = serde_json::from_value(
        serde_json::json!({ "pattern": "authenticate_user", "glob": "src/a.rs" }),
    )
    .expect("grep request");
    let Json(resp) = super::files::grep_handler(
        axum::extract::State(Arc::clone(&state)),
        axum::extract::Path("grep-cold-8266".to_string()),
        axum::extract::Json(req),
    )
    .await
    .expect("a cold grep answers");

    assert!(!resp.matches.is_empty(), "the pattern is found in src/a.rs");
    assert!(
        resp.matches.iter().all(|m| m.file == "src/a.rs"),
        "the glob confines matches to src/a.rs: {:?}",
        resp.matches.iter().map(|m| &m.file).collect::<Vec<_>>()
    );
    let meta = resp.meta.expect("a glob request carries meta");
    assert_eq!(meta.corpus_files, 2, "both indexed files were listed");
    assert_eq!(meta.glob_matched_files, 1);
    assert_eq!(
        indexer.read().await.chunk_count(),
        0,
        "a narrow grep must not rehydrate the corpus into memory"
    );
}
