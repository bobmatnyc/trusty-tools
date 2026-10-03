//! A newly created index is stamped at the current schema version (#8777).
//!
//! Why: `create_index` left the corpus unstamped, so the index read back as
//! schema 0 and its first restart ran M001-M005, including M005's corpus clear
//! and re-chunk, on chunks already written in the current shape.
//! What: a create through the real handler reads back
//! `CURRENT_SCHEMA_VERSION`; a corpus restored with unstamped rows is left for
//! the migration chain.
//! Test: this module.

use std::sync::Arc;

use super::registration_8499_tests::post_create;
use super::tests_8499::{clean_repo, mock_state, unregister};
use super::tests_components::IsolatedDataDir;
use crate::core::chunker::{ChunkType, RawChunk};
use crate::core::corpus::CorpusStore;
use crate::core::embed::{Embedder, MockEmbedder};
use crate::core::migration::{stamp_new_index_schema, CURRENT_SCHEMA_VERSION};
use crate::core::registry::IndexId;
use crate::service::persistence::{corpus_redb_path_for_entry, PersistedIndex};
use crate::service::persistence_loader::build_indexer_from_entry;
use axum::http::StatusCode;

/// #8777: the create stamps the corpus, so the first restart's migration run
/// finds nothing to do. On pre-fix code this reads back `0`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial_test::serial]
async fn a_created_index_is_stamped_at_the_current_schema() {
    let _data = IsolatedDataDir::new();
    let state = mock_state().await;
    let (_dir, root) = clean_repo("stamp-8777-", None);
    let (status, body) = post_create(&state, "stamp-8777", &root, true).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let handle = state
        .registry
        .get(&IndexId::new("stamp-8777"))
        .expect("registered");
    let version = handle.read_schema_version().await.expect("read stamp");
    assert_eq!(
        version, CURRENT_SCHEMA_VERSION,
        "#8777: a fresh index must not read back as schema 0"
    );
    unregister(&state, "stamp-8777").await;
}

/// #8777 control: a corpus that already holds unstamped rows came from an
/// older daemon, so stamping it would skip migrations it still needs.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_restored_unstamped_corpus_is_not_stamped() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut entry =
        PersistedIndex::new("stamp-restored-8777".to_string(), dir.path().to_path_buf());
    entry.colocated = true;
    let redb = corpus_redb_path_for_entry(&entry).expect("redb path");
    {
        let corpus = CorpusStore::open(&redb).expect("seed corpus");
        let chunk = RawChunk {
            id: "a.rs:1:2".into(),
            file: "a.rs".into(),
            start_line: 1,
            end_line: 2,
            content: "fn a() {}".into(),
            function_name: None,
            language: Some("rust".into()),
            chunk_type: ChunkType::Code,
            calls: Vec::new(),
            inherits_from: Vec::new(),
            chunk_depth: 0,
            parent_chunk_id: None,
            child_chunk_ids: Vec::new(),
            nlp_keywords: Vec::new(),
            nlp_code_refs: Vec::new(),
            virtual_terms: Vec::new(),
        };
        corpus.upsert_chunks(&[chunk]).expect("seed chunk");
    }
    let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder::new(8));
    let indexer = build_indexer_from_entry(&entry, &embedder)
        .await
        .expect("build indexer");

    let stamped = stamp_new_index_schema(&indexer).await.expect("stamp");
    assert!(!stamped, "a populated, unstamped corpus keeps version 0");
    let corpus = indexer.corpus_store().expect("corpus wired");
    assert_eq!(corpus.read_schema_version_sync().expect("read"), 0);
}
