//! redb table definitions and cache-size helpers for the corpus store.
//!
//! Why: centralising every `TableDefinition` constant and the cache-size
//! resolver here keeps the `store_impl` focused on logic rather than schema.
//! What: exports the eight table constants and the two cache-size items consumed
//! by `CorpusStore::open`.
//! Test: `redb_cache_size_default_and_env_override` in `tests`.

use redb::TableDefinition;

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
pub(super) const DEFAULT_REDB_CACHE_MB: usize = 64;

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
pub(super) fn redb_cache_size_bytes() -> usize {
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
pub(super) const CHUNKS_TABLE: TableDefinition<&str, &[u8]> = TableDefinition::new("chunks");

/// redb table holding the per-file entity lists, keyed by file path.
///
/// Why: `entities` are needed to rebuild the symbol graph on warm-boot and are
/// derived per file, so the file path is the natural key.
/// What: `&str → &[u8]` where the value is `serde_json`-encoded
/// `Vec<RawEntity>`.
pub(super) const ENTITIES_TABLE: TableDefinition<&str, &[u8]> = TableDefinition::new("entities");

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
