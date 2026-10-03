//! In-process re-open of a transiently quarantined corpus (#8085, #8958).
//!
//! Why: a contention quarantine used to last until a daemon restart, although
//! its status text said it self-heals (#8085), and a warm-boot open that lost
//! the lock race left the index without a durable store, so no reindex wrote
//! its content hashes again (#8958).
//! What: each test builds a colocated index with the production loader, seeds
//! one chunk, detaches the corpus, quarantines it as `Contention`, and holds the
//! redb lock with a second `CorpusStore` — the real `DatabaseAlreadyOpen`
//! shape. Releasing that holder is the event under test.
//! Test: this module.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::RwLock;

use super::corpus_reopen::{reopen_sweep_once, try_reopen_quarantined_corpus, ReopenOutcome};
use crate::core::chunker::{ChunkType, RawChunk};
use crate::core::corpus::{CorpusOpenFailure, CorpusStore};
use crate::core::embed::{Embedder, MockEmbedder};
use crate::core::registry::{IndexHandle, IndexId, IndexRegistry, StageStatus};
use crate::service::persistence::{corpus_redb_path_for_entry, PersistedIndex};
use crate::service::persistence_loader::build_indexer_from_entry;
use crate::service::reindex::ReindexStatus;
use crate::service::server::SearchAppState;

fn chunk(id: &str) -> RawChunk {
    RawChunk {
        id: id.to_string(),
        file: "src/lib.rs".to_string(),
        start_line: 1,
        end_line: 1,
        content: "fn reopen_marker() {}".to_string(),
        function_name: None,
        language: Some("rust".to_string()),
        chunk_type: ChunkType::Code,
        calls: Vec::new(),
        inherits_from: Vec::new(),
        chunk_depth: 0,
        parent_chunk_id: None,
        child_chunk_ids: Vec::new(),
        nlp_keywords: Vec::new(),
        nlp_code_refs: Vec::new(),
        virtual_terms: Vec::new(),
    }
}

/// A registered index quarantined as `Contention`, plus the redb path and a
/// live holder of its lock. Dropping the holder releases the lock.
async fn contended_index(id: &str, root: &Path) -> (Arc<SearchAppState>, PathBuf, CorpusStore) {
    std::fs::create_dir_all(root.join("src")).expect("src dir");
    std::fs::write(root.join("src/lib.rs"), "fn reopen_marker() {}\n").expect("source file");
    let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder::new(8));
    let mut entry = PersistedIndex::new(id.to_string(), root.to_path_buf());
    entry.colocated = true;
    let redb = corpus_redb_path_for_entry(&entry).expect("redb path");
    let mut indexer = build_indexer_from_entry(&entry, &embedder)
        .await
        .expect("build indexer");
    {
        let corpus = indexer.take_corpus_store().expect("corpus wired at build");
        corpus
            .upsert_chunks(&[chunk("src/lib.rs:1:1")])
            .expect("seed chunk");
    }
    indexer.quarantine_detached_corpus(CorpusOpenFailure::Contention, "test holder");
    let holder = CorpusStore::open(&redb).expect("holder takes the lock");
    let registry = IndexRegistry::new();
    registry.register(IndexHandle::bare(
        IndexId::new(id),
        Arc::new(RwLock::new(indexer)),
        root.to_path_buf(),
    ));
    (Arc::new(SearchAppState::new(registry)), redb, holder)
}

fn handle(state: &SearchAppState, id: &str) -> Arc<IndexHandle> {
    state.registry.get(&IndexId::new(id)).expect("registered")
}

/// #8958 acceptance 2, and the error arm of the re-open: a lock that is never
/// released leaves the index quarantined with its transient kind, which is what
/// `/health` and `GET /indexes/:id/status` report as degraded.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_lock_that_never_releases_keeps_the_index_degraded() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (state, _redb, _holder) = contended_index("held-8958", dir.path()).await;
    let h = handle(&state, "held-8958");

    let outcome = try_reopen_quarantined_corpus(&h, Duration::from_millis(120)).await;
    assert!(
        matches!(outcome, ReopenOutcome::StillUnavailable(_)),
        "{outcome:?}"
    );
    assert_eq!(reopen_sweep_once(&state).await, 0);
    let indexer = h.indexer.read().await;
    assert!(
        indexer.is_write_quarantined(),
        "the quarantine must stay up"
    );
    assert!(!indexer.has_corpus_store(), "no corpus may be wired");
    assert_eq!(
        indexer.corpus_open_failure,
        Some(CorpusOpenFailure::Contention)
    );
}

/// #8085: once the other opener releases the file, the background sweep lifts
/// the quarantine, reloads the chunks, and re-derives the stages, with no
/// restart. On pre-fix code nothing re-attempts the open.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_sweep_lifts_a_contention_quarantine_once_the_lock_is_released() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (state, _redb, holder) = contended_index("sweep-8085", dir.path()).await;
    let h = handle(&state, "sweep-8085");
    assert_eq!(reopen_sweep_once(&state).await, 0, "held: nothing re-opens");

    drop(holder);
    assert_eq!(
        reopen_sweep_once(&state).await,
        1,
        "released: the sweep re-opens"
    );
    {
        let indexer = h.indexer.read().await;
        assert!(!indexer.is_write_quarantined());
        assert!(indexer.has_corpus_store());
        assert_eq!(indexer.corpus_open_failure, None);
        assert_eq!(indexer.chunk_count(), 1, "seeded row reloaded");
    }
    assert_eq!(h.stages.read().await.lexical.status, StageStatus::Ready);
    assert_eq!(
        try_reopen_quarantined_corpus(&h, Duration::from_millis(50)).await,
        ReopenOutcome::NotQuarantined
    );
}

/// #8958 acceptance 1 and 3: a reindex requested while the lock is held is
/// refused as retryable; after the holder releases it, the same request
/// re-attaches the corpus, and the reindex writes content hashes into the redb.
/// On pre-fix code the second request is refused too.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_reindex_after_the_holder_releases_reattaches_the_corpus() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (state, _redb, holder) = contended_index("reindex-8958", dir.path()).await;
    let h = handle(&state, "reindex-8958");

    let (status, body) = crate::service::server::reindex_report(&state, "reindex-8958", None)
        .await
        .expect_err("held: the reindex is refused");
    assert_eq!(status, axum::http::StatusCode::CONFLICT, "{body}");
    assert_eq!(
        body["retryable"], true,
        "a transient quarantine is retried: {body}"
    );

    drop(holder);
    let body = crate::service::server::reindex_report(&state, "reindex-8958", None)
        .await
        .map_err(|(s, b)| format!("{s}: {b}"))
        .expect("released: the reindex re-attaches and is queued");
    assert_eq!(body["queued"], true, "{body}");
    assert!(!h.indexer.read().await.is_write_quarantined());

    let progress = state
        .reindex_progress
        .get(&IndexId::new("reindex-8958"))
        .map(|p| Arc::clone(&p))
        .expect("progress entry");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(120);
    while progress.status.load() == ReindexStatus::Running {
        assert!(
            tokio::time::Instant::now() < deadline,
            "reindex never ended"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(progress.status.load(), ReindexStatus::Complete);
    let corpus = h.indexer.read().await.corpus_store().expect("corpus wired");
    let hashes = corpus.load_file_hashes().expect("read hashes");
    assert!(
        hashes.iter().any(|(f, _)| f.ends_with("lib.rs")),
        "#8958: the reindex must write content hashes: {hashes:?}"
    );
}

/// #8085: a schema chain that failed at boot for lack of a corpus is re-run
/// once the corpus re-opens, which clears its recorded fault.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_reopen_reruns_a_schema_chain_that_failed_for_lack_of_a_corpus() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (state, _redb, holder) = contended_index("chain-8085", dir.path()).await;
    let h = handle(&state, "chain-8085");
    h.indexer.read().await.record_migration_failure(
        crate::core::indexer::MIGRATION_STAGE_SCHEMA_CHAIN,
        "cannot write schema_version: no durable corpus on this index".to_string(),
    );
    drop(holder);

    assert!(matches!(
        try_reopen_quarantined_corpus(&h, Duration::from_millis(200)).await,
        ReopenOutcome::Reopened { .. }
    ));
    let deadline = tokio::time::Instant::now() + Duration::from_secs(120);
    while !h.indexer.read().await.migration_faults().is_empty() {
        assert!(tokio::time::Instant::now() < deadline, "chain never re-ran");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}
