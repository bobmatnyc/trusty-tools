//! M003 — Rewrite absolute chunk IDs in `hnsw.keys.json` to root-relative.
//!
//! Why (issue #402 phase 2): M002 rewrites every chunk's `file`/`id` in the
//! redb corpus from absolute to root-relative paths, and refreshes the live
//! BM25 + in-memory chunks map.  It does **not** touch the HNSW key sidecar
//! (`hnsw.keys.json`), which maps string chunk IDs → `u64` HNSW labels.
//! After M002, vector search still returns the old absolute chunk IDs from
//! `hnsw.keys.json`.  `fetch_chunks_for_ids` then point-reads redb with those
//! absolute IDs — which no longer exist there — and returns 0 vector results
//! on every migrated legacy index.
//!
//! This migration fixes the mismatch by rewriting the in-memory
//! `id_to_key` / `key_to_id` maps from absolute to root-relative paths, then
//! flushing the updated sidecar to disk atomically.  The `.usearch` binary is
//! **not** rewritten: usearch stores vectors keyed by `u64` labels that have
//! no relation to file paths, so only the JSON sidecar needs updating.  No
//! re-embedding is required.
//!
//! This migration also repairs indexes that were already stamped at
//! `schema_version = 2` by a prior broken binary (which ran M002's redb
//! rewrite but did not rewrite `hnsw.keys.json`).  Because M003's
//! `source_version` is 2, `run_migrations` will apply it to all indexes
//! currently at v2, including those "stuck" indexes.
//!
//! What: `apply` resolves the HNSW path for the index (trying colocated
//! `<root>/.trusty-search/hnsw.usearch` first, then the legacy global path),
//! calls `CodeIndexer::rewrite_vector_store_keys` (which updates the live
//! maps and flushes the sidecar), and returns `Ok(())`.  The method is
//! idempotent: already-relative IDs are left unchanged and a count of 0 is
//! a clean no-op.
//!
//! Test: `m003::tests` covers the path-rewrite logic, idempotency (second
//! `apply` is a no-op), and the no-store fast path.

use anyhow::{Context, Result};
use async_trait::async_trait;

use crate::core::registry::IndexHandle;

use super::Migration;

// ── M003 struct ───────────────────────────────────────────────────────────────

/// Migration M003: rewrite absolute chunk IDs in `hnsw.keys.json` to
/// root-relative, matching the redb corpus rewrite performed by M002.
///
/// Why: see module-level doc.
/// What: calls `CodeIndexer::rewrite_vector_store_keys` after resolving the
/// HNSW path from the index's `id` and `root_path`.  No re-embedding required.
/// Test: `test_m003_from_target_version`, `test_m003_apply_no_store_is_ok`,
///       `test_m003_apply_idempotent_when_already_relative`.
pub struct M003HnswKeyRelativization;

#[async_trait]
impl Migration for M003HnswKeyRelativization {
    /// Why: M003 starts at schema_version 2 (after M002 has run).
    fn source_version(&self) -> u32 {
        2
    }

    /// Why: M003 advances the index to schema_version 3.
    fn target_version(&self) -> u32 {
        3
    }

    /// Why: human-readable description appears in log lines and error messages.
    fn description(&self) -> &'static str {
        "M003: rewrite absolute HNSW key IDs to root-relative (issue #402 phase 2)"
    }

    /// Apply M003 to `index`.
    ///
    /// Why: see module-level doc.
    /// What:
    /// 1. Resolve the HNSW path (colocated or legacy global).
    /// 2. Under a read lock on the indexer, call `rewrite_vector_store_keys`
    ///    which updates the live in-memory maps and flushes the sidecar.
    /// 3. Return `Ok(())` — a count of 0 (already relative / no store) is
    ///    treated as a clean idempotent no-op.
    /// Test: `test_m003_apply_no_store_is_ok`.
    async fn apply(&self, index: &IndexHandle) -> Result<(), anyhow::Error> {
        // ── Step 1: resolve the HNSW path ─────────────────────────────────
        let hnsw_path = resolve_hnsw_path(index)?;

        if !hnsw_path.exists() {
            tracing::debug!(
                index_id = %index.id,
                path = %hnsw_path.display(),
                "M003: no hnsw snapshot found; skipping (BM25-only or not yet indexed)"
            );
            return Ok(());
        }

        let root_path = index.root_path.clone();

        // ── Step 2: rewrite in-memory maps + flush sidecar ────────────────
        let count = {
            let indexer = index.indexer.read().await;
            indexer
                .rewrite_vector_store_keys(&hnsw_path, &root_path)
                .await
                .context("M003: rewrite_vector_store_keys failed")?
        };

        if count == 0 {
            tracing::info!(
                index_id = %index.id,
                "M003: all HNSW keys already relative (or no vector store wired); nothing to do"
            );
        } else {
            tracing::info!(
                index_id = %index.id,
                count,
                "M003: rewrote absolute HNSW key IDs to root-relative (sidecar flushed)"
            );
        }

        Ok(())
    }
}

// ── Path resolution helper ────────────────────────────────────────────────────

/// Resolve the HNSW snapshot path for `index`.
///
/// Why: indexes may store their HNSW snapshot in the colocated
/// `<root_path>/.trusty-search/hnsw.usearch` (issue #403) or in the legacy
/// global data dir (`<data_dir>/indexes/<id>/hnsw.usearch`).  M003 must find
/// the right file without re-introducing the colocated-vs-legacy branch that
/// lives in `service::persistence`.  The cheapest correct strategy is: try
/// the colocated path first (it is the newer convention); if that file does
/// not exist, fall through to the legacy global path.
/// What: returns the `PathBuf` of the first existing `hnsw.usearch`; when
/// neither exists, returns the legacy global path so the caller can perform its
/// own "file not found" check.
/// Test: covered indirectly by `test_m003_apply_no_store_is_ok` (no file →
/// apply returns Ok without panicking).
fn resolve_hnsw_path(index: &IndexHandle) -> Result<std::path::PathBuf> {
    // Try colocated first (issue #403).
    let colocated = index.root_path.join(".trusty-search").join("hnsw.usearch");
    if colocated.exists() {
        return Ok(colocated);
    }
    // Fall back to the legacy global data-dir path.
    crate::service::persistence::hnsw_path(&index.id.0)
        .context("M003: could not resolve legacy hnsw path")
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::indexer::CodeIndexer;
    use crate::core::registry::{IndexHandle, IndexId};
    use crate::core::store::{UsearchStore, VectorStore};
    use std::sync::Arc;
    use tokio::sync::RwLock;

    /// Why: validates the version contract that `run_migrations` depends on.
    /// What: `source_version` must be 2, `target_version` must be 3.
    #[test]
    fn test_m003_from_target_version() {
        let m = M003HnswKeyRelativization;
        assert_eq!(m.source_version(), 2);
        assert_eq!(m.target_version(), 3);
    }

    /// Why: validates that the description string is non-empty and contains
    /// the migration label for operator log triage.
    #[test]
    fn test_m003_description_non_empty() {
        let m = M003HnswKeyRelativization;
        let desc = m.description();
        assert!(!desc.is_empty());
        assert!(desc.contains("M003"), "description should include 'M003'");
    }

    /// Why: validates that `target_version - source_version == 1`, ensuring
    /// M003 advances exactly one schema version.
    #[test]
    fn test_m003_advances_exactly_one_version() {
        let m = M003HnswKeyRelativization;
        assert_eq!(
            m.target_version() - m.source_version(),
            1,
            "each migration must advance exactly one version"
        );
    }

    /// Why: ensures `apply` is a no-op (Ok) when the index has no persisted
    /// HNSW snapshot (BM25-only or never indexed), exercising the early-return
    /// guard on a missing file.
    #[tokio::test]
    async fn test_m003_apply_no_store_is_ok() {
        let indexer = CodeIndexer::new("m003-test", "/tmp/m003-test-no-store");
        let handle = IndexHandle::bare(
            IndexId::new("m003-test-no-store"),
            Arc::new(RwLock::new(indexer)),
            std::path::PathBuf::from("/tmp/m003-test-no-store"),
        );

        let m = M003HnswKeyRelativization;
        let result = m.apply(&handle).await;
        assert!(
            result.is_ok(),
            "no-snapshot apply must be Ok, got: {result:?}"
        );
    }

    /// Why: validates that `rewrite_keys_to_relative` is idempotent — calling
    /// it twice on an already-relative store returns 0 the second time and
    /// leaves the maps unchanged.
    #[tokio::test]
    async fn test_m003_idempotent_when_already_relative() {
        let store = UsearchStore::new(4).expect("store init");
        // Insert a chunk with a relative ID (already correct).
        store
            .upsert("src/lib.rs:10:40", vec![1.0, 0.0, 0.0, 0.0])
            .await
            .unwrap();

        let root = std::path::Path::new("/Users/alice/proj");
        // First call: no absolute IDs → count == 0.
        let count1 = store.rewrite_keys_to_relative(root).await.unwrap();
        assert_eq!(count1, 0, "already-relative store must rewrite 0 keys");
        // Second call: still 0.
        let count2 = store.rewrite_keys_to_relative(root).await.unwrap();
        assert_eq!(count2, 0, "idempotent second call must rewrite 0 keys");
        // The relative ID must still be searchable.
        let hits = store.search(&[1.0, 0.0, 0.0, 0.0], 1).await.unwrap();
        assert_eq!(hits[0].chunk_id, "src/lib.rs:10:40");
    }

    /// Why: validates the core rewrite logic — absolute IDs that share
    /// `root_path` as a prefix are stripped to root-relative, and a subsequent
    /// search returns the relative chunk ID.
    #[tokio::test]
    async fn test_m003_rewrites_absolute_to_relative() {
        let root = std::path::Path::new("/Users/alice/proj");

        let store = UsearchStore::new(4).expect("store init");
        // Insert chunks with absolute IDs (pre-M002 format).
        store
            .upsert(
                "/Users/alice/proj/src/lib.rs:10:40",
                vec![1.0, 0.0, 0.0, 0.0],
            )
            .await
            .unwrap();
        store
            .upsert(
                "/Users/alice/proj/tests/foo.rs:1:20",
                vec![0.0, 1.0, 0.0, 0.0],
            )
            .await
            .unwrap();

        let count = store.rewrite_keys_to_relative(root).await.unwrap();
        assert_eq!(count, 2, "two absolute IDs must be rewritten");

        // Vector search must now return relative IDs.
        let hits = store.search(&[1.0, 0.0, 0.0, 0.0], 1).await.unwrap();
        assert_eq!(
            hits[0].chunk_id, "src/lib.rs:10:40",
            "search after rewrite must return relative ID"
        );

        // Idempotency: a second rewrite must change nothing.
        let count2 = store.rewrite_keys_to_relative(root).await.unwrap();
        assert_eq!(count2, 0, "second rewrite must be a no-op");
    }

    /// Why: validates that an absolute ID that does NOT share `root_path` as a
    /// prefix is left unchanged (the defensive warn-and-skip branch).
    #[tokio::test]
    async fn test_m003_skips_out_of_root_absolute_ids() {
        let root = std::path::Path::new("/Users/alice/proj");

        let store = UsearchStore::new(4).expect("store init");
        // An absolute ID under a different root.
        store
            .upsert("/Users/bob/other/src/lib.rs:1:10", vec![1.0, 0.0, 0.0, 0.0])
            .await
            .unwrap();

        let count = store.rewrite_keys_to_relative(root).await.unwrap();
        assert_eq!(count, 0, "out-of-root absolute ID must not be rewritten");

        // The original absolute ID must still be searchable.
        let hits = store.search(&[1.0, 0.0, 0.0, 0.0], 1).await.unwrap();
        assert_eq!(
            hits[0].chunk_id, "/Users/bob/other/src/lib.rs:1:10",
            "out-of-root ID must survive unchanged"
        );
    }

    /// Why: validates the full round-trip: construct a store with absolute-keyed
    /// chunk IDs, save it to disk (writing an absolute-keyed sidecar), reload
    /// the store (simulating daemon restart), call `rewrite_keys_to_relative`
    /// (M003 apply), and assert that vector search returns relative IDs.
    /// This is the exact scenario for indexes stuck at schema_version=2.
    #[tokio::test]
    async fn test_m003_full_roundtrip_absolute_sidecar_to_relative() {
        let dir = tempfile::tempdir().unwrap();
        let hnsw_path = dir.path().join("hnsw.usearch");
        let root = dir.path().join("proj");

        let abs_id = format!("{}/src/lib.rs:42:78", root.display());

        // Step 1: Build and save a store with absolute-keyed IDs.
        {
            let store = UsearchStore::new(4).unwrap();
            store
                .upsert(&abs_id, vec![1.0, 0.0, 0.0, 0.0])
                .await
                .unwrap();
            store.save(&hnsw_path).await.unwrap();
        }

        // Verify the on-disk sidecar has absolute keys.
        {
            let json = std::fs::read(hnsw_path.with_extension("keys.json")).unwrap();
            let map: serde_json::Value = serde_json::from_slice(&json).unwrap();
            let keys: Vec<&str> = map["id_to_key"]
                .as_object()
                .unwrap()
                .keys()
                .map(|s| s.as_str())
                .collect();
            assert!(
                keys.iter().any(|k| k.starts_with('/')),
                "sidecar must have absolute keys before M003"
            );
        }

        // Step 2: Simulate daemon restart — reload from disk.
        let store = UsearchStore::load_from(&hnsw_path)
            .await
            .unwrap()
            .expect("load returned Some");

        // Confirm the reload has absolute keys in memory.
        let hits_before = store.search(&[1.0, 0.0, 0.0, 0.0], 1).await.unwrap();
        assert_eq!(
            hits_before[0].chunk_id, abs_id,
            "before M003, search must return the absolute ID"
        );

        // Step 3: Apply M003 key rewrite.
        let count = store.rewrite_keys_to_relative(&root).await.unwrap();
        assert_eq!(count, 1, "M003 must rewrite the one absolute key");

        // Flush updated sidecar.
        store.save(&hnsw_path).await.unwrap();

        // Step 4: After rewrite, search returns the relative ID.
        let hits_after = store.search(&[1.0, 0.0, 0.0, 0.0], 1).await.unwrap();
        assert_eq!(
            hits_after[0].chunk_id, "src/lib.rs:42:78",
            "after M003, search must return the relative ID"
        );

        // Step 5: Idempotency — rewrite again returns 0.
        let count2 = store.rewrite_keys_to_relative(&root).await.unwrap();
        assert_eq!(count2, 0, "second M003 apply must be a no-op");

        // Step 6: Reload once more and confirm the sidecar is now relative.
        let reloaded = UsearchStore::load_from(&hnsw_path)
            .await
            .unwrap()
            .expect("reload returned Some");
        let hits_reloaded = reloaded.search(&[1.0, 0.0, 0.0, 0.0], 1).await.unwrap();
        assert_eq!(
            hits_reloaded[0].chunk_id, "src/lib.rs:42:78",
            "reloaded store must return relative ID (persisted sidecar is now relative)"
        );
    }
}
