//! Tests for the corpus store.
//!
//! Why: exercising the durable corpus in a separate file keeps the impl free
//! of test scaffolding and makes the test surface easy to locate.
//! What: unit tests for `CorpusStore` round-trips, edge cases, and KG
//! persistence; also covers `redb_cache_size_bytes` and `PersistedKgNode`.
//! Test: run with `cargo test -p trusty-search`.

use super::store_impl::CorpusStore;
use super::tables::redb_cache_size_bytes;
use super::tables::DEFAULT_REDB_CACHE_MB;
use super::types::PersistedKgNode;
use crate::core::chunker::{ChunkType, RawChunk};

/// Build a minimal `RawChunk` for tests.
fn raw(id: &str, content: &str) -> RawChunk {
    RawChunk {
        id: id.to_string(),
        file: "src/lib.rs".to_string(),
        start_line: 1,
        end_line: 1,
        content: content.to_string(),
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

#[test]
fn redb_cache_size_default_and_env_override() {
    // Idle-memory audit: the redb page cache defaults to 64 MB (issue #329
    // B.2 quick-win; was 512 MB before empirical profiling confirmed actual
    // fill of ~87 MB) and is overridable via TRUSTY_REDB_CACHE_MB. This test
    // mutates a process-global env var, so it is intentionally self-contained
    // (save/restore the prior value) — no other test in this module reads
    // TRUSTY_REDB_CACHE_MB.
    let prior = std::env::var("TRUSTY_REDB_CACHE_MB").ok();

    // Default: unset → 64 MB.
    // SAFETY: corpus tests do not mutate this env var concurrently.
    unsafe { std::env::remove_var("TRUSTY_REDB_CACHE_MB") };
    assert_eq!(redb_cache_size_bytes(), DEFAULT_REDB_CACHE_MB * 1024 * 1024);

    // Valid override wins.
    // SAFETY: see above.
    unsafe { std::env::set_var("TRUSTY_REDB_CACHE_MB", "1024") };
    assert_eq!(redb_cache_size_bytes(), 1024 * 1024 * 1024);

    // Zero falls back to the default.
    // SAFETY: see above.
    unsafe { std::env::set_var("TRUSTY_REDB_CACHE_MB", "0") };
    assert_eq!(redb_cache_size_bytes(), DEFAULT_REDB_CACHE_MB * 1024 * 1024);

    // Garbage falls back to the default (with a warn).
    // SAFETY: see above.
    unsafe { std::env::set_var("TRUSTY_REDB_CACHE_MB", "not-a-number") };
    assert_eq!(redb_cache_size_bytes(), DEFAULT_REDB_CACHE_MB * 1024 * 1024);

    // Restore.
    // SAFETY: see above.
    unsafe {
        match prior {
            Some(v) => std::env::set_var("TRUSTY_REDB_CACHE_MB", v),
            None => std::env::remove_var("TRUSTY_REDB_CACHE_MB"),
        }
    }
}

#[test]
fn roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let store = CorpusStore::open(&dir.path().join("index.redb")).unwrap();

    let chunks = vec![raw("a:1:1", "fn a() {}"), raw("b:1:1", "fn b() {}")];
    store.upsert_chunks(&chunks).unwrap();
    store
        .upsert_entities(&[("src/lib.rs".to_string(), Vec::new())])
        .unwrap();
    assert_eq!(store.chunk_count().unwrap(), 2);

    // Reopen to simulate a daemon restart.
    drop(store);
    let store = CorpusStore::open(&dir.path().join("index.redb")).unwrap();
    let mut loaded = store.load_all_chunks().unwrap();
    loaded.sort_by(|x, y| x.id.cmp(&y.id));
    assert_eq!(loaded.len(), 2);
    assert_eq!(loaded[0].id, "a:1:1");
    assert_eq!(loaded[0].content, "fn a() {}");

    let entities = store.load_all_entities().unwrap();
    assert_eq!(entities.len(), 1);
    assert_eq!(entities[0].0, "src/lib.rs");
}

#[test]
fn batch_upsert_is_atomic_roundtrip() {
    // Issue #29: `upsert_batch` writes chunks + entities in one redb
    // transaction. A reopened store must see both, exactly as the
    // separate-call `roundtrip` test asserts for `upsert_chunks` /
    // `upsert_entities`.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("index.redb");
    {
        let store = CorpusStore::open(&path).unwrap();
        store
            .upsert_batch(
                &[raw("a:1:1", "fn a() {}"), raw("b:1:1", "fn b() {}")],
                &[("src/lib.rs".to_string(), Vec::new())],
            )
            .unwrap();
        assert_eq!(store.chunk_count().unwrap(), 2);
    }
    // Reopen to simulate a daemon restart — both tables must be intact.
    let store = CorpusStore::open(&path).unwrap();
    let mut loaded = store.load_all_chunks().unwrap();
    loaded.sort_by(|x, y| x.id.cmp(&y.id));
    assert_eq!(loaded.len(), 2);
    assert_eq!(loaded[0].id, "a:1:1");
    let entities = store.load_all_entities().unwrap();
    assert_eq!(entities.len(), 1);
    assert_eq!(entities[0].0, "src/lib.rs");

    // A batch with only chunks still writes the chunks table.
    store
        .upsert_batch(&[raw("c:1:1", "fn c() {}")], &[])
        .unwrap();
    assert_eq!(store.chunk_count().unwrap(), 3);

    // A batch with only entities still writes the entities table.
    store
        .upsert_batch(&[], &[("src/other.rs".to_string(), Vec::new())])
        .unwrap();
    assert_eq!(store.load_all_entities().unwrap().len(), 2);

    // A fully-empty batch is a silent no-op.
    store.upsert_batch(&[], &[]).unwrap();
    assert_eq!(store.chunk_count().unwrap(), 3);
}

#[test]
fn get_chunks_batch_reads_subset() {
    // Issue #28 deferred item: the query hot path materializes top-k
    // results via `get_chunks`. It must return only the requested ids, in
    // input order, and silently skip ids absent from the corpus.
    let dir = tempfile::tempdir().unwrap();
    let store = CorpusStore::open(&dir.path().join("index.redb")).unwrap();
    store
        .upsert_chunks(&[
            raw("a:1:1", "fn a() {}"),
            raw("b:1:1", "fn b() {}"),
            raw("c:1:1", "fn c() {}"),
        ])
        .unwrap();

    // Request a subset out of corpus order, with one unknown id mixed in.
    let got = store
        .get_chunks(&["c:1:1", "missing:0:0", "a:1:1"])
        .unwrap();
    assert_eq!(got.len(), 2, "unknown id must be skipped, not error");
    assert_eq!(got[0].id, "c:1:1", "input order must be preserved");
    assert_eq!(got[0].content, "fn c() {}");
    assert_eq!(got[1].id, "a:1:1");

    // Empty input is a no-op.
    assert!(store.get_chunks(&[]).unwrap().is_empty());

    // All-missing input yields an empty vec, never an error.
    assert!(store.get_chunks(&["nope:0:0"]).unwrap().is_empty());
}

#[test]
fn missing_db_is_empty() {
    // A brand-new database (post-upgrade / first-run) must open cleanly
    // and report an empty corpus rather than erroring.
    let dir = tempfile::tempdir().unwrap();
    let store = CorpusStore::open(&dir.path().join("fresh.redb")).unwrap();
    assert_eq!(store.chunk_count().unwrap(), 0);
    assert!(store.load_all_chunks().unwrap().is_empty());
    assert!(store.load_all_entities().unwrap().is_empty());
}

/// Why: #602 — the reindex orchestrator persists the canonical root the
/// corpus was relativized against so a later run can detect a move. Verify
/// the read returns `None` before any write and the written value round-trips.
/// Test: this test.
#[test]
fn test_meta_indexed_root_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let store = CorpusStore::open(&dir.path().join("index.redb")).unwrap();
    // Never written → None (legacy / first reindex).
    assert_eq!(store.read_indexed_root_sync().unwrap(), None);

    let root = std::path::PathBuf::from("/Users/me/code/project");
    store.write_indexed_root_sync(&root).unwrap();
    assert_eq!(store.read_indexed_root_sync().unwrap(), Some(root.clone()));

    // Overwrite with a new root (the index moved on disk).
    let moved = std::path::PathBuf::from("/mnt/serving/project");
    store.write_indexed_root_sync(&moved).unwrap();
    assert_eq!(store.read_indexed_root_sync().unwrap(), Some(moved));
}

#[test]
fn delete_removes_chunk() {
    let dir = tempfile::tempdir().unwrap();
    let store = CorpusStore::open(&dir.path().join("index.redb")).unwrap();
    store
        .upsert_chunks(&[raw("a:1:1", "x"), raw("b:1:1", "y")])
        .unwrap();
    store.delete_chunks(&["a:1:1".to_string()]).unwrap();
    assert_eq!(store.chunk_count().unwrap(), 1);
    let loaded = store.load_all_chunks().unwrap();
    assert_eq!(loaded.len(), 1);
    assert_eq!(loaded[0].id, "b:1:1");
    // Deleting an unknown id is a silent no-op.
    store.delete_chunks(&["nope:0:0".to_string()]).unwrap();
    assert_eq!(store.chunk_count().unwrap(), 1);
}

#[test]
fn empty_batches_are_noops() {
    let dir = tempfile::tempdir().unwrap();
    let store = CorpusStore::open(&dir.path().join("index.redb")).unwrap();
    store.upsert_chunks(&[]).unwrap();
    store.upsert_entities(&[]).unwrap();
    store.delete_chunks(&[]).unwrap();
    assert_eq!(store.chunk_count().unwrap(), 0);
}

#[test]
fn delete_entities_removes_file_row() {
    let dir = tempfile::tempdir().unwrap();
    let store = CorpusStore::open(&dir.path().join("index.redb")).unwrap();
    store
        .upsert_entities(&[
            ("src/a.rs".to_string(), Vec::new()),
            ("src/b.rs".to_string(), Vec::new()),
        ])
        .unwrap();
    assert_eq!(store.load_all_entities().unwrap().len(), 2);
    store.delete_entities("src/a.rs").unwrap();
    let remaining = store.load_all_entities().unwrap();
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].0, "src/b.rs");
    // Deleting an unknown file is a silent no-op.
    store.delete_entities("src/never.rs").unwrap();
    assert_eq!(store.load_all_entities().unwrap().len(), 1);
}

#[test]
fn path_accessor_returns_open_path() {
    // Issue #28 Phase 4: the atomic-swap path reads `path()` to know which
    // file to rename. It must echo back exactly what `open` was given.
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("index.redb");
    let store = CorpusStore::open(&p).unwrap();
    assert_eq!(store.path(), p.as_path());
}

/// Why: verifies that `upsert_file_hashes` + `load_file_hashes` round-trip
/// correctly across a store reopen (simulates daemon restart).
/// Test: this test.
#[test]
fn hash_cache_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("index.redb");
    {
        let store = CorpusStore::open(&path).unwrap();
        // Empty table before any writes.
        assert!(store.load_file_hashes().unwrap().is_empty());
        // Upsert two entries.
        store
            .upsert_file_hashes(&[("src/a.rs", "aabbcc"), ("src/b.rs", "ddeeff")])
            .unwrap();
        let mut loaded = store.load_file_hashes().unwrap();
        loaded.sort_by(|x, y| x.0.cmp(&y.0));
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0], ("src/a.rs".to_string(), "aabbcc".to_string()));
        assert_eq!(loaded[1], ("src/b.rs".to_string(), "ddeeff".to_string()));
        // Upsert is idempotent (overwrite with same value).
        store.upsert_file_hashes(&[("src/a.rs", "aabbcc")]).unwrap();
        assert_eq!(store.load_file_hashes().unwrap().len(), 2);
        // Upsert overwrites with new value.
        store.upsert_file_hashes(&[("src/a.rs", "112233")]).unwrap();
        let mut loaded2 = store.load_file_hashes().unwrap();
        loaded2.sort_by(|x, y| x.0.cmp(&y.0));
        assert_eq!(loaded2[0].1, "112233");
    }
    // Reopen simulates daemon restart — hashes must survive.
    let store = CorpusStore::open(&path).unwrap();
    let mut loaded = store.load_file_hashes().unwrap();
    loaded.sort_by(|x, y| x.0.cmp(&y.0));
    assert_eq!(loaded.len(), 2);
    assert_eq!(loaded[0].0, "src/a.rs");
    assert_eq!(loaded[0].1, "112233");
}

/// Why: verifies that `clear_file_hashes` removes all entries and an
/// empty input to `upsert_file_hashes` is a no-op.
/// Test: this test.
#[test]
fn hash_cache_clear() {
    let dir = tempfile::tempdir().unwrap();
    let store = CorpusStore::open(&dir.path().join("index.redb")).unwrap();
    store
        .upsert_file_hashes(&[("src/a.rs", "aa"), ("src/b.rs", "bb")])
        .unwrap();
    assert_eq!(store.load_file_hashes().unwrap().len(), 2);
    store.clear_file_hashes().unwrap();
    assert!(store.load_file_hashes().unwrap().is_empty());
    // Double-clear is a no-op, not an error.
    store.clear_file_hashes().unwrap();
    // Empty upsert is also a no-op.
    store.upsert_file_hashes(&[]).unwrap();
    assert!(store.load_file_hashes().unwrap().is_empty());
}

#[test]
fn open_fresh_truncates_stale_staging_file() {
    // Issue #28 Phase 4: a stale `index.redb.tmp` left by an aborted
    // reindex must not contribute pre-existing rows to the next staged
    // corpus — `open_fresh` discards the old file first.
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("index.redb.tmp");

    // Populate, then drop so the file is closed and persisted on disk.
    {
        let store = CorpusStore::open(&p).unwrap();
        store.upsert_chunks(&[raw("stale:1:1", "old")]).unwrap();
        assert_eq!(store.chunk_count().unwrap(), 1);
    }
    assert!(p.exists());

    // `open_fresh` must yield an empty corpus despite the existing file.
    let fresh = CorpusStore::open_fresh(&p).unwrap();
    assert_eq!(fresh.chunk_count().unwrap(), 0);
    assert_eq!(fresh.path(), p.as_path());

    // And `open_fresh` on a path that does not exist is also fine.
    let fresh2 = CorpusStore::open_fresh(&dir.path().join("never.redb.tmp")).unwrap();
    assert_eq!(fresh2.chunk_count().unwrap(), 0);
}

/// Issue #41 phase 2: round-trip a tiny KG through `save_kg_graph` and
/// `load_kg_graph`. Closes (and reopens) the store between save and load
/// to prove the data is durable, not just held in process memory.
#[test]
fn save_load_kg_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("index.redb");

    let nodes = vec![
        (
            "alpha".to_string(),
            PersistedKgNode {
                chunk_id: "a:1:1".into(),
                file: "a.rs".into(),
            },
        ),
        (
            "beta".to_string(),
            PersistedKgNode {
                chunk_id: "b:1:1".into(),
                file: "b.rs".into(),
            },
        ),
    ];
    let adj_fwd = vec![(
        "alpha".to_string(),
        vec![("CallsFunction".to_string(), "beta".to_string())],
    )];
    let adj_rev = vec![(
        "beta".to_string(),
        vec![("CallsFunction".to_string(), "alpha".to_string())],
    )];

    {
        let store = CorpusStore::open(&path).unwrap();
        store
            .save_kg_graph(&nodes, &adj_fwd, &adj_rev)
            .expect("save kg");
        assert_eq!(store.kg_node_count().unwrap(), 2);
    }

    // Reopen and assert every row survived.
    let store = CorpusStore::open(&path).unwrap();
    let (loaded_nodes, loaded_fwd, loaded_rev) = store.load_kg_graph().unwrap();
    assert_eq!(loaded_nodes.len(), 2);
    assert_eq!(loaded_fwd, adj_fwd);
    assert_eq!(loaded_rev, adj_rev);

    // Saving an empty graph clears every table.
    store.save_kg_graph(&[], &[], &[]).unwrap();
    assert_eq!(store.kg_node_count().unwrap(), 0);
    let (n, f, r) = store.load_kg_graph().unwrap();
    assert!(n.is_empty() && f.is_empty() && r.is_empty());
}

/// Why: validates that `copy_all_from` (the #839 fix) correctly bulk-copies
/// chunks, entities, and file hashes from a live corpus into a fresh staging
/// store, and that the staging store is empty before the copy so an empty
/// source is a harmless no-op.
///
/// What: writes chunks + entities + hashes to a source store, calls
/// `copy_all_from` to seed a fresh staging store, asserts all rows are
/// present in staging, then verifies an empty source no-ops cleanly.
///
/// Test: this test.
#[test]
fn copy_all_from_seeds_staging_corpus() {
    let dir = tempfile::tempdir().unwrap();

    // Build the live source corpus with chunks, entities, and hashes.
    let src_path = dir.path().join("index.redb");
    let src = CorpusStore::open(&src_path).unwrap();
    src.upsert_chunks(&[
        {
            let mut c = raw("stable:1:1", "fn stable() {}");
            c.file = "stable.rs".to_string();
            c
        },
        {
            let mut c = raw("other:1:1", "fn other() {}");
            c.file = "other.rs".to_string();
            c
        },
    ])
    .unwrap();
    src.upsert_entities(&[
        ("stable.rs".to_string(), Vec::new()),
        ("other.rs".to_string(), Vec::new()),
    ])
    .unwrap();
    src.upsert_file_hashes(&[("stable.rs", "aabbcc"), ("other.rs", "ddeeff")])
        .unwrap();
    let root = dir.path().to_path_buf();
    src.write_indexed_root_sync(&root).unwrap();

    // Seed a fresh staging corpus from the live source.
    let staging_path = dir.path().join("index.redb.tmp");
    let staging = CorpusStore::open_fresh(&staging_path).unwrap();
    assert_eq!(
        staging.chunk_count().unwrap(),
        0,
        "staging must start empty"
    );

    staging.copy_all_from(&src).unwrap();

    // All chunks must be present.
    assert_eq!(staging.chunk_count().unwrap(), 2);
    let mut chunks = staging.load_all_chunks().unwrap();
    chunks.sort_by(|a, b| a.id.cmp(&b.id));
    assert_eq!(chunks[0].id, "other:1:1");
    assert_eq!(chunks[1].id, "stable:1:1");

    // All entities must be present.
    let entities = staging.load_all_entities().unwrap();
    assert_eq!(entities.len(), 2);

    // All file hashes must be present.
    let mut hashes = staging.load_file_hashes().unwrap();
    hashes.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(hashes.len(), 2);
    assert_eq!(hashes[0], ("other.rs".to_string(), "ddeeff".to_string()));
    assert_eq!(hashes[1], ("stable.rs".to_string(), "aabbcc".to_string()));

    // The _meta indexed_root must also have been copied.
    assert_eq!(staging.read_indexed_root_sync().unwrap(), Some(root));

    // A second copy from the same source is idempotent (upsert semantics).
    staging.copy_all_from(&src).unwrap();
    assert_eq!(staging.chunk_count().unwrap(), 2);

    // Copying from an EMPTY source is a no-op — staging rows survive.
    let empty_src_path = dir.path().join("empty.redb");
    let empty_src = CorpusStore::open(&empty_src_path).unwrap();
    staging.copy_all_from(&empty_src).unwrap();
    assert_eq!(
        staging.chunk_count().unwrap(),
        2,
        "copy from empty source must not erase staging rows"
    );
}
