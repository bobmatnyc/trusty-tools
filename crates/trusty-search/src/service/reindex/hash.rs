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

use crate::core::indexer::RedbChunkDelete;
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
    let rows = vec![rel.to_string()];
    tokio::task::spawn_blocking(move || corpus.delete_file_hash_entries(&rows))
        .await
        .map_err(|e| anyhow::anyhow!("file-hash delete task did not complete: {e}"))?
}

impl crate::core::CodeIndexer {
    /// Purge one file for #8922: its chunks without a symbol-graph rebuild,
    /// then its content hash. Returns the chunks removed; the caller rebuilds
    /// the graph once, and only when something was removed. Stamps the
    /// durable graph-dirty mark first (#8959).
    ///
    /// A method, not a free function, so `scripts/check_teardown_guard.sh`
    /// sees every `.purge_file(` call site: both writes are durable and each
    /// caller must hold the teardown guard (#3049).
    pub(crate) async fn purge_file(&self, index_id: &IndexId, rel: &str) -> anyhow::Result<usize> {
        self.purge_file_with(index_id, rel, RedbChunkDelete::WarnOnly)
            .await
    }

    /// [`Self::purge_file`] with the redb chunk-delete failure mode chosen by
    /// the caller; `index_file`'s sops arm uses `FailClosed` (#8959), so a
    /// failed delete keeps the plaintext ids and the hash for a retry.
    pub(crate) async fn purge_file_with(
        &self,
        index_id: &IndexId,
        rel: &str,
        mode: RedbChunkDelete,
    ) -> anyhow::Result<usize> {
        // #8959: durable stale mark before anything leaves, so a crash before
        // the caller's per-pass rebuild still rebuilds at the next boot.
        let _graph_write = self.begin_graph_write_for_removal(rel).await?;
        let removed = self.remove_file_no_kg_rebuild_with(rel, mode).await?;
        forget_file_hash(index_id, self, rel).await?;
        Ok(removed)
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
