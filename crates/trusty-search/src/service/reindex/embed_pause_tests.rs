//! End-to-end behaviour of the embedding pause (#6524).
//!
//! Why: the pause is only worth having if three things hold together — the
//! lexical lane finishes while embedding is parked, the parked work is not lost,
//! and shutdown is never held open by it. Each is invisible from the outside
//! until it fails, and each fails differently: a pause that stalls BM25 defeats
//! the owner ruling, a pause that discards the queue silently loses vectors, and
//! a pause that blocks the drain hangs a `launchctl` stop.
//!
//! The deferred-embed queue these tests drive is process-global, so every case
//! that reads its depth is `#[serial_test::serial]`. Under `cargo nextest` each
//! test is its own process and the attribute is redundant but harmless (#4162).
//!
//! Test: this file IS the test module.

use std::sync::Arc;
use std::time::Duration;

use crate::core::embed::{Embedder, MockEmbedder};

use crate::core::embed_pause::{EmbeddingPause, PauseWait};
use crate::core::indexer::CodeIndexer;
use crate::core::registry::{IndexHandle, IndexId, StageStatus};
use crate::core::store::{UsearchStore, VectorStore};
use crate::service::reindex::{deferred_embed_queue_depth, ReindexProgress};

/// Embedding dimension for the mock embedder — small and deterministic.
const DIM: usize = 32;

/// Ceiling on every bounded poll below. Headroom on a loaded machine, not a
/// latency budget: the queue polls every 20 ms and the mock embedder runs
/// in-process, so the real settle time is milliseconds.
const GENEROUS: Duration = Duration::from_secs(20);

/// How long a "must NOT happen" window runs. Long enough that a broken pause —
/// one that lets embedding proceed — would have finished a three-file fixture
/// several times over.
const QUIET_WINDOW: Duration = Duration::from_millis(750);

/// A handle over `root` wired with a mock embedder and a real HNSW store.
///
/// Why: `embed_deferred_chunks_gated` short-circuits without BOTH an embedder
/// and a store, so a fixture missing either would pass every assertion here
/// while exercising none of the gate.
fn handle_with_embedder(id: &str, root: &std::path::Path) -> Arc<IndexHandle> {
    let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder::new(DIM));
    let store: Arc<dyn VectorStore> =
        Arc::new(UsearchStore::new(DIM).expect("a usearch store is constructible"));
    let indexer = CodeIndexer::new(id, root).with_components(embedder, store);
    Arc::new(IndexHandle::bare(
        IndexId::new(id.to_string()),
        Arc::new(tokio::sync::RwLock::new(indexer)),
        root.to_path_buf(),
    ))
}

/// Write a small Rust fixture the walker will chunk.
fn write_fixture(root: &std::path::Path) {
    for (name, body) in [
        ("a.rs", "pub fn alpha() -> u32 { 1 }\n"),
        ("b.rs", "pub fn bravo() -> u32 { 2 }\n"),
        ("c.rs", "pub fn charlie() -> u32 { 3 }\n"),
    ] {
        std::fs::write(root.join(name), body).expect("write the fixture file");
    }
}

/// Run one reindex to completion on `handle`, through the real pipeline.
///
/// Why the counter bump (#6574): `run_reindex` decrements
/// `BACKGROUND_QUEUE_DEPTH` once it holds the background permit, and the only
/// matching increment lives in `orchestrator::spawn_reindex_with_cleanup`, which
/// this helper deliberately bypasses to run the pass inline. Calling
/// `run_reindex` directly therefore drove the process-global counter BELOW zero,
/// wrapping it to `usize::MAX`; `tests::background_reindex_queue_depth_counts_waiting_tasks`
/// then panicked with "attempt to add with overflow" on `initial + 3` in every
/// run of this module. Mirroring the orchestrator's increment keeps the pairing
/// balanced without changing which semaphore the pass takes.
async fn reindex(handle: &Arc<IndexHandle>) {
    super::BACKGROUND_QUEUE_DEPTH.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let progress = Arc::new(ReindexProgress::new());
    super::runner::run_reindex(
        Arc::clone(handle),
        progress,
        false,
        None,
        None,
        None,
        false,
        None,
    )
    .await;
}

/// This index's semantic stage status right now.
async fn semantic(handle: &Arc<IndexHandle>) -> StageStatus {
    handle.stages.read().await.semantic.status
}

/// This index's live vector count right now.
async fn vectors(handle: &Arc<IndexHandle>) -> Option<usize> {
    handle.indexer.read().await.vector_count().await
}

/// Wait until the process-global deferred-embed queue is empty.
///
/// Why: the queue is process-global and the depth counter is shared with every
/// other test in this binary. A sibling that left a job mid-flight would make a
/// depth assertion here read someone else's state, so the depth cases below
/// start from a known zero rather than from whatever the previous test left.
/// `#[serial_test::serial]` guarantees nothing is being ADDED while this waits.
async fn wait_for_an_empty_queue(what: &str) {
    let deadline = tokio::time::Instant::now() + GENEROUS;
    while deferred_embed_queue_depth() > 0 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "{what}: the deferred-embed queue never emptied; depth is {}",
            deferred_embed_queue_depth()
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

// ------------------------------------------------------------------ test 1 ---

/// A pause stops the embed stage while the lexical lane finishes, and a resume
/// completes the work with the pending queue intact.
///
/// Why: this is the owner ruling, executable. "Pause embedding, since that's
/// the heavy process" means BM25 must still reach `Ready` — a pause that stalls
/// the walk would be a stop, not a pause — and it means the deferred work must
/// survive the park, or an operator's pause would silently cost them their
/// vectors.
/// What: pauses BEFORE the reindex, runs it, then holds a quiet window in which
/// `lexical` is `Ready` while `semantic` reaches neither `Ready` nor any
/// vectors. Resumes, and requires `semantic` to reach `Ready` with a vector for
/// every chunk — vectors the paused pass never computed, so they can only have
/// come from the queued job resuming in place.
/// Test: this test. It does not compile on `origin/main`: neither
/// `IndexHandle::embedding_pause` nor `core::embed_pause` exists there.
#[tokio::test]
#[serial_test::serial]
async fn a_paused_index_finishes_lexically_and_embeds_only_after_a_resume() {
    let tmp = tempfile::tempdir().expect("tempdir");
    write_fixture(tmp.path());
    let handle = handle_with_embedder("pause-e2e", tmp.path());

    // Pause before the walk, so the deferred pass parks at the queue's gate the
    // moment `finish_reindex` enqueues it.
    handle.embedding_pause.pause();
    reindex(&handle).await;

    // The lexical lane is the half that must NOT be affected.
    assert_eq!(
        handle.stages.read().await.lexical.status,
        StageStatus::Ready,
        "BM25 must complete through a pause (owner ruling, #6524)"
    );
    let chunk_count = handle.indexer.read().await.chunk_count();
    assert!(chunk_count > 0, "the fixture must produce chunks to embed");

    // Hold a window in which embedding must make no progress at all.
    tokio::time::sleep(QUIET_WINDOW).await;
    let parked = semantic(&handle).await;
    assert_ne!(
        parked,
        StageStatus::Ready,
        "a paused embed stage must not report Ready"
    );
    assert_ne!(
        parked,
        StageStatus::Skipped,
        "a pause must never degrade into a silently-skipped stage"
    );
    assert_eq!(
        vectors(&handle).await,
        Some(0),
        "no vector may be committed while embedding is paused"
    );

    // Resume: the parked job wakes and embeds exactly what is missing.
    handle.embedding_pause.resume();
    let deadline = tokio::time::Instant::now() + GENEROUS;
    while semantic(&handle).await != StageStatus::Ready {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the resumed embed pass must reach Ready; stage is {:?}",
            semantic(&handle).await
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(
        vectors(&handle).await,
        Some(chunk_count),
        "every chunk must be embedded after the resume — the pending work \
         survived the park"
    );
    // Leave the process-global queue as this test found it.
    wait_for_an_empty_queue("this test's own job to finish draining").await;
}

// ------------------------------------------------------------------ test 2 ---

/// Shutdown releases a parked embed stage promptly, and leaves its work OWED
/// rather than marked done.
///
/// FAIL-OPEN CHECK: a pause must never turn into a silently-skipped stage. The
/// tempting shortcut on the drain path is to settle `semantic` — `Skipped`
/// reads as "deliberately not doing this" and would make the queue drain
/// tidily — but the chunks really are still un-embedded, and `Skipped` is the
/// one state that tells warm boot, `/health` and `search_capabilities` that
/// nobody owes them. So this asserts the drain is FAST *and* that the stage is
/// left un-settled: not `Ready`, not `Skipped`. Resume, not shutdown, is what
/// completes the work.
///
/// Why the bound matters: `wait_while_paused` waits on an operator action and
/// shutdown cannot. Without the drain, a `launchctl` stop of a daemon holding
/// one paused index would hang until SIGKILL.
/// What: parks a real deferred job behind a pause, drains the gate exactly as
/// `service::daemon::drain_paused_embedders` does, and requires the queue to
/// empty inside a bounded window with the stage still un-settled.
/// Test: this test.
#[tokio::test]
#[serial_test::serial]
async fn shutdown_drain_releases_a_parked_embed_pass() {
    let tmp = tempfile::tempdir().expect("tempdir");
    write_fixture(tmp.path());
    let handle = handle_with_embedder("pause-drain", tmp.path());

    wait_for_an_empty_queue("before parking this test's own job").await;
    handle.embedding_pause.pause();
    reindex(&handle).await;

    // Prove it really is parked before draining, so a queue that emptied for
    // some unrelated reason cannot pass this test.
    tokio::time::sleep(QUIET_WINDOW).await;
    assert!(
        deferred_embed_queue_depth() > 0,
        "the paused index's embed job must still be outstanding"
    );

    handle.embedding_pause.drain();
    wait_for_an_empty_queue("a drained gate must release the parked job promptly").await;

    let settled = semantic(&handle).await;
    assert_ne!(
        settled,
        StageStatus::Skipped,
        "FAIL-OPEN CHECK: a drained pause must leave the work owed, never \
         silently skipped"
    );
    assert_ne!(
        settled,
        StageStatus::Ready,
        "a drained pause embedded nothing, so it must not claim Ready"
    );
    assert_eq!(
        vectors(&handle).await,
        Some(0),
        "the drain abandoned the pass rather than running it"
    );
}

// ------------------------------------------------------------------ test 3 ---

/// A paused pass reports unfinished and commits nothing; a resumed one finishes
/// the remainder and a third pass finds nothing left to do.
///
/// Why: two claims in one run. `EmbedCatchUp::paused` is the field that keeps
/// `run_embed_catch_up` from settling the stage — a short count with no flag is
/// indistinguishable from a completed pass, and the stage would flip `Ready`
/// over an index with no vectors, which is the #601 false-green reintroduced.
/// And the final zero-work pass is the resume-in-place proof: "not yet
/// embedded" is derived from the vector store, not from a list the stopped pass
/// was holding, which is what makes stopping mid-pass safe with no checkpoint
/// to persist. If that derivation ever moved into an in-memory list, the third
/// pass here would re-embed everything.
/// What: runs the gated pass paused, then un-paused, then once more.
/// Test: this test.
#[tokio::test]
#[serial_test::serial]
async fn a_paused_pass_owes_work_and_a_resumed_one_embeds_only_the_gap() {
    let tmp = tempfile::tempdir().expect("tempdir");
    write_fixture(tmp.path());
    let handle = handle_with_embedder("pause-gap", tmp.path());

    handle.embedding_pause.pause();
    reindex(&handle).await;
    let chunk_count = handle.indexer.read().await.chunk_count();
    assert!(chunk_count >= 3, "the fixture must produce several chunks");

    let stopped = {
        let indexer = handle.indexer.read().await;
        indexer
            .embed_deferred_chunks_gated(None, Some(&handle.embedding_pause))
            .await
            .expect("a paused pass returns rather than erroring")
    };
    assert!(stopped.paused, "the pass must report that it owes work");
    assert_eq!(stopped.embedded, 0, "nothing was embedded");
    assert_eq!(stopped.total, chunk_count);
    assert_eq!(
        vectors(&handle).await,
        Some(0),
        "a paused pass commits nothing"
    );

    handle.embedding_pause.resume();
    let finished = {
        let indexer = handle.indexer.read().await;
        indexer
            .embed_deferred_chunks_gated(None, Some(&handle.embedding_pause))
            .await
            .expect("an un-paused pass runs to completion")
    };
    assert!(!finished.paused, "an un-paused pass does not report paused");
    assert_eq!(
        finished.embedded, chunk_count,
        "the resumed pass embeds everything the paused one left"
    );
    assert_eq!(vectors(&handle).await, Some(chunk_count));

    let redundant = {
        let indexer = handle.indexer.read().await;
        indexer
            .embed_deferred_chunks_gated(None, Some(&handle.embedding_pause))
            .await
            .expect("a third pass is a no-op")
    };
    assert_eq!(
        redundant.embedded, 0,
        "a pass over an already-embedded corpus must do no work — the \
         remainder is derived from the vector store, not from a held list"
    );
    // Leave the process-global queue as this test found it.
    wait_for_an_empty_queue("this test's own job to finish draining").await;
}

/// The gate a drained daemon hands back is `Drained`, which is what every call
/// site branches on rather than re-reading the pause flag.
#[tokio::test]
async fn a_drained_handle_reports_drained_to_its_call_sites() {
    let gate = EmbeddingPause::new();
    gate.pause();
    gate.drain();
    assert_eq!(gate.wait_while_paused().await, PauseWait::Drained);
}
