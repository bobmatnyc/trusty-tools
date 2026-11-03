//! #10738: a force reindex, which rebuilds from an empty corpus, raises the
//! index generation; an incremental reindex does not.

use std::sync::Arc;

use super::progress::ReindexProgress;
use super::spawn_reindex_awaitable;
use crate::core::indexer::CodeIndexer;
use crate::core::registry::{IndexHandle, IndexId};
use crate::service::index_identity::{self, IndexIdentity};

async fn reindex(handle: &Arc<IndexHandle>, force: bool) {
    spawn_reindex_awaitable(handle.clone(), Arc::new(ReindexProgress::new()), force)
        .await
        .expect("reindex task must not panic");
}

fn identity(id: &str) -> IndexIdentity {
    index_identity::current(id).expect("read the identity")
}

/// Why: a force reindex can drop chunks that were pushed and never written
/// to disk, with no per-path remove; a journal bound to the old generation
/// must see a mismatch (SPEC-OKF-01 §4.9.2, F14).
/// What: two incremental reindexes keep the identity; a force reindex keeps
/// `instance_id` and raises `generation`.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread")]
// Same reason as `hash_withhold_tests`: a `#[serial]` writer's memory limit
// would memory-abort this reindex.
#[serial_test::parallel]
async fn generation_changes_when_a_force_reindex_resets_the_index() {
    let id = "identity-force-reindex-10738";
    let tmp = tempfile::tempdir().expect("tempdir");
    let store = tempfile::tempdir().expect("store dir");
    let root = tmp.path().to_path_buf();
    std::fs::write(
        root.join("a.rs"),
        "pub fn alpha_10738() -> u8 {\n    1\n}\n",
    )
    .expect("a.rs");
    let mut indexer = CodeIndexer::new(id, root.clone());
    indexer.set_corpus_store(Arc::new(
        crate::core::corpus::CorpusStore::open(&store.path().join("index.redb"))
            .expect("open corpus"),
    ));
    let handle = Arc::new(IndexHandle::bare(
        IndexId::new(id),
        Arc::new(tokio::sync::RwLock::new(indexer)),
        root,
    ));
    let _ = index_identity::retire(id);

    reindex(&handle, false).await;
    let before = identity(id);
    reindex(&handle, false).await;
    assert_eq!(
        identity(id),
        before,
        "an incremental reindex keeps the identity"
    );

    reindex(&handle, true).await;
    let after = identity(id);
    assert_eq!(
        after.instance_id, before.instance_id,
        "{before:?} -> {after:?}"
    );
    assert!(
        after.generation > before.generation,
        "a force reindex must raise the generation: {before:?} -> {after:?}"
    );
}
