//! Persistent per-palace BM25 index — the in-process owner of a palace's
//! lexical corpus.
//!
//! Why: this file is `trusty-bm25-daemon`'s `index.rs`, moved into
//! trusty-memory unchanged in behaviour (#5329). The subprocess it used to live
//! in was never enabled in any shipped configuration, and `trusty-search`
//! already runs the same `trusty_common::bm25::BM25Index` in-process against its
//! own redb/usearch locks — so the daemon's isolation was buying nothing an
//! in-memory inverted index needs.
//!
//! What: `PalaceBm25Index` owns a `BM25Index`, tracks a `dirty` flag, and
//! flushes to `<data_dir>/bm25_index.json` on demand.
//!
//! 🔴 The snapshot path AND format are deliberately byte-identical to the
//! daemon's, because that is the whole migration story: an operator who set
//! `TRUSTY_BM25_DAEMON=1` has real snapshots under `<data_root>/<palace>/bm25/`,
//! and [`load_or_create`](PalaceBm25Index::load_or_create) reads them in place —
//! no conversion step, no rebuild, and a downgrade to the previous release still
//! reads what this wrote. Each entry is `{"doc_id": "...", "text": "..."}`; the
//! BM25 internals (postings, doc-length sums, free-slot list) are rebuilt by
//! replaying documents through `upsert_document`, so the snapshot stays
//! version-agnostic across tokenizer revisions.
//!
//! Test: `bm25_index_tests.rs`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use trusty_common::bm25::BM25Index;

use crate::bm25_lane::BM25Hit;

/// On-disk snapshot filename, written inside the palace's `data_dir`.
///
/// Why: the loader, the flusher and any operator inspecting a palace by hand
/// must agree on one name — and #5329 additionally requires it to equal the
/// name `trusty-bm25-daemon` used, or every existing snapshot goes unread.
/// What: `bm25_index.json` — plain JSON, atomically written via `.tmp` + rename.
/// Test: `snapshot_written_by_the_daemon_is_read_in_place`.
pub const SNAPSHOT_FILENAME: &str = "bm25_index.json";

/// One row of the persistent snapshot.
///
/// Why: serialising raw `BM25Index` internals would couple the on-disk format to
/// the inverted-index layout. Storing `(doc_id, text)` lets the index be rebuilt
/// from scratch on every load with no version constraints.
/// What: a plain serde struct; the snapshot file is a JSON array of these.
/// Test: `snapshot_written_by_the_daemon_is_read_in_place`.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Document {
    doc_id: String,
    text: String,
}

/// Persistent BM25 index for one palace.
///
/// Why: trusty-memory needs a palace's lexical corpus to survive a restart with
/// exactly the documents it was serving before. Keeping the storage concern here
/// leaves [`crate::bm25_lane::Bm25Lane`] free to be only about which palaces are
/// resident and when they flush.
/// What: holds the index, the snapshot path, the live document set (kept so the
/// snapshot can be re-serialised), and a `dirty` bit so `flush` is a no-op when
/// nothing changed.
/// Test: `bm25_index_tests.rs`.
pub struct PalaceBm25Index {
    inner: BM25Index,
    /// `data_dir/bm25_index.json` — kept on the struct so `flush()` needs no
    /// arguments.
    snapshot_path: PathBuf,
    /// Authoritative copy of the indexed text, keyed by doc_id. `BM25Index`
    /// itself does not preserve the original input (it stores token lists only),
    /// so the `doc_id → text` map is what the snapshot is written from.
    /// `BTreeMap` so snapshot order is stable for diffing / inspection.
    docs: BTreeMap<String, String>,
    dirty: bool,
}

impl PalaceBm25Index {
    /// Load the snapshot from disk, or start with an empty index.
    ///
    /// Why: this is the migration path for #5329. A snapshot the old subprocess
    /// wrote is read here verbatim, so switching to the in-process lane neither
    /// loses a corpus nor needs a conversion pass.
    /// What: ensures `data_dir` exists, reads `<data_dir>/bm25_index.json` when
    /// present, replays each `Document` through `upsert_document`. A missing
    /// snapshot is the fresh-install case (no error); a corrupt one is logged and
    /// the index starts empty so recall still comes up. An I/O error other than
    /// `NotFound` propagates — refusing to start beats silently dropping a corpus
    /// the operator can still see on disk.
    /// Test: `snapshot_written_by_the_daemon_is_read_in_place`,
    /// `load_recovers_from_a_corrupt_snapshot`,
    /// `load_propagates_an_unreadable_snapshot`.
    pub fn load_or_create(data_dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(data_dir)
            .with_context(|| format!("create palace bm25 dir {}", data_dir.display()))?;
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
                    tracing::warn!(
                        path = %snapshot_path.display(),
                        "corrupt BM25 snapshot ({e}); starting with empty index"
                    );
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                tracing::debug!(
                    path = %snapshot_path.display(),
                    "no BM25 snapshot found — starting with empty index"
                );
            }
            Err(e) => {
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
    /// Why: the write path (`memory_remember` / `memory_note`) and the backfill
    /// both land here; keying by `doc_id` makes a re-run overwrite rather than
    /// duplicate, which is what lets the backfill be idempotent.
    /// Test: `index_doc_marks_dirty`.
    pub fn index_doc(&mut self, doc_id: &str, text: &str) {
        self.inner.upsert_document(doc_id, text);
        self.docs.insert(doc_id.to_string(), text.to_string());
        self.dirty = true;
    }

    /// Search the index. Read-only — does not mark dirty.
    ///
    /// What: forwards to `BM25Index::score_query_all`. `top_k` is clamped to ≥ 1
    /// so a misconfigured caller never silently asks for zero hits.
    /// Test: `search_returns_hits`.
    pub fn search(&self, query: &str, top_k: usize) -> Vec<BM25Hit> {
        let top_k = top_k.max(1);
        self.inner
            .score_query_all(query, top_k)
            .into_iter()
            .map(|(doc_id, score)| BM25Hit { doc_id, score })
            .collect()
    }

    /// Remove a document. Marks the index dirty.
    ///
    /// What: idempotent — a no-op for unknown ids, which is why it does not
    /// re-dirty a clean index. Returns `true` if the id was present beforehand.
    /// Test: `delete_doc_removes_and_marks_dirty`.
    pub fn delete_doc(&mut self, doc_id: &str) -> bool {
        let was_present = self.docs.remove(doc_id).is_some();
        if was_present {
            self.inner.remove_document(doc_id);
            self.dirty = true;
        }
        was_present
    }

    /// Live document count.
    pub fn doc_count(&self) -> usize {
        self.inner.len()
    }

    /// Corpus size in bytes of retained document text.
    ///
    /// Why (#2846, #5329): this struct keeps every document's full text so it
    /// can re-serialise the snapshot, so a resident palace costs RAM linear in
    /// its corpus. That figure is what [`crate::bm25_lane::Bm25Lane`] budgets
    /// against now that there is no child process whose RSS could be measured —
    /// 1311 one-line drawers and 1311 page-long ones cost very different memory.
    /// What: sums `str::len()` over the retained text. O(n) over documents, not
    /// bytes, and never called in the hot loop.
    /// Test: `stats_track_docs_and_bytes`.
    pub fn total_text_bytes(&self) -> u64 {
        self.docs.values().map(|t| t.len() as u64).sum()
    }

    /// Which of `doc_ids` this index does not hold.
    ///
    /// Why: coverage is a SET question, and [`Self::doc_count`] answers a
    /// different one. The two agree only while the index holds exactly the
    /// caller's documents and nothing else; one stale document left behind by a
    /// delete that never happened is enough for a count comparison to report
    /// coverage the set does not have (#5053).
    /// What: retains the requested ids absent from `docs`, in request order.
    /// Duplicated request ids are reported once per occurrence — the caller owns
    /// de-duplication.
    /// Test: `missing_docs_answers_by_identity_not_count`.
    pub fn missing_docs(&self, doc_ids: &[String]) -> Vec<String> {
        doc_ids
            .iter()
            .filter(|id| !self.docs.contains_key(*id))
            .cloned()
            .collect()
    }

    /// Snapshot path this index writes to. Exposed for diagnostics / tests.
    pub fn snapshot_path(&self) -> &Path {
        &self.snapshot_path
    }

    /// True iff the in-memory state has drifted from the on-disk snapshot.
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// Persist the current state to disk if it has changed.
    ///
    /// Why: the flush is O(corpus) because the whole snapshot is rewritten, so
    /// calling it per write would make a 1311-drawer backfill quadratic in
    /// bytes. [`crate::bm25_lane::Bm25Lane`] therefore coalesces flushes on a
    /// timer and this stays a no-op when `!dirty`.
    /// What: serialises `docs` to JSON, writes `<snapshot>.tmp`, renames over
    /// `snapshot_path` for atomic publication. Clears `dirty` only on success, so
    /// a failed flush is retried by the next tick rather than silently dropped.
    /// Test: `flush_round_trips`, `a_failed_flush_leaves_the_index_dirty`.
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
#[path = "bm25_index_tests.rs"]
mod tests;
