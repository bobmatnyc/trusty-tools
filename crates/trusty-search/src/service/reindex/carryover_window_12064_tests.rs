//! #12064: a write and a delete that commit to the live corpus during an
//! incremental run's carryover copy survive the promotion.

use std::sync::Arc;

use super::identity_10738_tests::{resolve, staged_index, PUSHED, PUSHED_BODY};
use crate::core::indexer::VectorCoverage;
use crate::core::registry::IndexHandle;

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
                crate::service::storage_layout::REDB_TMP_FILE,
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

/// Why: #12064 — a rewrite, a new file and a delete committed to live in the
/// carryover window were dropped or resurrected by the promotion, and the
/// rewritten file's old chunk ids stayed in the corpus with no vector.
/// What: in the window, rewrite `a.rs`, put `PUSHED` and remove `DOOMED`;
/// promote; assert the corpus holds the new `a.rs` and `PUSHED`, not
/// `DOOMED`, and every corpus chunk has a vector.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial_test::parallel]
async fn a_write_and_a_delete_in_the_carryover_window_survive_the_promotion() {
    let id = "carryover-window-12064";
    let (_dir, handle) = seed(id).await;

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
    let (removed, committed) = guard
        .remove_file_committed(DOOMED)
        .await
        .expect("delete in the window");
    assert!(removed > 0 && committed, "the delete hit live");
    drop(guard);
    let tmp = run
        .await
        .expect("begin task")
        .expect("staging")
        .expect("staged");
    assert!(
        !resolve(
            &handle,
            &tmp,
            &super::staging::StagingResolution::Commit,
            false
        )
        .await,
        "the promotion lands"
    );

    let indexer = handle.indexer.read().await;
    let corpus = indexer.corpus_store().expect("promoted");
    let files = corpus.list_indexed_files().expect("list");
    assert!(files.iter().any(|f| f == PUSHED), "put kept: {files:?}");
    assert!(!files.iter().any(|f| f == DOOMED), "delete kept: {files:?}");
    let a_rows: Vec<String> = corpus
        .load_all_chunks()
        .expect("chunks")
        .into_iter()
        .filter(|c| c.file == "a.rs")
        .map(|c| c.content)
        .collect();
    assert!(
        !a_rows.is_empty() && a_rows.iter().all(|c| c.contains("alpha_12064")),
        "only the rewritten a.rs survives: {a_rows:?}"
    );
    match indexer.vector_coverage().await {
        VectorCoverage::Measured { missing, .. } => {
            assert!(
                missing.is_empty(),
                "corpus chunks without a vector: {missing:?}"
            );
        }
        other => panic!("coverage must be measured: {other:?}"),
    }
}

/// Why: fail-open check — a re-apply that fails must not promote a staging
/// corpus missing the window's write while the run reports success.
/// What: inject a re-apply failure, write in the window, and assert the run
/// returns `Err`, the staging file is gone, and the live corpus, still
/// attached, holds the write.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial_test::parallel]
async fn a_failed_reapply_aborts_the_run_and_keeps_the_write_in_live() {
    let id = "carryover-reapply-fails-12064";
    let (_dir, handle, _state, live) = staged_index(id).await;
    super::carryover_resync::TEST_FAIL_REAPPLY
        .lock()
        .expect("seam")
        .push(id.to_string());

    let guard = handle.indexer.read().await;
    let run = begin_until_swap_queued(&handle).await;
    guard
        .index_file(PUSHED, PUSHED_BODY)
        .await
        .expect("put in the window");
    drop(guard);
    let err = run
        .await
        .expect("begin task")
        .expect_err("a failed re-apply must abort the run");
    super::carryover_resync::TEST_FAIL_REAPPLY
        .lock()
        .expect("seam")
        .retain(|i| i != id);

    assert!(
        format!("{err:#}").contains("not re-applied"),
        "the error names the cause: {err:#}"
    );
    let tmp = live.with_file_name(crate::service::storage_layout::REDB_TMP_FILE);
    assert!(!tmp.exists(), "staging removed: {}", tmp.display());
    let corpus = handle.indexer.read().await.corpus_store().expect("live");
    assert_eq!(corpus.path(), live.as_path(), "live stays attached");
    assert!(
        corpus
            .list_indexed_files()
            .expect("list")
            .iter()
            .any(|f| f == PUSHED),
        "the write stays in live"
    );
}
