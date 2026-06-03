//! redb-backed durable chunk corpus (issue #28).
//!
//! Why: prior to this module the chunk corpus was persisted as a single
//! `chunks.json` file rewritten in full after every committed batch. On a
//! 200k-chunk corpus that JSON blob is ~400 MB; serializing it on every batch
//! commit (a reindex emits one commit per 128 files) caused the
//! memory-explosion documented in `PersistState` and forced a full re-read of
//! the entire file into a `HashMap` on every daemon restart. redb gives us:
//!   * crash-safe, atomic per-batch commits (no half-written file window),
//!   * O(batch) incremental writes instead of O(corpus) full rewrites,
//!   * the option to stream chunks back at startup without holding two copies
//!     (the JSON `Vec<RawChunk>` plus the live `HashMap`) in RAM at once.
//!
//! What: [`CorpusStore`] wraps a `redb::Database` with two tables — one keyed
//! by `chunk_id` holding the serialized [`RawChunk`], one keyed by file path
//! holding the serialized per-file [`RawEntity`] list. Values are serialized
//! with `serde_json` (already a workspace dependency; no new crate, and the
//! human-readable form keeps `redb` dumps debuggable).
//!
//! Test: see the `tests` submodule — `roundtrip` writes chunks + entities and
//! reads them back into a fresh store; `missing_db_is_empty` covers the
//! first-run / post-upgrade fallback; `delete_removes_chunk` covers eviction.

use std::path::Path;

use anyhow::{Context, Result};
use redb::{Database, ReadableDatabase, ReadableTable, ReadableTableMetadata, TableDefinition};

use crate::core::chunker::RawChunk;
use crate::core::corpus_recovery::open_corpus_db_or_recreate;
use crate::core::entity::RawEntity;

/// Default application-level page cache size for the redb corpus database, in
/// megabytes (64 MB).
///
/// Why (B.2 quick-win, issue #329): redb treats `set_cache_size` as a *ceiling*
/// that fills lazily as pages are touched. Empirical profiling of the
/// trusty-tools corpus (23,513 chunks) showed the actual redb working set is
/// ~87 MB: a clean 512 MB cap run peaked at 557 MB RSS while an 8 MB cap run
/// peaked at 470 MB — a difference of exactly 87 MB. The 512 MB ceiling was
/// massively over-provisioned; 64 MB captures the full working set with ~27 MB
/// of headroom for B-tree internal nodes and future corpus growth without the
/// 33% indexing speed penalty observed at 8 MB (where I/O pressure becomes the
/// bottleneck). Lowering from 512 → 64 MB saves ~87 MB of peak RSS during a
/// force reindex. The previous value of 512 MB was the "smaller-than-16-GiB"
/// step from the original hardcoded 16 GiB; this is the next measured step.
/// The trade-off is explicit: a *larger* cache means fewer disk reads for warm
/// queries against a big corpus; a *smaller* cache means lower idle RSS at the
/// cost of more page faults on cold reads. Operators on large-corpus hosts can
/// raise it via `TRUSTY_REDB_CACHE_MB`.
/// What: 64, multiplied by 1 MiB in [`redb_cache_size_bytes`].
/// Test: `redb_cache_size_default_and_env_override` covers default + override.
const DEFAULT_REDB_CACHE_MB: usize = 64;

/// Resolve the redb application page-cache size (in bytes) from the
/// environment, falling back to [`DEFAULT_REDB_CACHE_MB`].
///
/// Why: the cache size is the single biggest lever on the daemon's idle RSS
/// (see [`DEFAULT_REDB_CACHE_MB`]). Making it configurable lets operators tune
/// the warm-query-latency vs. idle-memory trade-off per host without a
/// recompile — large-corpus hosts raise it, memory-constrained dev machines
/// keep the small default.
/// What: reads `TRUSTY_REDB_CACHE_MB`, parses it as `usize` megabytes, and
/// returns the value in bytes (`mb * 1024 * 1024`). Falls back to
/// [`DEFAULT_REDB_CACHE_MB`] when the var is unset, empty, unparseable, or
/// zero, logging a `warn` on a non-empty unparseable value so typos surface.
/// Test: `redb_cache_size_default_and_env_override`.
fn redb_cache_size_bytes() -> usize {
    let mb = match std::env::var("TRUSTY_REDB_CACHE_MB") {
        Ok(v) if !v.is_empty() => match v.parse::<usize>() {
            Ok(n) if n > 0 => n,
            Ok(_) => DEFAULT_REDB_CACHE_MB,
            Err(_) => {
                tracing::warn!(
                    "corpus: TRUSTY_REDB_CACHE_MB={v:?} is not a valid usize; \
                     using default ({DEFAULT_REDB_CACHE_MB} MB)"
                );
                DEFAULT_REDB_CACHE_MB
            }
        },
        _ => DEFAULT_REDB_CACHE_MB,
    };
    mb * 1024 * 1024
}

/// redb table holding the serialized chunk corpus, keyed by `chunk_id`.
///
/// Why: `chunk_id` (`"{path}:{start}:{end}"`) is the corpus's natural primary
/// key — it is collision-safe and is exactly what the in-memory `HashMap` is
/// keyed by, so a redb row maps 1:1 onto a `HashMap` entry.
/// What: `&str → &[u8]` where the value is `serde_json`-encoded [`RawChunk`].
const CHUNKS_TABLE: TableDefinition<&str, &[u8]> = TableDefinition::new("chunks");

/// redb table holding the per-file entity lists, keyed by file path.
///
/// Why: `entities` are needed to rebuild the symbol graph on warm-boot and are
/// derived per file, so the file path is the natural key.
/// What: `&str → &[u8]` where the value is `serde_json`-encoded
/// `Vec<RawEntity>`.
const ENTITIES_TABLE: TableDefinition<&str, &[u8]> = TableDefinition::new("entities");

/// redb table holding the persisted `SymbolGraph` nodes (issue #41 phase 2).
///
/// Why: cold-start graph rebuild from the chunk corpus is O(N chunks) and
/// loses Phase B/C edges. Persisting the graph adjacency lists alongside the
/// chunk corpus lets warm-boot rehydrate the KG in O(nodes + edges) without
/// re-running `build_from_chunks`.
/// What: `symbol → &[u8]` where the value is `serde_json`-encoded
/// [`PersistedKgNode`] (carries `chunk_id` + `file` for round-trip equality).
pub(crate) const KG_NODES_TABLE: TableDefinition<&str, &[u8]> = TableDefinition::new("kg_nodes");

/// redb table holding the forward (source → targets) KG adjacency list.
///
/// Why: BFS expansion walks outgoing edges by symbol; storing the full edge
/// list under the source key gives O(1) load of all outgoing edges per node.
/// What: `source_symbol → &[u8]` where the value is `serde_json`-encoded
/// `Vec<(EdgeKind, target_symbol)>`. One row per source symbol; empty
/// adjacency lists are omitted.
pub(crate) const KG_EDGES_TABLE: TableDefinition<&str, &[u8]> = TableDefinition::new("kg_edges");

/// redb table holding the reverse (target → sources) KG adjacency list.
///
/// Why: `callers_of` expansions walk *incoming* edges by symbol; a separate
/// reverse adjacency keeps that lookup O(1) instead of forcing a full
/// forward-edge scan.
/// What: `target_symbol → &[u8]` where the value is `serde_json`-encoded
/// `Vec<(EdgeKind, source_symbol)>`.
pub(crate) const KG_EDGES_REV_TABLE: TableDefinition<&str, &[u8]> =
    TableDefinition::new("kg_edges_rev");

/// redb table persisting per-file SHA-256 content hashes for the skip-unchanged
/// optimisation (issue #662).
///
/// Why: the in-process `file_hashes()` DashMap survives across multiple
/// `POST /reindex` calls but not daemon restarts — a cold-start re-embeds
/// every file even when nothing changed. Storing the same map in redb means
/// a warm daemon restart loads the hashes from the previous run's successful
/// commit and can skip unchanged files immediately.
///
/// Atomicity: this table lives in the SAME redb file as the chunk corpus.
/// When staging is active (#603) it is written to `index.redb.tmp`
/// alongside every batch's chunks.  The atomic rename on commit promotes
/// hashes and chunks together; a rollback discards them together.  Hashes
/// therefore never get out of sync with the committed chunks.
///
/// What: `relative_path (str) → sha256_hex (str as bytes)`.  Paths are
/// relative to the index root (consistent with the #602 portable-path
/// work) so a moved project doesn't false-skip.
pub(crate) const FILE_HASHES_TABLE: TableDefinition<&str, &[u8]> =
    TableDefinition::new("file_hashes");

/// redb table holding persisted Louvain community records (migration tolerance).
///
/// Why: kept for backward-compat with on-disk indexes created before v0.10.0
/// (issues #41 / #152). The Louvain community detection and `community_cohesion`
/// ranking were removed in v0.10.0 (PROVENANCE-ONLY decision, issue #145).
/// This table definition is retained so the redb schema initialisation does not
/// fail when opening old databases that already have the table.
/// What: `community_id (u64) → &[u8]` (was serde_json-encoded CommunityRecord).
/// The table is no longer written or read by the active search path.
pub(crate) const KG_COMMUNITIES_TABLE: TableDefinition<u64, &[u8]> =
    TableDefinition::new("kg_communities");

/// redb table mapping symbol → community id (migration tolerance).
///
/// Why: same as `KG_COMMUNITIES_TABLE` — retained to avoid schema errors on
/// old indexes. Not written or read by the active search path as of v0.10.0.
/// What: `symbol (str) → community_id (u64)`.
pub(crate) const KG_SYMBOL_COMMUNITY_TABLE: TableDefinition<&str, u64> =
    TableDefinition::new("kg_symbol_community");

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
    db: Database,
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
    /// [`DEFAULT_REDB_CACHE_MB`] MB, overridable via `TRUSTY_REDB_CACHE_MB`),
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
    /// Why: issue #28 deferred item — the search hot path used to materialize
    /// top-k results by joining fused `(id, score)` pairs against an in-memory
    /// `HashMap<String, RawChunk>` that held *every* chunk's text resident in
    /// the heap permanently (~45 GB RSS on a large monorepo). Reading the
    /// top-k chunk text straight out of redb at materialization time lets the
    /// daemon drop that HashMap from the query path entirely: redb's values are
    /// mmap-backed, so a point lookup is served from the OS page cache rather
    /// than process heap, cutting steady-state RSS to <10 GB. A typical
    /// `top_k=20` query does 20 point reads inside one read transaction —
    /// each is an O(log n) B-tree descent over an mmap'd file, well within the
    /// sub-10 ms query budget.
    /// What: opens a single redb read transaction and fetches each requested
    /// id. Missing ids are skipped (not an error) — a fused id with no redb row
    /// is almost always a benign race against a concurrent removal, and one
    /// missing chunk must not fail the whole query. A corrupt row is likewise
    /// skipped with a `warn`. The returned `Vec` preserves the input `ids`
    /// order for the ids that were found.
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

    /// Replace the persisted KG node set + forward/reverse adjacency lists in
    /// one atomic transaction (issue #41 phase 2).
    ///
    /// Why: persisting the symbol graph alongside the chunk corpus lets
    /// warm-boot skip the full `build_from_chunks` rebuild. Doing the whole
    /// write under one transaction guarantees readers never observe a
    /// half-rewritten graph.
    /// What: clears the three KG tables then re-inserts the supplied nodes and
    /// forward/reverse adjacencies. Each value is `serde_json`-encoded. An
    /// `(adj_fwd, adj_rev)` row whose vector is empty is skipped to keep the
    /// stored graph minimal.
    /// Test: `save_load_kg_roundtrip` round-trips a synthetic graph through
    /// `save_kg_graph` + `load_kg_graph` and asserts equality.
    pub fn save_kg_graph(
        &self,
        nodes: &[(String, PersistedKgNode)],
        adj_fwd: &[(String, Vec<(String, String)>)],
        adj_rev: &[(String, Vec<(String, String)>)],
    ) -> Result<()> {
        let txn = self.db.begin_write().context("begin kg graph upsert txn")?;
        {
            let mut nodes_tbl = txn.open_table(KG_NODES_TABLE)?;
            // Drain stale rows first so a shrinking graph doesn't leave orphans.
            nodes_tbl.retain(|_, _| false).context("clear kg_nodes")?;
            for (symbol, node) in nodes {
                let bytes = serde_json::to_vec(node)
                    .with_context(|| format!("serialize kg node {symbol}"))?;
                nodes_tbl
                    .insert(symbol.as_str(), bytes.as_slice())
                    .with_context(|| format!("insert kg node {symbol}"))?;
            }

            let mut fwd_tbl = txn.open_table(KG_EDGES_TABLE)?;
            fwd_tbl.retain(|_, _| false).context("clear kg_edges")?;
            for (src, targets) in adj_fwd {
                if targets.is_empty() {
                    continue;
                }
                let bytes = serde_json::to_vec(targets)
                    .with_context(|| format!("serialize kg fwd adjacency for {src}"))?;
                fwd_tbl
                    .insert(src.as_str(), bytes.as_slice())
                    .with_context(|| format!("insert kg fwd adjacency for {src}"))?;
            }

            let mut rev_tbl = txn.open_table(KG_EDGES_REV_TABLE)?;
            rev_tbl.retain(|_, _| false).context("clear kg_edges_rev")?;
            for (tgt, sources) in adj_rev {
                if sources.is_empty() {
                    continue;
                }
                let bytes = serde_json::to_vec(sources)
                    .with_context(|| format!("serialize kg rev adjacency for {tgt}"))?;
                rev_tbl
                    .insert(tgt.as_str(), bytes.as_slice())
                    .with_context(|| format!("insert kg rev adjacency for {tgt}"))?;
            }
        }
        txn.commit().context("commit kg graph upsert txn")?;
        Ok(())
    }

    /// Load the persisted symbol graph (issue #41 phase 2).
    ///
    /// Why: warm-boot wants to bring the KG back online without paying the
    /// `build_from_chunks` cost. Returning the raw node + adjacency lists lets
    /// the caller (`SymbolGraph::load_from_corpus`) rebuild the in-memory
    /// `petgraph` without re-touching the chunk corpus.
    /// What: returns `(nodes, adj_fwd, adj_rev)` where each list is the
    /// deserialized contents of the three KG tables. An empty (or fresh)
    /// database yields three empty vectors. Corrupt rows are skipped with a
    /// `warn` rather than failing the whole load.
    /// Test: `save_load_kg_roundtrip`.
    #[allow(clippy::type_complexity)]
    pub fn load_kg_graph(
        &self,
    ) -> Result<(
        Vec<(String, PersistedKgNode)>,
        Vec<(String, Vec<(String, String)>)>,
        Vec<(String, Vec<(String, String)>)>,
    )> {
        let txn = self.db.begin_read().context("begin kg graph read txn")?;

        let mut nodes: Vec<(String, PersistedKgNode)> = Vec::new();
        {
            let nodes_tbl = txn.open_table(KG_NODES_TABLE)?;
            for entry in nodes_tbl.iter().context("iterate kg_nodes table")? {
                let (key, value) = entry.context("read kg_nodes row")?;
                let symbol = key.value().to_string();
                match serde_json::from_slice::<PersistedKgNode>(value.value()) {
                    Ok(node) => nodes.push((symbol, node)),
                    Err(e) => tracing::warn!("kg: skipping corrupt kg_nodes row '{symbol}' ({e})"),
                }
            }
        }

        let adj_fwd = load_adjacency(&txn, KG_EDGES_TABLE, "kg_edges")?;
        let adj_rev = load_adjacency(&txn, KG_EDGES_REV_TABLE, "kg_edges_rev")?;
        Ok((nodes, adj_fwd, adj_rev))
    }

    /// Number of persisted KG nodes currently stored.
    ///
    /// Why: warm-boot uses this as a cheap "is the persisted graph populated?"
    /// probe before deciding whether to fall back to `build_from_chunks`.
    /// What: returns the row count of `KG_NODES_TABLE`.
    /// Test: covered by `save_load_kg_roundtrip` (asserts count after save).
    pub fn kg_node_count(&self) -> Result<usize> {
        let txn = self.db.begin_read().context("begin kg count txn")?;
        let table = txn.open_table(KG_NODES_TABLE)?;
        Ok(table.len().context("count kg_nodes")? as usize)
    }

    /// Replace the persisted community records + symbol→community map (migration
    /// tolerance, not called by the active search path as of v0.10.0).
    ///
    /// Why: retained so old tooling that still calls this (e.g. test helpers,
    /// migration utilities) compiles. The Louvain pipeline was removed in
    /// v0.10.0 (issue #152); this method is no longer called by the daemon.
    /// What: clears the two migration-tolerance community tables then re-inserts
    /// the supplied records and per-symbol mappings in one atomic transaction.
    /// Test: `save_load_communities_roundtrip` round-trips a synthetic partition.
    pub fn save_communities(
        &self,
        records: &[(u64, Vec<u8>)],
        symbol_to_community: &[(String, u64)],
    ) -> Result<()> {
        let txn = self
            .db
            .begin_write()
            .context("begin communities upsert txn")?;
        {
            let mut comm_tbl = txn.open_table(KG_COMMUNITIES_TABLE)?;
            comm_tbl
                .retain(|_, _| false)
                .context("clear kg_communities")?;
            for (id, bytes) in records {
                comm_tbl
                    .insert(id, bytes.as_slice())
                    .with_context(|| format!("insert community {id}"))?;
            }
            let mut sym_tbl = txn.open_table(KG_SYMBOL_COMMUNITY_TABLE)?;
            sym_tbl
                .retain(|_, _| false)
                .context("clear kg_symbol_community")?;
            for (sym, id) in symbol_to_community {
                sym_tbl
                    .insert(sym.as_str(), id)
                    .with_context(|| format!("insert symbol→community for {sym}"))?;
            }
        }
        txn.commit().context("commit communities upsert txn")?;
        Ok(())
    }

    /// Load persisted community records (migration tolerance, not called by
    /// the active search path as of v0.10.0).
    ///
    /// Why: retained for parity with `save_communities` so old code that calls
    /// both still compiles. The `/communities` HTTP endpoint was removed in
    /// v0.10.0 (issue #152).
    /// What: returns `Vec<(community_id, serialized_record_bytes)>` from the
    /// migration-tolerance `kg_communities` redb table.
    /// Test: `save_load_communities_roundtrip`.
    pub fn load_communities(&self) -> Result<Vec<(u64, Vec<u8>)>> {
        let txn = self.db.begin_read().context("begin communities read txn")?;
        let table = txn.open_table(KG_COMMUNITIES_TABLE)?;
        let mut out: Vec<(u64, Vec<u8>)> = Vec::new();
        for entry in table.iter().context("iterate kg_communities table")? {
            let (key, value) = entry.context("read kg_communities row")?;
            out.push((key.value(), value.value().to_vec()));
        }
        Ok(out)
    }

    /// Upsert a batch of relative-path → SHA-256-hex entries into the
    /// persistent file-hash table (issue #662).
    ///
    /// Why: called after every successful batch commit so the in-process
    /// content-hash cache is mirrored to redb.  Together with
    /// [`Self::load_file_hashes`] this means a daemon restart loads the last
    /// run's hashes and can skip unchanged files without re-embedding them.
    /// The caller enforces the eviction cap BEFORE calling this method.
    /// What: opens a single write transaction, upserts every entry, and
    /// commits.  Empty input is a no-op (no transaction opened).
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
    /// previous run's committed hashes.  After loading, unchanged files are
    /// skipped immediately — no cold-start re-embed.
    /// What: opens a read transaction, walks `FILE_HASHES_TABLE`, and returns
    /// every entry as `(path_string, hash_string)` pairs.  Corrupt rows are
    /// skipped with a `warn`.  Returns empty when the table is absent (first
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
    /// persisted hashes are cleared alongside the in-process cache.  Without
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

    /// Look up the community id for a single symbol (migration tolerance, not
    /// called by the active search path as of v0.10.0).
    ///
    /// Why: retained for parity with `save_communities` / `load_communities`
    /// so any surviving callers compile. Community id lookups were removed from
    /// the search materialisation path in v0.10.0 (issue #152).
    /// What: returns `Ok(Some(id))` when the symbol has an entry in the legacy
    /// `kg_symbol_community` table; `Ok(None)` otherwise.
    /// Test: `save_load_communities_roundtrip` asserts point reads.
    pub fn symbol_community(&self, symbol: &str) -> Result<Option<u64>> {
        let txn = self
            .db
            .begin_read()
            .context("begin symbol_community read txn")?;
        let table = txn.open_table(KG_SYMBOL_COMMUNITY_TABLE)?;
        Ok(table
            .get(symbol)
            .context("get symbol_community row")?
            .map(|v| v.value()))
    }

    /// Read the `schema_version` entry from the `_meta` table (migration
    /// framework, issue #migration).
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
    /// framework, issue #migration).
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

    /// Read the canonical root path the corpus's chunk `file` fields are stored
    /// relative to (#602).
    ///
    /// Why: the reindex orchestrator compares this against the current root to
    /// decide whether a move occurred between reindex runs and the stored paths
    /// must be re-relativized. Returning `None` for a legacy / never-stamped
    /// corpus means "unknown prior root" — the caller treats that as a
    /// first-ever reindex (no forced rewrite).
    /// What: opens a read transaction on `_meta`, looks up
    /// `META_KEY_INDEXED_ROOT`, and decodes the UTF-8 path string. Returns
    /// `None` when the table or key is absent or the bytes are not valid UTF-8.
    /// Test: `test_meta_indexed_root_roundtrip` in `corpus::tests`.
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
                Err(_) => Ok(None),
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
}

/// Iterate one of the KG adjacency tables and deserialize each row.
///
/// Why: `KG_EDGES_TABLE` and `KG_EDGES_REV_TABLE` have identical shapes
/// (`symbol → Vec<(edge_kind, peer_symbol)>`); centralising the read avoids
/// duplicating the corrupt-row tolerance and `serde_json` decode boilerplate.
/// What: walks the table on the supplied read transaction and returns a
/// `Vec<(key, adjacency)>`. Corrupt rows are logged at `warn` and skipped.
/// Test: covered transitively by `save_load_kg_roundtrip`.
#[allow(clippy::type_complexity)]
fn load_adjacency(
    txn: &redb::ReadTransaction,
    table_def: TableDefinition<'_, &str, &[u8]>,
    label: &str,
) -> Result<Vec<(String, Vec<(String, String)>)>> {
    let table = txn.open_table(table_def)?;
    let mut out: Vec<(String, Vec<(String, String)>)> = Vec::new();
    for entry in table
        .iter()
        .with_context(|| format!("iterate {label} table"))?
    {
        let (key, value) = entry.with_context(|| format!("read {label} row"))?;
        let sym = key.value().to_string();
        match serde_json::from_slice::<Vec<(String, String)>>(value.value()) {
            Ok(adj) => out.push((sym, adj)),
            Err(e) => tracing::warn!("kg: skipping corrupt {label} row '{sym}' ({e})"),
        }
    }
    Ok(out)
}

/// Compact on-disk representation of a [`crate::core::symbol_graph::SymbolNode`]
/// (issue #41 phase 2).
///
/// Why: the runtime `SymbolNode` carries the symbol name three times (as the
/// `petgraph` node weight, the `by_symbol` map key, and inside the node
/// itself). Storing only `chunk_id + file` (with the symbol implied by the
/// row key) keeps the on-disk size lean and avoids a String redundancy.
/// What: serde-derived JSON payload stored under `KG_NODES_TABLE[symbol]`.
/// Test: covered by `save_load_kg_roundtrip` in this module and by the
/// `SymbolGraph` round-trip test in `core::symbol_graph::tests`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct PersistedKgNode {
    pub chunk_id: String,
    pub file: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::chunker::{ChunkType, RawChunk};

    /// Build a minimal `RawChunk` for tests.
    fn raw(id: &str, content: &str) -> RawChunk {
        RawChunk {
            id: id.to_string(),
            file: "src/lib.rs".to_string(),
            start_line: 1,
            end_line: 1,
            content: content.to_string(),
            function_name: None,
            language: Some("rust".to_string()),
            chunk_type: ChunkType::Code,
            calls: Vec::new(),
            inherits_from: Vec::new(),
            chunk_depth: 0,
            parent_chunk_id: None,
            child_chunk_ids: Vec::new(),
            nlp_keywords: Vec::new(),
            nlp_code_refs: Vec::new(),
            virtual_terms: Vec::new(),
        }
    }

    #[test]
    fn redb_cache_size_default_and_env_override() {
        // Idle-memory audit: the redb page cache defaults to 64 MB (issue #329
        // B.2 quick-win; was 512 MB before empirical profiling confirmed actual
        // fill of ~87 MB) and is overridable via TRUSTY_REDB_CACHE_MB. This test
        // mutates a process-global env var, so it is intentionally self-contained
        // (save/restore the prior value) — no other test in this module reads
        // TRUSTY_REDB_CACHE_MB.
        let prior = std::env::var("TRUSTY_REDB_CACHE_MB").ok();

        // Default: unset → 64 MB.
        // SAFETY: corpus tests do not mutate this env var concurrently.
        unsafe { std::env::remove_var("TRUSTY_REDB_CACHE_MB") };
        assert_eq!(redb_cache_size_bytes(), DEFAULT_REDB_CACHE_MB * 1024 * 1024);

        // Valid override wins.
        // SAFETY: see above.
        unsafe { std::env::set_var("TRUSTY_REDB_CACHE_MB", "1024") };
        assert_eq!(redb_cache_size_bytes(), 1024 * 1024 * 1024);

        // Zero falls back to the default.
        // SAFETY: see above.
        unsafe { std::env::set_var("TRUSTY_REDB_CACHE_MB", "0") };
        assert_eq!(redb_cache_size_bytes(), DEFAULT_REDB_CACHE_MB * 1024 * 1024);

        // Garbage falls back to the default (with a warn).
        // SAFETY: see above.
        unsafe { std::env::set_var("TRUSTY_REDB_CACHE_MB", "not-a-number") };
        assert_eq!(redb_cache_size_bytes(), DEFAULT_REDB_CACHE_MB * 1024 * 1024);

        // Restore.
        // SAFETY: see above.
        unsafe {
            match prior {
                Some(v) => std::env::set_var("TRUSTY_REDB_CACHE_MB", v),
                None => std::env::remove_var("TRUSTY_REDB_CACHE_MB"),
            }
        }
    }

    #[test]
    fn roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let store = CorpusStore::open(&dir.path().join("index.redb")).unwrap();

        let chunks = vec![raw("a:1:1", "fn a() {}"), raw("b:1:1", "fn b() {}")];
        store.upsert_chunks(&chunks).unwrap();
        store
            .upsert_entities(&[("src/lib.rs".to_string(), Vec::new())])
            .unwrap();
        assert_eq!(store.chunk_count().unwrap(), 2);

        // Reopen to simulate a daemon restart.
        drop(store);
        let store = CorpusStore::open(&dir.path().join("index.redb")).unwrap();
        let mut loaded = store.load_all_chunks().unwrap();
        loaded.sort_by(|x, y| x.id.cmp(&y.id));
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].id, "a:1:1");
        assert_eq!(loaded[0].content, "fn a() {}");

        let entities = store.load_all_entities().unwrap();
        assert_eq!(entities.len(), 1);
        assert_eq!(entities[0].0, "src/lib.rs");
    }

    #[test]
    fn batch_upsert_is_atomic_roundtrip() {
        // Issue #29: `upsert_batch` writes chunks + entities in one redb
        // transaction. A reopened store must see both, exactly as the
        // separate-call `roundtrip` test asserts for `upsert_chunks` /
        // `upsert_entities`.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("index.redb");
        {
            let store = CorpusStore::open(&path).unwrap();
            store
                .upsert_batch(
                    &[raw("a:1:1", "fn a() {}"), raw("b:1:1", "fn b() {}")],
                    &[("src/lib.rs".to_string(), Vec::new())],
                )
                .unwrap();
            assert_eq!(store.chunk_count().unwrap(), 2);
        }
        // Reopen to simulate a daemon restart — both tables must be intact.
        let store = CorpusStore::open(&path).unwrap();
        let mut loaded = store.load_all_chunks().unwrap();
        loaded.sort_by(|x, y| x.id.cmp(&y.id));
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].id, "a:1:1");
        let entities = store.load_all_entities().unwrap();
        assert_eq!(entities.len(), 1);
        assert_eq!(entities[0].0, "src/lib.rs");

        // A batch with only chunks still writes the chunks table.
        store
            .upsert_batch(&[raw("c:1:1", "fn c() {}")], &[])
            .unwrap();
        assert_eq!(store.chunk_count().unwrap(), 3);

        // A batch with only entities still writes the entities table.
        store
            .upsert_batch(&[], &[("src/other.rs".to_string(), Vec::new())])
            .unwrap();
        assert_eq!(store.load_all_entities().unwrap().len(), 2);

        // A fully-empty batch is a silent no-op.
        store.upsert_batch(&[], &[]).unwrap();
        assert_eq!(store.chunk_count().unwrap(), 3);
    }

    #[test]
    fn get_chunks_batch_reads_subset() {
        // Issue #28 deferred item: the query hot path materializes top-k
        // results via `get_chunks`. It must return only the requested ids, in
        // input order, and silently skip ids absent from the corpus.
        let dir = tempfile::tempdir().unwrap();
        let store = CorpusStore::open(&dir.path().join("index.redb")).unwrap();
        store
            .upsert_chunks(&[
                raw("a:1:1", "fn a() {}"),
                raw("b:1:1", "fn b() {}"),
                raw("c:1:1", "fn c() {}"),
            ])
            .unwrap();

        // Request a subset out of corpus order, with one unknown id mixed in.
        let got = store
            .get_chunks(&["c:1:1", "missing:0:0", "a:1:1"])
            .unwrap();
        assert_eq!(got.len(), 2, "unknown id must be skipped, not error");
        assert_eq!(got[0].id, "c:1:1", "input order must be preserved");
        assert_eq!(got[0].content, "fn c() {}");
        assert_eq!(got[1].id, "a:1:1");

        // Empty input is a no-op.
        assert!(store.get_chunks(&[]).unwrap().is_empty());

        // All-missing input yields an empty vec, never an error.
        assert!(store.get_chunks(&["nope:0:0"]).unwrap().is_empty());
    }

    #[test]
    fn missing_db_is_empty() {
        // A brand-new database (post-upgrade / first-run) must open cleanly
        // and report an empty corpus rather than erroring.
        let dir = tempfile::tempdir().unwrap();
        let store = CorpusStore::open(&dir.path().join("fresh.redb")).unwrap();
        assert_eq!(store.chunk_count().unwrap(), 0);
        assert!(store.load_all_chunks().unwrap().is_empty());
        assert!(store.load_all_entities().unwrap().is_empty());
    }

    /// Why: #602 — the reindex orchestrator persists the canonical root the
    /// corpus was relativized against so a later run can detect a move. Verify
    /// the read returns `None` before any write and the written value round-trips.
    /// Test: this test.
    #[test]
    fn test_meta_indexed_root_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let store = CorpusStore::open(&dir.path().join("index.redb")).unwrap();
        // Never written → None (legacy / first reindex).
        assert_eq!(store.read_indexed_root_sync().unwrap(), None);

        let root = std::path::PathBuf::from("/Users/me/code/project");
        store.write_indexed_root_sync(&root).unwrap();
        assert_eq!(store.read_indexed_root_sync().unwrap(), Some(root.clone()));

        // Overwrite with a new root (the index moved on disk).
        let moved = std::path::PathBuf::from("/mnt/serving/project");
        store.write_indexed_root_sync(&moved).unwrap();
        assert_eq!(store.read_indexed_root_sync().unwrap(), Some(moved));
    }

    #[test]
    fn delete_removes_chunk() {
        let dir = tempfile::tempdir().unwrap();
        let store = CorpusStore::open(&dir.path().join("index.redb")).unwrap();
        store
            .upsert_chunks(&[raw("a:1:1", "x"), raw("b:1:1", "y")])
            .unwrap();
        store.delete_chunks(&["a:1:1".to_string()]).unwrap();
        assert_eq!(store.chunk_count().unwrap(), 1);
        let loaded = store.load_all_chunks().unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].id, "b:1:1");
        // Deleting an unknown id is a silent no-op.
        store.delete_chunks(&["nope:0:0".to_string()]).unwrap();
        assert_eq!(store.chunk_count().unwrap(), 1);
    }

    #[test]
    fn empty_batches_are_noops() {
        let dir = tempfile::tempdir().unwrap();
        let store = CorpusStore::open(&dir.path().join("index.redb")).unwrap();
        store.upsert_chunks(&[]).unwrap();
        store.upsert_entities(&[]).unwrap();
        store.delete_chunks(&[]).unwrap();
        assert_eq!(store.chunk_count().unwrap(), 0);
    }

    #[test]
    fn delete_entities_removes_file_row() {
        let dir = tempfile::tempdir().unwrap();
        let store = CorpusStore::open(&dir.path().join("index.redb")).unwrap();
        store
            .upsert_entities(&[
                ("src/a.rs".to_string(), Vec::new()),
                ("src/b.rs".to_string(), Vec::new()),
            ])
            .unwrap();
        assert_eq!(store.load_all_entities().unwrap().len(), 2);
        store.delete_entities("src/a.rs").unwrap();
        let remaining = store.load_all_entities().unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].0, "src/b.rs");
        // Deleting an unknown file is a silent no-op.
        store.delete_entities("src/never.rs").unwrap();
        assert_eq!(store.load_all_entities().unwrap().len(), 1);
    }

    #[test]
    fn path_accessor_returns_open_path() {
        // Issue #28 Phase 4: the atomic-swap path reads `path()` to know which
        // file to rename. It must echo back exactly what `open` was given.
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("index.redb");
        let store = CorpusStore::open(&p).unwrap();
        assert_eq!(store.path(), p.as_path());
    }

    /// Why: verifies that `upsert_file_hashes` + `load_file_hashes` round-trip
    /// correctly across a store reopen (simulates daemon restart).
    /// Test: this test.
    #[test]
    fn hash_cache_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("index.redb");
        {
            let store = CorpusStore::open(&path).unwrap();
            // Empty table before any writes.
            assert!(store.load_file_hashes().unwrap().is_empty());
            // Upsert two entries.
            store
                .upsert_file_hashes(&[("src/a.rs", "aabbcc"), ("src/b.rs", "ddeeff")])
                .unwrap();
            let mut loaded = store.load_file_hashes().unwrap();
            loaded.sort_by(|x, y| x.0.cmp(&y.0));
            assert_eq!(loaded.len(), 2);
            assert_eq!(loaded[0], ("src/a.rs".to_string(), "aabbcc".to_string()));
            assert_eq!(loaded[1], ("src/b.rs".to_string(), "ddeeff".to_string()));
            // Upsert is idempotent (overwrite with same value).
            store.upsert_file_hashes(&[("src/a.rs", "aabbcc")]).unwrap();
            assert_eq!(store.load_file_hashes().unwrap().len(), 2);
            // Upsert overwrites with new value.
            store.upsert_file_hashes(&[("src/a.rs", "112233")]).unwrap();
            let mut loaded2 = store.load_file_hashes().unwrap();
            loaded2.sort_by(|x, y| x.0.cmp(&y.0));
            assert_eq!(loaded2[0].1, "112233");
        }
        // Reopen simulates daemon restart — hashes must survive.
        let store = CorpusStore::open(&path).unwrap();
        let mut loaded = store.load_file_hashes().unwrap();
        loaded.sort_by(|x, y| x.0.cmp(&y.0));
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].0, "src/a.rs");
        assert_eq!(loaded[0].1, "112233");
    }

    /// Why: verifies that `clear_file_hashes` removes all entries and an
    /// empty input to `upsert_file_hashes` is a no-op.
    /// Test: this test.
    #[test]
    fn hash_cache_clear() {
        let dir = tempfile::tempdir().unwrap();
        let store = CorpusStore::open(&dir.path().join("index.redb")).unwrap();
        store
            .upsert_file_hashes(&[("src/a.rs", "aa"), ("src/b.rs", "bb")])
            .unwrap();
        assert_eq!(store.load_file_hashes().unwrap().len(), 2);
        store.clear_file_hashes().unwrap();
        assert!(store.load_file_hashes().unwrap().is_empty());
        // Double-clear is a no-op, not an error.
        store.clear_file_hashes().unwrap();
        // Empty upsert is also a no-op.
        store.upsert_file_hashes(&[]).unwrap();
        assert!(store.load_file_hashes().unwrap().is_empty());
    }

    #[test]
    fn open_fresh_truncates_stale_staging_file() {
        // Issue #28 Phase 4: a stale `index.redb.tmp` left by an aborted
        // reindex must not contribute pre-existing rows to the next staged
        // corpus — `open_fresh` discards the old file first.
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("index.redb.tmp");

        // Populate, then drop so the file is closed and persisted on disk.
        {
            let store = CorpusStore::open(&p).unwrap();
            store.upsert_chunks(&[raw("stale:1:1", "old")]).unwrap();
            assert_eq!(store.chunk_count().unwrap(), 1);
        }
        assert!(p.exists());

        // `open_fresh` must yield an empty corpus despite the existing file.
        let fresh = CorpusStore::open_fresh(&p).unwrap();
        assert_eq!(fresh.chunk_count().unwrap(), 0);
        assert_eq!(fresh.path(), p.as_path());

        // And `open_fresh` on a path that does not exist is also fine.
        let fresh2 = CorpusStore::open_fresh(&dir.path().join("never.redb.tmp")).unwrap();
        assert_eq!(fresh2.chunk_count().unwrap(), 0);
    }

    /// Issue #41 phase 2: round-trip a tiny KG through `save_kg_graph` and
    /// `load_kg_graph`. Closes (and reopens) the store between save and load
    /// to prove the data is durable, not just held in process memory.
    #[test]
    fn save_load_kg_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("index.redb");

        let nodes = vec![
            (
                "alpha".to_string(),
                PersistedKgNode {
                    chunk_id: "a:1:1".into(),
                    file: "a.rs".into(),
                },
            ),
            (
                "beta".to_string(),
                PersistedKgNode {
                    chunk_id: "b:1:1".into(),
                    file: "b.rs".into(),
                },
            ),
        ];
        let adj_fwd = vec![(
            "alpha".to_string(),
            vec![("CallsFunction".to_string(), "beta".to_string())],
        )];
        let adj_rev = vec![(
            "beta".to_string(),
            vec![("CallsFunction".to_string(), "alpha".to_string())],
        )];

        {
            let store = CorpusStore::open(&path).unwrap();
            store
                .save_kg_graph(&nodes, &adj_fwd, &adj_rev)
                .expect("save kg");
            assert_eq!(store.kg_node_count().unwrap(), 2);
        }

        // Reopen and assert every row survived.
        let store = CorpusStore::open(&path).unwrap();
        let (loaded_nodes, loaded_fwd, loaded_rev) = store.load_kg_graph().unwrap();
        assert_eq!(loaded_nodes.len(), 2);
        assert_eq!(loaded_fwd, adj_fwd);
        assert_eq!(loaded_rev, adj_rev);

        // Saving an empty graph clears every table.
        store.save_kg_graph(&[], &[], &[]).unwrap();
        assert_eq!(store.kg_node_count().unwrap(), 0);
        let (n, f, r) = store.load_kg_graph().unwrap();
        assert!(n.is_empty() && f.is_empty() && r.is_empty());
    }
}
