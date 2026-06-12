//! [`CorpusStore`] chunk and entity CRUD operations.
//!
//! Why: split from the monolithic `store_impl` to keep each file under 500
//! lines. This file owns all chunk/entity upsert, load, delete, and query
//! methods — nothing else.
//! What: `impl CorpusStore` block covering `upsert_chunks`, `upsert_entities`,
//! `upsert_batch`, `list_indexed_files`, `delete_file_hash_entries`,
//! `delete_chunks`, `delete_entities`, `load_all_chunks`, `get_chunks`,
//! `load_all_entities`, `chunk_count`, and `db`.
//! Test: covered by the `tests` submodule.

use anyhow::{Context, Result};
use redb::{Database, ReadableDatabase, ReadableTable, ReadableTableMetadata};

use super::store_impl::CorpusStore;
use super::tables::{CHUNKS_TABLE, ENTITIES_TABLE, FILE_HASHES_TABLE};
use crate::core::chunker::RawChunk;
use crate::core::entity::RawEntity;

impl CorpusStore {
    /// Upsert a batch of chunks in a single redb write transaction.
    ///
    /// Why: a batch commit (`commit_parsed_batch`) lands up to a few hundred
    /// chunks at once. One transaction per batch keeps the write amplification
    /// proportional to the batch size, not the whole corpus, and makes the
    /// batch atomic — a crash mid-commit rolls the whole batch back.
    /// What: serializes each [`RawChunk`] with `serde_json` and inserts it
    /// under its `id`. Existing ids are overwritten (upsert semantics).
    /// Test: `roundtrip` writes then reads; `delete_removes_chunk` re-upserts.
    pub fn upsert_chunks(&self, chunks: &[RawChunk]) -> Result<()> {
        if chunks.is_empty() {
            return Ok(());
        }
        let txn = self.db.begin_write().context("begin chunk upsert txn")?;
        {
            let mut table = txn.open_table(CHUNKS_TABLE)?;
            for chunk in chunks {
                let bytes = serde_json::to_vec(chunk)
                    .with_context(|| format!("serialize chunk {}", chunk.id))?;
                table
                    .insert(chunk.id.as_str(), bytes.as_slice())
                    .with_context(|| format!("insert chunk {}", chunk.id))?;
            }
        }
        txn.commit().context("commit chunk upsert txn")?;
        Ok(())
    }

    /// Upsert a batch of per-file entity lists in a single write transaction.
    ///
    /// Why: entity lists are committed alongside chunks; sharing the same
    /// one-txn-per-batch discipline keeps both tables consistent on a crash.
    /// What: serializes each `Vec<RawEntity>` and inserts it under its file
    /// path key.
    /// Test: `roundtrip` exercises this alongside `upsert_chunks`.
    pub fn upsert_entities(&self, entities: &[(String, Vec<RawEntity>)]) -> Result<()> {
        if entities.is_empty() {
            return Ok(());
        }
        let txn = self.db.begin_write().context("begin entity upsert txn")?;
        {
            let mut table = txn.open_table(ENTITIES_TABLE)?;
            for (file, ents) in entities {
                let bytes = serde_json::to_vec(ents)
                    .with_context(|| format!("serialize entities for {file}"))?;
                table
                    .insert(file.as_str(), bytes.as_slice())
                    .with_context(|| format!("insert entities for {file}"))?;
            }
        }
        txn.commit().context("commit entity upsert txn")?;
        Ok(())
    }

    /// Upsert a batch of chunks **and** their per-file entity lists in a
    /// single redb write transaction (issue #29).
    ///
    /// Why: `upsert_chunks` and `upsert_entities` each opened their own
    /// `begin_write()` transaction. A crash (or SIGTERM) landing between the
    /// two commits left the chunk corpus and the symbol-graph entity table
    /// inconsistent — a warm-boot would rehydrate chunks that the entity table
    /// no longer described, or vice versa. Folding both tables into one
    /// transaction makes the whole batch (chunks + entities) atomic: a crash
    /// either rolls back the entire batch or commits all of it.
    /// What: opens one write transaction, inserts every [`RawChunk`] into
    /// `CHUNKS_TABLE` and every per-file `Vec<RawEntity>` into `ENTITIES_TABLE`
    /// under that transaction, then commits once. Both table handles are
    /// dropped (inner scope closed) before `commit()` — redb requires every
    /// table opened in a write txn to be dropped before the txn can commit.
    /// Empty inputs on **both** sides are a no-op (no transaction opened); a
    /// non-empty input on either side still writes the other table even when
    /// it is empty, so callers get one consistent commit point.
    /// Test: `batch_upsert_is_atomic_roundtrip` writes chunks + entities via
    /// this method and reads them back from a reopened store.
    pub fn upsert_batch(
        &self,
        chunks: &[RawChunk],
        entities: &[(String, Vec<RawEntity>)],
    ) -> Result<()> {
        if chunks.is_empty() && entities.is_empty() {
            return Ok(());
        }
        let txn = self.db.begin_write().context("begin batch upsert txn")?;
        {
            // Single atomic transaction covering both tables. Table handles
            // live only inside this scope so they are dropped before commit.
            let mut chunks_tbl = txn
                .open_table(CHUNKS_TABLE)
                .context("open chunks table for batch upsert")?;
            for chunk in chunks {
                let bytes = serde_json::to_vec(chunk)
                    .with_context(|| format!("serialize chunk {}", chunk.id))?;
                chunks_tbl
                    .insert(chunk.id.as_str(), bytes.as_slice())
                    .with_context(|| format!("insert chunk {}", chunk.id))?;
            }
            let mut entities_tbl = txn
                .open_table(ENTITIES_TABLE)
                .context("open entities table for batch upsert")?;
            for (file, ents) in entities {
                let bytes = serde_json::to_vec(ents)
                    .with_context(|| format!("serialize entities for {file}"))?;
                entities_tbl
                    .insert(file.as_str(), bytes.as_slice())
                    .with_context(|| format!("insert entities for {file}"))?;
            }
        }
        txn.commit().context("commit batch upsert txn")?;
        Ok(())
    }

    /// Return the distinct set of file paths present in the chunk corpus.
    ///
    /// Why: the non-force reindex prune pass (issue #848) needs to compare the
    /// walked file set against what is stored in the corpus so it can identify
    /// files that were deleted from disk but whose stale chunks were carried
    /// forward by `copy_all_from`. Only the STAGING corpus is queried (after
    /// carryover and after all batch writes), so the result reflects the full
    /// committed state that will be promoted.
    /// What: opens a read transaction and collects every distinct `RawChunk.file`
    /// value by deserialising each row's JSON. Corrupt rows are skipped with a
    /// `warn` to match `load_all_chunks`'s tolerance.
    /// Test: `list_indexed_files_returns_distinct_files` below.
    pub fn list_indexed_files(&self) -> Result<Vec<String>> {
        use std::collections::HashSet;
        let txn = self.db.begin_read().context("begin list_files read txn")?;
        let table = txn.open_table(CHUNKS_TABLE)?;
        let mut seen: HashSet<String> = HashSet::new();
        for entry in table.iter().context("iterate chunks for list_files")? {
            let (key, value) = entry.context("read chunk row for list_files")?;
            match serde_json::from_slice::<RawChunk>(value.value()) {
                Ok(chunk) => {
                    seen.insert(chunk.file);
                }
                Err(e) => {
                    tracing::warn!(
                        "corpus: skipping corrupt chunk row '{}' in list_indexed_files ({e})",
                        key.value()
                    );
                }
            }
        }
        Ok(seen.into_iter().collect())
    }

    /// Delete `FILE_HASHES_TABLE` entries for the given file paths in one
    /// write transaction (issue #848 prune pass).
    ///
    /// Why: when a file is deleted from disk and its stale chunks are pruned
    /// from the staging corpus, the persisted file-hash entry must also be
    /// removed. Without this, the next reindex would load the stale hash,
    /// think the (now-absent) file is unchanged, and not re-index it — leaving
    /// the next promoted corpus with no chunks for a file that no longer exists.
    /// What: removes each path from `FILE_HASHES_TABLE`; unknown paths are
    /// silently ignored (idempotent). Empty input is a no-op.
    /// Test: covered transitively by `prune_deleted_files_removes_hashes` in
    /// `service::reindex::tests`.
    pub fn delete_file_hash_entries(&self, files: &[String]) -> Result<()> {
        if files.is_empty() {
            return Ok(());
        }
        let txn = self
            .db
            .begin_write()
            .context("begin file_hash delete txn")?;
        {
            let mut tbl = txn
                .open_table(FILE_HASHES_TABLE)
                .context("open file_hashes table for delete")?;
            for file in files {
                tbl.remove(file.as_str())
                    .with_context(|| format!("delete file hash entry for {file}"))?;
            }
        }
        txn.commit().context("commit file_hash delete txn")?;
        Ok(())
    }

    /// Delete a set of chunk ids in one write transaction.
    ///
    /// Why: `remove_file` / `remove_chunk` must evict from the durable store
    /// too, or a restart would resurrect deleted chunks.
    /// What: removes each id from `CHUNKS_TABLE`; unknown ids are a silent
    /// no-op (idempotent delete), matching the in-memory `HashMap::remove`.
    /// Test: `delete_removes_chunk`.
    pub fn delete_chunks(&self, ids: &[String]) -> Result<()> {
        if ids.is_empty() {
            return Ok(());
        }
        let txn = self.db.begin_write().context("begin chunk delete txn")?;
        {
            let mut table = txn.open_table(CHUNKS_TABLE)?;
            for id in ids {
                table
                    .remove(id.as_str())
                    .with_context(|| format!("delete chunk {id}"))?;
            }
        }
        txn.commit().context("commit chunk delete txn")?;
        Ok(())
    }

    /// Delete a per-file entity list. Idempotent.
    ///
    /// Why: `remove_file` drops the file's entities; the durable store must
    /// follow or the symbol graph would rebuild stale symbols on restart.
    /// What: removes the file key from `ENTITIES_TABLE`.
    /// Test: covered indirectly by `delete_removes_chunk` (same txn shape).
    pub fn delete_entities(&self, file: &str) -> Result<()> {
        let txn = self.db.begin_write().context("begin entity delete txn")?;
        {
            let mut table = txn.open_table(ENTITIES_TABLE)?;
            table
                .remove(file)
                .with_context(|| format!("delete entities for {file}"))?;
        }
        txn.commit().context("commit entity delete txn")?;
        Ok(())
    }

    /// Load every chunk in the corpus into a `Vec`.
    ///
    /// Why: the warm-boot path rehydrates the in-memory `HashMap` (and rebuilds
    /// BM25 + the symbol graph) from this. A streaming iterator would avoid the
    /// transient `Vec`, but the caller already needs an owned `RawChunk` per
    /// entry to insert into the map, so the `Vec` is not extra peak RAM beyond
    /// the map itself.
    /// What: opens a read transaction, walks `CHUNKS_TABLE`, and deserializes
    /// each value. A single corrupt row is skipped with a `warn` rather than
    /// failing the whole load — one bad chunk must not brick the daemon.
    /// Test: `roundtrip`.
    pub fn load_all_chunks(&self) -> Result<Vec<RawChunk>> {
        let txn = self.db.begin_read().context("begin chunk read txn")?;
        let table = txn.open_table(CHUNKS_TABLE)?;
        let mut out = Vec::new();
        for entry in table.iter().context("iterate chunks table")? {
            let (key, value) = entry.context("read chunk row")?;
            match serde_json::from_slice::<RawChunk>(value.value()) {
                Ok(chunk) => out.push(chunk),
                Err(e) => {
                    tracing::warn!("corpus: skipping corrupt chunk row '{}' ({e})", key.value())
                }
            }
        }
        Ok(out)
    }

    /// Batch point-read a set of chunks by `chunk_id`.
    ///
    /// Why: the search hot path reads top-k chunk text straight out of redb at
    /// materialization time, letting the daemon drop the in-memory
    /// `HashMap<String, RawChunk>` from the query path entirely. redb values
    /// are mmap-backed, so a point lookup is served from the OS page cache
    /// rather than process heap, cutting steady-state RSS significantly.
    /// What: opens a single redb read transaction and fetches each requested
    /// id. Missing ids are skipped (not an error); corrupt rows are skipped
    /// with a `warn`. The returned `Vec` preserves the input `ids` order for
    /// the ids that were found.
    /// Test: `get_chunks_batch_reads_subset` round-trips a corpus and asserts
    /// only the requested ids come back, in order, with missing ids skipped.
    pub fn get_chunks(&self, ids: &[&str]) -> Result<Vec<RawChunk>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let txn = self.db.begin_read().context("begin chunk point-read txn")?;
        let table = txn.open_table(CHUNKS_TABLE)?;
        let mut out = Vec::with_capacity(ids.len());
        for id in ids {
            let Some(value) = table
                .get(*id)
                .with_context(|| format!("point-read chunk {id}"))?
            else {
                tracing::warn!("corpus: chunk '{id}' not found in redb — skipping");
                continue;
            };
            match serde_json::from_slice::<RawChunk>(value.value()) {
                Ok(chunk) => out.push(chunk),
                Err(e) => {
                    tracing::warn!("corpus: skipping corrupt chunk row '{id}' ({e})")
                }
            }
        }
        Ok(out)
    }

    /// Load every per-file entity list.
    ///
    /// Why: counterpart of [`Self::load_all_chunks`] for the entities table;
    /// the warm-boot path needs both to rebuild the symbol graph.
    /// What: walks `ENTITIES_TABLE`, deserializing each `Vec<RawEntity>`. A
    /// corrupt row is skipped with a `warn`.
    /// Test: `roundtrip`.
    pub fn load_all_entities(&self) -> Result<Vec<(String, Vec<RawEntity>)>> {
        let txn = self.db.begin_read().context("begin entity read txn")?;
        let table = txn.open_table(ENTITIES_TABLE)?;
        let mut out = Vec::new();
        for entry in table.iter().context("iterate entities table")? {
            let (key, value) = entry.context("read entity row")?;
            let file = key.value().to_string();
            match serde_json::from_slice::<Vec<RawEntity>>(value.value()) {
                Ok(ents) => out.push((file, ents)),
                Err(e) => {
                    tracing::warn!("corpus: skipping corrupt entity row '{file}' ({e})")
                }
            }
        }
        Ok(out)
    }

    /// Number of chunks currently stored.
    ///
    /// Why: lets the warm-boot path log a count and lets callers cheaply check
    /// "is the durable corpus empty?" (first-run / post-upgrade case) without
    /// materializing every row.
    /// What: returns `CHUNKS_TABLE.len()`.
    /// Test: `roundtrip` asserts the count after upsert.
    pub fn chunk_count(&self) -> Result<usize> {
        let txn = self.db.begin_read().context("begin count txn")?;
        let table = txn.open_table(CHUNKS_TABLE)?;
        Ok(table.len().context("count chunks")? as usize)
    }

    /// Borrow the underlying `redb::Database` (issue #41 phase 2).
    ///
    /// Why: the `SymbolGraph` persistence helpers (`save_to_corpus`,
    /// `load_from_corpus`, …) need direct access to the KG tables that live
    /// alongside the chunk corpus in the same redb file. Exposing the
    /// `Database` here means we don't duplicate the file-open dance on every
    /// graph save and avoids opening a second .redb file per index.
    /// What: returns a borrow of `self.db`. Callers can begin read/write
    /// transactions against the KG tables exported as
    /// `pub(crate) const KG_*_TABLE` in this module.
    /// Test: covered indirectly by every `SymbolGraph::*_corpus` test.
    #[allow(dead_code)]
    pub(crate) fn db(&self) -> &Database {
        &self.db
    }
}
