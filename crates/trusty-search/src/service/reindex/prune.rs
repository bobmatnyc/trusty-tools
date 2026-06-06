//! Prune-pass logic for non-force incremental reindexes (issue #848).
//!
//! Why: after the #839 carryover fix, `copy_all_from` seeds the staging corpus
//! with ALL live rows before the batch loop runs.  A file deleted from disk is
//! never walked → never re-indexed → its rows survive in the staging corpus
//! untouched and are promoted with the rest.  This module computes the
//! set-difference between the walked files and the staged corpus, then removes
//! every stale file's data from all stores.
//!
//! What: exports `prune_deleted_files_from_staging` and `to_corpus_relative_path`.
//! The latter is the single canonical normalisation that BOTH the batch loop
//! and the prune pass use, guaranteeing the strings are identical so the
//! set-difference can never generate false "deleted" entries.
//!
//! Test: see `prune_tests.rs` (loaded as `#[cfg(test)] mod tests`).

use crate::core::registry::{IndexHandle, IndexId};
use dashmap::DashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Convert an absolute `path` to the corpus-relative string form.
///
/// Why: the batch loop and the prune pass must produce IDENTICAL strings
/// for the same file, or the prune's set-difference will falsely classify
/// live files as "deleted" and wipe the staging corpus.  Having one
/// canonical function called from both sites closes that risk permanently.
///
/// What: strips `root` from `path` via `strip_prefix` and calls
/// `display().to_string()`, normalising separators to the platform default
/// (forward-slash on Unix, back-slash on Windows — consistent within the
/// same daemon process, which is all that matters for a set-compare that
/// never crosses machine boundaries).  The `unwrap_or` branch that returns
/// the ABSOLUTE path is intentionally preserved for the edge case where
/// `strip_prefix` fails (e.g. a symlink whose target escapes the root);
/// the batch loop has the same fallback, so the strings still match.
///
/// Test: `to_corpus_relative_path_agrees_with_batch_loop` and
/// `disk_existence_guard_skips_live_file` in `prune_tests.rs`.
pub(super) fn to_corpus_relative_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .display()
        .to_string()
}

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
/// file set)` and, for each deleted file:
///   1. verifies it DOES NOT exist on disk (disk-existence guard — belt-and-
///      suspenders against any residual path-normalisation mismatch),
///   2. removes its data from: the staging redb corpus (chunk rows, entity row,
///      file-hash entry), the in-memory HNSW + BM25 + chunk map + embedding LRU,
///      and the in-process file-hash DashMap.
///
/// After the per-file loop, all confirmed-pruned hash entries are batched into a
/// single `delete_file_hash_entries` call (one redb write-txn instead of one
/// per file — fix 2).
///
/// Errors from individual store operations are counted and a single aggregated
/// `warn!` is emitted at the end if any files partially failed (fix 3).
///
/// The function is a no-op when `deleted_files` is empty (no files were removed
/// since the last reindex).  Corpus errors from `list_indexed_files` bubble up
/// via `warn` only (same tolerance as every other corpus helper in this module).
///
/// Applies ONLY to the NON-force incremental path (corpus_swap_tmp.is_some()
/// && !force && !memory_aborted); the caller already gates it.
///
/// Test: `prune_pass_removes_deleted_file_from_staged_corpus` and
/// `disk_existence_guard_skips_live_file` in `prune_tests.rs`.
pub(super) async fn prune_deleted_files_from_staging(
    handle: &IndexHandle,
    walked_files: &[PathBuf],
    canonical_root: &Path,
    hashes: &Arc<DashMap<PathBuf, String>>,
    index_id: &IndexId,
) {
    // Build the walked set using the shared canonical normalisation so strings
    // are guaranteed identical to those stored by the batch loop.
    let walked_set: std::collections::HashSet<String> = walked_files
        .iter()
        .map(|p| to_corpus_relative_path(canonical_root, p))
        .collect();

    // Query the staging corpus for all file paths currently stored.
    // TODO: `list_indexed_files` is a full CHUNKS_TABLE scan; a future
    // FILES_TABLE secondary index would make this O(1) per file instead of
    // O(total_chunks). Tracked as a known perf improvement — do not implement
    // a secondary index here.
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
    //
    // Fix 2: collect confirmed-pruned paths for a single batched hash delete.
    // Fix 3: count per-file failures; emit one aggregated warn at the end.
    let mut total_pruned_chunks: usize = 0;
    let mut pruned_paths_for_hash: Vec<String> = Vec::new();
    let mut failed_count: usize = 0;

    for file_path in &deleted_files {
        // SAFETY GUARD (fix 1): before removing anything, confirm the file
        // is genuinely absent from disk.  Reconstruct the absolute path from
        // the relative corpus key and the canonical root, then stat it.
        // If the file STILL EXISTS, the set-difference result is a false
        // positive caused by a residual path-normalisation mismatch (e.g.
        // an absolute fallback in `to_corpus_relative_path`).  Do NOT prune
        // it — log a warn and skip.  This guard can never fire on a truly
        // deleted file because `PathBuf::exists` returns false for any path
        // that has no corresponding directory entry (including ENOENT).
        let absolute = canonical_root.join(file_path);
        if absolute.exists() {
            tracing::warn!(
                "reindex[{}]: prune: skipping {} — still exists on disk, \
                 likely a path-normalisation mismatch; will NOT prune live data",
                index_id.0,
                file_path,
            );
            continue;
        }

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
                    failed_count += 1;
                    0
                }
            }
        };
        total_pruned_chunks += n;

        // Evict from the in-process PathBuf-keyed DashMap.
        // Chunk paths are relative strings; the DashMap is keyed by PathBuf.
        hashes.remove(&PathBuf::from(file_path));

        pruned_paths_for_hash.push(file_path.clone());

        tracing::debug!(
            "reindex[{}]: prune pass: removed {} stale chunk(s) for deleted file {}",
            index_id.0,
            n,
            file_path,
        );
    }

    // Fix 2: single batched hash-entry delete for all confirmed-pruned files
    // (one redb write-txn instead of one per file).
    if !pruned_paths_for_hash.is_empty() {
        let corpus = {
            let indexer = handle.indexer.read().await;
            indexer.corpus_store()
        };
        if let Some(corpus) = corpus {
            let paths = pruned_paths_for_hash.clone();
            let idx = index_id.0.clone();
            match tokio::task::spawn_blocking(move || corpus.delete_file_hash_entries(&paths)).await
            {
                Ok(Ok(())) => {}
                Ok(Err(e)) => {
                    tracing::warn!("reindex[{idx}]: prune pass: batched hash delete failed ({e})");
                    failed_count += 1;
                }
                Err(e) => {
                    tracing::warn!(
                        "reindex[{idx}]: prune pass: batched hash delete task panicked ({e})"
                    );
                    failed_count += 1;
                }
            }
        }
    }

    // Fix 3: aggregate failure report.
    if failed_count > 0 {
        tracing::warn!(
            "reindex[{}]: prune pass: {} file(s) failed to fully prune — \
             ghost chunks may persist until next reindex",
            index_id.0,
            failed_count,
        );
    }

    tracing::info!(
        "reindex[{}]: prune pass: pruned {} stale chunk(s) from {} deleted file(s)",
        index_id.0,
        total_pruned_chunks,
        pruned_paths_for_hash.len(),
    );
}

#[cfg(test)]
#[path = "prune_tests.rs"]
mod prune_tests;
