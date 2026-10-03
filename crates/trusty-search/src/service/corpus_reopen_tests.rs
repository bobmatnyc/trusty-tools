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

use super::corpus_reopen::{reopen_guarded, reopen_sweep_once, ReopenOutcome};
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

    let outcome = reopen_guarded(&state.registry, &h, Duration::from_millis(120)).await;
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
        reopen_guarded(&state.registry, &h, Duration::from_millis(50)).await,
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
        reopen_guarded(&state.registry, &h, Duration::from_millis(200)).await,
        ReopenOutcome::Reopened { .. }
    ));
    let deadline = tokio::time::Instant::now() + Duration::from_secs(120);
    while !h.indexer.read().await.migration_faults().is_empty() {
        assert!(tokio::time::Instant::now() < deadline, "chain never re-ran");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Assert the quarantine is still up, with `kind` and no corpus wired.
async fn assert_still_quarantined(h: &IndexHandle, kind: CorpusOpenFailure) {
    let indexer = h.indexer.read().await;
    assert!(
        indexer.is_write_quarantined(),
        "the quarantine must stay up"
    );
    assert!(!indexer.has_corpus_store(), "no corpus may be wired");
    assert_eq!(indexer.corpus_open_failure, Some(kind));
}

/// Run one re-open while a DELETE lands mid-attempt, and return its outcome.
///
/// The re-open starts against a held lock, so it sits in its retry loop
/// holding the teardown read guard; the DELETE then queues behind it; the
/// holder is released, so the re-open succeeds before the DELETE tears down.
/// The returned guard keeps the sandbox, and `TRUSTY_DATA_DIR`, alive.
async fn reopen_racing_a_delete(
    id: &str,
    delete_data: bool,
) -> (
    ReopenOutcome,
    PathBuf,
    crate::service::server::tests_components::IsolatedDataDir,
) {
    let isolated = crate::service::server::tests_components::IsolatedDataDir::new();
    let root = isolated.path().join("root");
    let (state, redb, holder) = contended_index(id, &root).await;
    let h = handle(&state, id);
    let reopen = {
        let (state, h) = (Arc::clone(&state), Arc::clone(&h));
        tokio::spawn(
            async move { reopen_guarded(&state.registry, &h, Duration::from_secs(3)).await },
        )
    };
    let lock = crate::service::reindex::index_teardown_lock(&IndexId::new(id));
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while lock.try_write().is_ok() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "#8085: the re-open never took the teardown read guard"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let delete = {
        let state = Arc::clone(&state);
        let id = id.to_string();
        tokio::spawn(async move {
            let params = crate::service::server::DeleteIndexParams {
                delete_data,
                expected_root_path: None,
            };
            crate::service::server::delete_index_report(&state, &id, params).await
        })
    };
    tokio::time::sleep(Duration::from_millis(100)).await;
    drop(holder);
    let outcome = reopen.await.expect("re-open task");
    let body = delete
        .await
        .expect("delete task")
        .map_err(|(s, b)| format!("{s}: {b}"))
        .expect("the delete succeeds");
    assert_eq!(body["removed"], true, "{body}");
    assert!(
        state.registry.get(&IndexId::new(id)).is_none(),
        "the index is gone"
    );
    (outcome, redb, isolated)
}

/// HIGH-1 (#8085 review): a `delete_data=true` DELETE landing during a re-open
/// waits for it, then closes and removes the corpus. On pre-fix code the DELETE
/// did not wait, removed the data dir, and the re-open's open recreated it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial_test::serial]
async fn a_delete_during_a_reopen_leaves_no_corpus_behind() {
    let (outcome, redb, _isolated) = reopen_racing_a_delete("del-data-8085", true).await;
    assert!(
        matches!(outcome, ReopenOutcome::Reopened { .. }),
        "the DELETE waits for the in-flight re-open: {outcome:?}"
    );
    assert!(
        !redb.exists(),
        "index.redb must be gone: {}",
        redb.display()
    );
}

/// HIGH-1: the same race with `delete_data=false` leaves the file in place but
/// closed, so a new opener gets it. On pre-fix code the re-open wired the
/// corpus onto the deregistered handle and held the file open.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial_test::serial]
async fn a_keep_data_delete_during_a_reopen_closes_the_corpus() {
    let (outcome, redb, _isolated) = reopen_racing_a_delete("del-keep-8085", false).await;
    assert!(
        matches!(outcome, ReopenOutcome::Reopened { .. }),
        "{outcome:?}"
    );
    assert!(redb.is_file(), "delete_data=false keeps index.redb");
    CorpusStore::open(&redb).expect("index.redb must be closed after the delete");
}

/// HIGH-1: a handle that is no longer the registered one is not re-attached,
/// and the corpus the attempt opened is closed again.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_swapped_handle_is_not_reattached() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (state, redb, holder) = contended_index("swap-8085", dir.path()).await;
    let old = handle(&state, "swap-8085");
    state.registry.register(IndexHandle::bare(
        IndexId::new("swap-8085"),
        Arc::new(RwLock::new(crate::core::indexer::CodeIndexer::new(
            "swap-8085",
            dir.path(),
        ))),
        dir.path().to_path_buf(),
    ));
    drop(holder);

    let outcome = reopen_guarded(&state.registry, &old, Duration::from_millis(200)).await;
    assert_eq!(outcome, ReopenOutcome::Superseded);
    assert_still_quarantined(&old, CorpusOpenFailure::Contention).await;
    CorpusStore::open(&redb).expect("the superseded attempt must close index.redb");
}

/// HIGH-1: a re-open never creates a missing corpus. `CorpusStore::open` runs
/// `create_dir_all` and creates the file, which resurrected a deleted store.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_reopen_never_creates_a_missing_corpus() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (state, redb, holder) = contended_index("gone-8085", dir.path()).await;
    let h = handle(&state, "gone-8085");
    drop(holder);
    let store_dir = redb.parent().expect("store dir").to_path_buf();
    std::fs::remove_dir_all(&store_dir).expect("remove the store");

    let outcome = reopen_guarded(&state.registry, &h, Duration::from_millis(200)).await;
    assert!(
        matches!(outcome, ReopenOutcome::StillUnavailable(_)),
        "{outcome:?}"
    );
    assert!(
        !store_dir.exists(),
        "the re-open recreated {}",
        store_dir.display()
    );
    assert_still_quarantined(&h, CorpusOpenFailure::Contention).await;
}

/// HIGH-1: the sweep skips an index whose permit a reindex, relocate or
/// catch-up holds, and re-opens it once the permit is free.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_reopen_skips_an_index_whose_permit_is_held() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (state, _redb, holder) = contended_index("busy-8085", dir.path()).await;
    let h = handle(&state, "busy-8085");
    drop(holder);
    let permit = crate::service::reindex::index_semaphore(&IndexId::new("busy-8085"))
        .try_acquire_owned()
        .expect("permit");

    let outcome = reopen_guarded(&state.registry, &h, Duration::from_millis(200)).await;
    assert_eq!(outcome, ReopenOutcome::Busy);
    assert_still_quarantined(&h, CorpusOpenFailure::Contention).await;
    drop(permit);
    assert!(matches!(
        reopen_guarded(&state.registry, &h, Duration::from_millis(200)).await,
        ReopenOutcome::Reopened { .. }
    ));
}

/// MEDIUM (#8958 review): the config-release catch-up re-opens under the
/// PATCH's own teardown read guard and permit (`permit_held = true`). A DELETE
/// queued on the fair teardown lock must not block it: a second teardown read
/// would queue behind that writer while the PATCH's first read holds it off,
/// and neither would ever proceed. Taking the permit the PATCH already holds
/// would answer `Busy` and leave the index quarantined, so the catch-up would
/// not start.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial_test::serial]
async fn a_release_catch_up_returns_while_a_delete_is_queued() {
    let isolated = crate::service::server::tests_components::IsolatedDataDir::new();
    let root = isolated.path().join("root");
    let id = "release-8958";
    let (state, _redb, holder) = contended_index(id, &root).await;
    let h = handle(&state, id);
    drop(holder);
    // The PATCH's guards, taken in its order: teardown read, then the permit.
    let teardown = crate::service::reindex::acquire_index_teardown_read(&h.id).await;
    let permit = crate::service::reindex::index_semaphore(&h.id)
        .try_acquire_owned()
        .expect("permit");
    let delete = {
        let state = Arc::clone(&state);
        tokio::spawn(async move {
            let params = crate::service::server::DeleteIndexParams {
                delete_data: false,
                expected_root_path: None,
            };
            crate::service::server::delete_index_report(&state, id, params).await
        })
    };
    let lock = crate::service::reindex::index_teardown_lock(&h.id);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while lock.try_read().is_ok() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the DELETE never queued on the teardown lock"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    let catch_up = tokio::time::timeout(
        Duration::from_secs(10),
        crate::service::server::start_release_catch_up(&state, Arc::clone(&h), true),
    )
    .await
    .expect("#8958: the catch-up took a second teardown read behind the queued DELETE");
    assert_eq!(catch_up["started"], true, "{catch_up}");
    assert!(
        !h.indexer.read().await.is_write_quarantined(),
        "the re-open runs under the PATCH's permit"
    );
    // Read before the DELETE, which drops the index's progress entry.
    let progress = state
        .reindex_progress
        .get(&h.id)
        .map(|p| Arc::clone(&p))
        .expect("progress entry");

    drop(permit);
    drop(teardown);
    let body = tokio::time::timeout(Duration::from_secs(60), delete)
        .await
        .expect("the delete ends once the PATCH's guards drop")
        .expect("delete task")
        .map_err(|(s, b)| format!("{s}: {b}"))
        .expect("the delete succeeds");
    assert_eq!(body["removed"], true, "{body}");
    // The spawned catch-up must end before the sandbox, and TRUSTY_DATA_DIR, go.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(120);
    while progress.status.load() == ReindexStatus::Running {
        assert!(
            tokio::time::Instant::now() < deadline,
            "catch-up never ended"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Add an entity row under `key`, then make that key invalid UTF-8 on disk.
///
/// `load_all_entities` decodes every key with redb's `&str` decoder, which is
/// `from_utf8(..).unwrap()`, so the read-back task panics and
/// `load_chunks_from_redb` returns `Err`. A row whose VALUE does not
/// deserialize is skipped rather than failing the load, and a chunk row's key
/// is decoded only inside a log macro, so neither can exercise this arm.
fn plant_an_undecodable_row(redb: &Path, key: &str) {
    CorpusStore::open(redb)
        .expect("open to plant the row")
        .upsert_entities(&[(key.to_string(), Vec::new())])
        .expect("plant the row");
    let mut bytes = std::fs::read(redb).expect("read redb");
    let needle = key.as_bytes();
    let mut hits = 0;
    let mut i = 0;
    while i + needle.len() <= bytes.len() {
        if &bytes[i..i + needle.len()] == needle {
            bytes[i] = 0xFF;
            hits += 1;
            i += needle.len();
        } else {
            i += 1;
        }
    }
    assert!(hits > 0, "the planted key is in the file");
    std::fs::write(redb, bytes).expect("write redb");
}

/// MEDIUM (#8085 review): a corpus that opens but cannot be read back (a row
/// whose key does not decode, the holder gone) stays
/// quarantined as `Unclassified`, with no corpus wired and the refused-write
/// count it had before the attempt.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_corpus_that_cannot_be_read_back_stays_quarantined() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (state, redb, holder) = contended_index("unread-8085", dir.path()).await;
    let h = handle(&state, "unread-8085");
    {
        let indexer = h.indexer.read().await;
        assert!(indexer.refuse_incremental_write("index_file", "src/a.rs"));
        assert!(indexer.refuse_incremental_write("index_file", "src/b.rs"));
    }
    drop(holder);
    plant_an_undecodable_row(&redb, "src/undecodable_key_8085.rs");

    let outcome = reopen_guarded(&state.registry, &h, Duration::from_millis(200)).await;
    assert!(
        matches!(outcome, ReopenOutcome::StillUnavailable(_)),
        "{outcome:?}"
    );
    assert_still_quarantined(&h, CorpusOpenFailure::Unclassified).await;
    assert_eq!(
        h.indexer.read().await.refused_incremental_writes(),
        2,
        "a failed read-back keeps the refused-write count"
    );
    assert_eq!(
        reopen_guarded(&state.registry, &h, Duration::from_millis(50)).await,
        ReopenOutcome::NotTransient,
        "an unreadable corpus is not retried"
    );
}
