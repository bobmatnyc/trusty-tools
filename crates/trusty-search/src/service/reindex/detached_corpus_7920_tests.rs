//! #7920 recurrence: a durable corpus detached by a staged swap and not
//! re-attached must never let a snapshot writer overwrite persisted data.
//!
//! Why: when another opener holds the corpus file, the re-open after a staged
//! swap fails and the indexer was left with no corpus and no quarantine — the
//! state in which both snapshot writers fall back to `chunks.json`.
//! What: the abort error arm, the promotion error arm, and the owning arm.
//! Test: `shutdown_flush_after_failed_reattach_leaves_chunks_json_byte_identical`,
//! `failed_promotion_reopen_quarantines_and_flush_leaves_chunks_json_byte_identical`,
//! `owning_daemon_reattaches_and_shutdown_flush_persists`.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::RwLock;

use super::{abort_staged_corpus_swap, commit_staged_corpus_swap};
use crate::core::chunker::{ChunkType, RawChunk};
use crate::core::corpus::{CorpusOpenFailure, CorpusStore};
use crate::core::indexer::CodeIndexer;
use crate::core::registry::{IndexHandle, IndexId, IndexRegistry};
use crate::service::server::SearchAppState;
use crate::service::shutdown_budget::ShutdownBudget;
use crate::service::shutdown_flush::flush_all_indexes_on_shutdown;

/// Puts `TRUSTY_DATA_DIR` back the way the test found it.
struct RestoreDataDir(Option<std::ffi::OsString>);

impl RestoreDataDir {
    /// Point the daemon data dir at `dir` for the span of one `#[serial]` test.
    fn set(dir: &std::path::Path) -> Self {
        let previous = std::env::var_os("TRUSTY_DATA_DIR");
        // SAFETY: #[serial] excludes every other #[serial] test for this span.
        unsafe { std::env::set_var("TRUSTY_DATA_DIR", dir) };
        Self(previous)
    }
}

impl Drop for RestoreDataDir {
    fn drop(&mut self) {
        // SAFETY: dropped inside the same #[serial] span that changed the var.
        match self.0.take() {
            Some(v) => unsafe { std::env::set_var("TRUSTY_DATA_DIR", v) },
            None => unsafe { std::env::remove_var("TRUSTY_DATA_DIR") },
        }
    }
}

fn chunk(id: &str) -> RawChunk {
    RawChunk {
        id: id.to_string(),
        file: format!("src/{id}.rs"),
        start_line: 1,
        end_line: 1,
        content: format!("fn {id}() {{}}"),
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

/// A colocated index mid staged swap: populated `chunks.json` loaded into
/// memory, a live `index.redb` on disk, and the staging corpus wired.
struct Fixture {
    data_dir: tempfile::TempDir,
    root: tempfile::TempDir,
    id: IndexId,
    indexer: Arc<RwLock<CodeIndexer>>,
    chunks_json: PathBuf,
    live: PathBuf,
    staging: PathBuf,
}

async fn fixture(name: &str) -> Fixture {
    let data_dir = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let store_dir = root.path().join(".trusty-search");
    std::fs::create_dir_all(&store_dir).unwrap();
    let chunks_json = store_dir.join("chunks.json");
    let seeded = serde_json::json!({
        "version": 1,
        "chunks": [chunk("a"), chunk("b"), chunk("c")],
        "entities": [],
    });
    // Pretty-printed, so any rewrite by the compact writer changes the bytes.
    std::fs::write(&chunks_json, serde_json::to_vec_pretty(&seeded).unwrap()).unwrap();
    let live = store_dir.join("index.redb");
    drop(CorpusStore::open(&live).unwrap());
    let staging = store_dir.join("index.redb.tmp");

    let id = IndexId(name.to_string());
    let mut indexer = CodeIndexer::new(id.0.clone(), root.path());
    assert_eq!(
        indexer.load_chunks_from_disk(&chunks_json).await.unwrap(),
        3
    );
    indexer.set_corpus_store(Arc::new(CorpusStore::open(&staging).unwrap()));
    Fixture {
        data_dir,
        root,
        id,
        indexer: Arc::new(RwLock::new(indexer)),
        chunks_json,
        live,
        staging,
    }
}

impl Fixture {
    fn handle(&self) -> IndexHandle {
        IndexHandle::bare(
            self.id.clone(),
            Arc::clone(&self.indexer),
            self.root.path().to_path_buf(),
        )
    }

    async fn shutdown_flush(&self) {
        let registry = IndexRegistry::new();
        registry.register(self.handle());
        let state = SearchAppState::new(registry);
        flush_all_indexes_on_shutdown(&state, ShutdownBudget::from_window(Duration::from_secs(65)))
            .await;
    }

    /// Assert the detached index is quarantined, the snapshot is untouched,
    /// and the incremental persister refuses too.
    async fn assert_refused_and_untouched(&self, before: &[u8]) {
        let after = std::fs::read(&self.chunks_json).unwrap();
        assert!(
            after == before,
            "#7920: a daemon that does not hold the corpus must not rewrite chunks.json \
             ({} bytes before, {} bytes after)",
            before.len(),
            after.len()
        );
        let idx = self.indexer.read().await;
        assert!(!idx.has_corpus_store());
        assert!(
            idx.corpus_open_failed,
            "the failed re-attach must be reported as a quarantine, not left silent"
        );
        assert_eq!(idx.corpus_open_failure, Some(CorpusOpenFailure::Contention));
        let refused = idx.refused_incremental_writes();
        assert!(refused >= 1, "the shutdown-flush refusal must be counted");
        idx.force_incremental_persist();
        assert_eq!(
            idx.refused_incremental_writes(),
            refused + 1,
            "the incremental persister must refuse as well"
        );
        let persister_target = crate::service::persistence::chunks_path(&self.id.0).unwrap();
        assert!(!persister_target.exists(), "the persister wrote a snapshot");
    }
}

/// Abort error arm: another opener holds the live corpus, so the re-attach fails.
#[tokio::test]
#[serial_test::serial]
async fn shutdown_flush_after_failed_reattach_leaves_chunks_json_byte_identical() {
    let fx = fixture("detached-7920").await;
    let _env = RestoreDataDir::set(fx.data_dir.path());
    let before = std::fs::read(&fx.chunks_json).unwrap();
    let other_opener = CorpusStore::open(&fx.live).unwrap();

    abort_staged_corpus_swap(&fx.handle(), &fx.id, &fx.staging).await;
    assert!(
        !fx.indexer.read().await.has_corpus_store(),
        "test setup: the re-attach must have failed"
    );
    fx.shutdown_flush().await;

    fx.assert_refused_and_untouched(&before).await;
    drop(other_opener);
}

/// Promotion error arm: the staging store is released and renamed over the
/// live file, but another opener still holds that file, so the re-open fails.
#[tokio::test]
#[serial_test::serial]
async fn failed_promotion_reopen_quarantines_and_flush_leaves_chunks_json_byte_identical() {
    let fx = fixture("promote-detached-7920").await;
    let _env = RestoreDataDir::set(fx.data_dir.path());
    let before = std::fs::read(&fx.chunks_json).unwrap();
    // A second handle on the staging file survives the indexer's release.
    let other_opener = fx.indexer.read().await.corpus_store().unwrap();

    let promoted = commit_staged_corpus_swap(&fx.handle(), &fx.id, &fx.staging).await;
    assert!(
        !promoted,
        "test setup: the promotion re-open must have failed"
    );
    fx.shutdown_flush().await;

    fx.assert_refused_and_untouched(&before).await;
    drop(other_opener);
}

/// Owning arm: the re-attach succeeds and the flush lands in redb as before.
#[tokio::test]
#[serial_test::serial]
async fn owning_daemon_reattaches_and_shutdown_flush_persists() {
    let fx = fixture("owned-7920").await;
    let _env = RestoreDataDir::set(fx.data_dir.path());
    let before = std::fs::read(&fx.chunks_json).unwrap();

    abort_staged_corpus_swap(&fx.handle(), &fx.id, &fx.staging).await;
    fx.shutdown_flush().await;

    let idx = fx.indexer.read().await;
    let corpus = idx
        .corpus_store()
        .expect("the live corpus must be re-attached");
    assert_eq!(
        corpus.chunk_count().unwrap(),
        3,
        "all in-memory chunks flushed"
    );
    assert!(!idx.corpus_open_failed);
    assert_eq!(idx.refused_incremental_writes(), 0);
    assert_eq!(std::fs::read(&fx.chunks_json).unwrap(), before);
}
