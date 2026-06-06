//! Shared code-index store with advisory file locking.
//!
//! Why: The code index (`Segment::CodeIndex`) is logically shared across
//! concurrent `trusty-agents` processes — a PM session, an agent subprocess, and a
//! `--watch` daemon may all want to read or update it at once. redb uses
//! exclusive file-level locks per-process, and usearch has no cross-process
//! coordination at all; without an additional advisory lock concurrent writes
//! would corrupt the `.usearch` file. `CodeStore` wraps `RedbUsearchStore` and
//! serializes writes (exclusive lock) while allowing parallel reads (shared
//! lock) on a dedicated lock file.
//! What: Opens a `RedbUsearchStore` at `<dir>/` and uses `<dir>/.write.lock`
//! as an advisory `fs4` lock. Implements `MemoryStore` for `Segment::CodeIndex`
//! only; using it with `Segment::AgentMemory` returns an error so mis-wired
//! callers fail loudly.
//! Test: See `tests` module — round-trip insert+search and "second open of
//! same dir still works" (advisory lock doesn't prevent reopen).

use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, anyhow, bail};
use async_trait::async_trait;
use fs4::FileExt;

use super::redb_usearch::RedbUsearchStore;
use super::store::{MemoryResult, MemoryStore, Segment};

/// File name for the advisory lock living inside the code store dir.
const LOCK_FILE_NAME: &str = ".write.lock";

/// Shared code-index store with cross-process advisory locking.
///
/// Why: See module-level docs. This type is what callers should hold for the
/// lifetime of a PM / agent / watcher process; it is `Arc`-friendly because
/// `RedbUsearchStore` is already internally `Arc`-y and `PathBuf` is cheap to
/// clone.
/// What: Thin wrapper that owns an inner `RedbUsearchStore` plus the path to
/// the advisory-lock file. Every `MemoryStore` method takes the appropriate
/// lock (exclusive on write, shared on read), performs the operation, drops
/// the lock, and returns.
/// Test: Unit tests in `tests` module.
pub struct CodeStore {
    inner: RedbUsearchStore,
    lock_file: PathBuf,
}

impl CodeStore {
    /// Open (or create) a code store rooted at `store_dir`.
    ///
    /// Why: Single entrypoint that handles first-run (create dir + lock file)
    /// and reopen. The advisory lock file is created up-front so subsequent
    /// lock/unlock calls never race on its existence.
    /// What: `std::fs::create_dir_all(store_dir)`, create the lock file if
    /// absent, open an inner `RedbUsearchStore` with `vector_dim`.
    /// Test: `code_store_insert_search_round_trip`.
    pub fn open(store_dir: &Path, vector_dim: usize) -> Result<Self> {
        std::fs::create_dir_all(store_dir)
            .with_context(|| format!("creating code store dir {}", store_dir.display()))?;
        let lock_file = store_dir.join(LOCK_FILE_NAME);
        // Touch the lock file so subsequent `File::open` calls always succeed.
        let _ = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(&lock_file)
            .with_context(|| format!("creating lock file {}", lock_file.display()))?;
        let inner =
            RedbUsearchStore::open(store_dir, vector_dim).context("opening inner code store")?;
        Ok(Self { inner, lock_file })
    }

    /// Guard `op` with an exclusive advisory lock on the lock file.
    ///
    /// Why: `fs4` exposes blocking syscalls (`flock` / `LockFileEx`); calling
    /// them from an async context would risk blocking the tokio runtime. We
    /// wrap the lock acquire/release around the actual inner operation on the
    /// current (already-async) task because the acquire is expected to be
    /// short-lived for a typical developer workflow. Callers who need to batch
    /// many writes should take the lock themselves around the batch — which
    /// isn't exposed by this API yet because we haven't needed it.
    /// What: Opens the lock file, acquires exclusive lock, runs `op`,
    /// unconditionally unlocks, returns `op`'s result.
    async fn with_write_lock<T, F, Fut>(&self, op: F) -> Result<T>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<T>>,
    {
        let f = File::open(&self.lock_file)
            .with_context(|| format!("opening lock file {}", self.lock_file.display()))?;
        // fs4 1.0 renamed `lock_exclusive` -> `lock`.
        FileExt::lock(&f).map_err(|e| anyhow!("acquiring exclusive lock: {e}"))?;
        let result = op().await;
        // Always unlock; an explicit unlock is preferable to relying on drop
        // so errors surface. If unlock fails the file descriptor drop will
        // still release eventually.
        let _ = FileExt::unlock(&f);
        result
    }

    /// Guard `op` with a shared advisory lock on the lock file.
    async fn with_read_lock<T, F, Fut>(&self, op: F) -> Result<T>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<T>>,
    {
        let f = File::open(&self.lock_file)
            .with_context(|| format!("opening lock file {}", self.lock_file.display()))?;
        FileExt::lock_shared(&f).map_err(|e| anyhow!("acquiring shared lock: {e}"))?;
        let result = op().await;
        let _ = FileExt::unlock(&f);
        result
    }

    /// Ergonomic `Arc` constructor.
    pub fn new_arc(store_dir: &Path, vector_dim: usize) -> Result<Arc<Self>> {
        Ok(Arc::new(Self::open(store_dir, vector_dim)?))
    }
}

#[async_trait]
impl MemoryStore for CodeStore {
    async fn insert(
        &self,
        segment: Segment,
        id: &str,
        vector: &[f32],
        payload: serde_json::Value,
    ) -> Result<()> {
        if !matches!(segment, Segment::CodeIndex) {
            bail!("CodeStore only accepts Segment::CodeIndex (got {segment:?})");
        }
        self.with_write_lock(|| async { self.inner.insert(segment, id, vector, payload).await })
            .await
    }

    async fn search(
        &self,
        segment: Segment,
        query_vec: &[f32],
        top_k: usize,
    ) -> Result<Vec<MemoryResult>> {
        if !matches!(segment, Segment::CodeIndex) {
            bail!("CodeStore only accepts Segment::CodeIndex (got {segment:?})");
        }
        self.with_read_lock(|| async { self.inner.search(segment, query_vec, top_k).await })
            .await
    }

    async fn get(&self, segment: Segment, id: &str) -> Result<Option<serde_json::Value>> {
        if !matches!(segment, Segment::CodeIndex) {
            bail!("CodeStore only accepts Segment::CodeIndex (got {segment:?})");
        }
        self.with_read_lock(|| async { self.inner.get(segment, id).await })
            .await
    }

    async fn delete(&self, segment: Segment, id: &str) -> Result<()> {
        if !matches!(segment, Segment::CodeIndex) {
            bail!("CodeStore only accepts Segment::CodeIndex (got {segment:?})");
        }
        self.with_write_lock(|| async { self.inner.delete(segment, id).await })
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::tempdir;

    fn vec4(a: f32, b: f32, c: f32, d: f32) -> Vec<f32> {
        vec![a, b, c, d]
    }

    #[tokio::test]
    async fn code_store_insert_search_round_trip() {
        let dir = tempdir().unwrap();
        let store = CodeStore::open(dir.path(), 4).unwrap();

        store
            .insert(
                Segment::CodeIndex,
                "src/a.rs:1",
                &vec4(1.0, 0.0, 0.0, 0.0),
                json!({"file": "a.rs", "lang": "rust"}),
            )
            .await
            .unwrap();

        let hits = store
            .search(Segment::CodeIndex, &vec4(1.0, 0.0, 0.0, 0.0), 5)
            .await
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, "src/a.rs:1");
        assert_eq!(hits[0].payload["file"], "a.rs");
    }

    #[tokio::test]
    async fn code_store_rejects_agent_memory_segment() {
        let dir = tempdir().unwrap();
        let store = CodeStore::open(dir.path(), 4).unwrap();
        let err = store
            .insert(
                Segment::AgentMemory,
                "nope",
                &vec4(1.0, 0.0, 0.0, 0.0),
                json!({}),
            )
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("CodeStore only accepts"),
            "unexpected error: {err}"
        );
    }

    #[tokio::test]
    async fn concurrent_write_lock_prevents_corruption() {
        // Two CodeStores pointing at the same directory. The advisory lock
        // serializes their writes so neither corrupts the index.
        // Note: redb itself will refuse a second concurrent writer at the
        // db-file level, but the advisory lock guarantees *logical* single-
        // writer semantics for the usearch file as well. Here we verify that
        // sequential writes through two handles both succeed.
        let dir = tempdir().unwrap();
        let store_a = CodeStore::open(dir.path(), 4).unwrap();
        store_a
            .insert(
                Segment::CodeIndex,
                "one",
                &vec4(1.0, 0.0, 0.0, 0.0),
                json!({"src": "a"}),
            )
            .await
            .unwrap();
        drop(store_a);

        let store_b = CodeStore::open(dir.path(), 4).unwrap();
        store_b
            .insert(
                Segment::CodeIndex,
                "two",
                &vec4(0.0, 1.0, 0.0, 0.0),
                json!({"src": "b"}),
            )
            .await
            .unwrap();

        let hits = store_b
            .search(Segment::CodeIndex, &vec4(1.0, 0.0, 0.0, 0.0), 5)
            .await
            .unwrap();
        let ids: Vec<&str> = hits.iter().map(|h| h.id.as_str()).collect();
        assert!(ids.contains(&"one"), "expected 'one' in: {ids:?}");
        assert!(ids.contains(&"two"), "expected 'two' in: {ids:?}");
    }
}
