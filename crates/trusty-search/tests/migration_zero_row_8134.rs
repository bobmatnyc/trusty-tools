//! Issue #8134 regression tests: a legacy `chunks.json` must be imported by
//! the index it belongs to, and by no other, and an index must never report a
//! lost snapshot as a finished migration.
//!
//! Why: a 0.27.1-built artifact (`chunks.json` + `hnsw.usearch`, no
//! `index.redb`) copied onto a 0.52 host and registered via `POST /indexes`
//! logged `restored chunks=0 hnsw_snapshot=true`, answered `status: "ready"`
//! with `search_capabilities: ["vector"]`, and returned zero results for every
//! query. The migration resolved its source through
//! `persistence::chunks_path`, which names the GLOBAL data dir only, so for a
//! colocated index it read a location that held nothing, reported the
//! genuine-first-boot success, and let the runner stamp schema v1 — after
//! which the populated snapshot next to `index.redb` was never read again.
//!
//! What: drives the real registration path (`build_indexer_from_entry`, the
//! same entry point `corpus_open_quarantine_4122.rs` uses) against temp roots.
//! Covers the colocated import, the genuine first boot, the #8438 layout gate
//! (a `DataDir` index never imports a colocated neighbour's snapshot), and the
//! existing-install recovery (a v1 stamp over an empty corpus re-imports its
//! own snapshot, or records a fault). Every test pins `TRUSTY_DATA_DIR` to a
//! tempdir under `#[serial]`, so none of them touches the real data dir.
//!
//! Test: `cargo test -p trusty-search --test migration_zero_row_8134`

use std::path::Path;
use std::sync::Arc;

use serial_test::serial;
use tempfile::{tempdir, TempDir};
use trusty_common::embedder::MockEmbedder;
use trusty_common::migrations::{file_stamp::write_version_to_file, SchemaVersion};
use trusty_search::core::indexer::MIGRATION_STAGE_JSON_TO_REDB;
use trusty_search::core::{ChunkType, CodeIndexer, Embedder, RawChunk};
use trusty_search::service::persistence::PersistedIndex;
use trusty_search::service::persistence_loader::build_indexer_from_entry;

fn mock_embedder() -> Arc<dyn Embedder> {
    Arc::new(MockEmbedder::new(8))
}

/// Pins `TRUSTY_DATA_DIR` to a fresh tempdir and restores the old value on
/// drop, so a test never creates `<real data dir>/indexes/<id>/`.
///
/// Why: `persistence::chunks_path` creates the per-index data dir as a side
/// effect; the variable is process-global, hence `#[serial]` on every user.
struct DataDirPin {
    previous: Option<std::ffi::OsString>,
    // Dropped after `Drop::drop` restores the variable.
    _dir: TempDir,
}

impl DataDirPin {
    fn new() -> Self {
        let dir = tempdir().expect("data-dir tempdir");
        let previous = std::env::var_os("TRUSTY_DATA_DIR");
        // SAFETY: every caller is #[serial], so no other test reads or writes
        // the environment for this span.
        unsafe { std::env::set_var("TRUSTY_DATA_DIR", dir.path()) };
        Self {
            previous,
            _dir: dir,
        }
    }
}

impl Drop for DataDirPin {
    fn drop(&mut self) {
        // SAFETY: dropped inside the same #[serial] span that set it.
        match self.previous.take() {
            Some(v) => unsafe { std::env::set_var("TRUSTY_DATA_DIR", v) },
            None => unsafe { std::env::remove_var("TRUSTY_DATA_DIR") },
        }
    }
}

/// Build a `PersistedIndex` entry rooted at `root` with the given layout.
fn entry_at(id: &str, root: &Path, colocated: bool) -> PersistedIndex {
    let mut e = PersistedIndex::new(id.to_string(), root.to_path_buf());
    e.colocated = colocated;
    e
}

fn chunk(id: &str) -> RawChunk {
    RawChunk {
        id: id.to_string(),
        file: format!("src/{id}.rs"),
        start_line: 1,
        end_line: 2,
        content: format!("pub fn {id}() {{}}"),
        function_name: Some(id.to_string()),
        language: Some("rust".to_string()),
        chunk_type: ChunkType::Function,
        calls: Vec::new(),
        inherits_from: Vec::new(),
        chunk_depth: 0,
        parent_chunk_id: None,
        child_chunk_ids: Vec::new(),
        nlp_keywords: Vec::new(),
        nlp_code_refs: Vec::new(),
        virtual_terms: Vec::new(),
    }
}

/// `<root>/.trusty-search/`, created.
fn colocated_dir(root: &Path) -> std::path::PathBuf {
    let dir = root.join(".trusty-search");
    std::fs::create_dir_all(&dir).expect("create colocated dir");
    dir
}

/// Write a populated legacy snapshot at `<root>/.trusty-search/chunks.json`,
/// the layout a colocated 0.27.1 artifact carries.
fn write_colocated_snapshot(root: &Path, ids: &[&str]) {
    let chunks: Vec<RawChunk> = ids.iter().map(|id| chunk(id)).collect();
    let json = serde_json::json!({ "version": 1, "chunks": chunks, "entities": [] });
    std::fs::write(
        colocated_dir(root).join("chunks.json"),
        serde_json::to_vec(&json).expect("serialize snapshot"),
    )
    .expect("write chunks.json");
}

/// Stamp a colocated index as already past the JSON → redb step (schema v1),
/// the state a pre-fix daemon left behind.
fn stamp_colocated_v1(root: &Path) {
    write_version_to_file(
        &colocated_dir(root).join("schema_version.json"),
        SchemaVersion(1),
    )
    .expect("write v1 stamp");
}

/// Rows in the durable redb corpus.
fn durable_rows(indexer: &CodeIndexer) -> usize {
    indexer
        .corpus_store()
        .expect("the index wires a corpus store")
        .chunk_count()
        .expect("read durable chunk count")
}

/// Issue #8134 — THE FAIL-OPEN, end to end.
///
/// Why: the migration must read the snapshot that sits beside the corpus it is
/// seeding. Reading only the global path turned a populated source into a
/// reported success over an empty index.
/// What: registers a colocated index whose only corpus is a three-chunk
/// `chunks.json`, then asserts the durable redb corpus holds those three rows
/// and that no migration fault was recorded.
/// Test: this IS the test. Against pre-fix code `chunk_count()` is 0 and the
/// registration reports success — `restored chunks=0`, exactly as reported.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn colocated_legacy_snapshot_is_migrated_not_reported_as_a_first_boot() {
    let _pin = DataDirPin::new();
    let dir = tempdir().expect("tempdir");
    let root = dir.path().to_path_buf();
    write_colocated_snapshot(&root, &["alpha", "beta", "gamma"]);

    let indexer =
        build_indexer_from_entry(&entry_at("colocated-8134", &root, true), &mock_embedder())
            .await
            .expect("build indexer");

    assert!(
        !indexer.corpus_open_failed,
        "precondition: the colocated corpus must open cleanly"
    );
    assert_eq!(
        durable_rows(&indexer),
        3,
        "#8134: the populated colocated chunks.json must be migrated into redb, \
         not skipped as a first boot"
    );
    assert_eq!(
        indexer.chunk_count(),
        3,
        "the restored rows must also be live in memory"
    );
    assert!(
        indexer.migration_faults().is_empty(),
        "a migration that did its job records no fault, got: {:?}",
        indexer.migration_faults()
    );
}

/// The anti-over-refusal control: no snapshot anywhere is a genuine first boot.
///
/// Why: a brand-new index restores zero rows too, and marking it degraded
/// would be a new outage rather than a fix.
/// What: registers a colocated index with no `chunks.json` at all and asserts
/// it comes up empty, clean, and fault-free.
/// Test: this IS the test.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn an_index_with_no_legacy_snapshot_still_registers_clean() {
    let _pin = DataDirPin::new();
    let dir = tempdir().expect("tempdir");
    let root = dir.path().to_path_buf();

    let indexer = build_indexer_from_entry(&entry_at("fresh-8134", &root, true), &mock_embedder())
        .await
        .expect("build indexer");

    assert!(!indexer.corpus_open_failed, "the corpus must open cleanly");
    assert_eq!(indexer.chunk_count(), 0, "a fresh index is empty");
    assert!(
        indexer.migration_faults().is_empty(),
        "#8134: an empty source is the first-boot case and must record no fault, got: {:?}",
        indexer.migration_faults()
    );
}

/// #8134 / #8438: the registry decides layout, not the disk.
///
/// Why: a colocated code index and a `DataDir` docs index can share root R.
/// The docs index boots with an empty redb; probing `<R>/.trusty-search/`
/// unconditionally imported the code index's chunks and stamped v1, making the
/// contamination permanent.
/// What: registers a `DataDir` index whose root holds a populated colocated
/// `chunks.json` belonging to another index, and asserts nothing is imported.
/// Test: this IS the test. Against 330ae753f it imports all three chunks.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn data_dir_index_does_not_import_a_colocated_neighbours_snapshot() {
    let _pin = DataDirPin::new();
    let dir = tempdir().expect("tempdir");
    let root = dir.path().to_path_buf();
    write_colocated_snapshot(&root, &["code_a", "code_b", "code_c"]);

    let indexer = build_indexer_from_entry(&entry_at("docs-8134", &root, false), &mock_embedder())
        .await
        .expect("build indexer");

    assert!(!indexer.corpus_open_failed, "the corpus must open cleanly");
    assert_eq!(
        indexer.chunk_count(),
        0,
        "#8438: a DataDir index must not import its colocated neighbour's chunks"
    );
    assert_eq!(
        durable_rows(&indexer),
        0,
        "nothing may reach its redb either"
    );
}

/// #8134 on an existing install: a v1 stamp over an empty corpus recovers.
///
/// Why: a pre-fix daemon already stamped v1 over the empty redb, so the runner
/// skips the JSON step for good. With no HNSW vectors nothing else trips, and
/// the index reported `ready` over 0 chunks forever.
/// What: a colocated index with its own populated `chunks.json`, an empty
/// `index.redb`, and a v1 stamp comes up holding the three rows, durably, with
/// no fault recorded.
/// Test: this IS the test. Against 330ae753f it comes up with 0 chunks.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn a_v1_stamp_over_an_empty_corpus_reimports_its_own_snapshot() {
    let _pin = DataDirPin::new();
    let dir = tempdir().expect("tempdir");
    let root = dir.path().to_path_buf();
    write_colocated_snapshot(&root, &["alpha", "beta", "gamma"]);
    stamp_colocated_v1(&root);

    let indexer =
        build_indexer_from_entry(&entry_at("stamped-8134", &root, true), &mock_embedder())
            .await
            .expect("build indexer");

    assert_eq!(
        indexer.chunk_count(),
        3,
        "#8134: a v1 stamp must not hide the index's own populated snapshot"
    );
    assert_eq!(durable_rows(&indexer), 3, "the rows must land in redb");
    assert!(
        indexer.migration_faults().is_empty(),
        "a recovered import records no fault, got: {:?}",
        indexer.migration_faults()
    );
}

/// #8134 on an existing install, failure arm: the state becomes visible.
///
/// Why: when the re-import cannot run, the index must not read `ready` over 0
/// chunks. A recorded `json_to_redb` fault is what turns `GET
/// /indexes/{id}/status` to `degraded` (asserted in
/// `status_fault_7979_tests::failed_schema_chain_is_reported_as_migration_error_in_status`).
/// What: the same v1-stamped colocated index with a corrupt snapshot comes up
/// empty with a `json_to_redb` migration fault.
/// Test: this IS the test. Against 330ae753f no fault is recorded.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn a_v1_stamp_over_an_unreadable_snapshot_is_a_migration_fault() {
    let _pin = DataDirPin::new();
    let dir = tempdir().expect("tempdir");
    let root = dir.path().to_path_buf();
    std::fs::write(
        colocated_dir(&root).join("chunks.json"),
        br#"{"version":1,"chunks":[{"id":"#,
    )
    .expect("write corrupt chunks.json");
    stamp_colocated_v1(&root);

    let indexer = build_indexer_from_entry(
        &entry_at("stamped-corrupt-8134", &root, true),
        &mock_embedder(),
    )
    .await
    .expect("build indexer");

    assert_eq!(indexer.chunk_count(), 0, "nothing could be imported");
    let stages: Vec<&str> = indexer.migration_faults().iter().map(|f| f.stage).collect();
    assert_eq!(
        stages,
        vec![MIGRATION_STAGE_JSON_TO_REDB],
        "#8134: an unreadable snapshot under a v1 stamp must be a recorded fault"
    );
}
