//! Filesystem watcher that keeps the code index fresh on change/delete.
//!
//! Why: Running `--reindex` every time a file changes is wasteful; a
//! background watcher with short debouncing lets the index track the
//! working tree in near real time without the latency of a full walk.
//! What: [`FileWatcher`] wraps a [`CodeIndexer`] and a root path; `watch()`
//! sets up a `notify-debouncer-mini` recommended watcher, bridges its
//! synchronous events into an async tokio mpsc channel, and re-indexes
//! (or removes) each changed file as events arrive. `reindex_all()`
//! performs a full directory walk on demand.
//! Test: See `#[cfg(test)]` at bottom — `reindex_all_calls_index_directory`,
//! `remove_file_deletes_manifest_and_chunks`, `reindex_updates_existing_file`.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use notify::RecursiveMode;
use notify_debouncer_mini::{DebounceEventResult, new_debouncer};
use tokio::sync::mpsc;

use crate::search::indexer::CodeIndexer;

/// Default debounce window for filesystem events, in milliseconds.
///
/// Why: Editors often emit a flurry of events per save (atomic rename,
/// metadata update, final rename). 500ms is long enough to coalesce them
/// and short enough to feel responsive.
const DEFAULT_DEBOUNCE_MS: u64 = 500;

/// Watches a directory tree and keeps a [`CodeIndexer`] in sync with it.
///
/// Why: Semantic search is only useful if it reflects current source; a
/// watcher removes the "did you remember to reindex?" failure mode.
/// What: Holds the indexer, the root, the set of extensions to care about,
/// and the debounce interval. Construct via [`FileWatcher::new`] and run
/// [`FileWatcher::watch`] (blocking) or [`FileWatcher::reindex_all`] (one-shot).
/// Test: See bottom-of-file unit tests.
pub struct FileWatcher {
    indexer: Arc<CodeIndexer>,
    root: PathBuf,
    extensions: Vec<String>,
    debounce_ms: u64,
}

impl FileWatcher {
    /// Construct a new `FileWatcher` with the default 500ms debounce.
    ///
    /// Why: Most callers want the default debounce; exposing it as a
    /// constant keeps construction trivial.
    /// What: Stores `indexer`, `root`, `extensions`; sets `debounce_ms` to
    /// [`DEFAULT_DEBOUNCE_MS`]. Returns `Self`.
    /// Test: Implicit via `reindex_all_calls_index_directory`.
    pub fn new(indexer: Arc<CodeIndexer>, root: PathBuf, extensions: Vec<String>) -> Self {
        Self {
            indexer,
            root,
            extensions,
            debounce_ms: DEFAULT_DEBOUNCE_MS,
        }
    }

    /// Override the debounce window (mostly for tests).
    ///
    /// Why: 500ms is a reasonable production default but too slow for
    /// tight unit tests; this builder method lets tests shrink it.
    pub fn with_debounce_ms(mut self, ms: u64) -> Self {
        self.debounce_ms = ms;
        self
    }

    /// Borrow the wrapped `CodeIndexer`.
    ///
    /// Why: Callers (e.g., `main`) need to call `warm_up()` and
    /// `spawn_cool_down_monitor()` on the same indexer the watcher mutates,
    /// so they don't end up with two indexers fighting over the same
    /// on-disk store. Exposing the existing `Arc` keeps them coordinated.
    /// What: Returns a clone of the `Arc<CodeIndexer>`.
    /// Test: Indirect — `--watch` startup uses this to warm the index.
    pub fn indexer(&self) -> Arc<CodeIndexer> {
        Arc::clone(&self.indexer)
    }

    /// One-shot full re-index of [`Self::root`].
    ///
    /// Why: On startup (and when the user passes `--reindex`) we want a
    /// clean rebuild of the index without waiting for filesystem events.
    /// What: Calls [`CodeIndexer::index_directory`] with the configured
    /// extensions, logs start+finish, and returns the total chunk count.
    /// Test: `reindex_all_calls_index_directory`.
    pub async fn reindex_all(&self) -> Result<usize> {
        tracing::info!(root = %self.root.display(), "Starting re-index of {}...", self.root.display());
        let ext_refs: Vec<&str> = self.extensions.iter().map(|s| s.as_str()).collect();
        let count = self
            .indexer
            .index_directory(&self.root, &ext_refs)
            .await
            .context("reindex_all: index_directory failed")?;
        tracing::info!(count, "Indexed {} chunks.", count);
        Ok(count)
    }

    /// Run the filesystem watcher loop until the process exits.
    ///
    /// Why: Keeps the index up to date in the background with minimal
    /// code-path overhead (only changed files are touched). Debouncing is
    /// delegated to `notify-debouncer-mini` so rapid successive writes
    /// coalesce into a single re-index.
    /// What: Spins up a debouncer watching `self.root` recursively, bridges
    /// its sync callback to an async `mpsc::unbounded_channel`, then loops
    /// receiving debounced events. Each event whose extension is in
    /// `self.extensions` is routed to `index_file` (if the path exists) or
    /// `remove_file` (if it was deleted). Errors are logged and skipped.
    /// This function returns `Ok(())` only if the channel closes
    /// (watcher dropped) — in normal operation it blocks forever.
    /// Test: Not unit-tested directly (infinite loop); exercised by
    /// integration testing or manual `--watch` runs.
    pub async fn watch(&self) -> Result<()> {
        tracing::info!(root = %self.root.display(), debounce_ms = self.debounce_ms, "starting file watcher");

        // Bridge sync notify callback to async land via an mpsc channel.
        let (tx, mut rx) = mpsc::unbounded_channel::<DebounceEventResult>();
        let mut debouncer = new_debouncer(
            Duration::from_millis(self.debounce_ms),
            move |res: DebounceEventResult| {
                if tx.send(res).is_err() {
                    // Receiver dropped; nothing we can do from the
                    // blocking thread but exit quietly.
                }
            },
        )
        .context("failed to create notify debouncer")?;

        debouncer
            .watcher()
            .watch(&self.root, RecursiveMode::Recursive)
            .with_context(|| format!("failed to watch {}", self.root.display()))?;

        // Hold the debouncer for the lifetime of the loop so it keeps
        // receiving events; dropping it stops the watcher.
        while let Some(events) = rx.recv().await {
            match events {
                Ok(events) => {
                    for ev in events {
                        self.handle_path_event(&ev.path).await;
                    }
                }
                Err(err) => {
                    tracing::warn!(error = %err, "notify watcher error");
                }
            }
        }

        // Channel closed → debouncer was dropped.
        drop(debouncer);
        Ok(())
    }

    /// Route a single debounced path event to index_file or remove_file.
    ///
    /// Why: The debouncer collapses create/modify/delete into "something
    /// happened to this path"; we pick the right action based on whether
    /// the path currently exists and whether its extension is interesting.
    /// What: If the extension isn't in `self.extensions`, does nothing.
    /// If the path exists as a file, calls `index_file`. If it no longer
    /// exists, calls `remove_file`. Errors are logged and swallowed so
    /// one bad file can't kill the watcher.
    async fn handle_path_event(&self, path: &Path) {
        let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
            return;
        };
        if !self.extensions.iter().any(|e| e == ext) {
            return;
        }

        if path.is_file() {
            match self.indexer.index_file(path, None).await {
                Ok(n) => {
                    tracing::info!(file = %path.display(), chunks = n, "re-indexed changed file");
                }
                Err(e) => {
                    tracing::warn!(file = %path.display(), error = %e, "index_file failed");
                }
            }
        } else {
            match self.indexer.remove_file(path).await {
                Ok(0) => {
                    tracing::debug!(file = %path.display(), "delete event for unknown file; nothing to remove");
                }
                Ok(n) => {
                    tracing::info!(file = %path.display(), chunks = n, "removed deleted file from index");
                }
                Err(e) => {
                    tracing::warn!(file = %path.display(), error = %e, "remove_file failed");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::collections::HashMap;
    use std::sync::Mutex;

    use async_trait::async_trait;
    use serde_json::Value;
    use tempfile::{Builder, TempDir};

    fn make_visible_tempdir() -> TempDir {
        // `TempDir::new()` produces `.tmpXXX` which the indexer's walker
        // treats as hidden (dot-prefix skip). Use a visible prefix so
        // walkdir descends into it.
        Builder::new()
            .prefix("watchtest-")
            .tempdir()
            .expect("tempdir")
    }

    use crate::memory::{Embedder, MemoryResult, MemoryStore, Segment};

    // ---------- mocks (mirrored from indexer tests; kept inline to
    //            avoid exposing them as a public test helper) ----------

    struct MockStore {
        inner: Mutex<HashMap<String, (Vec<f32>, Value)>>,
        order: Mutex<Vec<String>>,
    }
    impl MockStore {
        fn new() -> Self {
            Self {
                inner: Mutex::new(HashMap::new()),
                order: Mutex::new(Vec::new()),
            }
        }
        fn len(&self) -> usize {
            self.inner.lock().unwrap().len()
        }
        fn contains(&self, id: &str) -> bool {
            self.inner.lock().unwrap().contains_key(id)
        }
    }
    #[async_trait]
    impl MemoryStore for MockStore {
        async fn insert(
            &self,
            _segment: Segment,
            id: &str,
            vector: &[f32],
            payload: Value,
        ) -> anyhow::Result<()> {
            let mut order = self.order.lock().unwrap();
            if !order.contains(&id.to_string()) {
                order.push(id.to_string());
            }
            self.inner
                .lock()
                .unwrap()
                .insert(id.to_string(), (vector.to_vec(), payload));
            Ok(())
        }
        async fn search(
            &self,
            _segment: Segment,
            _query_vec: &[f32],
            top_k: usize,
        ) -> anyhow::Result<Vec<MemoryResult>> {
            let order = self.order.lock().unwrap().clone();
            let inner = self.inner.lock().unwrap();
            let mut out = Vec::new();
            for (i, id) in order.iter().take(top_k).enumerate() {
                if let Some((_, payload)) = inner.get(id) {
                    out.push(MemoryResult {
                        id: id.clone(),
                        score: 1.0 - (i as f32) * 0.01,
                        payload: payload.clone(),
                        segment: "code".to_string(),
                    });
                }
            }
            Ok(out)
        }
        async fn get(&self, _segment: Segment, id: &str) -> anyhow::Result<Option<Value>> {
            Ok(self.inner.lock().unwrap().get(id).map(|(_, p)| p.clone()))
        }
        async fn delete(&self, _segment: Segment, id: &str) -> anyhow::Result<()> {
            self.inner.lock().unwrap().remove(id);
            self.order.lock().unwrap().retain(|x| x != id);
            Ok(())
        }
    }

    struct MockEmbedder {
        dim: usize,
    }
    impl Embedder for MockEmbedder {
        fn embed(&self, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
            Ok(texts
                .iter()
                .map(|t| vec![t.len() as f32 / 100.0; self.dim])
                .collect())
        }
        fn embed_single(&self, text: &str) -> anyhow::Result<Vec<f32>> {
            Ok(vec![text.len() as f32 / 100.0; self.dim])
        }
        fn dimension(&self) -> usize {
            self.dim
        }
    }

    // ---------- tests ----------

    #[tokio::test]
    async fn reindex_all_calls_index_directory() {
        // Why: ensures `reindex_all` walks the root and writes entries to
        // the store for every matching file.
        let dir = make_visible_tempdir();
        let f1 = dir.path().join("a.rs");
        let f2 = dir.path().join("b.rs");
        tokio::fs::write(&f1, "fn a() {}\n").await.unwrap();
        tokio::fs::write(&f2, "fn b() {}\n").await.unwrap();

        let store = Arc::new(MockStore::new());
        let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder { dim: 8 });
        let indexer = Arc::new(CodeIndexer::new(store.clone(), embedder));

        let watcher = FileWatcher::new(indexer, dir.path().to_path_buf(), vec!["rs".to_string()]);
        let n = watcher.reindex_all().await.expect("reindex_all");
        assert!(n >= 2, "expected at least 2 chunks indexed, got {n}");
        // 2 chunks + 2 manifests at minimum.
        assert!(
            store.len() >= 4,
            "expected store entries, got {}",
            store.len()
        );
    }

    #[tokio::test]
    async fn remove_file_deletes_manifest_and_chunks() {
        // Why: remove_file must evict both chunk rows and the manifest
        // entry so subsequent searches and re-indexes are clean.
        let dir = make_visible_tempdir();
        let f = dir.path().join("c.rs");
        tokio::fs::write(&f, "fn c() {}\n").await.unwrap();

        let store = Arc::new(MockStore::new());
        let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder { dim: 8 });
        let indexer = CodeIndexer::new(store.clone(), embedder);

        let inserted = indexer.index_file(&f, None).await.expect("index_file");
        assert!(inserted >= 1);

        let canonical = f.canonicalize().unwrap();
        let manifest_id = format!("manifest:{}", canonical.display());
        assert!(
            store.contains(&manifest_id),
            "manifest should exist after index"
        );

        // Chunk id is canonical path + start line + end line (1-indexed,
        // #376 A4).
        let chunk_id = format!("{}:1:1", canonical.display());
        assert!(store.contains(&chunk_id), "chunk should exist after index");

        let removed = indexer.remove_file(&f).await.expect("remove_file");
        assert!(removed >= 1, "should have removed at least 1 chunk");
        assert!(
            !store.contains(&manifest_id),
            "manifest should be gone after remove_file"
        );
        assert!(
            !store.contains(&chunk_id),
            "chunk should be gone after remove_file"
        );
    }

    #[tokio::test]
    async fn reindex_updates_existing_file() {
        // Why: re-indexing the same path should replace old chunks, not
        // accumulate them — otherwise stale results survive forever.
        let dir = make_visible_tempdir();
        let f = dir.path().join("d.rs");

        let store = Arc::new(MockStore::new());
        let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder { dim: 8 });
        let indexer = CodeIndexer::new(store.clone(), embedder);

        // First version: one function on line 1.
        tokio::fs::write(&f, "fn first() {}\n").await.unwrap();
        indexer.index_file(&f, None).await.expect("first index");
        let canonical = f.canonicalize().unwrap();
        // Chunk IDs include start_line:end_line after #376 A4. Single-line
        // `fn first() {}` lives at lines 1..=1.
        let id_first = format!("{}:1:1", canonical.display());
        assert!(store.contains(&id_first));

        // Second version: add a second function.  After re-index both
        // current chunks should be present; stale-only ids should not.
        tokio::fs::write(&f, "fn first() {}\n\nfn second() {}\n")
            .await
            .unwrap();
        indexer.index_file(&f, None).await.expect("second index");

        let id_line1 = format!("{}:1:1", canonical.display());
        let id_line3 = format!("{}:3:3", canonical.display());
        assert!(store.contains(&id_line1), "line-1 chunk should exist");
        assert!(store.contains(&id_line3), "line-3 chunk should exist");

        // Third version: only the first function.  The line-3 chunk from
        // the second version must be gone.
        tokio::fs::write(&f, "fn first() {}\n").await.unwrap();
        indexer.index_file(&f, None).await.expect("third index");
        assert!(store.contains(&id_line1));
        assert!(
            !store.contains(&id_line3),
            "stale line-3 chunk should have been evicted"
        );
    }
}
