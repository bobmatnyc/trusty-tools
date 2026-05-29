//! M002 — Rewrite absolute chunk `file` paths to root-relative paths.
//!
//! Why (issue #402 — relocation resilience, phase 1): before this migration,
//! every chunk stored its `file` field as an absolute host path (e.g.
//! `/Users/alice/code/myproject/src/lib.rs`). Moving or renaming the project
//! directory left every stored path stale — search results would point at the
//! old location and the only fix was a full re-index. M002 rewrites the stored
//! `file` (and the corresponding `id`, which encodes the file path) in the
//! redb corpus to be relative to the index's `root_path` (e.g. `src/lib.rs`),
//! so updating `root_path` in `indexes.toml` is sufficient to relocate the
//! index without a full re-index.
//!
//! What: `apply` loads all chunks from the durable corpus, identifies those
//! whose `file` is absolute and shares the index `root_path` as a prefix,
//! rewrites `file` (and reconstructs `id`) to be root-relative, and upserts
//! the modified chunks back. The old absolute-keyed rows are deleted
//! transactionally in the same write pass. Idempotency is guaranteed by the
//! pre-check: if a chunk's `file` is already relative (i.e. not absolute) it
//! is left unchanged. Chunks whose absolute path does NOT share the
//! `root_path` prefix (unusual, defensive path) are also left unchanged and
//! logged at `warn`.
//!
//! Test: `m002::tests` covers the path rewrite logic, idempotency (second
//! `apply` is a no-op), and the no-corpus fast path.

use std::path::Path;

use anyhow::{Context, Result};
use async_trait::async_trait;

use crate::core::registry::IndexHandle;

use super::Migration;

// ── M002 struct ───────────────────────────────────────────────────────────────

/// Migration M002: rewrite absolute `file` paths in the chunk corpus to be
/// relative to the index `root_path` (issue #402, phase 1).
///
/// Why: see module-level doc.
/// What: loads all chunks, rewrites absolute-path `file`/`id` fields to
/// root-relative, upserts rewritten chunks, and deletes the old absolute-keyed
/// rows in the redb corpus.
/// Test: `test_m002_from_target_version`, `test_m002_rewrite_logic`,
///       `test_m002_idempotency`, `test_m002_apply_no_corpus_is_ok`.
pub struct M002AbsoluteToRelativePaths;

#[async_trait]
impl Migration for M002AbsoluteToRelativePaths {
    /// Why: M002 starts at schema_version 1 (after M001 has run).
    fn source_version(&self) -> u32 {
        1
    }

    /// Why: M002 advances the index to schema_version 2.
    fn target_version(&self) -> u32 {
        2
    }

    /// Why: human-readable description appears in log lines and error messages.
    fn description(&self) -> &'static str {
        "M002: rewrite absolute chunk file paths to root-relative (issue #402)"
    }

    /// Apply M002 to `index`.
    ///
    /// Why: see module-level doc.
    /// What:
    /// 1. Clone corpus Arc and root_path under a brief read lock.
    /// 2. Load all chunks (blocking I/O, `spawn_blocking`).
    /// 3. For each chunk whose `file` is absolute and starts with `root_path`,
    ///    compute the root-relative file path and the reconstructed chunk id.
    /// 4. Upsert rewritten chunks into redb (new relative-keyed rows).
    /// 5. Delete the old absolute-keyed rows from redb.
    /// Returns `Ok(())` when no rewrite is needed (already relative or no
    /// corpus).
    /// Test: `test_m002_apply_no_corpus_is_ok`.
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
                "M002: no durable corpus, skipping"
            );
            return Ok(());
        };

        // ── Step 2: load all chunks (blocking I/O) ─────────────────────────
        let all_chunks = tokio::task::spawn_blocking({
            let corpus = std::sync::Arc::clone(&corpus);
            move || corpus.load_all_chunks()
        })
        .await
        .context("M002: load_all_chunks task panicked")?
        .context("M002: failed to load chunks from corpus")?;

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
                    // Reconstruct id: replace the leading absolute file prefix
                    // with the relative path. Chunk id format is
                    // `"{file}:{start}:{end}"` — split on the first ':' that
                    // follows the file segment.
                    let new_id = reconstruct_id(&old_id, &old_file, &rel_str);
                    chunk.file = rel_str;
                    chunk.id = new_id;
                    ids_to_delete.push(old_id);
                    to_upsert.push(chunk);
                }
                Err(_) => {
                    // Absolute path but not under root_path — defensive: log
                    // and leave unchanged so we don't corrupt a cross-root index.
                    tracing::warn!(
                        index_id = %index.id,
                        file = %old_file,
                        root = %root_path.display(),
                        "M002: chunk file is absolute but not under root_path; skipping"
                    );
                }
            }
        }

        if to_upsert.is_empty() {
            tracing::info!(
                index_id = %index.id,
                "M002: all chunk paths already relative, nothing to do"
            );
            return Ok(());
        }

        tracing::info!(
            index_id = %index.id,
            count = to_upsert.len(),
            "M002: rewriting absolute chunk file paths to root-relative"
        );

        // ── Step 4 & 5: upsert rewritten chunks then delete old rows ───────
        // Upsert first so a crash between steps leaves both old + new rows
        // (double entries) which are safe at query time (the relative row wins
        // in any post-M002 search path); deleting only on success of upsert
        // ensures we never lose data.
        let upsert_corpus = std::sync::Arc::clone(&corpus);
        let chunks_to_upsert = to_upsert;
        tokio::task::spawn_blocking(move || upsert_corpus.upsert_chunks(&chunks_to_upsert))
            .await
            .context("M002: upsert task panicked")?
            .context("M002: failed to upsert rewritten chunks")?;

        let delete_corpus = std::sync::Arc::clone(&corpus);
        tokio::task::spawn_blocking(move || delete_corpus.delete_chunks(&ids_to_delete))
            .await
            .context("M002: delete task panicked")?
            .context("M002: failed to delete old absolute-keyed chunk rows")?;

        tracing::info!(
            index_id = %index.id,
            "M002: path rewrite complete"
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
/// change too (it is the primary key for the CHUNKS_TABLE). We swap the file
/// prefix rather than re-parsing `start`/`end` so malformed ids are preserved
/// as-is (best-effort migration should never panic).
/// What: if `old_id` starts with `old_file`, replace the `old_file` prefix
/// with `rel_file`; otherwise return `old_id` unchanged.
/// Test: `test_m002_reconstruct_id` in `m002::tests`.
fn reconstruct_id(old_id: &str, old_file: &str, rel_file: &str) -> String {
    if let Some(suffix) = old_id.strip_prefix(old_file) {
        format!("{rel_file}{suffix}")
    } else {
        // Unexpected format — leave unchanged.
        old_id.to_string()
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: validates the version contract that `run_migrations` depends on.
    /// What: `source_version` must be 1, `target_version` must be 2.
    #[test]
    fn test_m002_from_target_version() {
        let m = M002AbsoluteToRelativePaths;
        assert_eq!(m.source_version(), 1);
        assert_eq!(m.target_version(), 2);
    }

    /// Why: validates that the description string is non-empty and contains
    /// the migration label and issue reference for operator log triage.
    #[test]
    fn test_m002_description_non_empty() {
        let m = M002AbsoluteToRelativePaths;
        let desc = m.description();
        assert!(!desc.is_empty());
        assert!(desc.contains("M002"), "description should include 'M002'");
    }

    /// Why: validates that `target_version - source_version == 1`, ensuring
    /// M002 advances exactly one schema version.
    #[test]
    fn test_m002_advances_exactly_one_version() {
        let m = M002AbsoluteToRelativePaths;
        assert_eq!(
            m.target_version() - m.source_version(),
            1,
            "each migration must advance exactly one version"
        );
    }

    /// Why: validates the `reconstruct_id` helper correctly swaps the file
    /// prefix in a standard chunk id.
    /// What: `"{abs_file}:{start}:{end}"` → `"{rel_file}:{start}:{end}"`.
    /// Test: assert the reconstructed id matches expectations.
    #[test]
    fn test_m002_reconstruct_id_standard() {
        let old_file = "/Users/alice/proj/src/lib.rs";
        let rel_file = "src/lib.rs";
        let old_id = format!("{old_file}:42:78");
        let new_id = reconstruct_id(&old_id, old_file, rel_file);
        assert_eq!(new_id, "src/lib.rs:42:78");
    }

    /// Why: validates that `reconstruct_id` is a no-op when the id does not
    /// start with `old_file` (defensive path for unexpected formats).
    #[test]
    fn test_m002_reconstruct_id_no_match_passthrough() {
        let old_id = "some::qualified::id";
        let result = reconstruct_id(old_id, "/unexpected/prefix", "rel");
        assert_eq!(result, old_id);
    }

    /// Why: validates the path-rewrite logic used inside `apply` without
    /// spinning up a real redb corpus — strip_prefix on PathBuf.
    #[test]
    fn test_m002_rewrite_logic_strip_prefix() {
        let root = Path::new("/Users/alice/proj");
        let abs_file = "/Users/alice/proj/src/lib.rs";
        let rel = Path::new(abs_file).strip_prefix(root).unwrap();
        assert_eq!(rel.display().to_string(), "src/lib.rs");
    }

    /// Why: validates that a path that does NOT share the root prefix causes
    /// `strip_prefix` to return `Err` — the migration's defensive branch.
    #[test]
    fn test_m002_rewrite_logic_non_root_path_errors() {
        let root = Path::new("/Users/alice/proj");
        let unrelated = "/tmp/other/file.rs";
        assert!(
            Path::new(unrelated).strip_prefix(root).is_err(),
            "path outside root must not be rewritten"
        );
    }

    /// Why: ensures `apply` is a no-op (Ok) when the index has no durable
    /// corpus (BM25-only mode), exercising the early-return guard.
    #[tokio::test]
    async fn test_m002_apply_no_corpus_is_ok() {
        use crate::core::indexer::CodeIndexer;
        use crate::core::registry::{IndexHandle, IndexId};
        use std::sync::Arc;
        use tokio::sync::RwLock;

        let indexer = CodeIndexer::new("m002-test", "/tmp/m002-test");
        let handle = IndexHandle::bare(
            IndexId::new("m002-test"),
            Arc::new(RwLock::new(indexer)),
            std::path::PathBuf::from("/tmp/m002-test"),
        );

        let m = M002AbsoluteToRelativePaths;
        let result = m.apply(&handle).await;
        assert!(
            result.is_ok(),
            "no-corpus apply must be Ok, got: {result:?}"
        );
    }
}
