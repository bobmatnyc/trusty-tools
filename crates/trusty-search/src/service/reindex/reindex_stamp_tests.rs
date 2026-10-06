//! #9169: a reindex commit stamps its corpus; a load never does.
//!
//! Why: `search.project.resolve` ranks a repo's indexes by this stamp, so it
//! must move when a reindex commits and only then.
//! What: a real staged reindex (incremental, then force) against a colocated
//! corpus, a reload of that corpus, and the direct-write arm of
//! `resolve_corpus_swap`.
//! #9230 adds the negative arms: a failed, memory-aborted or rolled-back
//! reindex leaves the stamp it found.
//! Test: `a_committed_reindex_stamps_the_corpus_and_a_reload_does_not`,
//! `the_direct_write_path_stamps_the_live_corpus`,
//! `a_failed_or_memory_aborted_reindex_does_not_stamp`,
//! `a_rolled_back_reindex_keeps_the_previous_stamp`.

use std::sync::Arc;

use super::finish_teardown::resolve_corpus_swap;
use super::{staging::StagingResolution, validate::ReindexOutcome};
use crate::core::corpus::CorpusStore;
use crate::core::embed::MockEmbedder;
use crate::core::indexer::CodeIndexer;
use crate::core::registry::{IndexHandle, IndexId};
use crate::core::store::UsearchStore;
use crate::service::reindex::{spawn_reindex_awaitable, ReindexProgress, ReindexStatus};

/// A colocated index over a tempdir holding one source file, with a fresh
/// (unstamped) redb corpus attached.
fn colocated_index(tag: &str) -> (tempfile::TempDir, Arc<IndexHandle>) {
    let root = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir(root.path().join(".trusty-search")).expect("store dir");
    std::fs::write(root.path().join("a.rs"), "pub fn alpha() {}\n").expect("source");
    let corpus = CorpusStore::open(&root.path().join(".trusty-search/index.redb")).expect("open");
    let mut indexer = CodeIndexer::new(tag, root.path().to_path_buf())
        .with_storage_layout(crate::service::storage_layout::StorageLayout::Colocated)
        .with_components(
            Arc::new(MockEmbedder::new(8)),
            Arc::new(UsearchStore::new(8).expect("usearch")),
        );
    indexer.set_corpus_store(Arc::new(corpus));
    let mut handle = IndexHandle::bare(
        IndexId::new(tag),
        Arc::new(tokio::sync::RwLock::new(indexer)),
        root.path().to_path_buf(),
    );
    handle.defer_embed = false;
    handle.extra_skip_dirs.push(".trusty-search".into());
    (root, Arc::new(handle))
}

/// The stamp in the corpus the handle currently holds.
async fn stamp_of(handle: &IndexHandle) -> Option<u64> {
    let corpus = handle.indexer.read().await.corpus_store().expect("corpus");
    corpus.read_reindexed_unix_sync().expect("read stamp")
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("after epoch")
        .as_secs()
}

/// Why: #9169 — the resolver's "most recently indexed" is this stamp; a
/// reload must not move it, or a cold load would again look like a reindex.
/// What: an unstamped corpus gains a stamp from an incremental staged
/// reindex and a newer-or-equal one from a force reindex; reopening the
/// corpus, read-write and read-only, reads the same stamp back.
/// Test: this test.
#[tokio::test(flavor = "multi_thread")]
async fn a_committed_reindex_stamps_the_corpus_and_a_reload_does_not() {
    let (root, handle) = colocated_index("stamp-9169");
    assert_eq!(stamp_of(&handle).await, None, "a fresh corpus is unstamped");

    let started = now_unix();
    for force in [false, true] {
        let progress = Arc::new(ReindexProgress::new());
        spawn_reindex_awaitable(handle.clone(), progress.clone(), force)
            .await
            .expect("reindex task");
        assert_eq!(
            progress.status.load(),
            ReindexStatus::Complete,
            "force={force}"
        );
    }
    let stamp = stamp_of(&handle).await.expect("a committed reindex stamps");
    assert!((started..=now_unix()).contains(&stamp), "{stamp}");

    let redb = root.path().join(".trusty-search/index.redb");
    drop(handle.indexer.write().await.take_corpus_store());
    let reloaded = CorpusStore::open(&redb).expect("reload");
    assert_eq!(
        reloaded.read_reindexed_unix_sync().expect("read"),
        Some(stamp)
    );
    drop(reloaded);
    assert_eq!(
        crate::core::corpus::read_reindexed_unix_at(&redb).expect("read-only"),
        Some(stamp)
    );
}

/// Why: #9169 — a reindex that could not stage writes straight into the live
/// corpus, and that commit must stamp it too.
/// Test: this test.
#[tokio::test(flavor = "multi_thread")]
async fn the_direct_write_path_stamps_the_live_corpus() {
    let (_root, handle) = colocated_index("direct-9169");
    resolve_corpus_swap(
        &handle,
        &handle.id,
        handle.root_path.as_path(),
        None,
        &StagingResolution::Commit,
        &ReindexOutcome::Ready,
        false,
        true,
    )
    .await;
    assert!(stamp_of(&handle).await.is_some());
}

/// Run the direct-write arm of `resolve_corpus_swap` with `outcome` and
/// `memory_aborted`, and return the live corpus's stamp afterwards.
async fn direct_write_stamp(
    tag: &str,
    outcome: ReindexOutcome,
    memory_aborted: bool,
) -> Option<u64> {
    let (_root, handle) = colocated_index(tag);
    resolve_corpus_swap(
        &handle,
        &handle.id,
        handle.root_path.as_path(),
        None,
        &StagingResolution::Commit,
        &outcome,
        memory_aborted,
        false,
    )
    .await;
    stamp_of(&handle).await
}

/// Why: #9230 (PR-A review) — a reindex whose embed pass failed, or one the
/// memory guard aborted, committed nothing the resolver may call fresh.
/// Test: this test.
#[tokio::test(flavor = "multi_thread")]
async fn a_failed_or_memory_aborted_reindex_does_not_stamp() {
    let failed = ReindexOutcome::Failed {
        reason: "every batch failed to embed".into(),
    };
    assert_eq!(direct_write_stamp("failed-9230", failed, false).await, None);
    assert_eq!(
        direct_write_stamp("aborted-9230", ReindexOutcome::Ready, true).await,
        None
    );
}

/// Why: #9230 (PR-A review) — a rolled-back staged reindex discards staging
/// and reattaches the live corpus, whose stamp must be the one it had.
/// Test: this test.
#[tokio::test(flavor = "multi_thread")]
async fn a_rolled_back_reindex_keeps_the_previous_stamp() {
    let (root, handle) = colocated_index("rollback-9230");
    let live = handle
        .indexer
        .write()
        .await
        .take_corpus_store()
        .expect("live corpus");
    live.write_reindexed_unix_sync(100).expect("plant stamp");
    drop(live);
    let tmp = root.path().join(".trusty-search/index.redb.tmp");
    let staging = CorpusStore::open_fresh(&tmp).expect("staging");
    handle
        .indexer
        .write()
        .await
        .set_corpus_store(Arc::new(staging));

    resolve_corpus_swap(
        &handle,
        &handle.id,
        handle.root_path.as_path(),
        Some(tmp.as_path()),
        &StagingResolution::Rollback {
            reason: "test rollback".into(),
        },
        &ReindexOutcome::Ready,
        false,
        false,
    )
    .await;
    assert!(!tmp.exists(), "the rollback deletes staging");
    assert_eq!(stamp_of(&handle).await, Some(100));
}
