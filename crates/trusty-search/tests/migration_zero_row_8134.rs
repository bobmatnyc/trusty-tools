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
    let faults = indexer.migration_faults();
    let stages: Vec<&str> = faults.iter().map(|f| f.stage).collect();
    assert_eq!(
        stages,
        vec![MIGRATION_STAGE_JSON_TO_REDB],
        "#8134: an unreadable snapshot under a v1 stamp must be a recorded fault"
    );
    // A corrupt snapshot beside a legitimately empty index faults on every
    // boot, so the fault text must tell the operator how to clear it.
    let snapshot = colocated_dir(&root).join("chunks.json");
    let detail = &faults[0].detail;
    assert!(
        detail.contains(&snapshot.display().to_string()),
        "#8134: the fault must name the snapshot file, got: {detail}"
    );
    assert!(
        detail.contains("remove or rename it to clear this fault"),
        "#8134: the fault must say how to clear it, got: {detail}"
    );
    assert_eq!(
        std::fs::read(&snapshot).expect("snapshot still on disk"),
        br#"{"version":1,"chunks":[{"id":"#,
        "#7923: a failed import leaves the snapshot byte-identical"
    );
}

/// The chunks table as `CorpusStore` defines it (`core::corpus::tables`).
const CHUNKS_TABLE: redb::TableDefinition<&str, &[u8]> = redb::TableDefinition::new("chunks");

/// Plant a colocated `index.redb` whose only chunk row is `id` → `bytes`.
///
/// Why: `CorpusStore::load_all_chunks` skips a row it cannot decode, so a
/// corpus written by a build whose `RawChunk` this one cannot read loads as
/// `Ok(0)` while the durable table still holds rows (#5917).
fn plant_undecodable_row(root: &Path, id: &str, bytes: &[u8]) {
    let db = redb::Database::create(colocated_dir(root).join("index.redb")).expect("create redb");
    let txn = db.begin_write().expect("begin write");
    {
        let mut table = txn.open_table(CHUNKS_TABLE).expect("open chunks table");
        table.insert(id, bytes).expect("insert row");
    }
    txn.commit().expect("commit");
}

/// Read the raw bytes of chunk row `id` and the chunks-table row count.
fn raw_row(root: &Path, id: &str) -> (Option<Vec<u8>>, u64) {
    use redb::{ReadableDatabase, ReadableTableMetadata};
    let db = redb::Database::open(colocated_dir(root).join("index.redb")).expect("open redb");
    let txn = db.begin_read().expect("begin read");
    let table = txn.open_table(CHUNKS_TABLE).expect("open chunks table");
    let bytes = table.get(id).expect("read row").map(|v| v.value().to_vec());
    (bytes, table.len().expect("count rows"))
}

/// #8134 round 3, finding 1: the recovery never writes over rows it could not
/// load.
///
/// Why: the recovery keyed on the in-memory count. A populated redb whose rows
/// this build cannot decode loads as 0 chunks, so the recovery imported the
/// stale snapshot and `upsert_batch` overwrote the current rows that share its
/// `path:start:end` ids — with no fault, since `stored >= total` passed.
/// What: a v1-stamped colocated index whose redb holds one undecodable `alpha`
/// row and whose snapshot also carries `alpha`. After boot the row's bytes and
/// the row count are unchanged, nothing stale is live in memory, the snapshot
/// is untouched, and a `json_to_redb` fault names the refusal.
/// Test: this IS the test. Against 2e3cc97c5 the row is overwritten and no
/// fault is recorded.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn a_v1_stamp_never_imports_over_rows_the_load_could_not_decode() {
    let _pin = DataDirPin::new();
    let dir = tempdir().expect("tempdir");
    let root = dir.path().to_path_buf();
    let current: &[u8] = b"a row written by a build whose RawChunk this one cannot decode";
    plant_undecodable_row(&root, "alpha", current);
    write_colocated_snapshot(&root, &["alpha"]);
    let snapshot = colocated_dir(&root).join("chunks.json");
    let snapshot_before = std::fs::read(&snapshot).expect("read snapshot");
    stamp_colocated_v1(&root);

    let indexer =
        build_indexer_from_entry(&entry_at("undecodable-8134", &root, true), &mock_embedder())
            .await
            .expect("build indexer");

    assert!(!indexer.corpus_open_failed, "the corpus must open cleanly");
    assert_eq!(
        indexer.chunk_count(),
        0,
        "#8134: the stale snapshot must not be served from memory"
    );
    let faults = indexer.migration_faults();
    let stages: Vec<&str> = faults.iter().map(|f| f.stage).collect();
    assert_eq!(
        stages,
        vec![MIGRATION_STAGE_JSON_TO_REDB],
        "#8134: a refused import must be a recorded fault"
    );
    assert!(
        faults[0].detail.contains("refusing to import"),
        "the fault must say the import was refused, got: {}",
        faults[0].detail
    );
    drop(indexer);

    assert_eq!(
        raw_row(&root, "alpha"),
        (Some(current.to_vec()), 1),
        "#8134: the current row must be left exactly as it was"
    );
    assert_eq!(
        std::fs::read(&snapshot).expect("snapshot still on disk"),
        snapshot_before,
        "#7923: a refused import leaves the snapshot byte-identical"
    );
}

/// #8134 round 3, finding 2: a seeded snapshot is not imported a second time.
///
/// Why: nothing refreshes a colocated `chunks.json`. Once an index was seeded
/// from it, deleting every file and reindexing empties redb, and every restart
/// then re-imported the old snapshot over the empty corpus.
/// What: seeds a colocated index from its snapshot, deletes every row from the
/// durable corpus, and reboots. The index stays empty with no fault, and the
/// snapshot sits retired at `chunks.json.migrated`.
/// Test: this IS the test. Against 2e3cc97c5 the reboot restores all three rows.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn a_seeded_snapshot_is_retired_so_an_emptied_corpus_stays_empty() {
    let _pin = DataDirPin::new();
    let dir = tempdir().expect("tempdir");
    let root = dir.path().to_path_buf();
    write_colocated_snapshot(&root, &["alpha", "beta", "gamma"]);
    let entry = entry_at("retired-8134", &root, true);

    let seeded = build_indexer_from_entry(&entry, &mock_embedder())
        .await
        .expect("first boot");
    assert_eq!(
        durable_rows(&seeded),
        3,
        "precondition: the first boot seeds"
    );
    let corpus = seeded.corpus_store().expect("corpus store");
    let ids: Vec<String> = ["alpha", "beta", "gamma"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    corpus.delete_chunks(&ids).expect("delete every row");
    assert_eq!(corpus.chunk_count().expect("count"), 0);
    drop(corpus);
    drop(seeded);

    let rebooted = build_indexer_from_entry(&entry, &mock_embedder())
        .await
        .expect("second boot");
    assert_eq!(
        rebooted.chunk_count(),
        0,
        "#8134: deleted content must not come back from the old snapshot"
    );
    assert_eq!(durable_rows(&rebooted), 0, "nor reach redb");
    assert!(
        rebooted.migration_faults().is_empty(),
        "an emptied index is not a fault, got: {:?}",
        rebooted.migration_faults()
    );
    let colocated = colocated_dir(&root);
    assert!(!colocated.join("chunks.json").exists(), "snapshot retired");
    assert!(
        colocated.join("chunks.json.migrated").is_file(),
        "the retired snapshot is kept, renamed"
    );
}

/// #8134 round 4: a snapshot left beside a populated redb is retired at boot.
///
/// Why: only a snapshot that seeded redb was retired. One left beside a
/// populated redb by an earlier migration stayed live, and once that index
/// reached 0 durable rows the v1-stamp recovery brought the old content back
/// with no fault.
/// What: populates a colocated index's redb directly, plants a stale own-layout
/// snapshot (a stale `alpha` and an unknown `gamma`), and reboots. The
/// snapshot is renamed to `chunks.json.migrated`, redb keeps exactly its own
/// two rows, and no fault is recorded.
/// Test: this IS the test. Against 44ae78992 the snapshot stays in place.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn a_stale_snapshot_beside_a_populated_corpus_is_retired() {
    let _pin = DataDirPin::new();
    let dir = tempdir().expect("tempdir");
    let root = dir.path().to_path_buf();
    let entry = entry_at("superseded-8134", &root, true);

    let first = build_indexer_from_entry(&entry, &mock_embedder())
        .await
        .expect("first boot");
    let mut current = chunk("alpha");
    current.content = "pub fn alpha() { /* current */ }".to_string();
    first
        .corpus_store()
        .expect("corpus store")
        .upsert_batch(&[current, chunk("beta")], &[])
        .expect("populate redb");
    drop(first);
    write_colocated_snapshot(&root, &["alpha", "gamma"]);

    let rebooted = build_indexer_from_entry(&entry, &mock_embedder())
        .await
        .expect("second boot");
    assert!(
        rebooted.migration_faults().is_empty(),
        "a retired snapshot is not a fault, got: {:?}",
        rebooted.migration_faults()
    );
    assert_eq!(
        durable_rows(&rebooted),
        2,
        "redb keeps exactly its own rows"
    );
    let rows = rebooted
        .corpus_store()
        .expect("corpus store")
        .get_chunks(&["alpha", "gamma"])
        .expect("read rows");
    let contents: Vec<&str> = rows.iter().map(|c| c.content.as_str()).collect();
    assert_eq!(
        contents,
        vec!["pub fn alpha() { /* current */ }"],
        "#8134: redb is unchanged: no stale `alpha`, no imported `gamma`"
    );
    let colocated = colocated_dir(&root);
    assert!(
        !colocated.join("chunks.json").exists(),
        "#8134: a superseded snapshot must be retired"
    );
    assert!(
        colocated.join("chunks.json.migrated").is_file(),
        "the retired snapshot is kept, renamed"
    );
}
