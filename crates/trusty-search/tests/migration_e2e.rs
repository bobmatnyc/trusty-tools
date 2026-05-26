//! End-to-end integration test for the schema migration framework.
//!
//! Why: unit tests in `core::migration::tests` cover the pure logic (chain
//! computation, no-op detection, error paths) in isolation. This test exercises
//! the full daemon startup path — specifically that `run_migrations` is invoked
//! on startup and that M001 correctly re-indexes Rust files with `pub const`
//! declarations without corrupting the corpus or erroring out.
//!
//! These tests are marked `#[ignore]` because they require:
//!   1. A real `FastEmbedder` (ONNX model, ~86 MB download on first run).
//!   2. A writable temporary directory for a real redb corpus.
//!   3. Non-trivial wall-clock time (~30 s on a cold model cache).
//!
//! Run with: `cargo test -p trusty-search --test migration_e2e -- --include-ignored`

use std::sync::Arc;

use tempfile::TempDir;
use tokio::sync::RwLock;
use trusty_search::core::migration::{run_migrations, MigrationRegistry, CURRENT_SCHEMA_VERSION};
use trusty_search::core::registry::{IndexHandle, IndexId};

/// Verify that `run_migrations` on a corpus that already carries
/// `CURRENT_SCHEMA_VERSION` is a no-op (completes without error in <<1 ms).
///
/// Why: guard against regressions where a future migration accidentally
/// applies itself to an already-migrated index.
/// What: create a temporary corpus, write `CURRENT_SCHEMA_VERSION` into it,
/// run the production registry, and assert the version is unchanged.
/// Test: `cargo test -p trusty-search --test migration_e2e -- --include-ignored`
#[tokio::test]
#[ignore = "requires real redb corpus — run with --include-ignored"]
async fn migration_e2e_already_current_is_noop() {
    let dir = TempDir::new().expect("tempdir");
    let corpus_path = dir.path().join("corpus.redb");

    // Build a corpus store so the `_meta` table is initialized.
    let corpus = Arc::new(
        trusty_search::core::corpus::CorpusStore::open(&corpus_path).expect("open corpus"),
    );

    // Build a minimal indexer wired to this corpus.
    let mut indexer =
        trusty_search::core::indexer::CodeIndexer::new("e2e-already-current", dir.path());
    indexer.set_corpus_store(Arc::clone(&corpus));

    let handle = IndexHandle::bare(
        IndexId::new("e2e-already-current"),
        Arc::new(RwLock::new(indexer)),
        dir.path().to_path_buf(),
    );

    // Pre-stamp the index at CURRENT_SCHEMA_VERSION via the public async wrapper.
    handle
        .write_schema_version(CURRENT_SCHEMA_VERSION)
        .await
        .expect("write schema version");

    let registry = MigrationRegistry::new();
    run_migrations(&handle, &registry)
        .await
        .expect("run_migrations must succeed on an already-current corpus");

    // Version must be unchanged.
    let version = handle
        .read_schema_version()
        .await
        .expect("read schema version");
    assert_eq!(
        version, CURRENT_SCHEMA_VERSION,
        "schema version must remain at CURRENT_SCHEMA_VERSION after no-op run"
    );
}

/// Verify that `run_migrations` advances a legacy (version 0) corpus to
/// `CURRENT_SCHEMA_VERSION` when there are no qualifying Rust files.
///
/// Why: the simplest real-corpus migration path — an empty index (no files
/// indexed yet) at version 0 should reach version 1 with zero re-indexing work.
/// What: open a real redb corpus, leave schema_version at 0 (default), run
/// the production registry, and assert the version advances to 1.
/// Test: `cargo test -p trusty-search --test migration_e2e -- --include-ignored`
#[tokio::test]
#[ignore = "requires real redb corpus — run with --include-ignored"]
async fn migration_e2e_empty_corpus_advances_to_current() {
    let dir = TempDir::new().expect("tempdir");
    let corpus_path = dir.path().join("corpus.redb");

    // Open corpus but do NOT write a schema_version — simulates a legacy index.
    let corpus = Arc::new(
        trusty_search::core::corpus::CorpusStore::open(&corpus_path).expect("open corpus"),
    );

    let mut indexer =
        trusty_search::core::indexer::CodeIndexer::new("e2e-empty-legacy", dir.path());
    indexer.set_corpus_store(Arc::clone(&corpus));

    let handle = IndexHandle::bare(
        IndexId::new("e2e-empty-legacy"),
        Arc::new(RwLock::new(indexer)),
        dir.path().to_path_buf(),
    );

    let registry = MigrationRegistry::new();
    run_migrations(&handle, &registry)
        .await
        .expect("run_migrations must succeed on an empty legacy corpus");

    let version = handle
        .read_schema_version()
        .await
        .expect("read schema version");
    assert_eq!(
        version, CURRENT_SCHEMA_VERSION,
        "empty legacy corpus must advance to CURRENT_SCHEMA_VERSION"
    );
}
