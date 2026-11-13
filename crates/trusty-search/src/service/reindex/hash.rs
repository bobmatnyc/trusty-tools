//! Per-index content-hash cache for incremental reindex skip.
//!
//! Why: re-embedding an unchanged file is expensive (ONNX model inference).
//! Caching content hashes in memory (and mirroring them to redb via
//! `hash_cache::persist_batch`) lets subsequent reindexes skip every file
//! whose content did not change since the last run.
//!
//! What: `file_hashes()` is the process-global DashMap of per-index hash caches.
//! `hash_content` produces a stable SHA-256 fingerprint. `shrink_hashes_if_needed`
//! keeps the cache bounded.
//!
//! Test: covered indirectly by `reindex_walks_directory_and_emits_events` (the
//! second reindex run on an unchanged workspace must skip all files).

use crate::core::registry::IndexId;
use dashmap::DashMap;
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

/// Per-index ceiling on the content-hash cache (issue #75). Each entry holds
/// a `PathBuf` + 64-char hex SHA-256 string, so 200k entries ≈ ~30–60 MB.
/// When exceeded we drain ~10% of the entries (DashMap has no ordering, so
/// the eviction set is arbitrary — those files are simply re-hashed on the
/// next reindex, which is the safe, correct fallback).
pub(super) const MAX_FILE_HASHES_PER_INDEX: usize = 200_000;

/// Per-index, per-process content-hash cache. Used to skip reindexing files
/// whose content hasn't changed since the last reindex in this daemon's
/// lifetime.
///
/// Why: survives across `POST /indexes/:id/reindex` calls but not daemon
/// restarts (acceptable: cold start re-embeds everything anyway, and on warm
/// daemons the user expects "skip unchanged" behaviour).
/// What: `DashMap<IndexId, Arc<DashMap<PathBuf, String>>>` — one inner map per
/// index, accessed by `hashes_for`.
/// Test: the second pass of `reindex_walks_directory_and_emits_events` must
/// emit only `skip` events (all files hash-matched).
pub(super) fn file_hashes() -> &'static DashMap<IndexId, Arc<DashMap<PathBuf, String>>> {
    static FILE_HASHES: OnceLock<DashMap<IndexId, Arc<DashMap<PathBuf, String>>>> = OnceLock::new();
    FILE_HASHES.get_or_init(DashMap::new)
}

/// Return the per-index hash cache, creating an empty one on first access.
///
/// Why: each index independently tracks its own file hashes so a forced
/// reindex of index A does not clear the cache for index B.
/// What: `entry(id).or_insert_with(...)` on the global `DashMap`.
/// Test: covered indirectly by all reindex tests.
pub(crate) fn hashes_for(id: &IndexId) -> Arc<DashMap<PathBuf, String>> {
    file_hashes()
        .entry(id.clone())
        .or_insert_with(|| Arc::new(DashMap::new()))
        .clone()
}

/// Drop ~10% of entries from `map` when above `MAX_FILE_HASHES_PER_INDEX`.
///
/// Why: prevents an unbounded growth in the per-daemon content-hash cache
/// when a project gets ever-larger or files are renamed many times. The
/// hash cache is a pure speed optimisation (skip re-embed for unchanged
/// files), so evicting entries is always safe — affected files just get
/// re-hashed and re-embedded on the next reindex.
/// What: collects an arbitrary subset of keys and removes them. DashMap has
/// no insertion-order metadata so we can't do "true" LRU; arbitrary eviction
/// is acceptable for a cache whose miss penalty is just extra work.
/// Test: covered indirectly by the reindex test (oversizing not exercised).
pub(super) fn shrink_hashes_if_needed(map: &DashMap<PathBuf, String>) {
    let len = map.len();
    if len <= MAX_FILE_HASHES_PER_INDEX {
        return;
    }
    let target = MAX_FILE_HASHES_PER_INDEX * 9 / 10;
    let to_remove = len.saturating_sub(target);
    let keys: Vec<PathBuf> = map
        .iter()
        .take(to_remove)
        .map(|e| e.key().clone())
        .collect();
    for k in keys {
        map.remove(&k);
    }
    tracing::info!(
        "file-hash cache exceeded {} entries — dropped {} to bound memory",
        MAX_FILE_HASHES_PER_INDEX,
        to_remove
    );
}

/// Stable content fingerprint for the "skip unchanged file" optimization.
///
/// Why: SHA-256 is collision-resistant and stable across processes, builds,
/// and Rust versions. `DefaultHasher` (SipHash) is randomized per build and
/// has weaker collision properties — fine for `HashMap` keys but unsafe for
/// content fingerprinting where a false negative silently skips a real edit.
/// What: SHA-256 of the file's UTF-8 bytes, hex-encoded.
/// Test: see `reindex_walks_directory_and_emits_events` — a re-run of the
/// reindex with unchanged files must mark them as skipped (proves the hash
/// is stable across two invocations within the same process).
pub(crate) fn hash_content(content: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// #9236: test-only fault seam. For each index id listed here, the durable
/// hash-row delete in [`forget_file_hash`] fails after the cache entry is
/// dropped — the state a refused redb write leaves. #11096: the one-transaction
/// file delete fails before its commit instead. Keyed by index id so
/// concurrent tests never see each other's faults.
#[cfg(test)]
pub(crate) static TEST_FAIL_FORGET_HASH: std::sync::Mutex<Vec<String>> =
    std::sync::Mutex::new(Vec::new());

/// Drop a file's content hash from the in-process cache and the durable corpus.
///
/// Why (#8922): a purge that leaves the hash behind makes the next reindex or
/// rescan hash-skip the file when it is re-admitted unchanged, so it stays
/// unindexed with nothing to recover it.
/// What: removes `rel` from [`hashes_for`] and, when the indexer holds a durable
/// corpus, deletes the persisted row on a blocking worker. A failed delete is an
/// error: the caller reports the purge as incomplete.
/// Test: `forget_file_hash_drops_the_cached_and_persisted_hash`.
pub(crate) async fn forget_file_hash(
    index_id: &IndexId,
    indexer: &crate::core::CodeIndexer,
    rel: &str,
) -> anyhow::Result<()> {
    hashes_for(index_id).remove(std::path::Path::new(rel));
    let Some(corpus) = indexer.corpus_store() else {
        return Ok(());
    };
    #[cfg(test)]
    if TEST_FAIL_FORGET_HASH
        .lock()
        .is_ok_and(|f| f.contains(&index_id.0))
    {
        anyhow::bail!("injected file-hash delete failure (#9236 test seam)");
    }
    let rows = vec![rel.to_string()];
    tokio::task::spawn_blocking(move || corpus.delete_file_hash_entries(&rows))
        .await
        .map_err(|e| anyhow::anyhow!("file-hash delete task did not complete: {e}"))?
}

/// Record a file's content hash in the in-process cache and the durable corpus.
///
/// Why (#10738): `search.index.file.status` reports the hash of the content a
/// pushed file's chunks came from, so `search.index.file.put` must record it
/// the way a reindex batch does.
/// What: inserts `rel → hash` into [`hashes_for`] and, when the indexer holds a
/// durable corpus, upserts the row on a blocking worker. A failed upsert is an
/// error.
/// Test: `file_status_reports_the_put_hash_then_absent_after_remove`.
pub(crate) async fn record_file_hash(
    index_id: &IndexId,
    indexer: &crate::core::CodeIndexer,
    rel: &str,
    hash: &str,
) -> anyhow::Result<()> {
    hashes_for(index_id).insert(PathBuf::from(rel), hash.to_string());
    let Some(corpus) = indexer.corpus_store() else {
        return Ok(());
    };
    let (rel, hash) = (rel.to_string(), hash.to_string());
    tokio::task::spawn_blocking(move || corpus.upsert_file_hashes(&[(&rel, &hash)]))
        .await
        .map_err(|e| anyhow::anyhow!("file-hash upsert task did not complete: {e}"))?
}

impl crate::core::CodeIndexer {
    /// Purge one file for #8922: its chunks without a symbol-graph rebuild,
    /// and its content hash. Returns the chunks removed; the caller rebuilds
    /// the graph once, and only when something was removed. Stamps the
    /// durable graph-dirty mark first (#8959).
    ///
    /// Fail-closed (#8959, #9230): the chunk rows and the hash row leave in
    /// one redb transaction (#11096), so a failed delete keeps both, and the
    /// ids, for a retry.
    ///
    /// A method, not a free function, so `scripts/check_teardown_guard.sh`
    /// sees every `.purge_file_with(` call site: both writes are durable and
    /// each caller must hold the teardown guard (#3049).
    pub(crate) async fn purge_file_with(
        &self,
        index_id: &IndexId,
        rel: &str,
    ) -> anyhow::Result<usize> {
        // #8959: durable stale mark before anything leaves, so a crash before
        // the caller's per-pass rebuild still rebuilds at the next boot.
        let _graph_write = self.begin_graph_write_for_removal(rel).await?;
        let ids = self.file_chunk_ids(rel).await?;
        // #11096: chunks and hash in one commit, never two.
        self.remove_file_and_hash(index_id, rel, &ids).await?;
        Ok(ids.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #8922: both copies of the hash go, so a re-admitted file is re-read.
    /// Fails with either removal in `forget_file_hash` deleted.
    #[tokio::test]
    async fn forget_file_hash_drops_the_cached_and_persisted_hash() {
        let dir = tempfile::tempdir().unwrap();
        let corpus = crate::core::corpus::CorpusStore::open(&dir.path().join("i.redb")).unwrap();
        corpus
            .upsert_file_hashes(&[("secrets/prod.yaml", "aa"), ("src/lib.rs", "bb")])
            .unwrap();
        let mut indexer = crate::core::CodeIndexer::new("hash-forget-8922", dir.path());
        indexer.set_corpus_store(Arc::new(corpus));
        let id = IndexId::new("hash-forget-8922");
        hashes_for(&id).insert(PathBuf::from("secrets/prod.yaml"), "aa".into());

        forget_file_hash(&id, &indexer, "secrets/prod.yaml")
            .await
            .unwrap();

        assert!(hashes_for(&id)
            .get(&PathBuf::from("secrets/prod.yaml"))
            .is_none());
        let persisted = indexer.corpus_store().unwrap().load_file_hashes().unwrap();
        assert_eq!(
            persisted,
            vec![("src/lib.rs".to_string(), "bb".to_string())]
        );
    }
}
