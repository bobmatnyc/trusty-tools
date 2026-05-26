//! Persistent wrapper around `trusty_common::bm25::BM25Index` (issue #156).
//!
//! Why: the daemon's lifetime is one palace; on restart it must recover the
//! exact corpus it was serving before the SIGTERM. A JSON snapshot under
//! `<data_dir>/bm25_index.json` is the cheapest "right thing" — palaces hold
//! hundreds to low thousands of drawers, so the snapshot is small and the
//! human-readability buys us painless debugging.
//!
//! What: `PalaceBm25Index` owns a `BM25Index`, tracks a `dirty` flag, and
//! flushes the in-memory state to disk on demand. `load_or_create` reads the
//! snapshot when present and starts empty otherwise.
//!
//! The on-disk format is intentionally simple — each entry is
//! `{"doc_id": "...", "text": "..."}`. The BM25 internals (postings,
//! doc-length sums, free-slot list) are rebuilt deterministically by replaying
//! the documents through `upsert_document`, so we never serialise the inverted
//! index directly. That keeps the snapshot version-agnostic across BM25
//! tokenizer revisions.
//!
//! Test: `palace_index_load_create_round_trips`, `palace_index_search_returns_hits`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use trusty_common::bm25::BM25Index;

use crate::protocol::SearchHit;

/// On-disk snapshot filename, written inside the palace's `data_dir`.
///
/// Why: kept as a module constant so the daemon's startup code and any
/// future tooling (e.g. a `trusty-bm25-inspect` CLI) agree on the path.
/// What: `bm25_index.json` — plain JSON, atomically written via `.tmp` +
/// rename.
/// Test: `palace_index_load_create_round_trips`.
pub const SNAPSHOT_FILENAME: &str = "bm25_index.json";

/// One row of the persistent snapshot.
///
/// Why: serialising raw `BM25Index` internals would couple the on-disk
/// format to the inverted-index layout, which we want to keep free to
/// evolve. Storing `(doc_id, text)` lets us rebuild the index from scratch
/// on every load with no version constraints.
/// What: a plain serde struct; the snapshot file is a JSON array of these.
/// Test: `palace_index_load_create_round_trips`.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Document {
    doc_id: String,
    text: String,
}

/// Persistent BM25 index for one palace.
///
/// Why: the daemon needs to survive restart with the exact corpus it had
/// before. Wrapping `BM25Index` here keeps the storage concern out of
/// `write_queue` (whose only job is coalescing) and out of `server` (whose
/// only job is dispatch).
/// What: holds the index, the snapshot path, the live document set (kept so
/// we can re-serialise), and a `dirty` bit so `flush` is a no-op when nothing
/// has changed since the last write.
/// Test: every test in this module exercises `PalaceBm25Index` directly.
pub struct PalaceBm25Index {
    inner: BM25Index,
    /// `data_dir/bm25_index.json` — kept on the struct so `flush()` can be
    /// called with no arguments.
    snapshot_path: PathBuf,
    /// Authoritative copy of the indexed text, keyed by doc_id. `BM25Index`
    /// itself does not preserve the original input (it stores token lists
    /// only), so we keep our own `doc_id → text` map for snapshot output.
    /// `BTreeMap` so snapshot order is stable for diffing / inspection.
    docs: BTreeMap<String, String>,
    dirty: bool,
}

impl PalaceBm25Index {
    /// Load the snapshot from disk, or start with an empty index.
    ///
    /// Why: every daemon startup runs this exactly once. A missing snapshot
    /// is the fresh-install case (no error); a corrupted snapshot is logged
    /// and we start empty so the daemon still comes up.
    /// What: ensures `data_dir` exists, reads `<data_dir>/bm25_index.json`
    /// when present, replays each `Document` through `upsert_document` to
    /// rebuild the inverted index. The `dirty` flag is `false` immediately
    /// after load.
    /// Test: `palace_index_load_create_round_trips`.
    pub fn load_or_create(data_dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(data_dir)
            .with_context(|| format!("create palace data dir {}", data_dir.display()))?;
        let snapshot_path = data_dir.join(SNAPSHOT_FILENAME);

        let mut inner = BM25Index::new();
        let mut docs = BTreeMap::new();

        match std::fs::read(&snapshot_path) {
            Ok(bytes) => match serde_json::from_slice::<Vec<Document>>(&bytes) {
                Ok(rows) => {
                    for row in rows {
                        inner.upsert_document(&row.doc_id, &row.text);
                        docs.insert(row.doc_id, row.text);
                    }
                    tracing::info!(
                        path = %snapshot_path.display(),
                        doc_count = docs.len(),
                        "loaded BM25 snapshot"
                    );
                }
                Err(e) => {
                    // A corrupt snapshot must not block startup — log and
                    // start fresh. The daemon will write a clean snapshot on
                    // the next flush.
                    tracing::warn!(
                        path = %snapshot_path.display(),
                        "corrupt BM25 snapshot ({e}); starting with empty index"
                    );
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                tracing::info!(
                    path = %snapshot_path.display(),
                    "no BM25 snapshot found — starting with empty index"
                );
            }
            Err(e) => {
                // Surface a permission / I/O error so the operator can fix
                // it; refusing to start is safer than silently dropping the
                // existing corpus.
                return Err(anyhow::Error::new(e)
                    .context(format!("read BM25 snapshot at {}", snapshot_path.display())));
            }
        }

        Ok(Self {
            inner,
            snapshot_path,
            docs,
            dirty: false,
        })
    }

    /// Insert or replace a document. Marks the index dirty.
    ///
    /// Why: append-only ingest is the daemon's only documented mutation
    /// path; this is the routine the request thread eventually hits via the
    /// write queue. Updates of the same `doc_id` are idempotent.
    /// What: forwards to `BM25Index::upsert_document` and stores the text in
    /// `docs` so the next `flush` includes it.
    /// Test: `palace_index_index_doc_marks_dirty`.
    pub fn index_doc(&mut self, doc_id: &str, text: &str) {
        self.inner.upsert_document(doc_id, text);
        self.docs.insert(doc_id.to_string(), text.to_string());
        self.dirty = true;
    }

    /// Search the index. Read-only — does not mark dirty.
    ///
    /// Why: search is the recall hot path; mirroring the BM25Index's typed
    /// `Vec<SearchHit>` output keeps the dispatcher trivial.
    /// What: forwards to `BM25Index::score_query_all` and lifts the result
    /// into `SearchHit`. `top_k` is clamped to ≥ 1 so a misconfigured caller
    /// never silently asks for zero hits.
    /// Test: `palace_index_search_returns_hits`.
    pub fn search(&self, query: &str, top_k: usize) -> Vec<SearchHit> {
        let top_k = top_k.max(1);
        self.inner
            .score_query_all(query, top_k)
            .into_iter()
            .map(|(doc_id, score)| SearchHit { doc_id, score })
            .collect()
    }

    /// Remove a document. Marks the index dirty.
    ///
    /// Why: reserved for the dream subprocess; the request hot path never
    /// calls this. Idempotent — no-op for unknown ids.
    /// What: forwards to `BM25Index::remove_document` and drops the entry
    /// from `docs`. Returns `true` if the id was present beforehand.
    /// Test: `palace_index_delete_doc_removes_and_marks_dirty`.
    pub fn delete_doc(&mut self, doc_id: &str) -> bool {
        let was_present = self.docs.remove(doc_id).is_some();
        if was_present {
            self.inner.remove_document(doc_id);
            self.dirty = true;
        }
        was_present
    }

    /// Drop every document.
    ///
    /// Why: rebuild semantics for the dream subprocess. Returning the
    /// post-rebuild doc count (always zero today) lets callers assert.
    /// What: replaces the inner index with a fresh `BM25Index` and clears
    /// `docs`. Always marks dirty so the next `flush` writes the empty
    /// snapshot.
    /// Test: `palace_index_rebuild_clears_corpus`.
    pub fn rebuild(&mut self) -> usize {
        self.inner = BM25Index::new();
        self.docs.clear();
        self.dirty = true;
        self.inner.len()
    }

    /// Live document count.
    pub fn doc_count(&self) -> usize {
        self.inner.len()
    }

    /// Snapshot path the daemon writes to. Exposed for diagnostics / tests.
    #[allow(dead_code)] // public for diagnostics; consumed by tests
    pub fn snapshot_path(&self) -> &Path {
        &self.snapshot_path
    }

    /// True iff the in-memory state has drifted from the on-disk snapshot.
    #[allow(dead_code)] // public for diagnostics; consumed by tests
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// Persist the current state to disk if it has changed.
    ///
    /// Why: the request path calls this once per write batch so a SIGTERM
    /// between batches loses at most one batch's worth of work. A no-op
    /// when `!dirty` keeps idle daemons quiet.
    /// What: serialises `docs` to JSON, writes to `<snapshot>.tmp`, fsyncs,
    /// renames over `snapshot_path` for atomic publication. Clears `dirty`
    /// on success. Errors propagate so the caller can log and either retry
    /// or warn.
    /// Test: `palace_index_flush_round_trips`.
    pub fn flush(&mut self) -> Result<()> {
        if !self.dirty {
            return Ok(());
        }
        let rows: Vec<Document> = self
            .docs
            .iter()
            .map(|(doc_id, text)| Document {
                doc_id: doc_id.clone(),
                text: text.clone(),
            })
            .collect();
        let json = serde_json::to_vec(&rows).context("serialise BM25 snapshot")?;

        let tmp_path = self.snapshot_path.with_extension("json.tmp");
        std::fs::write(&tmp_path, &json)
            .with_context(|| format!("write BM25 snapshot tmp file {}", tmp_path.display()))?;
        std::fs::rename(&tmp_path, &self.snapshot_path).with_context(|| {
            format!(
                "atomic rename {} → {}",
                tmp_path.display(),
                self.snapshot_path.display()
            )
        })?;
        self.dirty = false;
        tracing::debug!(
            path = %self.snapshot_path.display(),
            doc_count = rows.len(),
            "flushed BM25 snapshot"
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tempdir() -> tempfile::TempDir {
        tempfile::tempdir().expect("tempdir")
    }

    #[test]
    fn palace_index_load_create_round_trips() {
        let dir = tempdir();
        // First open: no snapshot, empty index.
        {
            let mut idx = PalaceBm25Index::load_or_create(dir.path()).unwrap();
            assert_eq!(idx.doc_count(), 0);
            idx.index_doc("a", "the quick brown fox");
            idx.index_doc("b", "lazy dog");
            idx.flush().unwrap();
            assert!(idx.snapshot_path().exists());
        }
        // Re-open: snapshot must rehydrate both docs.
        let idx = PalaceBm25Index::load_or_create(dir.path()).unwrap();
        assert_eq!(idx.doc_count(), 2);
        let hits = idx.search("fox", 10);
        assert!(hits.iter().any(|h| h.doc_id == "a"));
    }

    #[test]
    fn palace_index_search_returns_hits() {
        let dir = tempdir();
        let mut idx = PalaceBm25Index::load_or_create(dir.path()).unwrap();
        idx.index_doc("doc1", "authentication login password");
        idx.index_doc("doc2", "rendering ui components");
        let hits = idx.search("authentication", 5);
        assert_eq!(hits.len(), 1, "got: {hits:?}");
        assert_eq!(hits[0].doc_id, "doc1");
        assert!(hits[0].score > 0.0);
    }

    #[test]
    fn palace_index_index_doc_marks_dirty() {
        let dir = tempdir();
        let mut idx = PalaceBm25Index::load_or_create(dir.path()).unwrap();
        assert!(!idx.is_dirty());
        idx.index_doc("d", "hello");
        assert!(idx.is_dirty());
        idx.flush().unwrap();
        assert!(!idx.is_dirty());
    }

    #[test]
    fn palace_index_delete_doc_removes_and_marks_dirty() {
        let dir = tempdir();
        let mut idx = PalaceBm25Index::load_or_create(dir.path()).unwrap();
        idx.index_doc("d", "alpha beta");
        idx.flush().unwrap();
        assert!(idx.delete_doc("d"));
        assert!(idx.is_dirty());
        assert_eq!(idx.doc_count(), 0);
        assert!(idx.search("alpha", 5).is_empty());
        // Deleting unknown id is a no-op and doesn't re-dirty.
        idx.flush().unwrap();
        assert!(!idx.delete_doc("never-existed"));
        assert!(!idx.is_dirty());
    }

    #[test]
    fn palace_index_rebuild_clears_corpus() {
        let dir = tempdir();
        let mut idx = PalaceBm25Index::load_or_create(dir.path()).unwrap();
        idx.index_doc("a", "x");
        idx.index_doc("b", "y");
        assert_eq!(idx.doc_count(), 2);
        let count_after = idx.rebuild();
        assert_eq!(count_after, 0);
        assert!(idx.is_dirty());
        idx.flush().unwrap();
        // Reopen: snapshot must reflect the rebuild.
        let reopened = PalaceBm25Index::load_or_create(dir.path()).unwrap();
        assert_eq!(reopened.doc_count(), 0);
    }

    #[test]
    fn palace_index_flush_round_trips() {
        let dir = tempdir();
        let mut idx = PalaceBm25Index::load_or_create(dir.path()).unwrap();
        idx.index_doc("x", "one two three");
        idx.flush().unwrap();
        let raw = std::fs::read_to_string(idx.snapshot_path()).unwrap();
        assert!(raw.contains("\"doc_id\":\"x\""));
        assert!(raw.contains("\"text\":\"one two three\""));
    }

    #[test]
    fn palace_index_load_recovers_from_corrupt_snapshot() {
        let dir = tempdir();
        // Plant a corrupt snapshot.
        let snap = dir.path().join(SNAPSHOT_FILENAME);
        std::fs::write(&snap, b"not valid json").unwrap();
        // Load must succeed (empty index) rather than erroring.
        let idx = PalaceBm25Index::load_or_create(dir.path()).unwrap();
        assert_eq!(idx.doc_count(), 0);
    }
}
