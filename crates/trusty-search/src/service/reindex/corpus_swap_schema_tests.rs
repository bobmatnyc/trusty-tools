//! Migration stamps must survive corpus staging and cold reopen.
//!
//! Why: force rebuilds derived rows, but replaying already-applied migrations
//! after promotion can change their vector coverage on the next startup.
//! What: exercises real staging, promotion, abort, and the migration entry point
//! with disposable colocated redb/HNSW stores; no daemon or model download.
//! Test: this module.

use super::{abort_staged_corpus_swap, begin_staged_corpus_swap, commit_staged_corpus_swap};
use crate::core::chunker::{ChunkType, RawChunk};
use crate::core::corpus::CorpusStore;
use crate::core::embed::MockEmbedder;
use crate::core::indexer::CodeIndexer;
use crate::core::migration::{run_migrations_exclusive, MigrationRegistry, CURRENT_SCHEMA_VERSION};
use crate::core::registry::{IndexHandle, IndexId};
use crate::core::store::{UsearchStore, VectorStore};
use crate::service::reindex::{spawn_reindex_awaitable, ReindexProgress, ReindexStatus};
use redb::ReadableDatabase;
use std::path::Path;
use std::sync::Arc;

fn chunk(id: &str) -> RawChunk {
    RawChunk {
        id: id.into(),
        file: "deleted.rs".into(),
        start_line: 1,
        end_line: 1,
        content: "fn deleted() {}".into(),
        function_name: None,
        language: Some("rust".into()),
        chunk_type: ChunkType::Code,
        calls: vec![],
        inherits_from: vec![],
        chunk_depth: 0,
        parent_chunk_id: None,
        child_chunk_ids: vec![],
        nlp_keywords: vec![],
        nlp_code_refs: vec![],
        virtual_terms: vec![],
    }
}

fn handle(
    root: &Path,
    id: &str,
    corpus: CorpusStore,
    store: Arc<UsearchStore>,
) -> Arc<IndexHandle> {
    let mut indexer = CodeIndexer::new(id, root.to_path_buf())
        .with_components(Arc::new(MockEmbedder::new(8)), store);
    indexer.set_corpus_store(Arc::new(corpus));
    let mut handle = IndexHandle::bare(
        IndexId::new(id),
        Arc::new(tokio::sync::RwLock::new(indexer)),
        root.to_path_buf(),
    );
    handle.defer_embed = false;
    handle.extra_skip_dirs.push(".trusty-search".into());
    Arc::new(handle)
}

fn fixture(version: u32) -> (tempfile::TempDir, Arc<IndexHandle>) {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join(".trusty-search")).unwrap();
    let corpus = CorpusStore::open(&root.path().join(".trusty-search/index.redb")).unwrap();
    if version != 0 {
        corpus.write_schema_version_sync(version).unwrap();
    }
    corpus.upsert_chunks(&[chunk("stale")]).unwrap();
    let id = format!(
        "schema-swap-{}",
        root.path().file_name().unwrap().to_string_lossy()
    );
    let handle = handle(
        root.path(),
        &id,
        corpus,
        Arc::new(UsearchStore::new(8).unwrap()),
    );
    (root, handle)
}

async fn reopen(handle: &IndexHandle) -> CorpusStore {
    drop(handle.indexer.write().await.take_corpus_store());
    CorpusStore::open(&handle.root_path.join(".trusty-search/index.redb")).unwrap()
}

/// Force starts with empty derived rows, but preserves exactly the applied
/// version; incremental continues carrying both rows and the same version.
#[tokio::test]
async fn staging_preserves_exact_applied_schema_without_advancing_it() {
    for force in [true, false] {
        for version in [0, CURRENT_SCHEMA_VERSION - 1, CURRENT_SCHEMA_VERSION] {
            let (_root, handle) = fixture(version);
            let tmp = begin_staged_corpus_swap(&handle, &handle.id, force, None, None)
                .await
                .unwrap()
                .expect("staging must engage");
            let staged = handle.indexer.read().await.corpus_store().unwrap();
            assert_eq!(staged.chunk_count().unwrap(), usize::from(!force));
            staged.upsert_chunks(&[chunk("rebuilt")]).unwrap();
            drop(staged);
            commit_staged_corpus_swap(&handle, &handle.id, &tmp).await;
            assert!(
                !tmp.exists(),
                "the real staging file must have been promoted"
            );
            let corpus = reopen(&handle).await;
            assert_eq!(
                corpus.read_schema_version_sync().unwrap(),
                version,
                "promotion changed applied migration history (force={force})"
            );
            assert_eq!(corpus.chunk_count().unwrap(), 1 + usize::from(!force));
            drop(corpus);
            if version == 0 {
                let db = redb::Database::open(handle.root_path.join(".trusty-search/index.redb"))
                    .unwrap();
                let read = db.begin_read().unwrap();
                match read.open_table(crate::core::migration::META_TABLE) {
                    Ok(meta) => assert!(
                        meta.get(crate::core::migration::META_KEY_SCHEMA_VERSION)
                            .unwrap()
                            .is_none(),
                        "force must not invent a stamp for an unversioned corpus"
                    ),
                    Err(redb::TableError::TableDoesNotExist(_)) => {}
                    Err(error) => panic!("read raw migration metadata: {error}"),
                }
            }
        }
    }
}

/// A full force pipeline followed by cold redb/HNSW reopen must not replay
/// M005 and collapse vector coverage for two files with identical contents.
#[tokio::test(flavor = "multi_thread")]
async fn force_reindex_preserves_schema_and_vectors_after_reopen() {
    let (root, live) = fixture(CURRENT_SCHEMA_VERSION);
    for file in ["a.rs", "b.rs"] {
        std::fs::write(root.path().join(file), "pub fn identical() {}\n").unwrap();
    }
    let progress = Arc::new(ReindexProgress::new());
    spawn_reindex_awaitable(live.clone(), progress.clone(), true)
        .await
        .unwrap();
    assert_eq!(progress.status.load(), ReindexStatus::Complete);
    let corpus = reopen(&live).await;
    assert_eq!(
        corpus.read_schema_version_sync().unwrap(),
        CURRENT_SCHEMA_VERSION
    );
    let chunks = corpus.load_all_chunks().unwrap();
    assert_eq!(
        chunks.len(),
        2,
        "force must replace stale rows with both source files"
    );
    assert_ne!(chunks[0].id, chunks[1].id);
    assert_eq!(chunks[0].content, chunks[1].content);
    let path = root.path().join(".trusty-search/hnsw.usearch");
    let vectors = Arc::new(
        UsearchStore::load_from(&path)
            .await
            .unwrap()
            .expect("durable HNSW"),
    );
    let cold = handle(root.path(), &live.id.0, corpus, vectors.clone());
    run_migrations_exclusive(&cold, &MigrationRegistry::new())
        .await
        .unwrap();
    assert_eq!(
        cold.read_schema_version().await.unwrap(),
        CURRENT_SCHEMA_VERSION
    );
    assert_eq!(vectors.len().await.unwrap(), 2);
    for chunk in chunks {
        assert!(
            vectors.contains(&chunk.id).await,
            "cold migration lost {}",
            chunk.id
        );
    }
}

/// Abort discards staged metadata and retains the previous durable corpus.
#[tokio::test]
async fn aborted_staging_preserves_live_schema_and_rows() {
    for force in [true, false] {
        let (_root, handle) = fixture(CURRENT_SCHEMA_VERSION - 1);
        let tmp = begin_staged_corpus_swap(&handle, &handle.id, force, None, None)
            .await
            .unwrap()
            .unwrap();
        handle
            .write_schema_version(CURRENT_SCHEMA_VERSION)
            .await
            .unwrap();
        abort_staged_corpus_swap(&handle, &handle.id, &tmp).await;
        assert!(!tmp.exists());
        let corpus = reopen(&handle).await;
        assert_eq!(
            corpus.read_schema_version_sync().unwrap(),
            CURRENT_SCHEMA_VERSION - 1
        );
        let chunks = corpus.load_all_chunks().unwrap();
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].id, "stale");
    }
}

/// Failure to open a force staging file must keep the live corpus installed;
/// no metadata or derived rows are promoted from the failed staging attempt.
#[tokio::test]
async fn failed_force_staging_preserves_live_schema_and_rows() {
    let (_root, handle) = fixture(CURRENT_SCHEMA_VERSION);
    let tmp = super::staging_corpus_path(&handle, &handle.id).unwrap();
    std::fs::create_dir(&tmp).unwrap();
    let before = handle.indexer.read().await.corpus_store().unwrap();
    let staged = begin_staged_corpus_swap(&handle, &handle.id, true, None, None)
        .await
        .unwrap();
    assert!(staged.is_none());
    let after = handle.indexer.read().await.corpus_store().unwrap();
    assert!(Arc::ptr_eq(&before, &after));
    assert_eq!(
        after.read_schema_version_sync().unwrap(),
        CURRENT_SCHEMA_VERSION
    );
    assert_eq!(after.load_all_chunks().unwrap()[0].id, "stale");
}

/// A metadata read error must take the existing force fallback without
/// installing or promoting the partial staging corpus over the live bytes.
#[tokio::test]
async fn failed_schema_read_does_not_promote_force_staging() {
    use std::sync::atomic::{AtomicBool, Ordering};

    let (_root, handle) = fixture(CURRENT_SCHEMA_VERSION);
    let live_path = handle.root_path.join(".trusty-search/index.redb");
    let live = handle.indexer.read().await.corpus_store().unwrap();
    let before = std::fs::read(&live_path).unwrap();
    let read_attempted = Arc::new(AtomicBool::new(false));
    let attempted = read_attempted.clone();
    let staged = super::begin_staged_corpus_swap_with_schema_reader(
        &handle,
        &handle.id,
        true,
        None,
        None,
        move |source| {
            assert_eq!(
                source.read_schema_version_sync().unwrap(),
                CURRENT_SCHEMA_VERSION
            );
            attempted.store(true, Ordering::Release);
            anyhow::bail!("injected migration metadata read failure")
        },
    )
    .await
    .unwrap();
    assert!(read_attempted.load(Ordering::Acquire));
    assert!(
        staged.is_none(),
        "unreadable migration history cannot be promoted"
    );
    let installed = handle.indexer.read().await.corpus_store().unwrap();
    assert!(Arc::ptr_eq(&live, &installed));
    assert_eq!(
        installed.read_schema_version_sync().unwrap(),
        CURRENT_SCHEMA_VERSION
    );
    assert_eq!(installed.load_all_chunks().unwrap()[0].id, "stale");
    assert_eq!(std::fs::read(&live_path).unwrap(), before);
}
