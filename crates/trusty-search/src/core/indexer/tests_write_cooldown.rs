//! Indexer-level tests for the #6826 write-cooldown HNSW demote.
//!
//! Why: `tests_idle_evict.rs` holds the #2164 clean-store demote tests, and
//! adding these to it pushed that file past the 500-SLOC production cap (its
//! basename is not `_tests.rs`, so it is counted as production). Same reason
//! `tests_idle_evict.rs` itself was split out of `tests.rs`.
//! What: covers `CodeIndexer::persist_and_demote_vector_store_after_write_cooldown`
//! — the zero-cooldown off switch, the not-yet-cooled skip, and the demote
//! itself with search results unchanged across it.
//! Test: this module.

use std::sync::Arc;
use std::time::Duration;

use super::tests_idle_evict::raw;
use super::{CodeIndexer, SearchQuery};
use crate::core::embed::{Embedder, MockEmbedder};
use crate::core::store::{UsearchStore, VectorStore};

/// The #6826 indexer-level wiring: a store that has been WRITTEN — so the
/// #2164 demote refuses it forever — is saved and re-viewed once its
/// write cooldown elapses.
///
/// Why: `demote_vector_store_if_idle` only clears the case where the
/// snapshot already matches the graph. On a developer machine nearly every
/// index has taken a write, so that path never fired for them and the daemon
/// held 9 GB of heap against 76 MB of mapped file. This asserts the written
/// case now reclaims, and that search is unchanged across it.
/// What: builds an index, saves it (recording `hnsw_path`), then writes again
/// WITHOUT saving so the store is dirty; asserts the #2164 demote refuses,
/// that a long cooldown refuses, and that a near-zero cooldown persists and
/// demotes. Takes the cooldown as an argument rather than through
/// `TRUSTY_HNSW_DEMOTE_COOLDOWN_SECS` — see the #3769 note in `tests_idle_evict.rs` for why this
/// binary has no env writers.
/// Test: this IS the test.
#[tokio::test]
async fn hnsw_write_cooldown_demotion_persists_and_reviews_dirty_store() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("hnsw.usearch");
    let dim = 32;

    let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder::new(dim));
    let usearch = Arc::new(UsearchStore::new(dim).expect("usearch new"));
    let store: Arc<dyn VectorStore> = usearch.clone();
    let idx = CodeIndexer::new("hnsw-cooldown-test", "/tmp/hnsw-cooldown-test")
        .with_components(embedder, store);

    idx.add_chunk(raw("a", "src/a.rs", "fn a() {}"))
        .await
        .expect("add chunk a");
    idx.save_vector_store(&path).await.expect("save hnsw");

    // The write that makes this the #6826 case: the graph now differs from
    // the snapshot, and nothing saves it on the idle path.
    idx.add_chunk(raw("b", "src/b.rs", "fn b() {}"))
        .await
        .expect("add chunk b");
    assert!(!usearch.in_view_mode());

    let q = SearchQuery {
        text: "fn b".to_string(),
        top_k: 5,
        expand_graph: false,
        compact: false,
        ..Default::default()
    };
    let before = idx.search(&q).await.expect("search before demotion");
    assert!(!before.is_empty(), "expected at least one hit");

    assert!(
        !idx.demote_vector_store_if_idle(Duration::from_nanos(1))
            .await,
        "the #2164 demote must still refuse a written store — that is the gap"
    );

    // Zero cooldown is the operator's off switch.
    assert!(
        !idx.persist_and_demote_vector_store_after_write_cooldown(Duration::ZERO)
            .await
    );
    assert!(!usearch.in_view_mode());

    // A long cooldown means "written too recently" — no-op.
    assert!(
        !idx.persist_and_demote_vector_store_after_write_cooldown(Duration::from_secs(3600))
            .await
    );
    assert!(!usearch.in_view_mode());

    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(
        idx.persist_and_demote_vector_store_after_write_cooldown(Duration::from_millis(1))
            .await,
        "a written, write-idle store must be persisted and demoted"
    );
    assert!(
        usearch.in_view_mode(),
        "store must be back in mmap-view mode after the cooldown sweep"
    );

    let after = idx.search(&q).await.expect("search after demotion");
    assert_eq!(
        after.iter().map(|c| &c.id).collect::<Vec<_>>(),
        before.iter().map(|c| &c.id).collect::<Vec<_>>(),
        "search results must be identical before and after the write-cooldown demote"
    );
}

/// A zero cooldown — what the ticker passes when
/// `TRUSTY_HNSW_DEMOTE_COOLDOWN_SECS` is `0` / `off` — must leave an
/// otherwise-eligible store exactly as it found it, saving nothing.
#[tokio::test]
async fn hnsw_write_cooldown_demotion_skips_when_cooldown_disabled() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("hnsw.usearch");
    let dim = 32;

    let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder::new(dim));
    let usearch = Arc::new(UsearchStore::new(dim).expect("usearch new"));
    let store: Arc<dyn VectorStore> = usearch.clone();
    let idx = CodeIndexer::new("hnsw-cooldown-off-test", "/tmp/hnsw-cooldown-off-test")
        .with_components(embedder, store);

    idx.add_chunk(raw("a", "src/a.rs", "fn a() {}"))
        .await
        .expect("add chunk a");
    idx.save_vector_store(&path).await.expect("save hnsw");
    idx.add_chunk(raw("b", "src/b.rs", "fn b() {}"))
        .await
        .expect("add chunk b");
    tokio::time::sleep(Duration::from_millis(20)).await;

    assert!(
        !idx.persist_and_demote_vector_store_after_write_cooldown(Duration::ZERO)
            .await,
        "a zero cooldown disables the write-cooldown demote outright"
    );
    assert!(
        !usearch.in_view_mode(),
        "the disabled path must leave the store heap-resident"
    );
}
