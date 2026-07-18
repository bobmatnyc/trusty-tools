//! Issue #1158 regression tests: redb corpus open failure must surface as
//! `StageStatus::Failed`, NOT the misleading `InProgress` ("walking").
//!
//! Why: before #1158 a failed `CorpusStore::open` (e.g. incompatible redb
//! page format, or any I/O error) set `corpus_store = None`; `chunk_count`
//! fell through to `unwrap_or(0)`; `derive_warm_boot_stages` saw `chunk_count
//! == 0` and emitted `InProgress` — indistinguishable from a freshly-created,
//! never-indexed handle. The index appeared healthy-ish (`chunks=0`) while the
//! durable store was actually unreadable.
//!
//! This file covers two layers:
//! 1. Pure classifier unit test — `corpus_open_failed = true` → `Failed`.
//! 2. Integration test — a directory at the `index.redb` path forces
//!    `CorpusStore::open` to fail, and the whole pipeline from
//!    `build_indexer_from_entry` → `derive_warm_boot_stages` emits `Failed`.
//!
//! Test: `cargo test -p trusty-search --test warm_boot_corpus_open_failure`

use trusty_search::core::registry::StageStatus;
use trusty_search::service::warm_boot::{derive_warm_boot_stages, WarmBootInputs};

/// Issue #1158 unit test: the stage classifier must map `corpus_open_failed = true`
/// to `StageStatus::Failed` with an actionable hint, regardless of `chunk_count`.
///
/// Why: the pure-classifier test isolates the business rule from disk I/O so
/// we can verify the classifier change alone without needing a failing FS.
/// What: set `corpus_open_failed = true` with `chunk_count = 0`; assert lexical
/// is `Failed`, `lifecycle_status` is `"failed"`, and no capabilities are
/// advertised; also assert the failure string contains the reindex hint.
/// Test: this test.
#[test]
fn corpus_open_failed_flag_emits_failed_stage() {
    let stages = derive_warm_boot_stages(WarmBootInputs {
        chunk_count: 0,
        hnsw_snapshot_ready: false,
        graph_node_count: 0,
        lexical_only: false,
        skip_kg: false,
        skip_vector: false,
        corpus_open_failed: true,
    });
    assert_eq!(
        stages.lexical.status,
        StageStatus::Failed,
        "corpus_open_failed must emit Failed, not InProgress (issue #1158)"
    );
    assert_eq!(
        stages.lifecycle_status(),
        "failed",
        "lifecycle_status must be 'failed' when corpus is unreadable"
    );
    assert!(
        stages.search_capabilities().is_empty(),
        "a Failed lexical stage must advertise no search capabilities"
    );
    // The failure reason must contain an actionable hint.
    assert!(
        stages
            .lexical
            .failure
            .as_deref()
            .unwrap_or("")
            .contains("trusty-search index"),
        "failure reason must contain the reindex hint (issue #1158)"
    );
}

/// Issue #1158 integration test: a corpus file that cannot be opened (a
/// directory where redb expects a plain file) must cause
/// `build_indexer_from_entry` to set `corpus_open_failed = true` on the
/// returned `CodeIndexer`, which the stage-classifier then maps to
/// `StageStatus::Failed` — NOT the misleading `InProgress`.
///
/// Why: this exercises the full load path from `persistence_loader` →
/// `corpus_open_failed` flag → `derive_warm_boot_stages` in one shot.
/// A directory at the `index.redb` path is the most portable way to
/// force `Database::create()` to fail with an I/O error on every OS
/// (not just Unix-style permission modes), without needing ONNX or a real
/// incompatible-format file.
/// What: places a DIRECTORY at the colocated `index.redb` path, builds an
/// indexer via `build_indexer_from_entry`, asserts `corpus_open_failed`,
/// then derives stages and asserts `Failed`.
/// Test: this test; runs without ONNX / embedder.
#[tokio::test]
async fn corpus_open_failure_propagates_to_failed_stage() {
    use std::sync::Arc;
    use tempfile::tempdir;
    use trusty_common::embedder::MockEmbedder;
    use trusty_search::core::Embedder;
    use trusty_search::service::persistence::PersistedIndex;
    use trusty_search::service::persistence_loader::build_indexer_from_entry;

    let tmp = tempdir().unwrap();
    let root = tmp.path().to_path_buf();

    // Create a DIRECTORY at the path where `index.redb` should be.
    // `redb::Database::create()` will fail with an I/O error when it
    // tries to open a directory as a file — this is the simplest portable
    // way to force `CorpusStore::open` to return `Err` and trigger the
    // `corpus_open_failed = true` path in `persistence_loader`.
    let colocated_dir = root.join(".trusty-search");
    std::fs::create_dir_all(&colocated_dir).unwrap();
    let redb_path = colocated_dir.join("index.redb");
    std::fs::create_dir_all(&redb_path).unwrap(); // directory, not file

    let entry = PersistedIndex {
        id: "test-1158".to_string(),
        root_path: root.clone(),
        colocated: true,
        ..Default::default()
    };
    let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder::new(8));

    // Phase 1: assert `corpus_open_failed` is set on the indexer.
    // Before #1158 this would be `false` — the error was swallowed and
    // `chunk_count` silently fell through to 0.
    let indexer = build_indexer_from_entry(&entry, &embedder).await.unwrap();
    assert!(
        indexer.corpus_open_failed,
        "a directory at the redb path must set corpus_open_failed=true \
         (issue #1158 — was silently masked as chunk_count=0)"
    );
    assert!(
        !indexer.has_corpus_store(),
        "corpus store must not be wired when open failed"
    );

    // Phase 2: stages derived from this indexer must be Failed, not InProgress.
    let chunk_count = indexer
        .corpus_store()
        .and_then(|c| c.chunk_count().ok())
        .unwrap_or(0);
    let stages = derive_warm_boot_stages(WarmBootInputs {
        chunk_count,
        hnsw_snapshot_ready: false,
        graph_node_count: 0,
        lexical_only: false,
        skip_kg: false,
        skip_vector: false,
        corpus_open_failed: indexer.corpus_open_failed,
    });
    assert_eq!(
        stages.lexical.status,
        StageStatus::Failed,
        "unreadable corpus must surface as Failed stage (issue #1158)"
    );
    assert_eq!(
        stages.lifecycle_status(),
        "failed",
        "lifecycle_status must be 'failed' for unreadable corpus"
    );
}
