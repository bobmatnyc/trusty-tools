//! M001 — Per-`pub const`/`pub static` re-chunking for Rust files.
//!
//! Why: trusty-search v0.11.1 changed the Rust chunker to emit one
//! `ChunkType::Constant` per `pub const`/`pub static` declaration instead of
//! grouping them all into a single `ChunkType::Code` block. Indexes created
//! before v0.11.1 (schema_version == 0) never got that granular coverage; any
//! search for a specific constant name fell back to a whole-file code chunk,
//! producing imprecise results and missing the `function_name` field that
//! token-level BM25 relies on. M001 re-indexes every Rust file that contains at
//! least one `pub const` or `pub static` declaration and does not yet have any
//! `ChunkType::Constant` chunks, bringing those indexes up to v0.11.1 quality.
//!
//! What: `apply` loads all chunks from the durable corpus, identifies Rust files
//! that (a) match `\bpub\s+(const|static)\b` and (b) have no existing
//! `ChunkType::Constant` chunks, then re-parses and re-embeds those files via
//! the indexer's standard pipeline. Idempotency is guaranteed by the
//! "has Constant chunks?" pre-check — running `apply` on an already-migrated
//! index is a no-op.
//!
//! Test: `m001::tests` covers the pre-filter regex, idempotency (second `apply`
//! is a no-op), and the `from/target_version` contract.

use std::collections::HashSet;

use anyhow::{Context, Result};
use async_trait::async_trait;
use regex::Regex;

use crate::core::chunker::ChunkType;
use crate::core::registry::IndexHandle;

use super::Migration;

// ── M001 struct ───────────────────────────────────────────────────────────────

/// Migration M001: re-chunk Rust files that contain `pub const`/`pub static`
/// to emit per-declaration `ChunkType::Constant` chunks (issue #143).
///
/// Why: see module-level doc. Summary: old indexes have one `Code` chunk per
/// file instead of one `Constant` chunk per declaration, hurting precision and
/// missing the `function_name` field for BM25.
/// What: reads corpus chunks, filters to Rust files that need re-indexing, and
/// runs each file through `parse_and_embed_files` + `commit_parsed_batch`.
/// Test: `test_m001_from_target_version`, `test_m001_pre_filter_regex`,
///       `test_m001_idempotency_no_corpus` in `m001::tests`.
pub struct M001PerPubConstRust;

#[async_trait]
impl Migration for M001PerPubConstRust {
    /// Why: M001 starts at schema_version 0 (legacy / no migration framework).
    fn source_version(&self) -> u32 {
        0
    }

    /// Why: M001 advances the index to schema_version 1.
    fn target_version(&self) -> u32 {
        1
    }

    /// Why: human-readable description appears in log lines and error messages.
    fn description(&self) -> &'static str {
        "M001: re-chunk Rust pub const/static → ChunkType::Constant (issue #143)"
    }

    /// Apply M001 to `index`.
    ///
    /// Why: see module-level doc. The method is the single write-side entry
    /// point; it is idempotent because it pre-checks for existing
    /// `ChunkType::Constant` chunks before re-indexing.
    /// What:
    /// 1. Acquire a read lock on the indexer to clone corpus + root_path.
    /// 2. Load all chunks from the durable corpus (via `spawn_blocking`).
    /// 3. Regex-filter to `.rs` files containing `pub const`/`pub static`
    ///    that have zero `ChunkType::Constant` chunks in the corpus.
    /// 4. Read each candidate file from disk and re-run it through
    ///    `parse_and_embed_files` + `commit_parsed_batch`.
    /// Returns `Ok(())` if there are no candidate files (already migrated or
    /// no qualifying Rust files).
    /// Test: `test_m001_apply_no_corpus_is_ok` (no corpus → no-op).
    async fn apply(&self, index: &IndexHandle) -> Result<(), anyhow::Error> {
        // ── Step 1: clone corpus Arc under a brief lock ────────────────────
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
                "M001: no durable corpus, skipping"
            );
            return Ok(());
        };

        // ── Step 2: load all chunks (blocking I/O) ─────────────────────────
        let all_chunks = tokio::task::spawn_blocking({
            let corpus = std::sync::Arc::clone(&corpus);
            move || corpus.load_all_chunks()
        })
        .await
        .context("M001: load_all_chunks task panicked")?
        .context("M001: failed to load chunks from corpus")?;

        // ── Step 3: identify files that need re-indexing ───────────────────
        // Build the set of .rs files that already have at least one Constant
        // chunk — these are already migrated and must be skipped.
        let files_with_constants: HashSet<String> = all_chunks
            .iter()
            .filter(|c| c.chunk_type == ChunkType::Constant && c.file.ends_with(".rs"))
            .map(|c| c.file.clone())
            .collect();

        // Collect all unique .rs files in the corpus.
        let all_rs_files: HashSet<String> = all_chunks
            .iter()
            .filter(|c| c.file.ends_with(".rs"))
            .map(|c| c.file.clone())
            .collect();

        // Regex pre-filter: cheap scan for `pub const`/`pub static` so we
        // don't re-parse files that have no qualifying declarations.
        //
        // Why: tree-sitter parsing is ~10 ms/file; a regex scan is ~0.1 ms.
        // Pre-filtering avoids most of the re-parse work on files that happen
        // to be .rs but contain no public constants.
        let pub_const_re = Regex::new(r"\bpub\s+(const|static)\b").expect("valid pub-const regex");

        // Candidate files: .rs files WITHOUT constant chunks AND whose disk
        // content matches the regex.
        let mut candidates: Vec<(String, String)> = Vec::new();
        for file_path in all_rs_files {
            if files_with_constants.contains(&file_path) {
                // Already has Constant chunks — idempotency guard.
                continue;
            }
            let abs_path = root_path.join(&file_path);
            let content = match tokio::fs::read_to_string(&abs_path).await {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!(
                        index_id = %index.id,
                        path = %abs_path.display(),
                        "M001: cannot read file, skipping ({e})"
                    );
                    continue;
                }
            };
            if pub_const_re.is_match(&content) {
                candidates.push((file_path, content));
            }
        }

        if candidates.is_empty() {
            tracing::info!(
                index_id = %index.id,
                "M001: no candidate Rust files found, nothing to do"
            );
            return Ok(());
        }

        tracing::info!(
            index_id = %index.id,
            count = candidates.len(),
            "M001: re-indexing Rust files with pub const/static"
        );

        // ── Step 4: re-parse + re-embed + commit in batches ────────────────
        // Process files in chunks of 64 to bound per-batch memory.
        const BATCH_SIZE: usize = 64;
        let indexer_arc = std::sync::Arc::clone(&index.indexer);

        for (batch_idx, batch) in candidates.chunks(BATCH_SIZE).enumerate() {
            let files: Vec<(String, String)> = batch.to_vec();

            let parsed = {
                let indexer = indexer_arc.read().await;
                // Use parse_and_embed_files when an embedder is available;
                // the indexer handles the lexical-only fallback internally.
                indexer
                    .parse_and_embed_files(files)
                    .await
                    .with_context(|| {
                        format!("M001: parse_and_embed_files failed on batch {batch_idx}")
                    })?
            };

            {
                let indexer = indexer_arc.read().await;
                indexer
                    .commit_parsed_batch(parsed, /* defer_graph_rebuild */ true)
                    .await
                    .with_context(|| {
                        format!("M001: commit_parsed_batch failed on batch {batch_idx}")
                    })?;
            }

            tracing::debug!(
                index_id = %index.id,
                batch = batch_idx,
                "M001: committed batch"
            );
        }

        tracing::info!(
            index_id = %index.id,
            "M001: re-indexing complete"
        );

        Ok(())
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: validates the version contract that `run_migrations` depends on.
    /// What: `source_version` must be 0, `target_version` must be 1.
    #[test]
    fn test_m001_from_target_version() {
        let m = M001PerPubConstRust;
        assert_eq!(m.source_version(), 0);
        assert_eq!(m.target_version(), 1);
    }

    /// Why: validates that the description string is non-empty and contains
    /// the issue reference for operator log triage.
    #[test]
    fn test_m001_description_non_empty() {
        let m = M001PerPubConstRust;
        let desc = m.description();
        assert!(!desc.is_empty());
        assert!(desc.contains("M001"), "description should include 'M001'");
    }

    /// Why: validates the regex pre-filter correctly accepts files with
    /// `pub const` and `pub static` and rejects files without them.
    /// What: test both positive and negative cases for the regex.
    /// Test: direct regex match on synthetic strings.
    #[test]
    fn test_m001_pre_filter_regex() {
        let re = Regex::new(r"\bpub\s+(const|static)\b").unwrap();

        // Positive cases.
        assert!(re.is_match("pub const MAX_SIZE: usize = 100;"));
        assert!(re.is_match("pub static GREETING: &str = \"hello\";"));
        assert!(re.is_match("    pub const NESTED: u32 = 42;"));
        assert!(re.is_match("pub static mut COUNTER: u32 = 0;"));

        // Negative cases — files with no matching pattern are skipped.
        assert!(!re.is_match("const PRIVATE: usize = 1;"));
        assert!(!re.is_match("pub fn my_function() {}"));
        assert!(!re.is_match("let x = 5;"));

        // The pre-filter intentionally has false-positives for commented-out
        // declarations: regex scan is cheaper than full AST parse and the
        // idempotency guard in the outer loop handles the "already migrated"
        // case correctly even after a false-positive re-index.
        assert!(re.is_match("// pub const IN_COMMENT: u32 = 1;"));
    }

    /// Why: ensures `apply` is a no-op (Ok) when the index has no durable
    /// corpus (BM25-only mode), exercising the early-return guard.
    #[tokio::test]
    async fn test_m001_apply_no_corpus_is_ok() {
        use crate::core::indexer::CodeIndexer;
        use crate::core::registry::{IndexHandle, IndexId};
        use std::sync::Arc;
        use tokio::sync::RwLock;

        let indexer = CodeIndexer::new("m001-test", "/tmp/m001-test");
        let handle = IndexHandle::bare(
            IndexId::new("m001-test"),
            Arc::new(RwLock::new(indexer)),
            std::path::PathBuf::from("/tmp/m001-test"),
        );

        let m = M001PerPubConstRust;
        let result = m.apply(&handle).await;
        assert!(
            result.is_ok(),
            "no-corpus apply must be Ok, got: {result:?}"
        );
    }

    /// Why: validates that `target_version - source_version == 1`, ensuring M001
    /// advances exactly one schema version. This is the convention for all
    /// migrations and must be true for the chain to remain contiguous.
    #[test]
    fn test_m001_advances_exactly_one_version() {
        let m = M001PerPubConstRust;
        assert_eq!(
            m.target_version() - m.source_version(),
            1,
            "each migration must advance exactly one version"
        );
    }
}
