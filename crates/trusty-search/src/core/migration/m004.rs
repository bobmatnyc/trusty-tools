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
//! What: `apply` delegates to
//! [`super::relativize::relativize_corpus_paths`], the pass M002 also runs, so
//! the two cannot drift apart again (#7923).
//!
//! Test: `m004::tests` covers the version contract and the no-corpus fast
//! path; `relativize::tests` covers the rewrite and its #7923 loss shapes.

use async_trait::async_trait;

use crate::core::registry::IndexHandle;

use super::relativize::relativize_corpus_paths;
use super::Migration;

/// Migration M004: second idempotent pass to repair absolute `file` fields
/// that slipped back into the corpus after M002 ran (issue #674).
///
/// Why: see module-level doc.
/// What: runs the shared relativization pass under the `M004` label.
/// Test: `test_m004_from_target_version`, `test_m004_apply_no_corpus_is_ok`,
/// `relativize::tests::relativize_preserves_every_chunk_for_m002_and_m004`.
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
    /// What: delegates to `relativize_corpus_paths`; an `Err` (including a
    /// changed row count, #7923) leaves the schema version unstamped.
    /// Test: `relativize_preserves_every_chunk_for_m002_and_m004`.
    async fn apply(&self, index: &IndexHandle) -> Result<(), anyhow::Error> {
        // #7923: the former inline rewrite lost a row per id collision.
        relativize_corpus_paths(index, "M004").await.map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::migration::relativize::reconstruct_id;

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

    /// Why: validates the path-rewrite logic used inside the pass without
    /// spinning up a real redb corpus — strip_prefix on PathBuf.
    #[test]
    fn test_m004_rewrite_logic_strip_prefix() {
        let root = std::path::Path::new("/mnt/efs/data/repos/proj");
        let abs_file = "/mnt/efs/data/repos/proj/src/lib.rs";
        let rel = std::path::Path::new(abs_file).strip_prefix(root).unwrap();
        assert_eq!(rel.display().to_string(), "src/lib.rs");
    }

    /// Why: validates that a path outside the root causes `strip_prefix` to
    /// return `Err` — the pass's warn-and-skip branch.
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
