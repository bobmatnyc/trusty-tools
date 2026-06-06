//! Prune-pass logic for non-force incremental reindexes (issue #848).
//!
//! Why: after the #839 carryover fix, `copy_all_from` seeds the staging corpus
//! with ALL live rows before the batch loop runs.  A file deleted from disk is
//! never walked → never re-indexed → its rows survive in the staging corpus
//! untouched and are promoted with the rest.  This module computes the
//! set-difference between the walked files and the staged corpus, then removes
//! every stale file's data from all stores.
//!
//! What: exports `prune_deleted_files_from_staging`, called once per non-force
//! reindex immediately after the producer task finishes and before the staging
//! corpus is promoted to live.
//!
//! Test: see `#[cfg(test)] mod tests` at the bottom of this file.

use crate::core::registry::{IndexHandle, IndexId};
use dashmap::DashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Prune stale data from the staging corpus for files deleted from disk (issue #848).
///
/// Why: after the #839 carryover fix, `copy_all_from` seeds the staging corpus
/// with ALL live rows before the batch loop runs.  A file deleted from disk is
/// never walked → never re-indexed → its rows survive in the staging corpus
/// untouched and are promoted with the rest. Search then returns results from
/// files that no longer exist. Only a `--force` reindex (empty staging, full
/// re-walk) avoided this; an incremental reindex permanently carried stale data.
///
/// What: computes `deleted_files = (files stored in staging corpus) − (walked
/// file set)` and, for each deleted file, removes its data from:
///   1. the staging redb corpus (chunk rows, entity row, file-hash entry),
///   2. the in-memory HNSW + BM25 + chunk map + embedding LRU, and
///   3. the in-process file-hash DashMap (so the NEXT reindex's warm-skip
///      does not see a stale hash for a no-longer-existing file).
///
/// The function is a no-op when `deleted_files` is empty (no files were removed
/// since the last reindex).  Errors from individual store operations are logged
/// at `warn` and swallowed — the prune is a best-effort clean-up; a miss only
/// means an extra ghost chunk that the NEXT reindex would also prune.  Corpus
/// errors from `list_indexed_files` or the batch-delete calls bubble up via
/// `warn` only (same tolerance as every other corpus helper in this module).
///
/// Applies ONLY to the NON-force incremental path (corpus_swap_tmp.is_some()
/// && !force && !memory_aborted); the caller already gates it.
///
/// Test: `prune_pass_removes_deleted_file_from_staged_corpus` in
/// `service::reindex::prune::tests`.
pub(super) async fn prune_deleted_files_from_staging(
    handle: &IndexHandle,
    walked_files: &[PathBuf],
    canonical_root: &Path,
    hashes: &Arc<DashMap<PathBuf, String>>,
    index_id: &IndexId,
) {
    // Build the walked set as relative-path strings — the same normalisation
    // the batch loop uses when writing chunk.file to redb.
    let walked_set: std::collections::HashSet<String> = walked_files
        .iter()
        .map(|p| {
            p.strip_prefix(canonical_root)
                .unwrap_or(p)
                .display()
                .to_string()
        })
        .collect();

    // Query the staging corpus for all file paths currently stored.
    let corpus = {
        let indexer = handle.indexer.read().await;
        indexer.corpus_store()
    };
    let Some(corpus) = corpus else {
        return; // No durable corpus — nothing to prune.
    };
    let indexed_files = match tokio::task::spawn_blocking(move || corpus.list_indexed_files()).await
    {
        Ok(Ok(files)) => files,
        Ok(Err(e)) => {
            tracing::warn!(
                "reindex[{}]: prune pass: could not list indexed files ({e}) — \
                     skipping prune",
                index_id.0
            );
            return;
        }
        Err(e) => {
            tracing::warn!(
                "reindex[{}]: prune pass: list_indexed_files task panicked ({e}) — \
                     skipping prune",
                index_id.0
            );
            return;
        }
    };

    // Set-difference: files in the corpus that were NOT walked.
    let deleted_files: Vec<String> = indexed_files
        .into_iter()
        .filter(|f| !walked_set.contains(f.as_str()))
        .collect();

    if deleted_files.is_empty() {
        tracing::debug!(
            "reindex[{}]: prune pass: no deleted files detected",
            index_id.0
        );
        return;
    }

    tracing::info!(
        "reindex[{}]: prune pass: {} deleted file(s) detected — pruning stale data",
        index_id.0,
        deleted_files.len()
    );

    // Per-file removal from in-memory + staging redb structures.
    // `remove_file_no_kg_rebuild` handles: in-memory chunks/HNSW/BM25/LRU
    // + redb chunks + redb entities. The KG rebuild is omitted per-file
    // because Phase 3 rebuilds it once for the whole reindex.
    let mut total_pruned_chunks: usize = 0;
    for file_path in &deleted_files {
        let n = {
            let indexer = handle.indexer.read().await;
            match indexer.remove_file_no_kg_rebuild(file_path).await {
                Ok(count) => count,
                Err(e) => {
                    tracing::warn!(
                        "reindex[{}]: prune pass: remove_file_no_kg_rebuild for {} failed ({e})",
                        index_id.0,
                        file_path,
                    );
                    0
                }
            }
        };
        total_pruned_chunks += n;

        // Remove the file-hash entry from the staging redb corpus AND from
        // the in-process DashMap so the next incremental reindex does not
        // false-skip the (now-absent) file.
        {
            let corpus = {
                let indexer = handle.indexer.read().await;
                indexer.corpus_store()
            };
            if let Some(corpus) = corpus {
                let file = file_path.clone();
                let idx = index_id.0.clone();
                match tokio::task::spawn_blocking(move || corpus.delete_file_hash_entries(&[file]))
                    .await
                {
                    Ok(Ok(())) => {}
                    Ok(Err(e)) => {
                        tracing::warn!(
                            "reindex[{idx}]: prune pass: hash delete for {file_path} failed ({e})"
                        );
                    }
                    Err(e) => {
                        tracing::warn!(
                            "reindex[{idx}]: prune pass: hash delete task panicked ({e})"
                        );
                    }
                }
            }
        }
        // Evict from the in-process PathBuf-keyed DashMap.
        // Chunk paths are relative strings; the DashMap is keyed by PathBuf.
        hashes.remove(&PathBuf::from(file_path));

        tracing::debug!(
            "reindex[{}]: prune pass: removed {} stale chunk(s) for deleted file {}",
            index_id.0,
            n,
            file_path,
        );
    }

    tracing::info!(
        "reindex[{}]: prune pass: pruned {} stale chunk(s) from {} deleted file(s)",
        index_id.0,
        total_pruned_chunks,
        deleted_files.len(),
    );
}

// ── Issue #848 regression tests ───────────────────────────────────────────

#[cfg(test)]
mod tests {
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
}
