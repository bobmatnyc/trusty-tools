//! Issue #4226 regression tests: a #4122 write-quarantined index must perform
//! NO durable write — including the snapshot writers that the quarantine
//! originally left ungated.
//!
//! Why: #4122 gated the *ingest* family (`refuse_incremental_write`) on the
//! predicate `corpus_open_failed`, and the load-bearing invariant
//! `corpus_open_failed ⇒ corpus == None` was taken to make everything else
//! safe. It does not, because `CodeIndexer` has a second write family — the
//! *snapshot* writers — whose enabling condition is `self.corpus.is_none()`,
//! the very thing quarantine guarantees. So `flush_corpus_to_disk` fell
//! through to `save_chunks_to_disk` at shutdown and wrote the quarantined
//! index's deliberately-EMPTY in-memory corpus over the legacy `chunks.json`
//! snapshot, while the refusal diagnostic told the operator "the on-disk
//! corpus is untouched and still recoverable". That snapshot is not inert:
//! `core::indexer::migrations::JsonCorpusToRedbMigration` reads it at warm
//! boot to seed an empty redb, so emptying it destroys a recovery source.
//!
//! What: four tests.
//!   1. `quarantined_shutdown_flush_does_not_destroy_chunks_json`
//!      — the defect verbatim, asserted at the byte level on a real snapshot
//!      file. Against pre-fix code the file comes back holding zero chunks.
//!   2. `quarantined_index_refuses_hnsw_snapshot_write`
//!      — the sibling snapshot writer. A corpus-open failure does not stop
//!      the HNSW store from being wired, so the same shutdown flush kept
//!      saving the graph.
//!   3. `quarantined_incremental_persist_is_refused`
//!      — the second caller of both snapshot writers, on the ingest hot path
//!      rather than at shutdown. Gating one and not the other would leave
//!      the hole open for any index that takes a batch commit.
//!   4. `unquarantined_legacy_index_still_writes_chunks_json`
//!      — the anti-over-refusal control. The gate is on `corpus_open_failed`,
//!      NOT on `corpus.is_none()`: a legitimate BM25-only / legacy indexer
//!      has no corpus either and must keep writing its snapshot.
//!
//! The quarantine is induced through a production path (a DIRECTORY where
//! `index.redb` belongs, the same portable technique
//! `corpus_open_quarantine_4122.rs` uses), never by setting
//! `corpus_open_failed` by hand.
//!
//! Test: `cargo test -p trusty-search --test quarantine_durable_writes_4226`

use std::path::{Path, PathBuf};
use std::sync::Arc;

use tempfile::{tempdir, TempDir};
use trusty_common::embedder::MockEmbedder;
use trusty_search::core::{chunk_ast, CodeIndexer, Embedder, RawChunk};
use trusty_search::service::persistence::PersistedIndex;
use trusty_search::service::persistence_loader::build_indexer_from_entry;

/// Pin `TRUSTY_DATA_DIR` at a scratch directory for this whole test binary.
///
/// Why: `spawn_incremental_persist` resolves the legacy snapshot path through
/// `service::persistence::chunks_path`, which falls back to the real user data
/// directory when the variable is unset. A test that exercises the pre-fix
/// path would then write into the operator's live daemon data dir. Pinning it
/// once, process-wide, makes that impossible regardless of which test runs.
/// What: leaks a `TempDir` for the process lifetime (dropping it would let a
/// later test resolve a dangling path) and sets the variable exactly once.
/// Test: called by every test below.
fn pin_scratch_data_dir() -> &'static Path {
    use std::sync::OnceLock;
    static SCRATCH: OnceLock<TempDir> = OnceLock::new();
    let dir = SCRATCH.get_or_init(|| {
        let dir = tempdir().expect("scratch data dir");
        // SAFETY: set once, before any other thread in this binary reads it.
        unsafe { std::env::set_var("TRUSTY_DATA_DIR", dir.path()) };
        dir
    });
    dir.path()
}

fn mock_embedder() -> Arc<dyn Embedder> {
    Arc::new(MockEmbedder::new(8))
}

/// Build a colocated `PersistedIndex` entry rooted at `root`.
///
/// Why: colocated storage keeps every path this test cares about under the
/// temp root, so nothing resolves against the machine's real data dir.
fn entry_at(id: &str, root: &Path) -> PersistedIndex {
    {
        let mut e = PersistedIndex::new(id.to_string(), root.to_path_buf());
        e.colocated = true;
        e
    }
}

/// Make `CorpusStore::open` fail portably by putting a DIRECTORY where redb
/// expects a plain file. Mirrors `corpus_open_quarantine_4122.rs`.
fn sabotage_corpus_path(root: &Path) {
    let colocated = root.join(".trusty-search");
    std::fs::create_dir_all(&colocated).expect("create colocated dir");
    std::fs::create_dir_all(colocated.join("index.redb")).expect("create dir at redb path");
}

/// The path `service::shutdown_flush::flush_one_index` hands to
/// `flush_corpus_to_disk` for a colocated index.
fn colocated_chunks_path(root: &Path) -> PathBuf {
    root.join(".trusty-search").join("chunks.json")
}

/// Write a real legacy snapshot at `path` and return the chunks it holds.
///
/// Why: the assertion has to be about DATA, not about a call not happening —
/// the defect is that a populated file comes back empty. The payload is a
/// genuine `chunk_ast` product serialized in the `ChunkSnapshot` shape
/// `load_chunks_from_disk` parses, so a surviving file is one the warm-boot
/// migration could still read.
fn seed_legacy_snapshot(path: &Path) -> Vec<RawChunk> {
    let (chunks, _) = chunk_ast(
        "src/original.rs",
        "pub fn alpha() -> u32 { 1 }\npub fn beta() -> u32 { 2 }\n",
    );
    assert!(
        !chunks.is_empty(),
        "precondition: the fixture must produce at least one chunk"
    );
    let snapshot = serde_json::json!({
        "version": 1,
        "chunks": chunks,
        "entities": [],
    });
    std::fs::create_dir_all(path.parent().expect("snapshot has a parent"))
        .expect("create snapshot parent");
    std::fs::write(
        path,
        serde_json::to_vec(&snapshot).expect("serialize snapshot"),
    )
    .expect("write snapshot");
    chunks
}

/// Number of chunks the snapshot at `path` currently holds.
fn snapshot_chunk_count(path: &Path) -> usize {
    let bytes = std::fs::read(path).expect("snapshot must still exist");
    let parsed: serde_json::Value = serde_json::from_slice(&bytes).expect("snapshot must parse");
    parsed["chunks"]
        .as_array()
        .expect("snapshot must carry a chunks array")
        .len()
}

/// Issue #4226 — THE DATA LOSS.
///
/// Why: this is the defect verbatim. The shutdown flush is the one durable
/// write a #4122-quarantined index still performed, and it wrote an empty
/// corpus over a populated legacy snapshot — silently, while the quarantine's
/// own ERROR diagnostic claimed the on-disk corpus was untouched.
/// What: quarantines a colocated index through the real loader, seeds a real
/// `chunks.json` next to it, calls `flush_corpus_to_disk` with exactly the
/// path `shutdown_flush` would pass, and asserts the file is byte-identical
/// afterwards.
/// Test: this IS the test. Against pre-fix code the byte comparison fails and
/// `snapshot_chunk_count` returns 0.
#[tokio::test]
async fn quarantined_shutdown_flush_does_not_destroy_chunks_json() {
    pin_scratch_data_dir();
    let tmp = tempdir().expect("tempdir");
    let root = tmp.path().to_path_buf();
    sabotage_corpus_path(&root);

    let chunks_path = colocated_chunks_path(&root);
    let seeded = seed_legacy_snapshot(&chunks_path);
    let before = std::fs::read(&chunks_path).expect("read seeded snapshot");

    let indexer = build_indexer_from_entry(&entry_at("flush-4226", &root), &mock_embedder())
        .await
        .expect("build indexer");
    assert!(
        indexer.is_write_quarantined(),
        "precondition: a directory at the redb path must quarantine the index"
    );
    assert_eq!(
        indexer.chunk_count(),
        0,
        "precondition: the quarantined index's in-memory corpus is empty — that \
         emptiness is what used to be written out"
    );

    // The shutdown flush, exactly as `service::shutdown_flush` performs it.
    indexer
        .flush_corpus_to_disk(&chunks_path)
        .await
        .expect("the flush must not surface an error — it is refused, not failed");

    let after = std::fs::read(&chunks_path).expect("snapshot must still exist");
    assert_eq!(
        snapshot_chunk_count(&chunks_path),
        seeded.len(),
        "#4226: a write-quarantined index must not overwrite the legacy \
         chunks.json snapshot with its empty in-memory corpus — that snapshot is \
         still read by the warm-boot chunks.json → index.redb migration, so \
         emptying it destroys a recovery source"
    );
    assert_eq!(
        before, after,
        "#4226: the snapshot must be byte-identical — a rewrite that happens to \
         preserve the chunk count is still a durable write a quarantined index \
         must not perform"
    );
    assert!(
        indexer.refused_incremental_writes() > 0,
        "the refusal must be counted so an operator can see that a durable write \
         was dropped rather than silently succeeding"
    );
    assert!(
        indexer.is_write_quarantined(),
        "the index must remain quarantined until a corpus open succeeds"
    );
}

/// Issue #4226 — THE SIBLING SNAPSHOT WRITER.
///
/// Why: gating the `chunks.json` call alone would leave the HNSW graph — the
/// other artifact the same shutdown flush writes — exposed. A corpus-open
/// failure does not prevent the vector store from being wired
/// (`build_store_for_entry` runs before the corpus open), and
/// `UsearchStore::save`'s own guards do not cover this case: the #1711
/// zero-vector guard only protects an existing snapshot, and the #1717 shrink
/// guard is inert below 1 000 on-disk vectors.
/// What: quarantines an index, then calls `save_vector_store` at a path that
/// does not yet exist and asserts nothing is created there.
/// Test: this IS the test. Against pre-fix code the file appears.
#[tokio::test]
async fn quarantined_index_refuses_hnsw_snapshot_write() {
    pin_scratch_data_dir();
    let tmp = tempdir().expect("tempdir");
    let root = tmp.path().to_path_buf();
    sabotage_corpus_path(&root);

    let indexer = build_indexer_from_entry(&entry_at("hnsw-4226", &root), &mock_embedder())
        .await
        .expect("build indexer");
    assert!(indexer.is_write_quarantined(), "precondition: quarantined");

    let hnsw_path = root.join(".trusty-search").join("hnsw.usearch");
    assert!(
        !hnsw_path.exists(),
        "precondition: the HNSW snapshot path starts empty"
    );

    let saved = indexer
        .save_vector_store(&hnsw_path)
        .await
        .expect("the save must not surface an error — it is refused, not failed");

    assert!(
        !saved,
        "#4226: save_vector_store must report that it wrote nothing while \
         quarantined"
    );
    assert!(
        !hnsw_path.exists(),
        "#4226: a write-quarantined index must not write its HNSW graph — the \
         store is wired even when the corpus open failed, so this path is live"
    );
    assert!(
        indexer.refused_incremental_writes() > 0,
        "the refused HNSW save must be counted"
    );
}

/// Issue #4226 — THE INGEST-SIDE CALLER OF THE SAME WRITERS.
///
/// Why: `spawn_incremental_persist` is the second caller of both snapshot
/// writers, reached from `commit_parsed_batch` rather than from shutdown. The
/// bulk reindex path is deliberately ungated on a quarantined index (boot
/// reconcile auto-fires it), so every batch it commits used to reach this
/// persister — which resolves `chunks.json` precisely because no corpus is
/// wired. Gating shutdown but not this would leave the hole open for any
/// quarantined index that takes a reindex.
/// What: quarantines an index and forces one checkpoint, asserting the
/// refusal is counted. The counter is the deterministic observable here: the
/// persister is a detached task, so "no file appeared" would be a race,
/// whereas the refusal is recorded synchronously before anything is spawned.
/// Test: this IS the test. Against pre-fix code the counter stays at 0.
#[tokio::test]
async fn quarantined_incremental_persist_is_refused() {
    pin_scratch_data_dir();
    let tmp = tempdir().expect("tempdir");
    let root = tmp.path().to_path_buf();
    sabotage_corpus_path(&root);

    let indexer = build_indexer_from_entry(&entry_at("persist-4226", &root), &mock_embedder())
        .await
        .expect("build indexer");
    assert!(indexer.is_write_quarantined(), "precondition: quarantined");
    let before = indexer.refused_incremental_writes();

    indexer.force_incremental_persist();

    assert_eq!(
        indexer.refused_incremental_writes(),
        before + 1,
        "#4226: the incremental persister must refuse to checkpoint a \
         write-quarantined index — it writes BOTH the HNSW graph and, because no \
         corpus is wired, the legacy chunks.json snapshot"
    );
}

/// Issue #4226 — ANTI-OVER-REFUSAL CONTROL.
///
/// Why: the enabling condition for the snapshot writers is
/// `corpus.is_none()`, and quarantine is only one of the two ways to satisfy
/// it. A gate placed on `corpus.is_none()` instead of on `corpus_open_failed`
/// would silently stop legitimate BM25-only / legacy indexers from persisting
/// anything at all — trading a data-loss bug for a data-never-saved bug.
/// What: builds a plain `CodeIndexer` with no corpus and no open failure, and
/// asserts `save_chunks_to_disk` still produces a readable snapshot.
/// Test: this IS the test. It fails against a fix that gates on the wrong
/// predicate.
#[tokio::test]
async fn unquarantined_legacy_index_still_writes_chunks_json() {
    pin_scratch_data_dir();
    let tmp = tempdir().expect("tempdir");
    let path = tmp.path().join("legacy").join("chunks.json");

    let indexer = CodeIndexer::new("legacy-4226", tmp.path());
    assert!(
        !indexer.is_write_quarantined(),
        "precondition: a legacy indexer has no corpus but is NOT quarantined"
    );

    indexer
        .save_chunks_to_disk(&path)
        .await
        .expect("a healthy legacy index must still write its snapshot");

    assert!(
        path.exists(),
        "#4226: the quarantine gate must key on corpus_open_failed, not on \
         corpus.is_none() — a legitimate legacy/BM25-only index must keep \
         persisting its snapshot"
    );
    assert_eq!(
        snapshot_chunk_count(&path),
        0,
        "the written snapshot must be a well-formed (here: empty) ChunkSnapshot"
    );
    assert_eq!(
        indexer.refused_incremental_writes(),
        0,
        "nothing may be counted as refused on a healthy index"
    );
}
