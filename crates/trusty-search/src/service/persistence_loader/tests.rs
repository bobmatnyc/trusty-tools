//! Unit tests for [`super::build_indexer_from_entry`] and its helpers.
//!
//! Why: split out of `persistence_loader/mod.rs` (issue #1195 / SLOC cap) —
//! the module grew past the 500-SLOC production cap once the #2984 Phase-0
//! follow-up regression test (`skip_kg_true_entry_never_loads_persisted_graph_via_build_indexer_from_entry`)
//! landed. Test code carries no production logic, so it lives in its own
//! file per the crate's directory-module convention (see
//! `core::indexer::tests`).
//! What: covers `build_indexer_from_entry` / `build_store_for_entry` across
//! the colocated/legacy, corrupt-snapshot, dedup, and skip_kg regression
//! scenarios.
//! Test: this module.
use super::*;
use crate::core::chunker::{ChunkType, RawChunk};
use crate::service::colocated_storage;
use crate::service::persistence::PersistedIndex;
use tempfile::tempdir;
use trusty_common::embedder::MockEmbedder;

// ── Helper ────────────────────────────────────────────────────────────────

fn mock_embedder() -> Arc<dyn crate::core::embed::Embedder> {
    Arc::new(MockEmbedder::new(8))
}

fn minimal_raw_chunk(id: &str) -> RawChunk {
    RawChunk {
        id: id.to_string(),
        file: "src/lib.rs".to_string(),
        start_line: 1,
        end_line: 5,
        content: "fn hello() {}".to_string(),
        function_name: None,
        language: Some("rust".to_string()),
        chunk_type: ChunkType::Code,
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

// ── Issue #313 / #2984 (Phase 0 follow-up) regression ────────────────────

/// End-to-end regression for the `create_index_handler` re-register door:
/// `build_indexer_from_entry` itself must never load a persisted symbol
/// graph for an entry whose `skip_kg` is `true`, even when that entry's
/// redb corpus already has a fully-built graph from an earlier session.
///
/// Why: this is the exact bug the code-critic caught in the initial
/// #2988 PR — the Phase-0 fix wired `CodeIndexer::skip_kg` into
/// `build_indexer_from_entry` via `entry.skip_kg`, but
/// `create_index_handler` (the `POST /indexes` create/re-register path,
/// issue #85's warm-restore-on-create) built its `init_entry` with
/// `..Default::default()` (`skip_kg = false`) BEFORE the real `skip_kg`
/// value was computed further down the handler, and never propagated the
/// real value back onto the entry/indexer. So a `POST /indexes` call with
/// `skip_kg=true` against an id with a persisted graph on disk still
/// loaded that graph into memory during the SAME call — the identical
/// warm-boot bug via a second door. This test exercises
/// `build_indexer_from_entry` directly (the shared helper both the
/// daemon-restart path and `create_index_handler` call) with a
/// `skip_kg=true` entry pointed at a corpus that already has a persisted
/// graph, proving the loader itself honors the flag regardless of caller.
/// What: builds an indexer with `skip_kg=false`, indexes two files with a
/// real caller→callee edge (persisting a non-empty graph to the redb
/// corpus), drops it, then calls `build_indexer_from_entry` again for the
/// SAME `id`/`root_path` with `skip_kg=true` and asserts the returned
/// indexer's symbol graph is empty.
/// Test: this IS the test.
#[tokio::test]
async fn skip_kg_true_entry_never_loads_persisted_graph_via_build_indexer_from_entry() {
    let tmp = tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    let embedder = mock_embedder();

    let entry_no_skip = PersistedIndex {
        id: "test-idx-2984-create-path".to_string(),
        root_path: root.clone(),
        colocated: true,
        skip_kg: false,
        ..Default::default()
    };

    // Phase 1: build with skip_kg=false, index chunks with a real call
    // edge, and confirm a non-empty graph was actually built + persisted.
    {
        let indexer = build_indexer_from_entry(&entry_no_skip, &embedder)
            .await
            .unwrap();
        indexer
            .index_files_batch(&[
                ("src/caller.rs".into(), "fn caller() { callee(); }".into()),
                ("src/callee.rs".into(), "fn callee() {}".into()),
            ])
            .await
            .expect("index batch");
        let graph = indexer.snapshot_symbol_graph().await;
        assert!(
            graph.node_count() > 0,
            "sanity: normal indexing must build a non-empty symbol graph"
        );
    } // indexer (and its corpus Arc) dropped — releases the redb file lock.

    // Phase 2: re-register the SAME id/root_path — this mirrors what
    // create_index_handler does when a caller re-creates an existing
    // index id with skip_kg=true — via a fresh `build_indexer_from_entry`
    // call with `skip_kg=true` on the entry.
    let entry_skip = PersistedIndex {
        skip_kg: true,
        ..entry_no_skip
    };
    let restored = build_indexer_from_entry(&entry_skip, &embedder)
        .await
        .unwrap();
    let graph = restored.snapshot_symbol_graph().await;
    assert_eq!(
        graph.node_count(),
        0,
        "#2984: build_indexer_from_entry with skip_kg=true must not load \
         a persisted symbol graph, even when one already exists on disk"
    );
    assert_eq!(
        graph.edge_count(),
        0,
        "#2984: build_indexer_from_entry with skip_kg=true must not load \
         persisted graph edges either"
    );
}

// ── Issue #483 regression ─────────────────────────────────────────────────

/// Why: guards the writer/loader path divergence (#483) where
/// `create_index_handler` used `colocated: false`, routing the corpus to
/// app-data, while warm-boot used `colocated: true`, opening an empty
/// `.trusty-search/` dir.
/// What: builds a colocated indexer, writes a chunk, drops the handle
/// (releasing the redb file lock), then reloads via `build_indexer_from_entry`
/// and asserts the chunk count is > 0.
/// Test: this IS the test; exercises the exact call path used by the fixed
/// `create_index_handler`.
#[tokio::test]
async fn colocated_create_handler_path_survives_simulated_reload() {
    let tmp = tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    let embedder = mock_embedder();

    // --- Phase 1: simulate what the fixed create_index_handler does ---
    let entry = PersistedIndex {
        id: "test-idx-483".to_string(),
        root_path: root.clone(),
        colocated: true,
        ..Default::default()
    };
    // build_indexer_from_entry (the fixed call path): colocated_storage_dir
    // is invoked via corpus_redb_path_for_entry → colocated_redb_path →
    // colocated_storage_dir, creating `.trusty-search/` on disk.
    let indexer = build_indexer_from_entry(&entry, &embedder).await.unwrap();

    // The corpus store must be wired and pointing into `.trusty-search/`.
    assert!(
        indexer.has_corpus_store(),
        "#483: indexer must have a corpus store after build_indexer_from_entry with colocated=true"
    );
    // `.trusty-search/` must exist so the write-path probes see it.
    assert!(
        colocated_storage::has_colocated_storage(&root),
        "#483: .trusty-search/ must exist after build_indexer_from_entry with colocated=true"
    );

    // Write a chunk to the wired corpus so the reload can verify it.
    // Scope the write so the corpus Arc is dropped before we re-open
    // the same redb file for the simulated restart (redb uses file locking
    // and rejects a second concurrent open of the same database file).
    {
        let corpus = indexer.corpus_store().expect("corpus store must be set");
        corpus
            .upsert_chunks(&[minimal_raw_chunk("src/lib.rs:1:5")])
            .expect("upsert must succeed");
        // corpus Arc dropped here.
    }
    // Drop the whole indexer (and its internal corpus Arc) before reopening.
    drop(indexer);

    // --- Phase 2: simulate daemon restart — reload from the same entry ---
    // This is what `restore_indexes` does on startup.
    let reloaded = build_indexer_from_entry(&entry, &embedder).await.unwrap();
    assert!(
        reloaded.has_corpus_store(),
        "#483: reloaded indexer must have a corpus store"
    );
    let chunk_count = reloaded
        .corpus_store()
        .expect("corpus store must be set")
        .chunk_count()
        .expect("chunk_count must succeed");
    assert!(
        chunk_count > 0,
        "#483: reloaded index must contain the written chunk (got {chunk_count}); \
         writer/loader paths must agree on the colocated location"
    );
}

// ── Issue #2922 regression ────────────────────────────────────────────────

/// Why: this is the acceptance test for the "health reports semantic
/// ready while silently BM25-only" half of issue #2922. Before the fix,
/// `hnsw_snapshot_ready` (and therefore `hnsw_load_failed`) had no way to
/// tell "a file exists" apart from "a file exists AND loaded correctly" —
/// a truncated `.usearch` snapshot still passed `has_persisted_hnsw`.
/// What: saves a real HNSW snapshot with one vector via `UsearchStore`,
/// truncates the on-disk `.usearch` file to simulate the exact corruption
/// reported in #2922 (a timed-out shutdown flush cutting a save short),
/// then calls `build_indexer_from_entry` and asserts `hnsw_load_failed`
/// is `true` and the restored store is empty (never a silent partial
/// state).
/// Test: this IS the test.
#[tokio::test]
async fn build_store_for_entry_flags_load_failure_on_corrupt_snapshot() {
    use crate::core::store::{UsearchStore, VectorStore};

    let tmp = tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    let embedder = mock_embedder();
    let dim = embedder.dimension();

    let entry = PersistedIndex {
        id: "test-idx-2922".to_string(),
        root_path: root.clone(),
        colocated: true,
        ..Default::default()
    };
    let hnsw_path = persistence::hnsw_path_for_entry(&entry).unwrap();

    // Write a real, valid snapshot containing one vector.
    let store = UsearchStore::new(dim).unwrap();
    store
        .upsert("chunk-1", vec![0.1; dim])
        .await
        .expect("upsert must succeed");
    store.save(&hnsw_path).await.expect("save must succeed");
    assert!(hnsw_path.exists(), "precondition: snapshot must exist");

    // Simulate the #2922 corruption: a shutdown flush cut off mid-write,
    // leaving a truncated (but still `path.exists()`-true) file.
    std::fs::write(&hnsw_path, b"truncated").expect("truncate hnsw.usearch");

    let (loaded_store, load_failed): (Arc<dyn VectorStore>, bool) =
        build_store_for_entry(&entry, dim).await.unwrap();
    assert!(
        load_failed,
        "#2922: a snapshot file that exists but fails to load must set load_failed=true"
    );
    assert_eq!(
        loaded_store.len().await.unwrap(),
        0,
        "#2922: a corrupt snapshot must never silently restore partial/wrong data"
    );

    // End-to-end: `build_indexer_from_entry` must propagate the flag onto
    // the indexer so the warm-boot / lazy-load call sites can fold it
    // into `hnsw_snapshot_ready`.
    let indexer = build_indexer_from_entry(&entry, &embedder).await.unwrap();
    assert!(
        indexer.hnsw_load_failed,
        "#2922: CodeIndexer::hnsw_load_failed must be propagated from build_store_for_entry"
    );
}

/// Why: the ordinary first-boot case (no snapshot has ever been written)
/// must NOT be flagged as a load failure — only a snapshot that existed
/// and couldn't be restored is a failure (issue #2922).
/// What: builds an indexer for an entry with no on-disk HNSW snapshot at
/// all and asserts `hnsw_load_failed` stays `false`.
/// Test: this IS the test.
#[tokio::test]
async fn build_store_for_entry_does_not_flag_first_boot_as_failure() {
    let tmp = tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    let embedder = mock_embedder();

    let entry = PersistedIndex {
        id: "test-idx-2922-firstboot".to_string(),
        root_path: root.clone(),
        colocated: true,
        ..Default::default()
    };
    let indexer = build_indexer_from_entry(&entry, &embedder).await.unwrap();
    assert!(
        !indexer.hnsw_load_failed,
        "#2922: a never-indexed entry (no snapshot file at all) must not be \
         flagged as a load failure"
    );
}

// ── Issue #485 regression ─────────────────────────────────────────────────

/// Why: guards the "cannot write schema_version: no durable corpus" error
/// (#485) that occurred when the corpus store was not wired for a colocated
/// entry.
/// What: builds an indexer via the colocated entry path and asserts both
/// `has_corpus_store` and that the corpus path is inside the project root.
/// Test: this IS the test.
#[tokio::test]
async fn colocated_create_path_wires_corpus_store_for_schema_version() {
    let tmp = tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    let embedder = mock_embedder();

    let entry = PersistedIndex {
        id: "test-idx-485".to_string(),
        root_path: root.clone(),
        colocated: true,
        ..Default::default()
    };
    let indexer = build_indexer_from_entry(&entry, &embedder).await.unwrap();

    // Corpus store must be present — this is the prerequisite that prevents
    // the "cannot write schema_version: no durable corpus" error (#485).
    assert!(
        indexer.has_corpus_store(),
        "#485: indexer built via colocated create path must have corpus store; \
         without it write_schema_version returns 'no durable corpus'"
    );

    // Also confirm the store is backed by the colocated path, not app-data.
    if let Some(corpus) = indexer.corpus_store() {
        let corpus_path = corpus.path().to_path_buf();
        assert!(
            corpus_path.starts_with(&root),
            "#485: corpus store must be inside the project root (colocated); \
             got {corpus_path:?}"
        );
    }
}

// ── Guard: legacy colocated=false path is unchanged ───────────────────────

/// Why: the fix must not break legacy (`colocated: false`) indexes.
/// What: builds with `colocated: false`; asserts no panic even when no
/// app-data corpus exists.
/// Test: this IS the test.
#[tokio::test]
async fn legacy_non_colocated_path_does_not_panic() {
    let tmp = tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    let embedder = mock_embedder();

    let entry = PersistedIndex {
        id: "test-idx-legacy".to_string(),
        root_path: root.clone(),
        colocated: false,
        ..Default::default()
    };
    // Must not fail even when no app-data corpus exists.
    build_indexer_from_entry(&entry, &embedder).await.unwrap();
    // Key invariant: no error. Corpus store presence is not asserted here:
    // app-data path may or may not resolve in a test environment.
}

// ── Issue #2305 regression: shared-root redb double-open ───────────────────

/// Why (issue #2305 — the root cause behind #1870): two colocated
/// registrations of the SAME `root_path` resolve to one
/// `<root>/.trusty-search/index.redb`. redb is a single-open database, so
/// warm-booting both opens the file twice and the second fails with
/// `DatabaseAlreadyOpen` (surfaced as `corpus_open_failed`) — the 50 ms
/// retry can never clear an in-process holder, so it recurs every restart.
/// This test proves the hazard is real (a second, concurrently-held open of
/// the shared redb fails) and that `dedup_entries_by_corpus_path` collapses
/// the pair to exactly one entry so the warm-boot open path opens once.
/// What: opens `apex`, then — while that handle is still held — opens `mpm`
/// at the same root and asserts `mpm` reports `corpus_open_failed`; then runs
/// the pair through the dedup and asserts it collapses to a single survivor.
/// Only one *successful* corpus open is performed (the failing second open
/// returns immediately) so the test stays cheap. Data-survival across a
/// reopen is already covered by
/// `colocated_create_handler_path_survives_simulated_reload`, and survivor
/// selection by `dedup_by_corpus_path_keeps_most_recent`.
/// Test: this IS the test.
#[tokio::test]
async fn warm_boot_shared_root_double_open_is_prevented_by_dedup() {
    let tmp = tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    let embedder = mock_embedder();

    let apex = PersistedIndex {
        id: "apex".to_string(),
        root_path: root.clone(),
        colocated: true,
        last_indexed_unix: Some(100),
        ..Default::default()
    };
    let mpm = PersistedIndex {
        id: "trusty-mpm".to_string(),
        root_path: root.clone(),
        colocated: true,
        last_indexed_unix: Some(50),
        ..Default::default()
    };

    // Prove the hazard: the first open succeeds and holds the corpus; a
    // second open of the same redb file — the exact shape of the sequential
    // warm-boot restore over two shared-root slots — fails with
    // DatabaseAlreadyOpen (surfaced as corpus_open_failed, the #1870 symptom).
    let first = build_indexer_from_entry(&apex, &embedder).await.unwrap();
    let second = build_indexer_from_entry(&mpm, &embedder).await.unwrap();
    assert!(
        first.has_corpus_store() && !first.corpus_open_failed,
        "the first open of the shared redb must succeed"
    );
    assert!(
        second.corpus_open_failed,
        "#2305: a concurrent second open of the shared redb must fail \
         (DatabaseAlreadyOpen) — this is the bug the dedup prevents"
    );
    drop(first);
    drop(second);

    // Apply the fix: dedup collapses the two shared-root entries to one, so
    // the warm-boot restore opens the shared redb exactly once → zero
    // DatabaseAlreadyOpen. (The survivor is the most recent, `apex`.)
    let survivors = crate::service::warm_boot::dedup_entries_by_corpus_path(vec![apex, mpm]);
    assert_eq!(
        survivors.len(),
        1,
        "#2305: two entries sharing one redb corpus must collapse to one"
    );
    assert_eq!(
        survivors[0].id, "apex",
        "the most-recently-active shared-root entry must be the survivor"
    );
}

/// Why: the fix must not merge indexes that live in different redb files.
/// Two colocated indexes at distinct roots must open independent corpora
/// simultaneously with zero `DatabaseAlreadyOpen`, and dedup must keep both.
/// What: builds two colocated indexers at distinct roots while both handles
/// are held; asserts both are healthy and dedup returns both entries.
/// Test: this IS the test.
#[tokio::test]
async fn distinct_roots_open_independent_corpora() {
    let a = tempdir().unwrap();
    let b = tempdir().unwrap();
    let embedder = mock_embedder();

    let ea = PersistedIndex {
        id: "a".to_string(),
        root_path: a.path().to_path_buf(),
        colocated: true,
        ..Default::default()
    };
    let eb = PersistedIndex {
        id: "b".to_string(),
        root_path: b.path().to_path_buf(),
        colocated: true,
        ..Default::default()
    };

    // Both held open at once — distinct redb files, so no double-open.
    let ia = build_indexer_from_entry(&ea, &embedder).await.unwrap();
    let ib = build_indexer_from_entry(&eb, &embedder).await.unwrap();
    assert!(
        ia.has_corpus_store() && !ia.corpus_open_failed,
        "distinct-root corpus a must open cleanly"
    );
    assert!(
        ib.has_corpus_store() && !ib.corpus_open_failed,
        "distinct-root corpus b must open cleanly"
    );

    // Dedup must leave distinct roots untouched.
    let kept = crate::service::warm_boot::dedup_entries_by_corpus_path(vec![ea, eb]);
    assert_eq!(
        kept.len(),
        2,
        "distinct roots resolve to distinct redb files and must both survive dedup"
    );
}
