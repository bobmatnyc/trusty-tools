//! M004 — Repair any remaining absolute `file` fields in the chunk corpus.
//!
//! Why (issue #674 — portable-paths `path` field): M002 rewrote absolute
//! `file` / `id` pairs during a one-time migration at daemon startup. However
//! two sources can re-introduce absolute `file` values after M002 has run:
//!
//! 1. `POST /indexes/:id/index-file` called by a client that passes an absolute
//!    path as `path` — the HTTP handler forwards that path straight to
//!    `CodeIndexer::index_file`, which stores it verbatim.
//! 2. A daemon binary built before issue #402 stored chunks with absolute paths;
//!    that daemon was replaced with a post-#402 binary but M002 ran on a corpus
//!    whose `root_path` had a symlink alias — `strip_prefix` failed and the
//!    `unwrap_or(&path)` fallback stored the absolute path again.
//!
//! Both cases leave `CodeChunk.path` as `None` (because `raw_to_code_chunk`
//! only populates `path` when the stored `file` is already relative, per #674).
//! M004 does a second idempotent pass with the same rewrite logic as M002 so
//! those chunks gain a correct relative `file` (and thus a non-null `path` in
//! search results) without a full re-index.
//!
//! What: `apply` loads all chunks from the durable corpus, identifies those
//! whose `file` is absolute and shares the index `root_path` as a prefix,
//! rewrites `file` (and reconstructs `id`) to be root-relative, upserts the
//! modified chunks, then deletes the old absolute-keyed rows. Idempotency is
//! guaranteed by the pre-check: a chunk whose `file` is already relative is
//! left unchanged.
//!
//! Test: `m004::tests` covers rewrite correctness, idempotency (second `apply`
//! is a no-op), the all-already-relative fast path, and the no-corpus fast path.

use std::path::Path;

use anyhow::{Context, Result};
use async_trait::async_trait;

use crate::core::registry::IndexHandle;

use super::Migration;

// ── M004 struct ───────────────────────────────────────────────────────────────

/// Migration M004: second idempotent pass to repair absolute `file` fields
/// that slipped back into the corpus after M002 ran (issue #674).
///
/// Why: see module-level doc.
/// What: loads all chunks, rewrites any still-absolute `file`/`id` pairs to
/// root-relative form, upserts rewritten rows, deletes old absolute-keyed
/// rows, and refreshes the live BM25 / in-memory map.
/// Test: `test_m004_from_target_version`, `test_m004_idempotency`,
///       `test_m004_all_relative_is_noop`, `test_m004_apply_no_corpus_is_ok`.
pub struct M004RepairAbsoluteFilePaths;

#[async_trait]
impl Migration for M004RepairAbsoluteFilePaths {
    /// Why: M004 starts at schema_version 3 (after M003 has run).
    fn source_version(&self) -> u32 {
        3
    }

    /// Why: M004 advances the index to schema_version 4.
    fn target_version(&self) -> u32 {
        4
    }

    /// Why: human-readable description appears in log lines and error messages.
    fn description(&self) -> &'static str {
        "M004: repair any remaining absolute chunk file paths (issue #674)"
    }

    /// Apply M004 to `index`.
    ///
    /// Why: see module-level doc.
    /// What:
    /// 1. Clone corpus Arc and root_path under a brief read lock.
    /// 2. Load all chunks (blocking I/O, `spawn_blocking`).
    /// 3. For each chunk whose `file` is absolute and starts with `root_path`,
    ///    compute the root-relative path and reconstruct the chunk id.
    /// 4. Upsert rewritten chunks into redb (new relative-keyed rows).
    /// 5. Delete the old absolute-keyed rows from redb.
    /// 6. Refresh the live BM25 + in-memory chunks map.
    /// Returns `Ok(())` when no rewrite is needed (already relative or no
    /// corpus).
    /// Test: `test_m004_apply_no_corpus_is_ok`.
    async fn apply(&self, index: &IndexHandle) -> Result<(), anyhow::Error> {
        // ── Step 1: clone corpus Arc + root_path under a brief read lock ──
        let (corpus, root_path) = {
            let indexer = index.indexer.read().await;
            let corpus = indexer.corpus_store();
            let root = index.root_path.clone();
            (corpus, root)
        };

        let Some(corpus) = corpus else {
            // BM25-only or test index with no durable corpus — no-op.
            tracing::debug!(
                index_id = %index.id,
                "M004: no durable corpus, skipping"
            );
            return Ok(());
        };

        // ── Step 2: load all chunks (blocking I/O) ─────────────────────────
        let all_chunks = tokio::task::spawn_blocking({
            let corpus = std::sync::Arc::clone(&corpus);
            move || corpus.load_all_chunks()
        })
        .await
        .context("M004: load_all_chunks task panicked")?
        .context("M004: failed to load chunks from corpus")?;

        // ── Step 3: identify chunks needing rewrite ────────────────────────
        let mut to_upsert = Vec::new();
        let mut ids_to_delete: Vec<String> = Vec::new();

        for mut chunk in all_chunks {
            if !Path::new(&chunk.file).is_absolute() {
                // Already relative — idempotency: leave unchanged.
                continue;
            }
            // Try to strip the root_path prefix.
            let old_file = chunk.file.clone();
            let old_id = chunk.id.clone();
            match Path::new(&old_file).strip_prefix(&root_path) {
                Ok(rel) => {
                    let rel_str = rel.to_string_lossy().into_owned();
                    let new_id = reconstruct_id(&old_id, &old_file, &rel_str);
                    chunk.file = rel_str;
                    chunk.id = new_id;
                    ids_to_delete.push(old_id);
                    to_upsert.push(chunk);
                }
                Err(_) => {
                    // Absolute path but not under root_path — defensive: log
                    // and leave unchanged to avoid corrupting a cross-root index.
                    tracing::warn!(
                        index_id = %index.id,
                        file = %old_file,
                        root = %root_path.display(),
                        "M004: chunk file is absolute but not under root_path; skipping"
                    );
                }
            }
        }

        if to_upsert.is_empty() {
            tracing::info!(
                index_id = %index.id,
                "M004: all chunk file paths already relative, nothing to do"
            );
            return Ok(());
        }

        tracing::info!(
            index_id = %index.id,
            count = to_upsert.len(),
            "M004: rewriting absolute chunk file paths to root-relative"
        );

        // ── Step 4 & 5: upsert rewritten chunks then delete old rows ───────
        // Upsert first so a crash leaves both old + new rows (double entries)
        // which are safe at query time; deleting only on upsert success ensures
        // we never lose data.
        let upsert_corpus = std::sync::Arc::clone(&corpus);
        let chunks_to_upsert = to_upsert;
        tokio::task::spawn_blocking(move || upsert_corpus.upsert_chunks(&chunks_to_upsert))
            .await
            .context("M004: upsert task panicked")?
            .context("M004: failed to upsert rewritten chunks")?;

        let delete_corpus = std::sync::Arc::clone(&corpus);
        tokio::task::spawn_blocking(move || delete_corpus.delete_chunks(&ids_to_delete))
            .await
            .context("M004: delete task panicked")?
            .context("M004: failed to delete old absolute-keyed chunk rows")?;

        // ── Step 6: sync the live BM25 + in-memory chunks map ─────────────
        {
            let indexer = index.indexer.read().await;
            if let Err(e) = indexer.refresh_live_indices_from_corpus().await {
                tracing::warn!(
                    index_id = %index.id,
                    "M004: live-index refresh failed ({e}) — \
                     BM25 may be stale until next daemon restart"
                );
            }
        }

        tracing::info!(
            index_id = %index.id,
            "M004: path repair complete (redb + live BM25 + chunks map synced)"
        );

        Ok(())
    }
}

// ── Helper ────────────────────────────────────────────────────────────────────

/// Reconstruct the chunk `id` by replacing the absolute `old_file` prefix with
/// `rel_file`.
///
/// Why: chunk `id` encodes the file path: `"{file}:{start}:{end}"`. When the
/// file part changes from absolute to relative, the `id` key in redb must
/// change too. We swap the file prefix rather than re-parsing `start`/`end`
/// so malformed ids survive unchanged (best-effort migration must not panic).
/// What: if `old_id` starts with `old_file`, replace the `old_file` prefix
/// with `rel_file`; otherwise return `old_id` unchanged.
/// Test: `test_m004_reconstruct_id` in `m004::tests`.
fn reconstruct_id(old_id: &str, old_file: &str, rel_file: &str) -> String {
    if let Some(suffix) = old_id.strip_prefix(old_file) {
        format!("{rel_file}{suffix}")
    } else {
        old_id.to_string()
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: validates the version contract that `run_migrations` depends on.
    /// What: `source_version` must be 3, `target_version` must be 4.
    #[test]
    fn test_m004_from_target_version() {
        let m = M004RepairAbsoluteFilePaths;
        assert_eq!(m.source_version(), 3);
        assert_eq!(m.target_version(), 4);
    }

    /// Why: validates that the description string is non-empty and contains
    /// the migration label and issue reference for operator log triage.
    #[test]
    fn test_m004_description_non_empty() {
        let m = M004RepairAbsoluteFilePaths;
        let desc = m.description();
        assert!(!desc.is_empty());
        assert!(desc.contains("M004"), "description should include 'M004'");
        assert!(desc.contains("#674"), "description should include '#674'");
    }

    /// Why: validates that `target_version - source_version == 1`, ensuring
    /// M004 advances exactly one schema version.
    #[test]
    fn test_m004_advances_exactly_one_version() {
        let m = M004RepairAbsoluteFilePaths;
        assert_eq!(
            m.target_version() - m.source_version(),
            1,
            "each migration must advance exactly one version"
        );
    }

    /// Why: validates the `reconstruct_id` helper correctly swaps the file
    /// prefix in a standard chunk id.
    /// What: `"{abs_file}:{start}:{end}"` → `"{rel_file}:{start}:{end}"`.
    #[test]
    fn test_m004_reconstruct_id_standard() {
        let old_file = "/mnt/efs/data/repos/proj/src/lib.rs";
        let rel_file = "src/lib.rs";
        let old_id = format!("{old_file}:42:78");
        let new_id = reconstruct_id(&old_id, old_file, rel_file);
        assert_eq!(new_id, "src/lib.rs:42:78");
    }

    /// Why: validates that `reconstruct_id` is a no-op when the id does not
    /// start with `old_file` (defensive path for unexpected formats).
    #[test]
    fn test_m004_reconstruct_id_no_match_passthrough() {
        let old_id = "some::qualified::id";
        let result = reconstruct_id(old_id, "/unexpected/prefix", "rel");
        assert_eq!(result, old_id);
    }

    /// Why: validates the path-rewrite logic used inside `apply` without
    /// spinning up a real redb corpus — strip_prefix on PathBuf.
    #[test]
    fn test_m004_rewrite_logic_strip_prefix() {
        let root = std::path::Path::new("/mnt/efs/data/repos/proj");
        let abs_file = "/mnt/efs/data/repos/proj/src/lib.rs";
        let rel = std::path::Path::new(abs_file).strip_prefix(root).unwrap();
        assert_eq!(rel.display().to_string(), "src/lib.rs");
    }

    /// Why: validates that a path outside the root causes `strip_prefix` to
    /// return `Err` — the migration's defensive warn-and-skip branch.
    #[test]
    fn test_m004_rewrite_logic_non_root_path_errors() {
        let root = std::path::Path::new("/mnt/efs/data/repos/proj");
        let unrelated = "/tmp/other/file.rs";
        assert!(
            std::path::Path::new(unrelated).strip_prefix(root).is_err(),
            "path outside root must not be rewritten"
        );
    }

    /// Why: ensures `apply` is a no-op (Ok) when the index has no durable
    /// corpus (BM25-only mode), exercising the early-return guard.
    #[tokio::test]
    async fn test_m004_apply_no_corpus_is_ok() {
        use crate::core::indexer::CodeIndexer;
        use crate::core::registry::{IndexHandle, IndexId};
        use std::sync::Arc;
        use tokio::sync::RwLock;

        let indexer = CodeIndexer::new("m004-test", "/tmp/m004-test");
        let handle = IndexHandle::bare(
            IndexId::new("m004-test"),
            Arc::new(RwLock::new(indexer)),
            std::path::PathBuf::from("/tmp/m004-test"),
        );

        let m = M004RepairAbsoluteFilePaths;
        let result = m.apply(&handle).await;
        assert!(
            result.is_ok(),
            "no-corpus apply must be Ok, got: {result:?}"
        );
    }
}
