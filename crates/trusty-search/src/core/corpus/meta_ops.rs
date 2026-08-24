//! [`CorpusStore`] file-hash, schema-version, indexed-root, and bulk-copy ops.
//!
//! Why: split from the monolithic `store_impl` to keep each file under 500
//! lines. This file owns all `_meta` table operations and the `copy_all_from`
//! bulk-copy needed by the incremental reindex staging path.
//! What: `impl CorpusStore` block covering `upsert_file_hashes`,
//! `load_file_hashes`, `clear_file_hashes`, `read_schema_version_sync`,
//! `write_schema_version_sync`, `read_indexed_root_sync`,
//! `write_indexed_root_sync`, and `copy_all_from`.
//! Test: covered by the `tests` submodule (e.g. `hash_cache_roundtrip`,
//! `test_meta_schema_version_roundtrip`, `copy_all_from_seeds_staging_corpus`).

use anyhow::{Context, Result};
use redb::{ReadableDatabase, ReadableTable};

use super::store_impl::CorpusStore;
use super::tables::{CHUNKS_TABLE, ENTITIES_TABLE, FILE_HASHES_TABLE};

impl CorpusStore {
    /// Upsert a batch of relative-path → SHA-256-hex entries into the
    /// persistent file-hash table (issue #662).
    ///
    /// Why: called after every successful batch commit so the in-process
    /// content-hash cache is mirrored to redb. Together with
    /// [`Self::load_file_hashes`] this means a daemon restart loads the last
    /// run's hashes and can skip unchanged files without re-embedding them.
    /// The caller enforces the eviction cap BEFORE calling this method.
    /// What: opens a single write transaction, upserts every entry, and
    /// commits. Empty input is a no-op (no transaction opened).
    /// Test: `hash_cache_roundtrip` in `corpus::tests`.
    pub(crate) fn upsert_file_hashes(&self, entries: &[(&str, &str)]) -> Result<()> {
        if entries.is_empty() {
            return Ok(());
        }
        let txn = self
            .db
            .begin_write()
            .context("begin file_hashes upsert txn")?;
        {
            let mut tbl = txn
                .open_table(FILE_HASHES_TABLE)
                .context("open file_hashes table")?;
            for (path, hash) in entries {
                tbl.insert(*path, hash.as_bytes())
                    .with_context(|| format!("insert file hash for {path}"))?;
            }
        }
        txn.commit().context("commit file_hashes upsert txn")?;
        Ok(())
    }

    /// Load all persisted relative-path → SHA-256-hex entries from redb
    /// (issue #662).
    ///
    /// Why: called at reindex start to warm the in-process cache from the
    /// previous run's committed hashes. After loading, unchanged files are
    /// skipped immediately — no cold-start re-embed.
    /// What: opens a read transaction, walks `FILE_HASHES_TABLE`, and returns
    /// every entry as `(path_string, hash_string)` pairs. Corrupt rows are
    /// skipped with a `warn`. Returns empty when the table is absent (first
    /// run / legacy database).
    /// Test: `hash_cache_roundtrip` in `corpus::tests`.
    pub(crate) fn load_file_hashes(&self) -> Result<Vec<(String, String)>> {
        let txn = self.db.begin_read().context("begin file_hashes read txn")?;
        let table = match txn.open_table(FILE_HASHES_TABLE) {
            Ok(t) => t,
            Err(redb::TableError::TableDoesNotExist(_)) => return Ok(Vec::new()),
            Err(e) => return Err(anyhow::anyhow!("open file_hashes table: {e}")),
        };
        let mut out = Vec::new();
        for entry in table.iter().context("iterate file_hashes table")? {
            let (k, v) = entry.context("read file_hashes row")?;
            let path = k.value().to_string();
            match std::str::from_utf8(v.value()) {
                Ok(hash) => out.push((path, hash.to_string())),
                Err(_) => {
                    tracing::warn!("corpus: skipping corrupt file_hashes row '{path}'")
                }
            }
        }
        Ok(out)
    }

    /// Atomically clear the entire file-hash table (issue #662).
    ///
    /// Why: called when `force=true` or a root move is detected so the
    /// persisted hashes are cleared alongside the in-process cache. Without
    /// this, a force-reindex that clears the in-process map but not the redb
    /// table would reload stale hashes on next daemon restart and false-skip
    /// force-reindexed files.
    /// What: opens one write transaction, drains `FILE_HASHES_TABLE` via
    /// `retain(|_,_| false)`, and commits.
    /// Test: `hash_cache_clear` in `corpus::tests`.
    pub(crate) fn clear_file_hashes(&self) -> Result<()> {
        let txn = self
            .db
            .begin_write()
            .context("begin file_hashes clear txn")?;
        {
            let mut tbl = txn
                .open_table(FILE_HASHES_TABLE)
                .context("open file_hashes table for clear")?;
            tbl.retain(|_, _| false)
                .context("drain file_hashes table")?;
        }
        txn.commit().context("commit file_hashes clear txn")?;
        Ok(())
    }

    /// Read the `schema_version` entry from the `_meta` table (migration
    /// framework).
    ///
    /// Why: the migration runner needs to know the index's current schema
    /// version before deciding which migrations to apply. Keeping the read
    /// synchronous (like all other `CorpusStore` methods) lets callers manage
    /// the async boundary via `spawn_blocking`.
    /// What: opens a read transaction on `_meta`, looks up
    /// `META_KEY_SCHEMA_VERSION`, and decodes the 4-byte little-endian value.
    /// Returns `0` when the table or key is absent (legacy indexes created
    /// before the migration framework was introduced).
    /// Test: `test_meta_schema_version_roundtrip` in `corpus::tests`.
    pub(crate) fn read_schema_version_sync(&self) -> Result<u32> {
        use crate::core::migration::{META_KEY_SCHEMA_VERSION, META_TABLE};
        let txn = self.db.begin_read().context("begin _meta read txn")?;
        let table = match txn.open_table(META_TABLE) {
            Ok(t) => t,
            Err(redb::TableError::TableDoesNotExist(_)) => return Ok(0),
            Err(e) => return Err(anyhow::anyhow!("open _meta table: {e}")),
        };
        match table
            .get(META_KEY_SCHEMA_VERSION)
            .context("read schema_version")?
        {
            Some(v) => {
                let bytes = v.value();
                if bytes.len() == 4 {
                    Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
                } else {
                    Ok(0)
                }
            }
            None => Ok(0),
        }
    }

    /// Write the `schema_version` entry to the `_meta` table (migration
    /// framework).
    ///
    /// Why: the migration runner writes the new version after a successful
    /// `apply` so the version advances durably. Crash between `apply` and this
    /// write → retry next startup (idempotent `apply` makes that safe).
    /// What: opens a write transaction, creates `_meta` if absent, and upserts
    /// `schema_version` as a 4-byte little-endian value.
    /// Test: `test_meta_schema_version_roundtrip` in `corpus::tests`.
    pub(crate) fn write_schema_version_sync(&self, version: u32) -> Result<()> {
        use crate::core::migration::{META_KEY_SCHEMA_VERSION, META_TABLE};
        let txn = self.db.begin_write().context("begin _meta write txn")?;
        {
            let mut table = txn.open_table(META_TABLE).context("open _meta table")?;
            let bytes = version.to_le_bytes();
            table
                .insert(META_KEY_SCHEMA_VERSION, bytes.as_slice())
                .context("insert schema_version")?;
        }
        txn.commit().context("commit _meta write txn")?;
        Ok(())
    }

    /// Read the canonical root path the corpus's chunk `file` fields are
    /// stored relative to (#602).
    ///
    /// Why: the reindex orchestrator compares this against the current root to
    /// decide whether a move occurred between reindex runs and the stored paths
    /// must be re-relativized. Returning `None` for a legacy / never-stamped
    /// corpus means "unknown prior root" — the caller treats that as a
    /// first-ever reindex (no forced rewrite).
    /// What: opens a read transaction on `_meta`, looks up
    /// `META_KEY_INDEXED_ROOT`, and decodes the UTF-8 path string. An absent
    /// table or key is `Ok(None)` — this index has no prior root. Anything
    /// else, including a stored value that is not valid UTF-8, is an error.
    /// Test: `test_meta_indexed_root_roundtrip` in `corpus::tests`;
    /// `service::reindex::root_hijack_tests::reindex_refuses_when_the_indexed_root_value_is_corrupt`
    /// covers the corrupt-value arm.
    pub(crate) fn read_indexed_root_sync(&self) -> Result<Option<std::path::PathBuf>> {
        use crate::core::migration::{META_KEY_INDEXED_ROOT, META_TABLE};
        let txn = self.db.begin_read().context("begin _meta read txn")?;
        let table = match txn.open_table(META_TABLE) {
            Ok(t) => t,
            Err(redb::TableError::TableDoesNotExist(_)) => return Ok(None),
            Err(e) => return Err(anyhow::anyhow!("open _meta table: {e}")),
        };
        match table
            .get(META_KEY_INDEXED_ROOT)
            .context("read indexed_root")?
        {
            Some(v) => match std::str::from_utf8(v.value()) {
                Ok(s) => Ok(Some(std::path::PathBuf::from(s))),
                // #5357: this arm used to return `Ok(None)` — a CORRUPTED root
                // read back as "this index has no prior root", which is the one
                // answer that skips the #2178 root-move gate entirely. The
                // candidate root was then walked and pruned against. Only
                // `write_indexed_root_sync` writes this key and it always writes
                // valid UTF-8 (`to_string_lossy`), so reaching here means the
                // stored bytes are damaged and refusing is correct.
                Err(e) => Err(anyhow::anyhow!(
                    "indexed_root bytes at {META_KEY_INDEXED_ROOT} are not valid UTF-8: {e}"
                )),
            },
            None => Ok(None),
        }
    }

    /// Persist the canonical root path the corpus's chunk `file` fields are
    /// stored relative to (#602).
    ///
    /// Why: written at the end of every successful reindex so a subsequent run
    /// can detect a root move and re-relativize. See `read_indexed_root_sync`.
    /// What: opens a write transaction, creates `_meta` if absent, and upserts
    /// the path as its UTF-8 byte string.
    /// Test: `test_meta_indexed_root_roundtrip` in `corpus::tests`.
    pub(crate) fn write_indexed_root_sync(&self, root: &std::path::Path) -> Result<()> {
        use crate::core::migration::{META_KEY_INDEXED_ROOT, META_TABLE};
        let txn = self.db.begin_write().context("begin _meta write txn")?;
        {
            let mut table = txn.open_table(META_TABLE).context("open _meta table")?;
            let s = root.to_string_lossy();
            table
                .insert(META_KEY_INDEXED_ROOT, s.as_bytes())
                .context("insert indexed_root")?;
        }
        txn.commit().context("commit _meta write txn")?;
        Ok(())
    }

    /// Read the raw in-progress reindex-checkpoint blob from `_meta` (#3979).
    ///
    /// Why: the reindex runner must decide whether the staging corpus it found
    /// on disk was left by a run it can safely continue. That decision has to
    /// come from state written *inside the same redb file* as the partial
    /// chunks, so a torn write can never leave the marker and the data
    /// disagreeing: redb commits the blob in its own ACID transaction, which is
    /// a strictly stronger crash-safety guarantee than a write-then-rename
    /// sidecar file (there is no window in which a half-written record is
    /// readable).
    /// What: opens a read transaction on `_meta` and returns the bytes stored
    /// under `META_KEY_REINDEX_CHECKPOINT`. Returns `None` when the table or
    /// the key is absent — the normal case for a corpus that was never staged.
    /// Parsing and validating the bytes is the caller's job
    /// (`service::reindex::checkpoint`), so a corrupt blob degrades to "no
    /// resume" rather than an open failure.
    /// Test: `reindex_checkpoint_roundtrip` in `corpus::tests`.
    pub(crate) fn read_reindex_checkpoint_sync(&self) -> Result<Option<Vec<u8>>> {
        use crate::core::migration::{META_KEY_REINDEX_CHECKPOINT, META_TABLE};
        let txn = self.db.begin_read().context("begin _meta read txn")?;
        let table = match txn.open_table(META_TABLE) {
            Ok(t) => t,
            Err(redb::TableError::TableDoesNotExist(_)) => return Ok(None),
            Err(e) => return Err(anyhow::anyhow!("open _meta table: {e}")),
        };
        match table
            .get(META_KEY_REINDEX_CHECKPOINT)
            .context("read reindex_checkpoint")?
        {
            Some(v) => Ok(Some(v.value().to_vec())),
            None => Ok(None),
        }
    }

    /// Write the in-progress reindex-checkpoint blob into `_meta` (#3979).
    ///
    /// Why: written into the STAGING corpus immediately after it is opened and
    /// seeded, before the first batch commits. Any later crash therefore leaves
    /// a staging file that is self-describing: it carries the index id, root,
    /// and config fingerprint of the run that produced it.
    /// What: one redb write transaction upserting `bytes` under
    /// `META_KEY_REINDEX_CHECKPOINT`. The commit is atomic, so a crash during
    /// this call leaves either the old value or the new one — never a partial
    /// record.
    /// Test: `reindex_checkpoint_roundtrip` in `corpus::tests`.
    pub(crate) fn write_reindex_checkpoint_sync(&self, bytes: &[u8]) -> Result<()> {
        use crate::core::migration::{META_KEY_REINDEX_CHECKPOINT, META_TABLE};
        let txn = self.db.begin_write().context("begin _meta write txn")?;
        {
            let mut table = txn.open_table(META_TABLE).context("open _meta table")?;
            table
                .insert(META_KEY_REINDEX_CHECKPOINT, bytes)
                .context("insert reindex_checkpoint")?;
        }
        txn.commit().context("commit _meta write txn")?;
        Ok(())
    }

    /// Remove the in-progress reindex-checkpoint blob from `_meta` (#3979).
    ///
    /// Why: the staging corpus is promoted by RENAMING it over the live
    /// `index.redb`, so anything left in its `_meta` table becomes live state.
    /// A stale "reindex in progress" marker sitting in a promoted, complete
    /// corpus would be a lie about that corpus, so it is cleared in the last
    /// moments before the rename.
    /// What: one redb write transaction removing the key. Removing an absent
    /// key is a no-op, so this is safe to call unconditionally.
    /// Test: `reindex_checkpoint_roundtrip` in `corpus::tests`.
    pub(crate) fn clear_reindex_checkpoint_sync(&self) -> Result<()> {
        use crate::core::migration::{META_KEY_REINDEX_CHECKPOINT, META_TABLE};
        let txn = self.db.begin_write().context("begin _meta write txn")?;
        {
            let mut table = txn.open_table(META_TABLE).context("open _meta table")?;
            table
                .remove(META_KEY_REINDEX_CHECKPOINT)
                .context("remove reindex_checkpoint")?;
        }
        txn.commit().context("commit _meta write txn")?;
        Ok(())
    }

    /// Bulk-copy all durable rows from `source` into `self` (issue #839).
    ///
    /// Why: the incremental reindex staging path opens a FRESH empty
    /// `index.redb.tmp`, then writes only the re-embedded (changed) files'
    /// chunks. Hash-skipped (unchanged) files are never committed to staging,
    /// so when the staging corpus is atomically promoted over the live
    /// `index.redb` the skipped files' chunks are gone — lost on the next
    /// daemon restart (durable data loss). The fix: before any batch writes,
    /// copy every row from the LIVE corpus into the fresh staging store. The
    /// batch loop then UPSERTS changed files' chunks, overwriting their
    /// pre-copied rows. The promoted corpus therefore contains ALL files:
    /// changed (fresh) + unchanged (copied).
    ///
    /// Tables copied: `CHUNKS_TABLE`, `ENTITIES_TABLE`, `FILE_HASHES_TABLE`,
    /// and `_meta` (indexed_root, schema_version).
    ///
    /// The KG tables split in two, and only one half is regenerable. The
    /// DERIVED tables (`kg_nodes`, `kg_edges`) are computed from the chunk
    /// corpus and genuinely rebuilt at the end of every reindex via
    /// `rebuild_symbol_graph_for_reindex` + `save_kg_graph`, so not copying
    /// them is correct. The CONTRIBUTED table (`kg_contrib`, ADR-0009) is not:
    /// it is written only by the external `POST /indexes/{id}/graph` ingest,
    /// which a reindex never calls, so nothing regenerates it and dropping it
    /// destroys data no part of this daemon can recover. It is carried across
    /// the swap by `CorpusStore::copy_contrib_from`, which
    /// `begin_staged_corpus_swap` calls on the force path as well as this one —
    /// reading an absent `kg_contrib` here as "it gets rebuilt" is what let
    /// every reindex silently discard every contribution while reporting
    /// success. See PR #5527.
    ///
    /// `META_KEY_REINDEX_CHECKPOINT` is likewise and deliberately NOT in the
    /// copied-key list (#3979): the checkpoint describes the run building
    /// *this* staging corpus, so inheriting one from the live corpus would
    /// mean a fresh staging file claiming to be resumable partial work.
    ///
    /// What: opens one read transaction on `source` and one write transaction
    /// on `self`, streams every row from the four core tables, and commits the
    /// write transaction once all rows have been inserted. Any row error or
    /// I/O failure is fatal and propagated immediately via `?` — a partial
    /// copy that gets promoted would be data loss, so all-or-nothing semantics
    /// are required. An empty `source` is a no-op (zero rows → commits an
    /// empty transaction).
    /// Test: `copy_all_from_seeds_staging_corpus` in `corpus::tests`.
    pub(crate) fn copy_all_from(&self, source: &CorpusStore) -> Result<()> {
        use crate::core::migration::{META_KEY_INDEXED_ROOT, META_KEY_SCHEMA_VERSION, META_TABLE};

        // Single read transaction on the source — consistent snapshot.
        let src_txn = source.db.begin_read().context("begin source read txn")?;

        // Single write transaction on self — everything goes in atomically.
        let dst_txn = self.db.begin_write().context("begin staging write txn")?;
        {
            // --- chunks ---
            let src_chunks = src_txn.open_table(CHUNKS_TABLE)?;
            let mut dst_chunks = dst_txn.open_table(CHUNKS_TABLE)?;
            for entry in src_chunks.iter().context("iterate source chunks")? {
                let (key, value) = entry.context("read source chunk row")?;
                dst_chunks
                    .insert(key.value(), value.value())
                    .with_context(|| format!("copy chunk row '{}'", key.value()))?;
            }
            drop(src_chunks);
            drop(dst_chunks);

            // --- entities ---
            let src_ents = src_txn.open_table(ENTITIES_TABLE)?;
            let mut dst_ents = dst_txn.open_table(ENTITIES_TABLE)?;
            for entry in src_ents.iter().context("iterate source entities")? {
                let (key, value) = entry.context("read source entity row")?;
                dst_ents
                    .insert(key.value(), value.value())
                    .with_context(|| format!("copy entity row '{}'", key.value()))?;
            }
            drop(src_ents);
            drop(dst_ents);

            // --- file hashes ---
            let src_hashes = match src_txn.open_table(FILE_HASHES_TABLE) {
                Ok(t) => Some(t),
                Err(redb::TableError::TableDoesNotExist(_)) => None,
                Err(e) => return Err(anyhow::anyhow!("open source file_hashes: {e}")),
            };
            if let Some(src_hashes) = src_hashes {
                let mut dst_hashes = dst_txn.open_table(FILE_HASHES_TABLE)?;
                for entry in src_hashes.iter().context("iterate source file_hashes")? {
                    let (key, value) = entry.context("read source file_hash row")?;
                    dst_hashes
                        .insert(key.value(), value.value())
                        .with_context(|| format!("copy file_hash row '{}'", key.value()))?;
                }
            }

            // --- _meta (indexed_root + schema_version) ---
            let src_meta = match src_txn.open_table(META_TABLE) {
                Ok(t) => Some(t),
                Err(redb::TableError::TableDoesNotExist(_)) => None,
                Err(e) => return Err(anyhow::anyhow!("open source _meta: {e}")),
            };
            if let Some(src_meta) = src_meta {
                let mut dst_meta = dst_txn.open_table(META_TABLE)?;
                // Copy only the two well-known meta keys — skip any unknown
                // future keys to stay forward-compatible.
                // #6171: `kg_graph_format_version` is deliberately absent from
                // this list, for the same reason `kg_nodes`/`kg_edges` are —
                // the rows it describes are not copied, and a stamp without
                // its rows would claim a format for a graph that is not there.
                // The end-of-reindex `save_kg_graph` writes both together.
                for key in &[META_KEY_INDEXED_ROOT, META_KEY_SCHEMA_VERSION] {
                    if let Some(val) = src_meta
                        .get(key)
                        .with_context(|| format!("read _meta[{key}]"))?
                    {
                        dst_meta
                            .insert(*key, val.value())
                            .with_context(|| format!("copy _meta[{key}]"))?;
                    }
                }
            }
        }
        dst_txn.commit().context("commit staging copy txn")?;
        tracing::info!(
            "corpus: copied {} chunks from live corpus into staging",
            self.chunk_count().unwrap_or(0),
        );
        Ok(())
    }
}
