//! #12064: a write or a delete that commits to the live corpus during an
//! incremental run's carryover copy stops the run before its swap; the next
//! run promotes a corpus that holds it.

use std::sync::Arc;

use super::identity_10738_tests::{begin, resolve, staged_index, PUSHED, PUSHED_BODY};
use crate::core::registry::IndexHandle;
use crate::service::storage_layout::REDB_TMP_FILE;

const A_V2: &str = "pub fn alpha_12064(x: u8) -> u8 {\n    let y = x + 1;\n    y * 2\n}\n";
const DOOMED: &str = "src/doomed_12064.rs";
const DOOMED_BODY: &str = "pub fn doomed_12064() -> u8 {\n    9\n}\n";

/// Start a staged run while the caller holds a read guard, and return once
/// its copy is done and the swap queues for the write guard.
///
/// Why: the write must land after the copy's snapshot and before the swap,
/// with no timing guess.
/// What: the run's probe takes a read guard beside the caller's, copies, then
/// queues for the write guard; a queued writer makes `try_read` fail, which
/// is the sync point.
async fn begin_until_swap_queued(
    handle: &Arc<IndexHandle>,
) -> tokio::task::JoinHandle<anyhow::Result<Option<std::path::PathBuf>>> {
    let run = {
        let handle = Arc::clone(handle);
        tokio::spawn(async move {
            super::corpus_swap::begin_staged_corpus_swap(
                &handle,
                &handle.id,
                false,
                None,
                None,
                REDB_TMP_FILE,
            )
            .await
        })
    };
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while handle.indexer.try_read().is_ok() {
        assert!(
            std::time::Instant::now() < deadline,
            "the swap never queued for the write lock"
        );
        tokio::task::yield_now().await;
    }
    run
}

/// Seed `DOOMED` beside the fixture's `a.rs`.
async fn seed(id: &str) -> (tempfile::TempDir, Arc<IndexHandle>) {
    let (dir, handle, _state, _live) = staged_index(id).await;
    handle
        .indexer
        .read()
        .await
        .index_file(DOOMED, DOOMED_BODY)
        .await
        .expect("seed the doomed file");
    (dir, handle)
}

/// The files the attached corpus lists.
async fn attached_files(handle: &Arc<IndexHandle>) -> Vec<String> {
    let indexer = handle.indexer.read().await;
    let corpus = indexer.corpus_store().expect("a corpus is attached");
    corpus.list_indexed_files().expect("list")
}

/// Assert the run stopped: `Err`, the #12064 stop reason, no staging file.
async fn assert_stopped(
    handle: &Arc<IndexHandle>,
    outcome: anyhow::Result<Option<std::path::PathBuf>>,
) {
    let err = match outcome {
        Err(e) => e,
        Ok(path) => panic!("a write in the carryover window must stop the run, got Ok({path:?})"),
    };
    let text = format!("{err:#}");
    assert!(
        text.contains("during the carryover copy") && text.contains("#12064"),
        "the stop names the window write: {text}"
    );
    let tmp = super::corpus_swap::staging_corpus_path(handle, &handle.id, REDB_TMP_FILE)
        .await
        .expect("staging path");
    assert!(
        !tmp.exists(),
        "the stale copy is removed: {}",
        tmp.display()
    );
}

/// Why: #12064 — a delete in the window was resurrected by the promotion,
/// and the #10738 counter counted puts only, so a delete alone was not seen.
/// What: in the window, remove `DOOMED` and nothing else; assert the run
/// stops with live attached and the delete kept.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial_test::parallel]
async fn a_delete_alone_in_the_carryover_window_stops_the_run() {
    let (_dir, handle) = seed("carryover-delete-12064").await;

    let guard = handle.indexer.read().await;
    let run = begin_until_swap_queued(&handle).await;
    let (removed, committed) = guard
        .remove_file_committed(DOOMED)
        .await
        .expect("delete in the window");
    assert!(removed > 0 && committed, "the delete hit live");
    drop(guard);

    assert_stopped(&handle, run.await.expect("begin task")).await;
    let files = attached_files(&handle).await;
    assert!(!files.iter().any(|f| f == DOOMED), "delete kept: {files:?}");
}

/// Why: #12064 — a rewrite, a new file and a delete committed to live in the
/// carryover window were dropped or resurrected by the promotion.
/// What: in the window, rewrite `a.rs`, put `PUSHED` and remove `DOOMED`;
/// assert the run stops with live attached and holding all three. Then a
/// later run with no window write promotes, and the promoted corpus holds
/// them too: nothing from the stopped run's copy carries over.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial_test::parallel]
async fn a_write_and_a_delete_in_the_carryover_window_stop_the_run() {
    let (_dir, handle) = seed("carryover-window-12064").await;

    let guard = handle.indexer.read().await;
    let run = begin_until_swap_queued(&handle).await;
    guard
        .index_file("a.rs", A_V2)
        .await
        .expect("rewrite in the window");
    guard
        .index_file(PUSHED, PUSHED_BODY)
        .await
        .expect("put in the window");
    guard
        .remove_file_committed(DOOMED)
        .await
        .expect("delete in the window");
    drop(guard);

    assert_stopped(&handle, run.await.expect("begin task")).await;
    let live = attached_files(&handle).await;
    assert!(
        live.iter().any(|f| f == PUSHED),
        "live keeps the put: {live:?}"
    );
    assert!(
        !live.iter().any(|f| f == DOOMED),
        "live keeps the delete: {live:?}"
    );

    // The retry: nothing writes in this run's window, so it promotes.
    let tmp = begin(&handle, false).await;
    assert!(
        !resolve(
            &handle,
            &tmp,
            &super::staging::StagingResolution::Commit,
            false
        )
        .await,
        "the retried promotion lands"
    );
    let indexer = handle.indexer.read().await;
    let corpus = indexer.corpus_store().expect("promoted");
    let files = corpus.list_indexed_files().expect("list");
    assert!(files.iter().any(|f| f == PUSHED), "put promoted: {files:?}");
    assert!(
        !files.iter().any(|f| f == DOOMED),
        "delete promoted: {files:?}"
    );
    let a_rows: Vec<String> = corpus
        .load_all_chunks()
        .expect("chunks")
        .into_iter()
        .filter(|c| c.file == "a.rs")
        .map(|c| c.content)
        .collect();
    assert!(
        !a_rows.is_empty() && a_rows.iter().all(|c| c.contains("alpha_12064")),
        "only the rewritten a.rs is promoted: {a_rows:?}"
    );
}

/// Why: #12064 review — the stop set semantic and graph `Failed` and counted
/// toward quarantine, though the live corpus it leaves attached is intact.
/// What: one real `run_reindex`, with a put landed between its carryover
/// copy and its swap. The run ends `Failed` with the stop reason; the stages
/// equal their pre-run snapshot; the index stamps do not move; the
/// quarantine records no failure.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial_test::parallel]
async fn a_carryover_window_stop_keeps_stages_stamps_and_quarantine() {
    use super::progress::{ReindexProgress, ReindexStatus};
    let id = "carryover-run-12064";
    let _ = crate::service::index_identity::retire(id);
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("a.rs"), A_V2).expect("a.rs");
    let handle = super::identity_10738_tests::walkable_index(id, dir.path());
    super::spawn_reindex_awaitable(handle.clone(), Arc::new(ReindexProgress::new()), false)
        .await
        .expect("first reindex");
    *handle.indexed_head_sha.write().await = Some("sha-before-12064".into());
    *handle.last_indexed_at.write().await = Some("at-before-12064".into());
    let before = serde_json::to_value(&*handle.stages.read().await).expect("stages");
    assert_eq!(before["lexical"]["status"], "ready", "test setup: {before}");

    // The run's copy shares this read guard; its swap queues behind it.
    let guard = handle.indexer.read().await;
    let quarantine = super::ReindexQuarantine::new();
    let progress = Arc::new(ReindexProgress::new());
    let claim =
        crate::service::exclude_hold::claim_reindex(&handle, "test", false).expect("claim the run");
    let run = tokio::spawn(super::runner::run_reindex(
        handle.clone(),
        progress.clone(),
        false,
        None,
        None,
        None,
        true,
        Some(quarantine.clone()),
        claim,
    ));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while handle.indexer.try_read().is_ok() {
        assert!(
            std::time::Instant::now() < deadline,
            "the swap never queued for the write lock"
        );
        tokio::task::yield_now().await;
    }
    guard
        .index_file(PUSHED, PUSHED_BODY)
        .await
        .expect("put in the window");
    drop(guard);
    run.await.expect("reindex task must not panic");

    assert_eq!(progress.status.load(), ReindexStatus::Failed);
    let events = progress.events.lock().await.clone();
    assert!(
        events
            .iter()
            .any(|e| e.contains("during the carryover copy")),
        "the run names the stop: {events:?}"
    );
    let after = serde_json::to_value(&*handle.stages.read().await).expect("stages");
    assert_eq!(after, before, "the stop restores the pre-run stages");
    assert_eq!(
        handle.indexed_head_sha.read().await.as_deref(),
        Some("sha-before-12064")
    );
    assert_eq!(
        handle.last_indexed_at.read().await.as_deref(),
        Some("at-before-12064")
    );
    assert_eq!(
        quarantine.failure_count(&handle.id),
        0,
        "the stop does not count toward quarantine"
    );
}
