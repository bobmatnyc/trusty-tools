//! Writes issued during a corpus rehydrate (#10209).
//!
//! Why: a rehydrate reads redb, then publishes what it read. An id update
//! landing in between was reverted to the scanned text, and a remove-file in
//! between read the empty evicted map, deleted nothing and answered `Ok(0)`.
//! What: each test pauses a rehydrate after its redb read
//! ([`RehydrateScanPause`]), issues one write, and releases the rehydrate only
//! once the write has either finished or is waiting on the fence
//! ([`FenceWaitWatch`]) — no sleep decides the interleaving.
//! Test: this module.

use std::sync::Arc;
use std::time::Duration;

use super::idle_evict::RehydrateScanPause;
use super::rehydrate_fence::{FenceWaitWatch, TEST_FAIL_FILE_ID_READ};
use super::CodeIndexer;
use crate::core::corpus::CorpusStore;

/// Shrinks `TRUSTY_REHYDRATE_WAIT_MS` so a writer stops waiting on the
/// paused rehydrate after 5 ms, as it would after 9 s in production.
/// Restores the prior value on drop; callers are `#[serial]`.
struct ShortRehydrateWait(Option<String>);

impl ShortRehydrateWait {
    fn set() -> Self {
        let prev = std::env::var("TRUSTY_REHYDRATE_WAIT_MS").ok();
        // SAFETY: every caller is `#[serial_test::serial]`, as the other
        // tests that set this variable are.
        unsafe { std::env::set_var("TRUSTY_REHYDRATE_WAIT_MS", "5") };
        Self(prev)
    }
}

impl Drop for ShortRehydrateWait {
    fn drop(&mut self) {
        // SAFETY: as in `set`.
        match self.0.take() {
            Some(v) => unsafe { std::env::set_var("TRUSTY_REHYDRATE_WAIT_MS", v) },
            None => unsafe { std::env::remove_var("TRUSTY_REHYDRATE_WAIT_MS") },
        }
    }
}

/// An indexed, then fully evicted, corpus-backed indexer.
async fn evicted_indexer(dir: &std::path::Path, files: &[(&str, &str)]) -> Arc<CodeIndexer> {
    let mut idx = CodeIndexer::new("rehydrate-writes-10209", dir);
    idx.set_corpus_store(Arc::new(
        CorpusStore::open(&dir.join("index.redb")).expect("open corpus"),
    ));
    let idx = Arc::new(idx);
    let batch: Vec<(String, String)> = files
        .iter()
        .map(|(p, c)| (p.to_string(), c.to_string()))
        .collect();
    idx.index_files_batch(&batch).await.expect("index batch");
    tokio::time::sleep(Duration::from_millis(5)).await;
    idx.evict_bm25_entities_if_idle(Duration::from_nanos(1))
        .await;
    idx.evict_chunks_if_idle(Duration::from_nanos(1)).await;
    assert!(idx.corpus_evicted(), "precondition: the corpus is evicted");
    idx
}

/// Run `write` while a rehydrate of `idx` is paused after its redb read;
/// return the write's result once the rehydrate has committed.
async fn write_during_rehydrate<T, F>(idx: &Arc<CodeIndexer>, write: F) -> T
where
    T: Send + 'static,
    F: std::future::Future<Output = T> + Send + 'static,
{
    let pause = RehydrateScanPause::arm(idx);
    let fence = FenceWaitWatch::arm(idx);
    let notify = idx
        .begin_corpus_rehydrate()
        .expect("an evicted corpus starts a rehydrate");
    let done = notify.notified();
    tokio::pin!(done);
    done.as_mut().enable();
    pause.scanned().await;

    let mut handle = tokio::spawn(write);
    // Pre-fix the write finishes inside the window; fixed, it waits on the
    // fence the paused rehydrate holds. Either way the window is exercised.
    let early = tokio::select! {
        r = &mut handle => Some(r.expect("write task")),
        () = fence.contended() => None,
    };
    pause.release();
    done.await;
    match early {
        Some(r) => r,
        None => handle.await.expect("write task"),
    }
}

/// #10209 (2): an update to an existing chunk id committed while a rehydrate
/// is building survives the rehydrate's publish, in the map and in BM25.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial_test::serial]
async fn an_id_update_during_a_rehydrate_survives_its_commit() {
    let dir = tempfile::tempdir().unwrap();
    let idx = evicted_indexer(dir.path(), &[("src/a.rs", "fn alpha() { ocelot(); }")]).await;
    // Read from redb: a map read here would rehydrate the corpus.
    let before: Vec<String> = idx
        .corpus_arc()
        .unwrap()
        .load_all_chunks()
        .unwrap()
        .into_iter()
        .map(|c| c.id)
        .collect();
    let _wait = ShortRehydrateWait::set();

    let w = Arc::clone(&idx);
    write_during_rehydrate(&idx, async move {
        w.index_file("src/a.rs", "fn alpha() { pangolin(); }").await
    })
    .await
    .expect("index_file");

    let after = idx.chunk_ids_for_file("src/a.rs").await;
    assert_eq!(before, after, "precondition: the write updates the same id");
    let content = idx.chunk_content_by_id(&after[0]).await.expect("chunk");
    assert!(
        content.contains("pangolin"),
        "the rehydrate reverted the update in the chunk map: {content}"
    );
    let new_hits = idx.bm25_search("pangolin", 5, None).await.expect("bm25");
    assert!(!new_hits.is_empty(), "BM25 lost the updated text");
    let old_hits = idx.bm25_search("ocelot", 5, None).await.expect("bm25");
    assert!(old_hits.is_empty(), "BM25 still serves the reverted text");
}

/// #10209 (1): a remove-file issued during a rehydrate removes the file's
/// chunks from redb, and the rehydrate does not publish them back.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial_test::serial]
async fn a_remove_file_during_a_rehydrate_is_not_resurrected() {
    let dir = tempfile::tempdir().unwrap();
    let idx = evicted_indexer(
        dir.path(),
        &[
            ("src/a.rs", "fn alpha() { zebracorn(); }"),
            ("src/b.rs", "fn beta() { quokka(); }"),
        ],
    )
    .await;
    let _wait = ShortRehydrateWait::set();

    let w = Arc::clone(&idx);
    let removed =
        write_during_rehydrate(&idx, async move { w.remove_file("src/a.rs").await }).await;

    assert!(
        removed.expect("remove_file") > 0,
        "remove_file answered 0 for a file holding chunks"
    );
    assert!(idx.chunk_ids_for_file("src/a.rs").await.is_empty());
    let durable = idx.corpus_arc().unwrap().load_all_chunks().unwrap();
    assert!(
        durable.iter().all(|c| c.file != "src/a.rs"),
        "redb kept the removed file's rows"
    );
    let hits = idx.bm25_search("zebracorn", 5, None).await.expect("bm25");
    assert!(hits.is_empty(), "BM25 serves the removed file");
    let kept = idx.bm25_search("quokka", 5, None).await.expect("bm25");
    assert!(!kept.is_empty(), "the other file must stay searchable");
}

/// #10209: a new file indexed during a rehydrate is searchable after it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial_test::serial]
async fn an_insert_during_a_rehydrate_survives_its_commit() {
    let dir = tempfile::tempdir().unwrap();
    let idx = evicted_indexer(dir.path(), &[("src/a.rs", "fn alpha() {}")]).await;
    let _wait = ShortRehydrateWait::set();

    let w = Arc::clone(&idx);
    write_during_rehydrate(&idx, async move {
        w.index_file("src/c.rs", "fn gamma() { narwhal(); }").await
    })
    .await
    .expect("index_file");

    assert!(!idx.chunk_ids_for_file("src/c.rs").await.is_empty());
    let hits = idx.bm25_search("narwhal", 5, None).await.expect("bm25");
    assert!(!hits.is_empty(), "BM25 lost the inserted file");
}

/// #10209 error arm: when the durable id read fails on an evicted map,
/// remove-file fails instead of answering `Ok(0)`, keeps the rows, and a
/// retry once the read succeeds removes them.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial_test::serial]
async fn a_failed_durable_id_read_fails_the_remove_and_a_retry_removes_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let idx = evicted_indexer(dir.path(), &[("src/a.rs", "fn alpha() {}")]).await;
    let _wait = ShortRehydrateWait::set();
    TEST_FAIL_FILE_ID_READ
        .lock()
        .unwrap()
        .push(idx.index_id.clone());

    let w = Arc::clone(&idx);
    let refused =
        write_during_rehydrate(&idx, async move { w.remove_file("src/a.rs").await }).await;
    TEST_FAIL_FILE_ID_READ
        .lock()
        .unwrap()
        .retain(|id| id != &idx.index_id);

    let err = refused.expect_err("an unreadable id set must fail the remove");
    assert!(format!("{err:#}").contains("#10209 test seam"), "{err:#}");
    assert!(!idx.chunk_ids_for_file("src/a.rs").await.is_empty());
    let removed = idx.remove_file("src/a.rs").await.expect("retry");
    assert!(removed > 0);
    assert!(idx.chunk_ids_for_file("src/a.rs").await.is_empty());
}

/// #10209 watcher error arm: when `index_file` fails and the landed-id read
/// fails too, the watcher tracks the ids the file parsed into, so a later
/// delete still reaches any chunk that landed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial_test::serial]
async fn a_failed_landed_id_read_tracks_the_parsed_ids() {
    use crate::core::registry::IndexId;
    use crate::service::watch_loop::{handle_modified, handle_removed};
    use crate::service::IndexedFiles;
    use tokio::sync::RwLock;

    const PARTIAL: &str = "pub fn one_10209() {}\npub fn two_10209() {}\n";
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    std::fs::write(root.join("base.rs"), "pub fn base_10209() {}\n").unwrap();
    let mut idx = CodeIndexer::new("watcher-id-read-10209", &root);
    let store = tempfile::tempdir().unwrap();
    idx.set_corpus_store(Arc::new(
        CorpusStore::open(&store.path().join("index.redb")).expect("open corpus"),
    ));
    let id = idx.index_id.clone();
    let index_id = IndexId::new(id.as_str());
    let indexer = Arc::new(RwLock::new(idx));
    let tracked = IndexedFiles::new();
    let event = |name: &str| root.join(name);

    handle_modified(
        &event("base.rs"),
        &index_id,
        &root,
        &root,
        &indexer,
        &tracked,
    )
    .await;
    {
        let g = indexer.read().await;
        tokio::time::sleep(Duration::from_millis(5)).await;
        g.evict_bm25_entities_if_idle(Duration::from_nanos(1)).await;
        g.evict_chunks_if_idle(Duration::from_nanos(1)).await;
        assert!(g.corpus_evicted(), "precondition: the corpus is evicted");
    }
    let _wait = ShortRehydrateWait::set();
    // Every rehydrate fails before its publish, so the map stays evicted and
    // both id reads (the write's own and the error arm's) reach the seam.
    let panic = super::idle_evict::CommitPhasePanicArmed::arm(&*indexer.read().await);
    TEST_FAIL_FILE_ID_READ.lock().unwrap().push(id.clone());
    std::fs::write(root.join("partial.rs"), PARTIAL).unwrap();
    handle_modified(
        &event("partial.rs"),
        &index_id,
        &root,
        &root,
        &indexer,
        &tracked,
    )
    .await;
    TEST_FAIL_FILE_ID_READ.lock().unwrap().retain(|i| i != &id);
    drop(panic);

    let mut parsed: Vec<String> = crate::core::chunk_ast("partial.rs", PARTIAL)
        .0
        .into_iter()
        .map(|c| c.id)
        .collect();
    parsed.sort();
    let mut recorded = tracked
        .take(std::path::Path::new("partial.rs"))
        .await
        .expect("a failed write with an unreadable id set must still be tracked");
    recorded.sort();
    assert_eq!(recorded, parsed, "tracking must hold the parsed ids");

    // Removing ids that never landed is harmless.
    tracked.record("partial.rs".into(), recorded).await;
    std::fs::remove_file(root.join("partial.rs")).unwrap();
    handle_removed(
        &event("partial.rs"),
        &index_id,
        &root,
        &root,
        &indexer,
        &tracked,
    )
    .await;
    assert_eq!(
        tracked.paths().await,
        vec![std::path::PathBuf::from("base.rs")],
        "the delete must consume the partial.rs entry"
    );
    let base = indexer.read().await.chunk_ids_for_file("base.rs").await;
    assert!(!base.is_empty(), "the delete must not touch another file");
}
