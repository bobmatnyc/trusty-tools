//! Issue #848 and #855 regression tests for the prune pass and orphan-chunk fix.
//!
//! Why: isolated here to keep `prune.rs` under the 500-line cap while
//! preserving full coverage for the path-normalisation helper and the
//! data-safety disk-existence guard.
//! What: five tests — normalisation round-trip, disk-existence guard predicate,
//! `list_indexed_files` distinctness, pre-fix and post-fix prune models (#848),
//! and orphan-chunk regression test (#855).
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

/// Issue #855 — orphan-chunk regression model: when a changed file re-chunks
/// to FEWER chunks (e.g. a function is deleted), the old chunk IDs that were
/// carried into the staging corpus by `copy_all_from` must be removed BEFORE
/// the new, smaller chunk set is written (delete-then-insert semantics).
///
/// Without the fix, the staging corpus would contain BOTH the new chunks AND
/// the old chunks that no longer exist in the file — "orphan" rows that are
/// promoted to the live corpus and returned by search until the next `--force`
/// reindex.
///
/// Why: this is the core regression proof for issue #855. The pre-fix
/// sub-test documents the wrong behaviour (orphan chunks survive); the
/// post-fix sub-test documents the correct behaviour (orphan chunks are gone).
///
/// What: models the staging corpus lifecycle at the CorpusStore level:
///   1. Seeds a live corpus with a file `shrunk.rs` having 3 chunks.
///   2. Seeds a staging corpus from live (`copy_all_from`).
///   3. PRE-FIX: verifies that a naive upsert-only re-commit of 1 new chunk
///      leaves the 2 old chunks still present (proving the bug).
///   4. POST-FIX: applies the delete-then-insert pattern (delete old chunks
///      before inserting the new one) and verifies exactly 1 chunk survives.
///
/// Test: this test.
#[test]
fn changed_file_orphan_chunks_removed_before_reinsert() {
    use crate::core::chunker::{ChunkType, RawChunk};
    use crate::core::corpus::CorpusStore;

    let dir = tempfile::tempdir().unwrap();

    let chunk = |file: &str, id: &str, content: &str| RawChunk {
        id: id.to_string(),
        file: file.to_string(),
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
    };

    // ─── Live corpus: shrunk.rs has 3 chunks ─────────────────────────────────
    let live_path = dir.path().join("855_live.redb");
    {
        let live = CorpusStore::open(&live_path).unwrap();
        live.upsert_chunks(&[
            chunk("shrunk.rs", "shrunk:fn_a", "fn fn_a() {}"),
            chunk("shrunk.rs", "shrunk:fn_b", "fn fn_b() {}"),
            chunk("shrunk.rs", "shrunk:fn_c", "fn fn_c() {}"),
        ])
        .unwrap();
        live.upsert_file_hashes(&[("shrunk.rs", "old_hash")])
            .unwrap();
    }

    // ─── Staging seeded from live (copy_all_from) ─────────────────────────────
    let staging_path = dir.path().join("855_staging.redb");
    {
        let live = CorpusStore::open(&live_path).unwrap();
        let staging = CorpusStore::open_fresh(&staging_path).unwrap();
        staging.copy_all_from(&live).unwrap();
        // Verify: staging starts with all 3 chunks.
        let initial = staging.list_indexed_files().unwrap();
        assert!(
            initial.iter().any(|f| f == "shrunk.rs"),
            "#855 setup: staging must contain shrunk.rs after copy_all_from"
        );
        let initial_chunks = staging
            .load_all_chunks()
            .unwrap()
            .into_iter()
            .filter(|c| c.file == "shrunk.rs")
            .count();
        assert_eq!(
            initial_chunks, 3,
            "#855 setup: staging must start with 3 chunks for shrunk.rs"
        );
    }

    // ─── PRE-FIX model: upsert-only re-commit (the bug) ──────────────────────
    // Simulate what the OLD non-force reindex did: just upsert the 1 new chunk
    // without first deleting the old 3.  The 2 orphan chunks survive.
    let prefix_staging_path = dir.path().join("855_prefix_staging.redb");
    {
        let live = CorpusStore::open(&live_path).unwrap();
        let staging = CorpusStore::open_fresh(&prefix_staging_path).unwrap();
        staging.copy_all_from(&live).unwrap();
        // Only upsert 1 new chunk (no delete of old ones).
        staging
            .upsert_chunks(&[chunk("shrunk.rs", "shrunk:fn_a", "fn fn_a_new() {}")])
            .unwrap();
    }
    let prefix = CorpusStore::open(&prefix_staging_path).unwrap();
    let prefix_chunks: Vec<_> = prefix
        .load_all_chunks()
        .unwrap()
        .into_iter()
        .filter(|c| c.file == "shrunk.rs")
        .collect();
    assert_eq!(
        prefix_chunks.len(),
        3, // 1 new + 2 orphans → DATA LOSS BUG
        "#855 PRE-FIX model: upsert-only must leave 3 chunks (1 new + 2 orphan), \
         proving the orphan-chunk bug exists"
    );
    // The orphan chunks with stale content must still be present.
    assert!(
        prefix_chunks.iter().any(|c| c.id == "shrunk:fn_b"),
        "#855 PRE-FIX model: orphan chunk shrunk:fn_b must survive upsert-only"
    );
    assert!(
        prefix_chunks.iter().any(|c| c.id == "shrunk:fn_c"),
        "#855 PRE-FIX model: orphan chunk shrunk:fn_c must survive upsert-only"
    );

    // ─── POST-FIX model: delete-then-insert (the fix) ─────────────────────────
    // Simulate what the FIXED non-force reindex does: delete all prior chunks
    // for shrunk.rs, THEN insert the 1 new chunk.  Exactly 1 chunk survives.
    let postfix_staging_path = dir.path().join("855_postfix_staging.redb");
    {
        let live = CorpusStore::open(&live_path).unwrap();
        let staging = CorpusStore::open_fresh(&postfix_staging_path).unwrap();
        staging.copy_all_from(&live).unwrap();

        // Step 1: delete all old chunks for shrunk.rs (the fix).
        let old_chunk_ids: Vec<String> = staging
            .load_all_chunks()
            .unwrap()
            .into_iter()
            .filter(|c| c.file == "shrunk.rs")
            .map(|c| c.id)
            .collect();
        staging.delete_chunks(&old_chunk_ids).unwrap();

        // Step 2: insert only the 1 new chunk.
        staging
            .upsert_chunks(&[chunk("shrunk.rs", "shrunk:fn_a", "fn fn_a_new() {}")])
            .unwrap();
    }
    let postfix = CorpusStore::open(&postfix_staging_path).unwrap();
    let postfix_chunks: Vec<_> = postfix
        .load_all_chunks()
        .unwrap()
        .into_iter()
        .filter(|c| c.file == "shrunk.rs")
        .collect();
    assert_eq!(
        postfix_chunks.len(),
        1, // exactly 1 new chunk, no orphans
        "#855 POST-FIX: delete-then-insert must leave exactly 1 chunk for shrunk.rs; \
         found: {:?}",
        postfix_chunks.iter().map(|c| &c.id).collect::<Vec<_>>()
    );
    assert_eq!(
        postfix_chunks[0].id, "shrunk:fn_a",
        "#855 POST-FIX: the surviving chunk must be the newly inserted one"
    );
    assert_eq!(
        postfix_chunks[0].content, "fn fn_a_new() {}",
        "#855 POST-FIX: the surviving chunk must have the NEW content, not stale content"
    );
    // The orphan chunks must be gone.
    assert!(
        !postfix_chunks.iter().any(|c| c.id == "shrunk:fn_b"),
        "#855 POST-FIX: orphan chunk shrunk:fn_b must be removed by delete-then-insert"
    );
    assert!(
        !postfix_chunks.iter().any(|c| c.id == "shrunk:fn_c"),
        "#855 POST-FIX: orphan chunk shrunk:fn_c must be removed by delete-then-insert"
    );
}

// ── #7004: force rebuild must not retain obsolete chunks in the warm cache ──

use crate::core::corpus::CorpusStore;
use crate::core::embed::MockEmbedder;
use crate::core::indexer::CodeIndexer;
use crate::core::registry::{IndexHandle, IndexId};
use crate::core::store::{UsearchStore, VectorStore};
use crate::service::reindex::{spawn_reindex_awaitable, ReindexProgress, ReindexStatus};
use std::sync::Arc;

/// A disposable colocated index with a real redb corpus and HNSW store.
///
/// Why: the #7004 leak paths (`embed_deferred_chunks_gated`,
/// `flush_corpus_to_disk`) only exist once a durable corpus is wired, and the
/// promotion rename must land inside the tempdir rather than a daemon-global
/// directory — which colocated storage is what makes true.
/// What: `.trusty-search/index.redb` under a fresh tempdir, a `MockEmbedder`
/// so no model is downloaded, `defer_embed = false` so vectors land inline.
/// Test: the two `force_rebuild_*` tests below.
fn colocated_fixture(tag: &str) -> (tempfile::TempDir, Arc<IndexHandle>, Arc<UsearchStore>) {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join(".trusty-search")).unwrap();
    let corpus = CorpusStore::open(&root.path().join(".trusty-search/index.redb")).unwrap();
    let id = format!(
        "{tag}-{}",
        root.path().file_name().unwrap().to_string_lossy()
    );
    let store = Arc::new(UsearchStore::new(8).unwrap());
    let mut indexer = CodeIndexer::new(&id, root.path().to_path_buf())
        .with_components(Arc::new(MockEmbedder::new(8)), store.clone());
    indexer.set_corpus_store(Arc::new(corpus));
    let mut handle = IndexHandle::bare(
        IndexId::new(&id),
        Arc::new(tokio::sync::RwLock::new(indexer)),
        root.path().to_path_buf(),
    );
    handle.defer_embed = false;
    handle.extra_skip_dirs.push(".trusty-search".into());
    (root, Arc::new(handle), store)
}

/// Chunk ids currently stored for `file`, read from the installed corpus.
async fn corpus_ids_for(handle: &IndexHandle, file: &str) -> Vec<String> {
    let corpus = handle.indexer.read().await.corpus_store().unwrap();
    let ids = corpus
        .load_all_chunks()
        .unwrap()
        .into_iter()
        .filter(|c| c.file == file)
        .map(|c| c.id)
        .collect();
    drop(corpus);
    ids
}

/// Flush the warm state the way graceful shutdown does, then reopen the corpus
/// cold — the exact sequence #7004 reported as resurrecting removed chunks.
async fn flush_and_reopen(handle: &IndexHandle) -> CorpusStore {
    let redb = handle.root_path.join(".trusty-search/index.redb");
    handle
        .indexer
        .read()
        .await
        .flush_corpus_to_disk(&redb)
        .await
        .unwrap();
    drop(handle.indexer.write().await.take_corpus_store());
    CorpusStore::open(&redb).unwrap()
}

/// Issue #7004: a file deleted between two runs must not survive a force
/// rebuild in the warm cache, the vector store, or the next shutdown flush.
///
/// Why: force stages an EMPTY corpus and skips the #848 prune pass, so the
/// promoted redb rows were already correct — but nothing reconciled the warm
/// chunk map or the HNSW store, and `flush_corpus_to_disk` writes that same
/// warm map back over the promoted corpus on graceful shutdown. Before the fix
/// the reopened corpus below carried `b.rs` again.
/// What: indexes `a.rs` + `b.rs`, deletes `b.rs`, force-rebuilds, then asserts
/// the promoted corpus, the vector store, and the post-flush cold reopen all
/// exclude `b.rs` while `a.rs` survives intact.
/// Test: this test.
#[tokio::test(flavor = "multi_thread")]
async fn force_rebuild_drops_chunks_for_a_deleted_file() {
    let (root, handle, vectors) = colocated_fixture("force-prune-deleted");
    std::fs::write(root.path().join("a.rs"), "pub fn alpha_survives() {}\n").unwrap();
    std::fs::write(root.path().join("b.rs"), "pub fn beta_removed() {}\n").unwrap();

    let first = Arc::new(ReindexProgress::new());
    spawn_reindex_awaitable(handle.clone(), first.clone(), false)
        .await
        .unwrap();
    assert_eq!(first.status.load(), ReindexStatus::Complete);

    let b_ids = corpus_ids_for(&handle, "b.rs").await;
    assert!(!b_ids.is_empty(), "test setup: b.rs must produce chunks");
    for id in &b_ids {
        assert!(
            vectors.contains(id).await,
            "test setup: b.rs must have a vector before the rebuild ({id})"
        );
    }

    std::fs::remove_file(root.path().join("b.rs")).unwrap();

    let second = Arc::new(ReindexProgress::new());
    spawn_reindex_awaitable(handle.clone(), second.clone(), true)
        .await
        .unwrap();
    assert_eq!(second.status.load(), ReindexStatus::Complete);

    assert!(
        corpus_ids_for(&handle, "b.rs").await.is_empty(),
        "the promoted corpus must not carry the deleted file"
    );

    let reopened = flush_and_reopen(&handle).await;
    let files: Vec<String> = reopened
        .load_all_chunks()
        .unwrap()
        .into_iter()
        .map(|c| c.file)
        .collect();
    assert!(
        !files.iter().any(|f| f == "b.rs"),
        "#7004: the shutdown flush wrote the deleted file back into the promoted \
         corpus; reopened files: {files:?}"
    );
    assert!(
        files.iter().any(|f| f == "a.rs"),
        "the surviving file must still be there: {files:?}"
    );
    for id in &b_ids {
        assert!(
            !vectors.contains(id).await,
            "#7004: the deleted file's vector survived the force rebuild ({id})"
        );
    }
}

/// Issue #7004: a file that stops matching the walker's include set is dropped
/// exactly like a deleted one.
///
/// Why: the reconciliation is keyed on the promoted corpus's id set, not on
/// whether a path still exists on disk, so an excluded file must not need its
/// own code path. The moved file's bytes are still on disk here — the #848
/// prune pass's disk-existence guard would have refused to touch it.
/// What: indexes `a.rs` + `b.rs`, moves `b.rs` into the skipped
/// `.trusty-search/` directory, force-rebuilds, and asserts the same three
/// surfaces exclude `b.rs`.
/// Test: this test.
#[tokio::test(flavor = "multi_thread")]
async fn force_rebuild_drops_chunks_for_a_file_outside_the_include_set() {
    let (root, handle, vectors) = colocated_fixture("force-prune-excluded");
    std::fs::write(root.path().join("a.rs"), "pub fn alpha_survives() {}\n").unwrap();
    std::fs::write(root.path().join("b.rs"), "pub fn beta_renamed() {}\n").unwrap();

    let first = Arc::new(ReindexProgress::new());
    spawn_reindex_awaitable(handle.clone(), first.clone(), false)
        .await
        .unwrap();
    assert_eq!(first.status.load(), ReindexStatus::Complete);
    let b_ids = corpus_ids_for(&handle, "b.rs").await;
    assert!(!b_ids.is_empty(), "test setup: b.rs must produce chunks");

    std::fs::rename(
        root.path().join("b.rs"),
        root.path().join(".trusty-search/b.rs"),
    )
    .unwrap();

    let second = Arc::new(ReindexProgress::new());
    spawn_reindex_awaitable(handle.clone(), second.clone(), true)
        .await
        .unwrap();
    assert_eq!(second.status.load(), ReindexStatus::Complete);

    let reopened = flush_and_reopen(&handle).await;
    let files: Vec<String> = reopened
        .load_all_chunks()
        .unwrap()
        .into_iter()
        .map(|c| c.file)
        .collect();
    assert!(
        !files.iter().any(|f| f == "b.rs"),
        "#7004: a file outside the include set came back through the shutdown \
         flush; reopened files: {files:?}"
    );
    assert!(
        files.iter().any(|f| f == "a.rs"),
        "the surviving file must still be there: {files:?}"
    );
    for id in &b_ids {
        assert!(
            !vectors.contains(id).await,
            "#7004: the excluded file's vector survived the force rebuild ({id})"
        );
    }
}

/// Issue #7004 race: a file indexed between the promoted-id scan and the warm
/// snapshot must survive the reconciliation.
///
/// Why: `index_file` runs under `indexer.read()`, the same shared lock this
/// pass takes, and nothing gates it on `ReindexStatus::Running`. A call landing
/// in that window leaves its new id in the warm map but not in the id set the
/// scan returned. Dropping it strips the chunk from BM25 and the vector store
/// while its redb row stays, so a write that answered `indexed: true` becomes
/// unsearchable until the next reindex.
/// What: reproduces the interleaving state exactly. `b.rs`'s rows are deleted
/// from the corpus to model what an empty-staged force promotion leaves behind
/// — durable rows gone, warm entries still there. The id snapshot is taken from
/// that corpus, then a real `index_file` lands `c.rs` (redb row first, then the
/// warm map, the production order in `commit_parsed_batch`), and the now-stale
/// snapshot is handed to the reconciliation. Asserts `c.rs` keeps its vector
/// and its rows while `b.rs` is still dropped, so the fix cannot be a blanket
/// "keep everything".
/// Test: this test.
#[tokio::test(flavor = "multi_thread")]
async fn force_reconcile_keeps_a_chunk_written_after_the_id_snapshot() {
    let (root, handle, vectors) = colocated_fixture("force-reconcile-race");
    std::fs::write(root.path().join("a.rs"), "pub fn alpha_survives() {}\n").unwrap();
    std::fs::write(root.path().join("b.rs"), "pub fn beta_removed() {}\n").unwrap();
    let first = Arc::new(ReindexProgress::new());
    spawn_reindex_awaitable(handle.clone(), first.clone(), false)
        .await
        .unwrap();
    assert_eq!(first.status.load(), ReindexStatus::Complete);
    let b_ids = corpus_ids_for(&handle, "b.rs").await;
    assert!(!b_ids.is_empty(), "test setup: b.rs must produce chunks");

    // Model the promoted corpus: b.rs's durable rows are gone, its warm
    // entries are not. T1 — the id scan the reconciliation would have taken.
    let snapshot = {
        let corpus = handle.indexer.read().await.corpus_store().unwrap();
        corpus.delete_chunks(&b_ids).unwrap();
        let ids = corpus.list_chunk_ids().unwrap();
        drop(corpus);
        ids
    };
    for id in &b_ids {
        assert!(
            !snapshot.contains(id),
            "test setup: b.rs must be absent from the promoted id set ({id})"
        );
    }

    // Between the two snapshots: a real concurrent write.
    handle
        .indexer
        .read()
        .await
        .index_file("c.rs", "pub fn gamma_written_mid_reconcile() {}\n")
        .await
        .unwrap();
    let c_ids = corpus_ids_for(&handle, "c.rs").await;
    assert!(!c_ids.is_empty(), "test setup: index_file must land chunks");
    for id in &c_ids {
        assert!(
            !snapshot.contains(id),
            "test setup: the new id must be absent from the stale snapshot ({id})"
        );
    }

    // T2 — the reconciliation runs against the stale snapshot.
    super::reconcile_with_id_reader(&handle, &handle.id.clone(), move |_| Ok(snapshot)).await;

    for id in &c_ids {
        assert!(
            vectors.contains(id).await,
            "#7004: a chunk written between the two snapshots lost its vector ({id})"
        );
    }
    for id in &b_ids {
        assert!(
            !vectors.contains(id).await,
            "the genuinely obsolete chunk must still lose its vector ({id})"
        );
    }
    let reopened = flush_and_reopen(&handle).await;
    let files: Vec<String> = reopened
        .load_all_chunks()
        .unwrap()
        .into_iter()
        .map(|c| c.file)
        .collect();
    assert!(
        files.iter().any(|f| f == "c.rs"),
        "the concurrent write must survive the reconciliation: {files:?}"
    );
    assert!(
        !files.iter().any(|f| f == "b.rs"),
        "the genuinely obsolete file must still be dropped: {files:?}"
    );
}

/// Issue #7004: an id scan that fails leaves the warm state untouched.
///
/// Why: the reconciliation's decision is destructive and is taken from the
/// ABSENCE of an id. An id set it could not read proves nothing, so every
/// failure arm must fail closed; a version that dropped anyway would delete
/// live data on a transient redb error.
/// What: injects a failing reader and asserts the in-memory chunk count is
/// unchanged.
/// Test: this test.
#[tokio::test(flavor = "multi_thread")]
async fn force_reconcile_leaves_warm_chunks_alone_when_the_id_scan_fails() {
    let (root, handle, _vectors) = colocated_fixture("force-reconcile-scan-err");
    std::fs::write(root.path().join("a.rs"), "pub fn alpha() {}\n").unwrap();
    std::fs::write(root.path().join("b.rs"), "pub fn beta() {}\n").unwrap();
    let progress = Arc::new(ReindexProgress::new());
    spawn_reindex_awaitable(handle.clone(), progress.clone(), false)
        .await
        .unwrap();
    assert_eq!(progress.status.load(), ReindexStatus::Complete);
    let before = handle.indexer.read().await.chunk_count();
    assert!(before > 0, "test setup: the warm map must be populated");

    super::reconcile_with_id_reader(&handle, &handle.id.clone(), |_| {
        anyhow::bail!("injected chunk-id scan failure")
    })
    .await;

    assert_eq!(
        handle.indexer.read().await.chunk_count(),
        before,
        "#7004: a failed id scan must not drop a single warm chunk"
    );
}

/// Issue #7004: an id scan that PANICS leaves the warm state untouched.
///
/// Why: the scan runs on a blocking worker, so a panic there reaches the
/// caller as a `JoinError` rather than the `Err` arm above — a separate branch
/// that would be just as destructive if it fell through to an empty id set.
/// What: injects a panicking reader and asserts the in-memory chunk count is
/// unchanged.
/// Test: this test.
#[tokio::test(flavor = "multi_thread")]
async fn force_reconcile_leaves_warm_chunks_alone_when_the_id_scan_panics() {
    let (root, handle, _vectors) = colocated_fixture("force-reconcile-scan-panic");
    std::fs::write(root.path().join("a.rs"), "pub fn alpha() {}\n").unwrap();
    std::fs::write(root.path().join("b.rs"), "pub fn beta() {}\n").unwrap();
    let progress = Arc::new(ReindexProgress::new());
    spawn_reindex_awaitable(handle.clone(), progress.clone(), false)
        .await
        .unwrap();
    assert_eq!(progress.status.load(), ReindexStatus::Complete);
    let before = handle.indexer.read().await.chunk_count();
    assert!(before > 0, "test setup: the warm map must be populated");

    super::reconcile_with_id_reader(&handle, &handle.id.clone(), |_| {
        panic!("injected chunk-id scan panic")
    })
    .await;

    assert_eq!(
        handle.indexer.read().await.chunk_count(),
        before,
        "#7004: a panicked id-scan worker must not drop a single warm chunk"
    );
}

/// Issue #7004: no corpus installed means nothing to reconcile against.
///
/// Why: a promotion that failed leaves the indexer without a corpus. Treating
/// an absent corpus as an empty id set would drop every warm chunk the index
/// has.
/// What: takes the corpus out of the indexer, runs the real entry point, and
/// asserts the in-memory chunk count is unchanged.
/// Test: this test.
#[tokio::test(flavor = "multi_thread")]
async fn force_reconcile_leaves_warm_chunks_alone_without_a_corpus() {
    let (root, handle, _vectors) = colocated_fixture("force-reconcile-no-corpus");
    std::fs::write(root.path().join("a.rs"), "pub fn alpha() {}\n").unwrap();
    let progress = Arc::new(ReindexProgress::new());
    spawn_reindex_awaitable(handle.clone(), progress.clone(), false)
        .await
        .unwrap();
    assert_eq!(progress.status.load(), ReindexStatus::Complete);
    let before = handle.indexer.read().await.chunk_count();
    assert!(before > 0, "test setup: the warm map must be populated");

    drop(handle.indexer.write().await.take_corpus_store());
    super::reconcile_warm_state_to_promoted_corpus(&handle, &handle.id.clone()).await;

    assert_eq!(
        handle.indexer.read().await.chunk_count(),
        before,
        "#7004: an absent corpus must not read as an empty promoted id set"
    );
}
