//! [`CorpusStore`] struct definition and lifecycle methods (open / close).
//!
//! Why: the full corpus store implementation was split into focused sub-files
//! to stay under the 500-line cap while keeping each file's responsibility
//! clear. This file owns only struct construction and lifecycle — upsert/query
//! ops live in `corpus_ops`, KG persistence in `kg_ops`, and file-hash +
//! meta + bulk-copy in `meta_ops`.
//! What: defines [`CorpusStore`] and provides `open`, `open_fresh`, `path`.
//! Test: covered by the `tests` submodule.

use std::path::Path;

use anyhow::{Context, Result};
use redb::Database;

use super::tables::redb_cache_size_bytes;
use super::tables::{
    CHUNKS_TABLE, ENTITIES_TABLE, FILE_HASHES_TABLE, KG_COMMUNITIES_TABLE, KG_EDGES_REV_TABLE,
    KG_EDGES_TABLE, KG_NODES_TABLE, KG_SYMBOL_COMMUNITY_TABLE,
};
use crate::core::corpus_recovery::open_corpus_db_or_recreate;

/// Durable, redb-backed store for an index's chunk corpus + entity lists.
///
/// Why: see module docs — replaces the full-rewrite `chunks.json` snapshot
/// with an embedded transactional KV store so per-batch commits are O(batch)
/// and crash-safe.
/// What: owns a `redb::Database`; exposes batched upsert, full enumeration,
/// per-id/per-file deletion, and a count. Every mutating call is its own redb
/// write transaction, so a crash between calls never leaves a torn corpus.
/// Test: covered by the `tests` submodule.
pub struct CorpusStore {
    pub(super) db: Database,
    /// Filesystem path the `db` was opened at. Retained so the atomic
    /// `--force` reindex swap (issue #28, Phase 4) knows which file to rename
    /// without the caller having to pass the path back in.
    path: std::path::PathBuf,
}

impl CorpusStore {
    /// Open (creating if absent) the redb database at `path`.
    ///
    /// Why: the daemon resolves one `index.redb` per index under its data dir;
    /// opening here is the single entry point so table-creation and the
    /// create-if-missing semantics live in one place.
    /// What: opens the database via `Database::builder()` with an application
    /// page cache sized by [`redb_cache_size_bytes`] (default
    /// `DEFAULT_REDB_CACHE_MB` MB, overridable via `TRUSTY_REDB_CACHE_MB`),
    /// then runs a no-op write transaction that `open_table`s both tables so
    /// they exist before any reader runs (redb requires a table to have been
    /// created in a committed write txn before it can be opened read-only).
    /// This single builder call is the only place a corpus `redb::Database` is
    /// opened, so the cache size applies to the live `index.redb` and the
    /// `--force` staging `index.redb.tmp` alike (`open_fresh` delegates here).
    /// The effective cache size is logged at `info` so operators can confirm
    /// the resolved value at daemon startup.
    /// Test: `roundtrip` and `missing_db_is_empty` both exercise `open`.
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create parent of {}", path.display()))?;
        }
        let cache_bytes = redb_cache_size_bytes();
        tracing::info!(
            "corpus: opening {} with redb page cache = {} MB \
             (set TRUSTY_REDB_CACHE_MB to override)",
            path.display(),
            cache_bytes / (1024 * 1024),
        );
        // Issue #702: recovery-aware open — a stale redb-2.x `index.redb` is
        // moved aside and replaced with a fresh empty corpus (warm-boot reindex).
        let db = open_corpus_db_or_recreate(path, cache_bytes)?;
        // Materialize both tables in a committed write txn so later read-only
        // transactions can `open_table` them even on a brand-new database.
        {
            let txn = db.begin_write().context("begin corpus init txn")?;
            {
                txn.open_table(CHUNKS_TABLE).context("init chunks table")?;
                txn.open_table(ENTITIES_TABLE)
                    .context("init entities table")?;
                // Issue #41 phase 2: materialize the KG persistence tables
                // alongside the chunk/entity tables so warm-boot reads never
                // race a missing-table error on a fresh database.
                txn.open_table(KG_NODES_TABLE)
                    .context("init kg_nodes table")?;
                txn.open_table(KG_EDGES_TABLE)
                    .context("init kg_edges table")?;
                txn.open_table(KG_EDGES_REV_TABLE)
                    .context("init kg_edges_rev table")?;
                // Issue #41 phase 3: materialize the community persistence
                // tables alongside the KG tables so warm-boot reads never race
                // a missing-table error on a fresh database.
                txn.open_table(KG_COMMUNITIES_TABLE)
                    .context("init kg_communities table")?;
                txn.open_table(KG_SYMBOL_COMMUNITY_TABLE)
                    .context("init kg_symbol_community table")?;
                // Issue #662: materialize the file-hash persistence table so
                // warm-start reads never race a missing-table error.
                txn.open_table(FILE_HASHES_TABLE)
                    .context("init file_hashes table")?;
                // Migration framework: materialize `_meta` so the schema-version
                // read never races a missing-table error on fresh databases.
                txn.open_table(crate::core::migration::META_TABLE)
                    .context("init _meta table")?;
            }
            txn.commit().context("commit corpus init txn")?;
        }
        Ok(Self {
            db,
            path: path.to_path_buf(),
        })
    }

    /// Open a fresh (truncated) redb corpus at `path`, discarding any existing
    /// file first.
    ///
    /// Why: the `--force` reindex (issue #28, Phase 4) stages the rebuilt
    /// corpus in `index.redb.tmp`. A stale `.tmp` left behind by a previously
    /// aborted reindex must not contribute pre-existing rows to the new staged
    /// corpus — the staged file must reflect *only* this reindex's output so
    /// the post-reindex atomic rename produces a corpus identical to a clean
    /// rebuild.
    /// What: best-effort removes any file already at `path`, then delegates to
    /// [`Self::open`]. A `NotFound` removal error is ignored (nothing to
    /// clear); any other removal error is surfaced.
    /// Test: `tests::test_force_reindex_atomic_corpus_swap`.
    pub fn open_fresh(path: &Path) -> Result<Self> {
        match std::fs::remove_file(path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                return Err(e)
                    .with_context(|| format!("clear stale staging corpus at {}", path.display()))
            }
        }
        Self::open(path)
    }

    /// Filesystem path this store's database was opened at.
    ///
    /// Why: the atomic `--force` reindex swap needs to know the staging file's
    /// path to rename it over the live `index.redb`, and the caller would
    /// otherwise have to thread the path alongside every `Arc<CorpusStore>`.
    /// What: returns the stored `PathBuf`.
    /// Test: `tests::test_force_reindex_atomic_corpus_swap` asserts the path.
    pub fn path(&self) -> &Path {
        &self.path
    }
}
