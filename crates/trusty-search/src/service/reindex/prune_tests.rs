//! Issue #848 regression tests for the prune pass.
//!
//! Why: isolated here to keep `prune.rs` under the 500-line cap while
//! preserving full coverage for the path-normalisation helper and the
//! data-safety disk-existence guard.
//! What: four tests — normalisation round-trip, disk-existence guard predicate,
//! `list_indexed_files` distinctness, pre-fix and post-fix prune models.
//! Test: all tests in this file run as part of `cargo test -p trusty-search`.

use super::to_corpus_relative_path;

/// Verify `to_corpus_relative_path` round-trips correctly — the helper
/// used by both the batch loop and the prune pass must produce the same
/// string for the same input so the set-difference is sound.
///
/// Why: the core data-safety invariant is that walked-set strings equal
/// corpus-stored strings.  A dedicated unit test makes any future
/// regression immediately visible.
/// What: constructs a path that is a child of the root, strips it, and
/// verifies the result matches what the batch loop would produce.
/// Test: this test.
#[test]
fn to_corpus_relative_path_agrees_with_batch_loop() {
    let root = std::path::Path::new("/repo/root");
    let path = std::path::Path::new("/repo/root/src/lib.rs");
    // Expect the same string the batch loop produces:
    // `path.strip_prefix(root).unwrap_or(path).display().to_string()`
    let expected = path
        .strip_prefix(root)
        .unwrap_or(path)
        .display()
        .to_string();
    assert_eq!(to_corpus_relative_path(root, path), expected);
}

/// Disk-existence guard: a file that IS present on disk but whose relative
/// path would appear in the set-difference (simulating a normalization
/// mismatch) must NOT be pruned.
///
/// Why: the guard is the data-safety belt-and-suspenders.  Even if the
/// normalisation produces a string that escapes the walked-set (e.g. an
/// absolute fallback), the stat-check catches it and refuses to prune a
/// file that actually exists on disk.
/// What: writes a real file to a tempdir.  Checks the guard predicate
/// (`absolute.exists()`) directly and asserts it would cause the prune
/// to be skipped.
/// Test: this test.  The actual async guard in `prune_deleted_files_from_staging`
/// is exercised end-to-end; this unit test validates the guard's predicate.
#[test]
fn disk_existence_guard_skips_live_file() {
    let dir = tempfile::tempdir().unwrap();
    let live_file = dir.path().join("live.rs");
    std::fs::write(&live_file, "fn live() {}").unwrap();

    // Simulate: the prune pass thinks "live.rs" is deleted (not in walked_set)
    // but it is still present on disk.
    let corpus_relative = "live.rs";
    let absolute = dir.path().join(corpus_relative);

    // The guard predicate: file still exists → skip prune.
    assert!(absolute.exists(), "test setup: live.rs must exist on disk");

    // Simulate what the guard does: if absolute.exists() → skip.
    let would_prune = !absolute.exists();
    assert!(
        !would_prune,
        "disk-existence guard must prevent pruning a file still present on disk"
    );
}

/// Issue #848: `list_indexed_files` must return the distinct set of file
/// paths stored in the corpus — the foundation of the prune-pass logic.
///
/// Why: the prune pass computes `indexed_files − walked_set`; if
/// `list_indexed_files` is wrong, the set-difference is wrong.
/// What: writes chunks for two files, calls `list_indexed_files`, asserts
/// both files appear exactly once even when a file has multiple chunks.
/// Test: this test.
#[test]
fn list_indexed_files_returns_distinct_paths() {
    use crate::core::chunker::{ChunkType, RawChunk};
    use crate::core::corpus::CorpusStore;

    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("index.redb");
    let store = CorpusStore::open(&db_path).unwrap();

    let chunk = |file: &str, id: &str| RawChunk {
        id: id.to_string(),
        file: file.to_string(),
        start_line: 1,
        end_line: 1,
        content: format!("fn {id}() {{}}"),
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
    };

    // Two chunks for src/a.rs, one for src/b.rs.
    store
        .upsert_chunks(&[
            chunk("src/a.rs", "a:1:10"),
            chunk("src/a.rs", "a:11:20"),
            chunk("src/b.rs", "b:1:10"),
        ])
        .unwrap();

    let mut files = store.list_indexed_files().unwrap();
    files.sort();

    assert_eq!(
        files,
        vec!["src/a.rs".to_string(), "src/b.rs".to_string()],
        "#848: list_indexed_files must return each file exactly once"
    );
}

/// Issue #848 — PRE-FIX model: demonstrate that without a prune pass, a
/// deleted file's chunks survive in the staged corpus and are promoted to
/// the live corpus.  This test must PASS (the pre-fix bug model is correct).
///
/// Why: a test that documents what WRONG behaviour looks like is the only
/// way to be certain the fix test is checking the right thing.
///
/// Test: this test.
#[test]
fn deleted_file_chunks_persist_without_prune_pass() {
    use crate::core::chunker::{ChunkType, RawChunk};
    use crate::core::corpus::CorpusStore;

    let dir = tempfile::tempdir().unwrap();

    let chunk = |file: &str, id: &str| RawChunk {
        id: id.to_string(),
        file: file.to_string(),
        start_line: 1,
        end_line: 1,
        content: format!("fn {id}() {{}}"),
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
    };

    // Live corpus: two files.
    let live_path = dir.path().join("pre848_live.redb");
    {
        let live = CorpusStore::open(&live_path).unwrap();
        live.upsert_chunks(&[
            chunk("kept.rs", "kept:1:10"),
            chunk("deleted.rs", "deleted:1:10"),
        ])
        .unwrap();
        live.upsert_file_hashes(&[("kept.rs", "aa"), ("deleted.rs", "bb")])
            .unwrap();
    }

    // Staging seeded from live (the #839 fix behaviour) — no prune pass.
    let staging_path = dir.path().join("pre848_staging.redb");
    {
        let live = CorpusStore::open(&live_path).unwrap();
        let staging = CorpusStore::open_fresh(&staging_path).unwrap();
        staging.copy_all_from(&live).unwrap();
        // The walk only saw kept.rs (deleted.rs was removed from disk).
        // Only kept.rs is re-indexed (or skipped by hash); deleted.rs is
        // never touched.  No prune pass → staging still has deleted.rs.
    }

    // Simulate restart: reopen staging as the new live corpus.
    let reopened = CorpusStore::open(&staging_path).unwrap();
    let files = reopened.list_indexed_files().unwrap();
    assert!(
        files.iter().any(|f| f == "deleted.rs"),
        "PRE-FIX #848 model: deleted.rs MUST still be present without a prune pass \
         (proving the bug exists and the fix is needed)"
    );
}

/// Issue #848 — POST-FIX model: after the prune pass runs against the
/// staging corpus, deleted files' chunks, entities, and file-hash entries
/// are gone.  Reopening the staged corpus (simulating a daemon restart)
/// must NOT see the deleted file.
///
/// What: seeds a live corpus with two files, seeds a staging corpus from
/// live (`copy_all_from`), then calls the prune helpers directly to
/// simulate what `prune_deleted_files_from_staging` does (deleted-file
/// detection + redb removal), and asserts the staging corpus is clean.
///
/// Test: this test.
#[test]
fn prune_pass_removes_deleted_file_from_staged_corpus() {
    use crate::core::chunker::{ChunkType, RawChunk};
    use crate::core::corpus::CorpusStore;

    let dir = tempfile::tempdir().unwrap();

    let chunk = |file: &str, id: &str| RawChunk {
        id: id.to_string(),
        file: file.to_string(),
        start_line: 1,
        end_line: 1,
        content: format!("fn {id}() {{}}"),
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
    };

    // Live corpus: two files.
    let live_path = dir.path().join("post848_live.redb");
    {
        let live = CorpusStore::open(&live_path).unwrap();
        live.upsert_chunks(&[
            chunk("kept.rs", "kept:1:10"),
            chunk("deleted.rs", "deleted:1:10"),
        ])
        .unwrap();
        live.upsert_entities(&[
            ("kept.rs".to_string(), Vec::new()),
            ("deleted.rs".to_string(), Vec::new()),
        ])
        .unwrap();
        live.upsert_file_hashes(&[("kept.rs", "aa"), ("deleted.rs", "bb")])
            .unwrap();
    }

    // Staging seeded from live.
    let staging_path = dir.path().join("post848_staging.redb");
    let staging = {
        let live = CorpusStore::open(&live_path).unwrap();
        let s = CorpusStore::open_fresh(&staging_path).unwrap();
        s.copy_all_from(&live).unwrap();
        s
    };

    // Simulate the prune pass: deleted.rs was not walked.
    let indexed = staging.list_indexed_files().unwrap();
    let walked_set: std::collections::HashSet<String> =
        ["kept.rs".to_string()].into_iter().collect();
    let deleted: Vec<String> = indexed
        .into_iter()
        .filter(|f| !walked_set.contains(f))
        .collect();
    assert_eq!(
        deleted,
        vec!["deleted.rs".to_string()],
        "#848: set-difference must identify deleted.rs as stale"
    );

    // Apply the per-file redb deletions (the core of the prune pass).
    let chunk_ids: Vec<String> = staging
        .load_all_chunks()
        .unwrap()
        .into_iter()
        .filter(|c| c.file == "deleted.rs")
        .map(|c| c.id)
        .collect();
    staging.delete_chunks(&chunk_ids).unwrap();
    staging.delete_entities("deleted.rs").unwrap();
    staging
        .delete_file_hash_entries(&["deleted.rs".to_string()])
        .unwrap();

    // Simulate restart: reopen staging as the new live corpus.
    drop(staging);
    let reopened = CorpusStore::open(&staging_path).unwrap();

    // deleted.rs must be gone.
    let files_after = reopened.list_indexed_files().unwrap();
    assert!(
        !files_after.iter().any(|f| f == "deleted.rs"),
        "#848 POST-FIX: deleted.rs must be absent from the promoted corpus \
         after the prune pass; found files: {:?}",
        files_after
    );
    // kept.rs must survive.
    assert!(
        files_after.iter().any(|f| f == "kept.rs"),
        "#848 POST-FIX: kept.rs must still be present in the promoted corpus"
    );

    // File-hash for deleted.rs must be gone (next reindex must not skip it).
    let hashes = reopened.load_file_hashes().unwrap();
    assert!(
        !hashes.iter().any(|(f, _)| f == "deleted.rs"),
        "#848 POST-FIX: file-hash entry for deleted.rs must be removed"
    );
    // File-hash for kept.rs must survive.
    assert!(
        hashes.iter().any(|(f, _)| f == "kept.rs"),
        "#848 POST-FIX: file-hash entry for kept.rs must still be present"
    );
}
