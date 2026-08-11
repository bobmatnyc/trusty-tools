//! End-to-end proof that a contributed graph survives a reindex (ADR-0009).
//!
//! Why: ADR-0009 §Decision 1 promises that "reindex regenerates derived tables
//! only; contributed data survives restart and reindex by construction", and
//! the code did the opposite. Every reindex opened a fresh staging corpus,
//! seeded it with four tables (never `kg_contrib`), and renamed it over the
//! live file — so the contribution was gone, the run reported `Complete`, the
//! graph stage reported `Ready`, and `load_contrib_graphs()` afterwards
//! returned `Ok(vec![])`, which is indistinguishable from "nothing was ever
//! contributed". Permanent loss of unrecoverable data on the happy path.
//!
//! The seam already had coverage — `ingest_survives_derived_rebuild` proves the
//! rebuild does not evict contributions — and it passed throughout. Testing the
//! seam is what let this survive, so these tests drive the real entry point,
//! `spawn_reindex_awaitable`, and assert on the corpus and the serving graph
//! the daemon is left holding.
//!
//! What: the colocated-storage harness `resume_tests` uses (`.trusty-search/`
//! under a tempdir), so the live corpus, the staging corpus, and the rename all
//! resolve inside the test's own directory rather than the daemon's global data
//! dir — hermetic and safe to run in parallel with the rest of the suite.
//!
//! Test: this IS the test module.

use super::*;
use crate::core::corpus::contrib::{ContribEdge, ContribGraph, ContribNode};
use crate::core::corpus::CorpusStore;
use crate::core::indexer::CodeIndexer;
use crate::core::registry::{IndexHandle, IndexId, IndexStages};
use std::path::Path;
use std::sync::Arc;

/// Project root with colocated storage and one indexable source file.
fn make_root() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(tmp.path().join(".trusty-search")).expect("colocated dir");
    std::fs::write(tmp.path().join("lib.rs"), "fn load_orders() {}\n").expect("write fixture");
    tmp
}

/// Index handle backed by the colocated live corpus under `root`.
///
/// Staging (and therefore the swap this module is about) only engages when the
/// index has a durable corpus store. No embedder is wired, so the run takes the
/// parse-only path and stays hermetic.
fn make_handle(root: &Path, id: &str) -> Arc<IndexHandle> {
    let db_path = crate::service::colocated_storage::colocated_redb_path(root).expect("redb path");
    let corpus = CorpusStore::open(&db_path).expect("open live corpus");
    let mut indexer = CodeIndexer::new(id, root.to_path_buf());
    indexer.set_corpus_store(Arc::new(corpus));
    let mut skip_dirs = crate::service::walker::default_extra_skip_dirs();
    skip_dirs.push(".trusty-search".to_string());
    Arc::new(IndexHandle {
        id: IndexId::new(id),
        indexer: Arc::new(tokio::sync::RwLock::new(indexer)),
        root_path: root.to_path_buf(),
        include_paths: vec![],
        exclude_globs: vec![],
        extensions: vec![],
        domain_terms: vec![],
        include_docs: false,
        respect_gitignore: true,
        follow_links: true,
        extra_skip_dirs: skip_dirs,
        data_file_max_bytes: crate::service::walker::DEFAULT_DATA_FILE_MAX_BYTES,
        path_filter: vec![],
        context_embedding: Arc::new(tokio::sync::RwLock::new(None)),
        context_summary: Arc::new(tokio::sync::RwLock::new(None)),
        indexed_head_sha: Arc::new(tokio::sync::RwLock::new(None)),
        last_indexed_at: Arc::new(tokio::sync::RwLock::new(None)),
        lexical_only: false,
        skip_kg: false,
        skip_vector: false,
        defer_embed: true,
        stages: Arc::new(tokio::sync::RwLock::new(IndexStages::default())),
        search_pressure: Arc::new(tokio::sync::Notify::new()),
        walk_diagnostics: Arc::new(tokio::sync::RwLock::new(
            crate::core::registry::WalkDiagnostics::default(),
        )),
    })
}

/// The contribution under test: one proc that writes one table.
fn sample_contribution(producer: &str) -> ContribGraph {
    ContribGraph {
        producer: producer.to_string(),
        producer_version: Some("0.1.0".into()),
        git_sha: Some("abc123".into()),
        nodes: vec![
            ContribNode {
                id: "dbo.usp_load_orders".into(),
                kind: "proc".into(),
            },
            ContribNode {
                id: "dbo.orders".into(),
                kind: "table".into(),
            },
        ],
        edges: vec![ContribEdge {
            from: "dbo.usp_load_orders".into(),
            to: "dbo.orders".into(),
            kind: Some("writes".into()),
            tag: None,
            provenance: vec!["load_orders.sql".into()],
            linked_server: None,
        }],
    }
}

/// Store `producer`'s contribution through the corpus the handle currently
/// holds — the same store `POST /indexes/{id}/graph` writes through.
async fn ingest(handle: &IndexHandle, producer: &str) {
    handle
        .indexer
        .read()
        .await
        .corpus_store()
        .expect("index must hold a durable corpus")
        .save_contrib_graph(&sample_contribution(producer))
        .expect("save contrib graph");
}

/// Run one reindex to completion through the real entry point.
async fn reindex(handle: &Arc<IndexHandle>, force: bool) {
    let progress = Arc::new(ReindexProgress::new());
    spawn_reindex_awaitable(handle.clone(), progress.clone(), force)
        .await
        .expect("reindex task must not panic");
    assert_eq!(
        progress.status.load(),
        ReindexStatus::Complete,
        "reindex must complete (force={force})"
    );
}

/// Assert the contribution is both durable and queryable after a reindex.
///
/// Both halves matter and they fail independently: the corpus check is the data
/// (is it still on disk after the rename?), the graph check is the contract
/// callers actually depend on (can a traversal reach it?).
async fn assert_contribution_intact(handle: &IndexHandle, producer: &str, force: bool) {
    let stored = handle
        .indexer
        .read()
        .await
        .corpus_store()
        .expect("index must hold a durable corpus after a staged reindex")
        .load_contrib_graphs()
        .expect("load contributions");
    let producers: Vec<&str> = stored.iter().map(|g| g.producer.as_str()).collect();
    assert_eq!(
        producers,
        vec![producer],
        "the promoted corpus must still hold the contribution (force={force}) — an empty \
         kg_contrib is indistinguishable from 'nothing was ever contributed', which is why \
         this loss was silent (ADR-0009)"
    );
    assert_eq!(
        stored[0].edges.len(),
        1,
        "the contribution must survive whole, not as an empty envelope"
    );

    let graph = handle.indexer.read().await.snapshot_symbol_graph().await;
    assert_eq!(
        graph.node_kind("dbo.orders"),
        Some("table"),
        "the contributed node must be back in the serving graph after the reindex's own KG \
         rebuild (force={force})"
    );
    assert_eq!(graph.node_kind("dbo.usp_load_orders"), Some("proc"));
}

/// Why: an incremental reindex seeds staging from live via `copy_all_from`,
/// which copies four tables and not `kg_contrib` — so the promoted corpus lost
/// every contribution while the run reported success.
/// Test: this test. Multi-threaded flavour because the pipeline puts blocking
/// redb work on `spawn_blocking`.
#[tokio::test(flavor = "multi_thread")]
async fn contributed_graph_survives_an_incremental_reindex() {
    let tmp = make_root();
    let handle = make_handle(tmp.path(), "contrib-survives-incremental");
    ingest(&handle, "navigatsql").await;

    reindex(&handle, false).await;

    assert_contribution_intact(&handle, "navigatsql", false).await;
}

/// Why: a force reindex skips the seeding copy entirely — it starts from an
/// empty staging corpus by design. That design is right for chunks, which the
/// run rebuilds, and wrong for contributions, which nothing rebuilds: no
/// reindex phase writes `kg_contrib`, only the external ingest endpoint does.
/// Test: this test.
#[tokio::test(flavor = "multi_thread")]
async fn contributed_graph_survives_a_force_reindex() {
    let tmp = make_root();
    let handle = make_handle(tmp.path(), "contrib-survives-force");
    ingest(&handle, "navigatsql").await;

    reindex(&handle, true).await;

    assert_contribution_intact(&handle, "navigatsql", true).await;
}

/// Why: each reindex reads its contributions from the corpus the PREVIOUS
/// reindex promoted, not from the file the ingest originally wrote. A carry
/// that works once but does not chain would still lose the data on the second
/// reindex — and the loss would look exactly the same.
/// Test: this test, alternating force and incremental across three runs.
#[tokio::test(flavor = "multi_thread")]
async fn contributed_graph_survives_repeated_reindexes() {
    let tmp = make_root();
    let handle = make_handle(tmp.path(), "contrib-survives-repeat");
    ingest(&handle, "navigatsql").await;

    for force in [true, false, true] {
        reindex(&handle, force).await;
        assert_contribution_intact(&handle, "navigatsql", force).await;
    }
}
