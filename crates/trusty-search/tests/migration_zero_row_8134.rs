//! Issue #8134 regression test: registering a legacy artifact whose
//! `chunks.json` sits beside its corpus must not report a zero-row migration
//! as a finished one.
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
//! same entry point `corpus_open_quarantine_4122.rs` uses) against a colocated
//! root holding a populated `chunks.json` and no `index.redb`, and asserts the
//! rows land durably with no migration fault recorded. Against pre-fix code
//! the durable corpus comes back holding zero chunks — the defect verbatim.
//! The control asserts the opposite direction: a root with NO snapshot is the
//! genuine first boot and must still register cleanly, so the guard cannot
//! turn an empty index into a reported fault.
//!
//! Test: `cargo test -p trusty-search --test migration_zero_row_8134`

use std::path::Path;
use std::sync::Arc;

use tempfile::tempdir;
use trusty_common::embedder::MockEmbedder;
use trusty_search::core::{ChunkType, Embedder, RawChunk};
use trusty_search::service::persistence::PersistedIndex;
use trusty_search::service::persistence_loader::build_indexer_from_entry;

fn mock_embedder() -> Arc<dyn Embedder> {
    Arc::new(MockEmbedder::new(8))
}

/// Build a colocated `PersistedIndex` entry rooted at `root`.
fn entry_at(id: &str, root: &Path) -> PersistedIndex {
    let mut e = PersistedIndex::new(id.to_string(), root.to_path_buf());
    e.colocated = true;
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

/// Write a populated legacy snapshot at `<root>/.trusty-search/chunks.json`,
/// the layout a colocated 0.27.1 artifact carries.
fn write_colocated_snapshot(root: &Path, ids: &[&str]) {
    let dir = root.join(".trusty-search");
    std::fs::create_dir_all(&dir).expect("create colocated dir");
    let chunks: Vec<RawChunk> = ids.iter().map(|id| chunk(id)).collect();
    let json = serde_json::json!({ "version": 1, "chunks": chunks, "entities": [] });
    std::fs::write(
        dir.join("chunks.json"),
        serde_json::to_vec(&json).expect("serialize snapshot"),
    )
    .expect("write chunks.json");
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
async fn colocated_legacy_snapshot_is_migrated_not_reported_as_a_first_boot() {
    let dir = tempdir().expect("tempdir");
    let root = dir.path().to_path_buf();
    write_colocated_snapshot(&root, &["alpha", "beta", "gamma"]);

    let indexer = build_indexer_from_entry(&entry_at("colocated-8134", &root), &mock_embedder())
        .await
        .expect("build indexer");

    assert!(
        !indexer.corpus_open_failed,
        "precondition: the colocated corpus must open cleanly"
    );
    let durable = indexer
        .corpus_store()
        .expect("a colocated index wires a corpus store")
        .chunk_count()
        .expect("read durable chunk count");
    assert_eq!(
        durable, 3,
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
/// Why: the #8134 guard fails a zero-row restore from a populated source. A
/// brand-new index restores zero rows too, and marking it degraded would be a
/// new outage rather than a fix. The two are told apart by whether a source
/// existed and what it held.
/// What: registers a colocated index with no `chunks.json` at all and asserts
/// it comes up empty, clean, and fault-free.
/// Test: this IS the test.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_index_with_no_legacy_snapshot_still_registers_clean() {
    let dir = tempdir().expect("tempdir");
    let root = dir.path().to_path_buf();

    let indexer = build_indexer_from_entry(&entry_at("fresh-8134", &root), &mock_embedder())
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
