//! Persist / load the per-index file-hash cache to/from redb (issue #662).
//!
//! Why: the in-process `file_hashes()` DashMap (see `super`) survives multiple
//! `POST /reindex` calls but NOT daemon restarts — cold-start re-embeds every
//! file even when nothing changed since the last committed reindex.  Mirroring
//! the map to the index's redb corpus file lets a restarted daemon load the
//! previous run's hashes and skip unchanged files immediately.
//!
//! Atomicity guarantee (#603 / #662): the hash table is written to the SAME
//! redb file as the chunk corpus.  When staging is active the writes land in
//! `index.redb.tmp`; the atomic rename on commit promotes hashes and chunks
//! together; a rollback discards them together.  Hashes therefore never get
//! out of sync with the committed chunks.
//!
//! What: two public helpers — `load_into_cache` (warm the DashMap from redb
//! at reindex start) and `persist_batch` (write the batch's new hashes to redb
//! after a successful commit).  Both are no-ops when the index has no durable
//! corpus (BM25-only / test indexes).
//!
//! Test: see `tests` submodule below; integration coverage lives in
//! `super::tests`.

use std::path::PathBuf;
use std::sync::Arc;

use dashmap::DashMap;

use crate::core::registry::IndexHandle;

/// Load all persisted file hashes from redb into `map`, warming the in-process
/// cache before the batch loop starts (issue #662).
///
/// Why: called once per reindex run, just after `hashes_for()` returns an
/// arc to the (initially empty, for a fresh daemon) per-index DashMap.  If
/// the map already has entries — from a previous reindex in this daemon's
/// lifetime — the redb values are merged in, with redb winning on collision
/// (the persisted values reflect a completed commit; in-process values are at
/// most equally fresh).
/// What: grabs a read lock on the indexer, clones the corpus Arc, then runs
/// `load_file_hashes` on a blocking worker.  Entries are inserted into `map`
/// only when the redb value differs from the in-process one, to avoid
/// unnecessary hash churn.  Returns the number of hashes actually loaded from
/// redb so callers can surface it in SSE events for operator observability
/// (issue #840 Part 2).  Errors are logged at `warn` and the function returns
/// 0 — the cache is a pure speed optimisation; a miss just causes a re-embed.
/// Test: `load_into_cache_populates_map` below.
pub(super) async fn load_into_cache(
    handle: &IndexHandle,
    map: &Arc<DashMap<PathBuf, String>>,
) -> usize {
    let corpus = {
        let indexer = handle.indexer.read().await;
        indexer.corpus_store()
    };
    let Some(corpus) = corpus else {
        // BM25-only / no durable corpus — nothing to load.
        return 0;
    };
    let result = tokio::task::spawn_blocking(move || corpus.load_file_hashes()).await;
    match result {
        Ok(Ok(entries)) => {
            let count = entries.len();
            for (path_str, hash) in entries {
                let path = PathBuf::from(&path_str);
                // Only insert if absent or stale; avoids unnecessary clones.
                let needs_insert = map.get(&path).map(|v| v.value() != &hash).unwrap_or(true);
                if needs_insert {
                    map.insert(path, hash);
                }
            }
            if count > 0 {
                tracing::info!(
                    "reindex: loaded {} persisted file hashes from redb (warm skip-cache)",
                    count
                );
            }
            count
        }
        Ok(Err(e)) => {
            tracing::warn!("reindex: could not load persisted file hashes ({e}) — cold start");
            0
        }
        Err(e) => {
            tracing::warn!("reindex: file-hash load task panicked ({e}) — cold start");
            0
        }
    }
}

/// Persist `new_hashes` to the current corpus store (staging or live) after a
/// successful batch commit (issue #662).
///
/// Why: called from `apply_successful_commit` so every successfully committed
/// batch's hashes are durably recorded.  When staging is active (#603) the
/// writes land in `index.redb.tmp` alongside the batch's chunks; the atomic
/// rename at the end of the reindex promotes both together.  This is the
/// critical atomicity guarantee: hashes are never persisted for a batch that
/// didn't commit its chunks.
/// What: borrows the current corpus store from the indexer (read lock, then
/// clone the Arc), converts `new_hashes` to `(&str, &str)` slices, and calls
/// `upsert_file_hashes` on a blocking worker.  Errors are logged at `warn`
/// and silently ignored — the cache is optional; a miss just causes a re-embed
/// next restart.  Applies eviction before writing to mirror the in-process
/// `shrink_hashes_if_needed` call that already ran in `apply_successful_commit`.
/// Test: `persist_batch_writes_to_store` below.
pub(super) async fn persist_batch(
    handle: &IndexHandle,
    new_hashes: &[(PathBuf, String)],
    max_entries: usize,
    current_map_len: usize,
) {
    if new_hashes.is_empty() {
        return;
    }
    // Skip persistence when the map is over-cap — the in-process eviction
    // already fired; the redb table will catch up on the next full persist.
    // (The in-process `shrink_hashes_if_needed` is called BEFORE this function
    // so `current_map_len` already reflects the post-eviction size.)
    if current_map_len > max_entries {
        tracing::debug!(
            "reindex: skipping hash persistence — cache over cap ({} > {})",
            current_map_len,
            max_entries
        );
        return;
    }
    let corpus = {
        let indexer = handle.indexer.read().await;
        indexer.corpus_store()
    };
    let Some(corpus) = corpus else {
        return;
    };
    // Build owned pairs for the blocking closure.
    let pairs: Vec<(String, String)> = new_hashes
        .iter()
        .map(|(p, h)| {
            let rel = p.to_string_lossy().into_owned();
            (rel, h.clone())
        })
        .collect();
    let result = tokio::task::spawn_blocking(move || {
        let refs: Vec<(&str, &str)> = pairs
            .iter()
            .map(|(p, h)| (p.as_str(), h.as_str()))
            .collect();
        corpus.upsert_file_hashes(&refs)
    })
    .await;
    match result {
        Ok(Ok(())) => {}
        Ok(Err(e)) => {
            tracing::warn!("reindex: could not persist file hashes to redb ({e})");
        }
        Err(e) => {
            tracing::warn!("reindex: file-hash persist task panicked ({e})");
        }
    }
}

/// Clear the persisted file-hash table from the current corpus store (issue #662).
///
/// Why: called when `force=true` or a root move is detected.  The in-process
/// DashMap is cleared by the caller; this mirrors that clear to redb so a
/// subsequent daemon restart doesn't reload stale hashes that were intentionally
/// invalidated.
/// What: grabs the corpus store and calls `clear_file_hashes` on a blocking
/// worker.  Errors are logged at `warn` and ignored (same reasoning as
/// `persist_batch`).
/// Test: `clear_persisted_hashes_empties_store` below.
pub(super) async fn clear_persisted(handle: &IndexHandle) {
    let corpus = {
        let indexer = handle.indexer.read().await;
        indexer.corpus_store()
    };
    let Some(corpus) = corpus else {
        return;
    };
    let result = tokio::task::spawn_blocking(move || corpus.clear_file_hashes()).await;
    match result {
        Ok(Ok(())) => {
            tracing::debug!("reindex: cleared persisted file hashes from redb");
        }
        Ok(Err(e)) => {
            tracing::warn!("reindex: could not clear persisted file hashes ({e})");
        }
        Err(e) => {
            tracing::warn!("reindex: file-hash clear task panicked ({e})");
        }
    }
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::corpus::CorpusStore;
    use crate::core::indexer::CodeIndexer;
    use crate::core::registry::{IndexHandle, IndexId, IndexStages};
    use std::sync::Arc;
    use tokio::sync::RwLock;

    /// Build a minimal `IndexHandle` backed by a real redb corpus so the
    /// persist/load helpers have something to write to.
    ///
    /// Why: unit tests need a handle with a durable corpus without spinning up
    /// a full daemon or embedder.
    fn make_handle_with_corpus(dir: &tempfile::TempDir) -> IndexHandle {
        let root = dir.path().to_path_buf();
        let db_path = root.join("index.redb");
        let corpus = Arc::new(CorpusStore::open(&db_path).expect("open test corpus"));
        let mut indexer = CodeIndexer::new("hash-cache-test", root.clone());
        indexer.set_corpus_store(corpus);
        IndexHandle {
            id: IndexId::new("hash-cache-test"),
            indexer: Arc::new(RwLock::new(indexer)),
            root_path: root,
            include_paths: vec![],
            exclude_globs: vec![],
            extensions: vec![],
            domain_terms: vec![],
            include_docs: false,
            respect_gitignore: true,
            path_filter: vec![],
            context_embedding: Arc::new(RwLock::new(None)),
            context_summary: Arc::new(RwLock::new(None)),
            indexed_head_sha: Arc::new(RwLock::new(None)),
            lexical_only: false,
            skip_kg: false,
            stages: Arc::new(RwLock::new(IndexStages::default())),
            search_pressure: Arc::new(tokio::sync::Notify::new()),
            walk_diagnostics: Arc::new(RwLock::new(
                crate::core::registry::WalkDiagnostics::default(),
            )),
        }
    }

    /// Why: `load_into_cache` must populate the in-process map from the redb
    /// store written by a previous run.
    /// Test: this test.
    #[tokio::test]
    async fn load_into_cache_populates_map() {
        let dir = tempfile::tempdir().unwrap();
        let handle = make_handle_with_corpus(&dir);

        // Pre-populate the redb store directly.
        {
            let indexer = handle.indexer.read().await;
            let corpus = indexer.corpus_store().unwrap();
            corpus
                .upsert_file_hashes(&[("src/a.rs", "aaa"), ("src/b.rs", "bbb")])
                .unwrap();
        }

        // Load into a fresh map; count must equal the number of entries written.
        let map: Arc<DashMap<PathBuf, String>> = Arc::new(DashMap::new());
        let count = load_into_cache(&handle, &map).await;

        assert_eq!(
            count, 2,
            "load_into_cache must return the number of hashes loaded"
        );
        assert_eq!(map.len(), 2);
        assert_eq!(
            map.get(&PathBuf::from("src/a.rs"))
                .map(|v| v.clone())
                .unwrap(),
            "aaa"
        );
        assert_eq!(
            map.get(&PathBuf::from("src/b.rs"))
                .map(|v| v.clone())
                .unwrap(),
            "bbb"
        );
    }

    /// Why: `persist_batch` must write new hashes to the corpus store so
    /// `load_file_hashes` can retrieve them on the next run.
    /// Test: this test.
    #[tokio::test]
    async fn persist_batch_writes_to_store() {
        let dir = tempfile::tempdir().unwrap();
        let handle = make_handle_with_corpus(&dir);

        let new_hashes = vec![
            (PathBuf::from("src/a.rs"), "aaa".to_string()),
            (PathBuf::from("src/b.rs"), "bbb".to_string()),
        ];
        // 2 entries, cap = 200_000, map_len = 2 → well within cap.
        persist_batch(&handle, &new_hashes, 200_000, 2).await;

        // Read back from redb.
        let corpus = handle.indexer.read().await.corpus_store().unwrap();
        let mut loaded = corpus.load_file_hashes().unwrap();
        loaded.sort_by(|x, y| x.0.cmp(&y.0));
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0], ("src/a.rs".to_string(), "aaa".to_string()));
    }

    /// Why: `clear_persisted` must empty the redb hash table so a restarted
    /// daemon doesn't reload stale hashes after a force reindex.
    /// Test: this test.
    #[tokio::test]
    async fn clear_persisted_hashes_empties_store() {
        let dir = tempfile::tempdir().unwrap();
        let handle = make_handle_with_corpus(&dir);

        // Write some hashes.
        {
            let indexer = handle.indexer.read().await;
            let corpus = indexer.corpus_store().unwrap();
            corpus.upsert_file_hashes(&[("src/a.rs", "aaa")]).unwrap();
        }

        // Clear them.
        clear_persisted(&handle).await;

        // Table must be empty.
        let corpus = handle.indexer.read().await.corpus_store().unwrap();
        assert!(corpus.load_file_hashes().unwrap().is_empty());
    }

    /// Why: `persist_batch` must be a no-op when the map is over-cap so we
    /// don't write unbounded data to redb.
    /// Test: this test.
    #[tokio::test]
    async fn persist_batch_skips_when_over_cap() {
        let dir = tempfile::tempdir().unwrap();
        let handle = make_handle_with_corpus(&dir);

        let new_hashes = vec![(PathBuf::from("src/a.rs"), "aaa".to_string())];
        // max_entries = 5, current_map_len = 6 → over cap, must skip.
        persist_batch(&handle, &new_hashes, 5, 6).await;

        let corpus = handle.indexer.read().await.corpus_store().unwrap();
        assert!(
            corpus.load_file_hashes().unwrap().is_empty(),
            "over-cap persist must not write anything"
        );
    }

    /// Why: `load_into_cache` on an index with no corpus must be a silent no-op,
    /// not a panic.
    /// Test: this test.
    #[tokio::test]
    async fn load_into_cache_no_corpus_is_noop() {
        let indexer = CodeIndexer::new("no-corpus", "/tmp/no-corpus");
        let handle = IndexHandle::bare(
            IndexId::new("no-corpus"),
            Arc::new(RwLock::new(indexer)),
            PathBuf::from("/tmp/no-corpus"),
        );
        let map: Arc<DashMap<PathBuf, String>> = Arc::new(DashMap::new());
        // Must not panic; must return 0 with no corpus.
        let count = load_into_cache(&handle, &map).await;
        assert_eq!(count, 0);
        assert!(map.is_empty());
    }

    // ── Issue #840 regression ─────────────────────────────────────────────────

    /// Why: guards the post-restart warm-skip regression (#840).  Before the
    /// fix, `build_indexer_from_entry` failed to open the redb corpus on
    /// warm-boot, so `load_into_cache` returned 0 and every post-restart
    /// reindex cold-started (Skipped 0).
    ///
    /// This test simulates the full cycle:
    ///   1. Build indexer + persist file hashes to redb.
    ///   2. Drop the corpus handle (release redb file lock — simulates daemon
    ///      shutdown / `Drop` at the end of the previous process).
    ///   3. Reopen the corpus via a new `CorpusStore::open` call (warm-boot).
    ///   4. Load hashes into a fresh map and assert count > 0.
    ///
    /// What: uses `CorpusStore::open` directly rather than going through
    /// `build_indexer_from_entry` so the test stays focused on the hash-cache
    /// layer.  The companion test
    /// `colocated_create_handler_path_survives_simulated_reload` in
    /// `persistence_loader.rs` exercises the full `build_indexer_from_entry`
    /// warm-boot path.
    ///
    /// Test: this IS the test.
    #[tokio::test]
    async fn warm_boot_hash_load_after_simulated_restart() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("index.redb");

        // --- Phase 1: first daemon lifetime — persist hashes ---
        {
            let handle = make_handle_with_corpus(&dir);
            let new_hashes = vec![
                (PathBuf::from("src/a.rs"), "aaa111".to_string()),
                (PathBuf::from("src/b.rs"), "bbb222".to_string()),
                (PathBuf::from("src/c.rs"), "ccc333".to_string()),
            ];
            // Persist the hashes as if a reindex committed them.
            persist_batch(&handle, &new_hashes, 200_000, 3).await;
            // handle (and its corpus Arc) is dropped here — redb file lock released.
        }

        // --- Phase 2: simulated restart — reopen corpus (warm-boot path) ---
        let corpus = CorpusStore::open(&db_path).expect("#840: reopen must succeed after drop");
        let mut indexer = CodeIndexer::new("840-test", dir.path());
        indexer.set_corpus_store(Arc::new(corpus));
        let handle = IndexHandle {
            id: IndexId::new("840-test"),
            indexer: Arc::new(RwLock::new(indexer)),
            root_path: dir.path().to_path_buf(),
            include_paths: vec![],
            exclude_globs: vec![],
            extensions: vec![],
            domain_terms: vec![],
            include_docs: false,
            respect_gitignore: true,
            path_filter: vec![],
            context_embedding: Arc::new(RwLock::new(None)),
            context_summary: Arc::new(RwLock::new(None)),
            indexed_head_sha: Arc::new(RwLock::new(None)),
            lexical_only: false,
            skip_kg: false,
            stages: Arc::new(RwLock::new(IndexStages::default())),
            search_pressure: Arc::new(tokio::sync::Notify::new()),
            walk_diagnostics: Arc::new(RwLock::new(
                crate::core::registry::WalkDiagnostics::default(),
            )),
        };

        // Load hashes into a fresh map — this is what `spawn_reindex` does.
        let map: Arc<DashMap<PathBuf, String>> = Arc::new(DashMap::new());
        let count = load_into_cache(&handle, &map).await;

        // Post-restart the map must be primed so unchanged files are skipped.
        assert_eq!(
            count, 3,
            "#840: warm-boot must load all 3 persisted hashes; got {count}"
        );
        assert_eq!(
            map.get(&PathBuf::from("src/a.rs")).as_deref().cloned(),
            Some("aaa111".to_string()),
            "#840: hash for src/a.rs must match what was persisted"
        );
    }
}
