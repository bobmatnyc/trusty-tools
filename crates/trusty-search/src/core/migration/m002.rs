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
//! What: `apply` delegates to
//! [`super::relativize::relativize_corpus_paths`], shared with M004. That pass
//! rewrites every absolute `file` under `root_path` and its id, keeps a row
//! whose relative id is already taken (reported, #7923), and verifies the row
//! count is unchanged. Already-relative chunks are left unchanged, which makes
//! a second `apply` a no-op.
//!
//! Test: `m002::tests` covers the version contract and the no-corpus fast
//! path; `relativize::tests` covers the rewrite and its #7923 loss shapes.

use async_trait::async_trait;

use crate::core::registry::IndexHandle;

use super::relativize::relativize_corpus_paths;
use super::Migration;

/// Migration M002: rewrite absolute `file` paths in the chunk corpus to be
/// relative to the index `root_path` (issue #402, phase 1).
///
/// Why: see module-level doc.
/// What: runs the shared relativization pass under the `M002` label.
/// Test: `test_m002_from_target_version`, `test_m002_apply_no_corpus_is_ok`,
/// `relativize::tests::relativize_preserves_every_chunk_for_m002_and_m004`.
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
    /// What: delegates to `relativize_corpus_paths`; an `Err` (including a
    /// changed row count, #7923) leaves the schema version unstamped.
    /// Test: `relativize_preserves_every_chunk_for_m002_and_m004`.
    async fn apply(&self, index: &IndexHandle) -> Result<(), anyhow::Error> {
        // #7923: the former inline rewrite lost a row per id collision.
        relativize_corpus_paths(index, "M002").await.map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::migration::relativize::reconstruct_id;
    use std::path::Path;

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

    /// Why: validates the path-rewrite logic used inside the pass without
    /// spinning up a real redb corpus — strip_prefix on PathBuf.
    #[test]
    fn test_m002_rewrite_logic_strip_prefix() {
        let root = Path::new("/Users/alice/proj");
        let abs_file = "/Users/alice/proj/src/lib.rs";
        let rel = Path::new(abs_file).strip_prefix(root).unwrap();
        assert_eq!(rel.display().to_string(), "src/lib.rs");
    }

    /// Why: validates that a path that does NOT share the root prefix causes
    /// `strip_prefix` to return `Err` — the pass's defensive branch.
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
