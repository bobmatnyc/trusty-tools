# Fjall vs. redb: Deeper Evaluation and Benchmark Spike Plan

**Issue**: #692 — Evaluate backing stores; storage-adapter trait  
**Date**: 2026-06-03  
**Author**: Research agent  
**Status**: Proposal — awaiting review before implementation  
**Scope**: trusty-search (primary), trusty-common/memory-core, trusty-analyze, trusty-review (cross-crate impact noted)

---

## Summary

fjall 3.x (LSM-tree, MVCC, pure-Rust, MIT/Apache) is a **credible spike candidate** for the
high-concurrency read paths in trusty-search and trusty-common/memory-core. Its principal
advantage over redb — true multi-reader/multi-writer MVCC that never serialises concurrent
`begin_read()` callers — directly addresses the most significant structural pain point in the
current codebase: the ~30 `spawn_blocking` wrappers scattered across five crates that exist
solely because redb's synchronous transaction API blocks the tokio reactor. The #694 SIGKILL
durability incident is a genuine advantage point for fjall in the sense that LSM WAL journals are
structurally more tolerant of abrupt kills than a CoW B-tree mid-flush — but fjall's *default*
durability mode is OS-buffer-only, matching RocksDB's default, and full disk-safety requires
calling `persist(PersistMode::SyncAll)` explicitly. Neither engine is safe by default against
a SIGKILL with zero application-side care; the story is comparable and both require explicit
hardening.

The recommendation is: **proceed with a bounded benchmark spike** against a real trusty-tools
corpus. The top three metrics to measure are (1) concurrent read p50/p95 under
writer-active load, (2) batch-commit throughput during a full reindex, and (3) cold-start
time (warm-boot corpus restore latency). The spike should take 1–2 engineer-days behind the
`KvStore` trait adapter defined in #692, so the incumbents remain live and the spike is
reversible.

---

## Part 1: Our redb Usage Inventory

### 1.1 trusty-search — Primary Consumer

**File**: `crates/trusty-search/src/core/corpus.rs`

The `CorpusStore` wraps a single `redb::Database` that backs seven tables in one file
(`index.redb` per registered index):

| Table constant | Type | Purpose |
|---|---|---|
| `CHUNKS_TABLE` | `&str → &[u8]` | Postcard-free JSON-encoded `RawChunk`; keyed by `"{path}:{start}:{end}"` |
| `ENTITIES_TABLE` | `&str → &[u8]` | JSON `Vec<RawEntity>` per file path |
| `KG_NODES_TABLE` | `&str → &[u8]` | JSON `PersistedKgNode` per symbol |
| `KG_EDGES_TABLE` | `&str → &[u8]` | JSON forward adjacency `Vec<(EdgeKind, symbol)>` per source symbol |
| `KG_EDGES_REV_TABLE` | `&str → &[u8]` | JSON reverse adjacency per target symbol |
| `KG_COMMUNITIES_TABLE` | `u64 → &[u8]` | Legacy Louvain data (migration-tolerance only, no writes since v0.10.0) |
| `KG_SYMBOL_COMMUNITY_TABLE` | `&str → u64` | Legacy symbol→community (migration-tolerance only) |
| `META_TABLE` | `&str → &[u8]` | `schema_version` (u32 LE) + `indexed_root` (UTF-8 path) |

**Access patterns**:

- **Write (reindex)**: `upsert_batch(chunks, entities)` — a single `begin_write()` transaction
  committing up to a few hundred chunks + their entity lists at once. Called from
  `commit_corpus_to_redb` (`corpus.rs:936`) inside `spawn_blocking`; the comment at line 912
  is explicit: *"The redb write runs on `spawn_blocking` because redb's transaction API is
  synchronous and a large batch's serde_json encode plus fsync would otherwise pin a tokio
  worker thread."* (`crates/trusty-search/src/core/indexer/ingest.rs:912-936`)

- **Write (KG save)**: `save_kg_graph(nodes, adj_fwd, adj_rev)` — clears and rewrites the three
  KG tables in one transaction (`corpus.rs:571`). Wrapped in `spawn_blocking` at
  `indexer/ingest.rs:146`.

- **Read (warm-boot)**: `load_all_chunks()` + `load_all_entities()` — full table scan on startup,
  deserializing every row into memory. `spawn_blocking` at `persist.rs:156`.

- **Read (query hot path)**: `get_chunks(ids: &[&str])` — batch point-reads inside a single
  `begin_read()` transaction for each query (`corpus.rs:479`). Called from
  `search.rs:167` inside `spawn_blocking`. This is the **single most latency-sensitive redb
  call** — it runs on every search request that hits the corpus.

- **Read (migration)**: `read_schema_version_sync()` / `read_indexed_root_sync()` — short point
  reads behind `spawn_blocking` at `migration/mod.rs:365, 388, 414, 438`.

**Observed spawn_blocking call sites** (trusty-search only):

```
migration/mod.rs:365    spawn_blocking → corpus.read_schema_version_sync()
migration/mod.rs:388    spawn_blocking → corpus.write_schema_version_sync()
migration/mod.rs:414    spawn_blocking → corpus.read_indexed_root_sync()
migration/mod.rs:438    spawn_blocking → corpus.write_indexed_root_sync()
migration/m001.rs:99    spawn_blocking → corpus.load_all_chunks()
migration/m002.rs:97    spawn_blocking → corpus.load_all_chunks()
migration/m002.rs:164   spawn_blocking → corpus.upsert_chunks()
migration/m002.rs:170   spawn_blocking → corpus.delete_chunks()
indexer/ingest.rs:146   spawn_blocking → graph.save_to_corpus()
indexer/ingest.rs:503   spawn_blocking → corpus.save_kg_graph() (indirect)
indexer/ingest.rs:936   spawn_blocking → corpus.upsert_batch()
indexer/files.rs:253    spawn_blocking → corpus.delete_entities()
indexer/files.rs:280    spawn_blocking → corpus.delete_chunks()
indexer/persist.rs:156  spawn_blocking → corpus.load_all_chunks() + load_all_entities()
indexer/persist.rs:226  spawn_blocking → corpus.load_all_chunks()
indexer/persist.rs:288  spawn_blocking → corpus.load_all_chunks() (colocated)
indexer/persist.rs:373  spawn_blocking → corpus save + migrate path
indexer/persist.rs:420  spawn_blocking → corpus.save_kg_graph()
indexer/search.rs:167   spawn_blocking → corpus.get_chunks() [HOT PATH]
indexer/mod.rs:1111     spawn_blocking → corpus.load_all_chunks()
```

That is 20 `spawn_blocking` call sites in trusty-search alone, driven entirely by redb's
synchronous transaction model.

**Value sizes**: JSON-encoded `RawChunk` values run ~500–2000 bytes each for typical Rust source
chunks (content field dominates). KG edge adjacency rows are typically 100–500 bytes. The
`_meta` rows are tiny (4 bytes for schema version). The corpus for trusty-tools itself is ~23,500
chunks; a large monorepo can reach 200k+ chunks at ~200 MB of redb data.

**Range scans**: Used in `load_all_chunks()` / `load_all_entities()` (startup), `save_kg_graph`
(clear + re-insert via `retain`), and `load_kg_graph` (iterate all three KG tables). Range scans
over ordered KG edge tables (`KG_EDGES_TABLE`, `KG_EDGES_REV_TABLE`) are accessed by prefix
(source symbol) via point-read only — there are no open-range prefix scans in the current hot
path. The BFS KG expansion uses point-reads, not scans.

**Read/write ratio**: During a running daemon that is not actively reindexing, the pattern is
read-heavy (~20:1). During a reindex it inverts to write-dominated for the duration of the
batch-commit loop.

### 1.2 trusty-common/memory-core — Palace KV Stores

`kg_redb.rs` (`KgStoreRedb`) wraps one `redb::Database` per palace (`kg.redb`) storing:
- `TRIPLES` — SPO triple rows (postcard-encoded `TripleValue`)
- `TRIPLES_BY_OBJECT`, `TRIPLES_BY_PREDICATE` — secondary indexes for reverse lookups
- `DRAWERS` — palace drawer records (postcard-encoded `DrawerRecord`)
- `ACTIVE_SUBJECT_COUNTS` — aggregated active-triple counts per subject

`payload_store.rs` (`PayloadStore`) — one `redb::Database` per palace (`payloads.redb`) storing
`PAYLOADS` table with composite segment+id key → postcard-encoded `PayloadRecord` (uuid + JSON payload).

`analytics.rs` (`AnalyticsLog`) — `recall.redb` per daemon, `RECALL_LOG` table, u64 key → JSON event.

All three are wrapped in `spawn_blocking` at their async call sites
(`memory_core/store/kg.rs:711, 742, 977`; `analytics.rs:247, 395`; `memory_core/store/vector.rs:407, 421, 454`).

The `spawn_blocking` note in `kg.rs` line 12 is explicit:
*"spawn_blocking so the async reactor isn't stalled."*

### 1.3 trusty-analyze — facts.redb

`crates/trusty-analyze/src/core/facts.rs`: single `redb::Database`, one table `FACTS: u64 → &[u8]`
(JSON `FactRecord`). Access pattern: point-reads by fact hash ID, prefix scans for SPO queries
(subject/predicate/object filters), occasional bulk inserts from the analysis pipeline. Lighter
write pressure than trusty-search; higher concurrent read pressure during LLM-deep-analysis
passes that fan out across many fact queries.

### 1.4 trusty-review — dedup.redb

`crates/trusty-review/src/store/dedup.rs`: single `redb::Database`, `CLAIMS` table,
composite key → JSON `ClaimRecord`. Write-then-read pattern: claim a slot before posting a PR
review comment, expire old claims. Very low volume; correctness (dedup guarantee) matters more
than throughput.

---

## Part 2: fjall Architecture

### 2.1 Overview

fjall 3.x (current: 3.1.4, released 2026-04-14, MIT/Apache-2.0) is an LSM-tree storage engine
built on top of the `lsm-tree` crate (also at 3.1.4, MSRV 1.90.0). The two-crate split is
intentional: `lsm-tree` is the raw LSM primitive (no WAL of its own), and `fjall` adds the
database-level WAL/journal, cross-partition atomic writes, and the `Keyspace` → `Partition`
abstraction.

**Sources**:
- https://github.com/fjall-rs/fjall (README, v3.1.4)
- https://fjall-rs.github.io/post/fjall-3/ (v3.0 release post)
- https://docs.rs/fjall/latest/fjall/ (API docs)

### 2.2 LSM-tree Anatomy

Write path: write → append to database-level WAL journal → insert into per-partition memtable
(skip list) → memtable sealed and flushed to Level 0 SSTable → background compaction merges
L0 → L1 → … → L6 (leveled strategy, reworked in 3.0 to promote directly to L6 to avoid
tombstone accumulation in middle levels).

The `lsm-tree` crate is `forbid(unsafe_code)`; 100% safe Rust.

### 2.3 Keyspace / Partition Model

- `Keyspace` = the database handle; owns the WAL journal and shared background thread pool.
- `Partition` = a named logical collection, each backed by its own physical LSM-tree and its own
  compaction strategy. Partitions are independent physically; writes across partitions are atomic
  via the shared journal (a single journal commit covers multi-partition write batches).
- Terminology shifted in v3.0: what was previously called `partition` is now `keyspace`, and what
  was called `keyspace` is now the top-level `Database`. This is a docs-only change.

Mapping to our use case: one `Keyspace` per `index.redb` file, with each redb `TableDefinition`
becoming a separate `Partition`. This maps cleanly — our CHUNKS, ENTITIES, KG_NODES, KG_EDGES,
KG_EDGES_REV, and META tables become six partitions in a single fjall keyspace.

### 2.4 Concurrency Model — The Key Differentiator

redb uses a **single global write lock** — only one `begin_write()` can hold the write lock at a
time, and `begin_read()` is serialised through the same lock on some code paths (the changelog
for v4.1 notes "~15% speedup on concurrent reads" as a targeted improvement, confirming the
serialization was a measured bottleneck). This is why every redb call in the workspace goes
through `spawn_blocking`: the synchronous lock acquisition would park a tokio worker thread.

fjall uses **MVCC seqno-based snapshot reads**. A reader captures a snapshot (sequence number)
and sees a consistent point-in-time view. Writers never block readers; readers never block
writers. Multiple write transactions *can* run concurrently in `OptimisticTxDatabase` mode
(with OCC conflict detection on commit), or serialised in `SingleWriterTxDatabase` mode.
For our workload — many concurrent search queries (readers) + occasional batch commit (one writer
at a time) — `OptimisticTxDatabase` with a single background writer is ideal.

**Concrete implication**: the ~20 `spawn_blocking` wrappers in trusty-search could be replaced
by non-blocking async reads that call into fjall directly on the tokio worker thread, since fjall
reads return immediately from the MVCC snapshot without waiting for a lock. This reduces
thread-pool churn and eliminates the `JoinHandle` allocation overhead on the hot query path
(`search.rs:167`).

**Caveat**: fjall's `WriteBatch` (the main write API) does not support reads of intermediary
state — it is append-only until commit. The `OptimisticTxDatabase` supports read-in-write but
adds OCC overhead. For our write path (append-only batch inserts during reindex), `WriteBatch` is
sufficient and likely faster.

### 2.5 WAL / Journal and Crash Recovery

fjall maintains a **single database-level journal** shared across all partitions. On restart,
the journal is replayed to restore the consistent state. All cross-partition write batches are
atomic: if a batch spans CHUNKS + ENTITIES + META (analogous to our current `upsert_batch`),
the journal either replays all or none.

**Durability modes** (from `PersistMode` enum):
- Default: OS buffer flush (no fsync). Equivalent to `fdatasync=false` in PostgreSQL.
  A SIGKILL after this may lose the last unflushed write batch.
- `PersistMode::SyncAll`: fsync journal + all SSTable files. Full disk durability.
  Automatically applied on graceful `Drop`.

**SIGKILL / truncation resilience vs. redb**: fjall's LSM journal handles partial writes
(truncated records) gracefully — any partially-written journal record has an invalid xxh3
checksum (added in v3.0) and is discarded on replay; recovery completes to the last
fully-committed sequence number. The database file is not corrupted; only the last
uncommitted write batch is lost. This is structurally safer than the #694 scenario, where
redb's CoW B-tree page file was truncated mid-flush causing an assertion failure in the page
layout check. The LSM journal's append-only nature means truncation at the tail is benign;
the journal is simply truncated to the last valid CRC-checked record on replay.

**However**: this is not "free" durability. With fjall's default `PersistMode` (OS buffer only),
a power loss or SIGKILL could still lose writes not yet journaled-then-flushed. The crucial
difference from #694 is: fjall would lose the *last write batch* but open cleanly at the
*previous* consistent state; redb left the file in an unrecoverable truncated state that
caused a panic on open.

For the #694 use case specifically, fjall wins: an abrupt kill during an LSM flush leaves the
journal consistent at the pre-flush checkpoint, while redb's in-place CoW B-tree page mutation
can leave an internally inconsistent file. The mitigation for fjall is explicit
`persist(PersistMode::SyncAll)` calls at the end of each reindex batch — cheap because LSM
journal sync only writes the WAL, not the full SSTable hierarchy.

### 2.6 Trade-offs vs. redb (Our Access Patterns)

| Dimension | redb 2.6 (incumbent) | fjall 3.1.4 |
|---|---|---|
| Storage engine | CoW B-tree (mmap) | LSM-tree (WAL + SSTables) |
| Pure Rust | Yes | Yes (forbid(unsafe)) |
| MSRV | ≥ 1.69 (workspace: 1.91) | 1.90 (within our MSRV) |
| Read tx concurrency | Single global write lock; `begin_read` serialised | MVCC snapshot: readers never block, writers never block readers |
| `spawn_blocking` need | Mandatory (sync API, blocks reactor) | Not mandatory for reads (MVCC, non-blocking); still advisable for large writes |
| Write path (reindex batch) | Single `begin_write` txn; O(batch) fsync per commit | `WriteBatch` per batch; one journal append + async compaction in background |
| Batch commit throughput | Good for moderate batches (128-file batches, ~500 chunks); fsync on each commit | Likely higher: LSM write path is sequential-IO; compaction is background; 2× write amplification for ascending keys |
| Point-read latency (hot) | O(log N) B-tree descent over mmap; ~sub-ms for 23k chunks | MVCC snapshot read; must check memtable → L0 → L1 → … (Bloom filter cuts most I/O); block cache avoids most SSD reads; roughly comparable on warm cache |
| Point-read latency (cold) | Excellent: OS mmap page cache serves reads lazily | Moderate: cold reads must traverse Bloom filter → SSTable blocks; 2–5× higher than B-tree on truly cold data |
| Range scan (KG edge iteration) | B-tree range is sequential; excellent for ordered prefix scans | SSTable merge scan; competitive on warm cache, slightly more overhead due to merge logic |
| Space amplification | Very low (CoW B-tree shares pages; no compaction overhead) | ~1.0–1.4× with LZ4 compression (fjall's compression is a net win for text values like JSON chunks) |
| Write amplification | Low for updates (CoW in-place); high for SIGKILL scenario (mid-page-flush) | ~2× for ascending-key bulk loads (our reindex pattern); ~10× worst-case random writes (not our pattern) |
| SIGKILL resilience | **Vulnerability**: truncated B-tree page → assertion failure on open (#694) | **Safer**: WAL append-only; truncated tail discarded on replay; opens cleanly at prior consistent checkpoint |
| Block cache / memory | Configurable `set_cache_size`; default 64 MB (our tuned value); mmap-backed | Per-keyspace block cache (~20–25% of RAM recommended); memtables add ~8–32 MB per active partition; more moving parts |
| Cold-start | Immediate: mmap maps pages on first access | Moderate: must scan version history file, validate journal, potentially replay since last checkpoint |
| On-disk size | Single file per database | Directory of SSTables + WAL + manifest files; less predictable layout |
| Atomic rename swap | Easy: one file, `fs::rename` is atomic | Harder: directory swap requires more care (copy + rename dir, or fjall's own keyspace API) |
| Schema migration framework | `_meta` table via `TableDefinition` | `_meta` partition in fjall keyspace; same semantics, different API |
| File descriptor usage | 1 FD per open database | Multiple FDs per keyspace (one per SSTable + journal); important for daemon with 200+ indexes |
| API maturity / stability | Stable (v2.6 in workspace; v4.1 latest) | Active development; v3.0 had breaking API changes (terminology rename); v3.x API appears stable |

### 2.7 File Descriptor Budget — Critical Concern

This deserves special attention for trusty-search, which can hold 200+ indexes open
simultaneously. Each fjall `Keyspace` with 6 partitions opens multiple FDs:
- 1 journal FD
- N SSTable FDs per partition (N grows with corpus size and compaction state; may be 5–20 per partition under load)
- 1 manifest/version FD per keyspace

For 200 indexes × 6 partitions × 10 SSTable FDs = ~12,000 FDs, well above the default macOS
`ulimit -n` of 256 and approaching Linux defaults of 1024. redb uses exactly 1 FD per database.
The `quick-cache`-backed FD cache in fjall (added in v3.0) bounds this but does not eliminate it.
Any spike implementation must measure FD consumption at scale and verify it stays within
the system limit. The `trusty-common/launchd.rs` comment already notes that trusty-memory
opens ~3 redb files per palace — fjall would multiply this significantly.

---

## Part 3: Benchmark / Spike Plan

### 3.1 Objective

Determine whether fjall's MVCC multi-reader model produces a measurable latency improvement
on the trusty-search hot path (concurrent `get_chunks` during search) at the cost of acceptable
write amplification, memory overhead, and FD pressure — using the real trusty-tools corpus
(~23,500 chunks as of 2026-06-03).

### 3.2 Prerequisites

1. Phase 1 of #692 must be complete: `KvStore` / `KvTxn` trait in `trusty-common`
   (`storage-traits` feature) with `RedbKvStore` as the concrete adapter. The fjall spike
   implements `FjallKvStore` behind the same trait.
2. Verify fjall 3.1.4 MSRV = 1.90.0 satisfies our workspace MSRV constraint of 1.91
   (fjall's MSRV is lower, so it passes; the workspace MSRV is set by `aws-smithy-*` deps at
   1.91.1 — fjall fits comfortably).
3. Instrument `CorpusStore` to log per-call latency at `RUST_LOG=trusty_search=trace`
   (or via `tokio-metrics` spans) so baseline redb numbers are captured before swapping.

### 3.3 Metrics to Measure (Priority Order)

**Metric 1: Concurrent read p50/p95 latency under writer pressure**

Setup: Start the trusty-search daemon with the trusty-tools corpus indexed (~23,500 chunks).
Drive 50 concurrent search requests (each calling `get_chunks` on the hot path) while a
background reindex batch is committing (simulating the live-reindex + concurrent-query scenario).

Measure: wall-clock latency of `spawn_blocking` call to `corpus.get_chunks()` (redb) vs.
direct async call to fjall read (fjall). Report p50, p95, p99.

Expected signal: fjall should show reduced tail latency (p95/p99) because concurrent readers
are not serialised behind the reindex writer. If the p50 degrades by >2 ms, the Bloom filter
overhead on the hot read path is too high.

**Metric 2: Reindex batch-commit throughput**

Setup: `POST /indexes/trusty-tools/reindex?force=true` and measure wall-clock time from start
to final `complete` SSE event. Compare total reindex time: redb vs. fjall.

The trusty-search reindex emits one `upsert_batch` per 128-file batch (~300–600 chunks per
batch). Measure the fsync overhead per batch: redb does one `commit()` per batch (synchronous
fsync); fjall does one `WriteBatch::commit()` per batch (journal append, no SSTable fsync
unless `persist(SyncAll)` called explicitly).

Report: total elapsed seconds; per-batch commit latency distribution; final RSS at completion.

**Metric 3: Cold-start / warm-boot corpus restore latency**

Setup: Kill the daemon (SIGTERM), restart. Measure time from process start to first query
returning results (as reported by the daemon's `RUST_LOG=info` lines for "restored N chunks").

redb: mmap-backed scan; first-access page faults but typically fast.
fjall: must open keyspace, validate journal, scan version manifest, then read all partitions.
For 23,500 chunks across 6 partitions this is likely slower than redb's single-file mmap path.

If cold-start regression exceeds 5 seconds, that is a usability concern for the daemon restart
workflow (see #534 graceful restart convention).

**Additional metrics (if time allows)**

- On-disk size: `du -sh` of the fjall keyspace directory vs. the redb file.
- RSS at steady state: `ps -o rss` after all queries warm the block cache.
- FD count: `lsof -p <pid> | wc -l` with 200 indexes loaded (critical for multi-index daemon).
- Range scan over KG edges: `load_kg_graph()` wall-clock on a large KG (10k+ nodes).

### 3.4 Spike Implementation Notes

The `FjallKvStore` adapter maps as follows:

- One `fjall::Keyspace` per database path (replaces `redb::Database`).
- One `fjall::Partition` per table name (replaces `redb::TableDefinition`).
- `WriteBatch` replaces `begin_write()` + `commit()`.
- Snapshot reads replace `begin_read()` — no `spawn_blocking` required.

The `_meta` schema version uses the `META_TABLE` partition with the same key strings
(`"schema_version"`, `"indexed_root"`). No on-disk format changes to the JSON chunk/entity
values — fjall stores `&[u8]` values identically to redb.

The migration framework (`core::migration::run_migrations`) should be refactored to take
`&dyn KvStore` before the spike (per #692 Phase 1), but a prototype spike can use conditional
compilation to keep the redb path live while testing fjall.

### 3.5 Pass/Fail Criteria for Go/No-Go Decision

| Criterion | Pass | Fail |
|---|---|---|
| p50 concurrent read latency | ≤ incumbent redb latency + 1 ms | > redb + 1 ms |
| p95 concurrent read latency | ≥ 20% improvement over redb under writer load | < 10% improvement |
| Reindex batch-commit throughput | ≤ 10% regression vs. redb | > 10% regression |
| Cold-start latency | ≤ 15 seconds for 23,500 chunks | > 15 seconds |
| FD count (200 indexes) | < system ulimit with 25% headroom | Would exceed default ulimit |
| On-disk size (LZ4 on) | ≤ 1.5× redb file size | > 2× redb (excessive amplification) |

---

## Part 4: Durability Deep-Dive — #694 SIGKILL Case

### 4.1 What Happened in #694

A `systemd` SIGKILL during trusty-search shutdown sent the daemon process an uncatchable
kill signal mid-flush of `index.redb`. redb's CoW B-tree writes new pages in-place; when the
process was killed, the file was left in a state where `storage.raw_file_len()? >= header.layout().len()`
failed — the header described a layout longer than the file contents, i.e., a truncated file
that redb could not reconcile. The daemon then fell back to an empty corpus on next start.

All derived copies (EFS colocated copy, `.prev` rollback, S3 tarball) were taken *after*
the corruption, so no clean snapshot was available.

### 4.2 How fjall Would Have Behaved

With fjall, the SIGKILL would have interrupted either:

(a) A journal append in progress — the journal record's trailing checksum (xxh3, added in v3.0)
would be missing or invalid. On replay, fjall discards the truncated record and replays to the
last fully-committed sequence number. The *previous* write batch's data is intact and readable.

(b) An SSTable flush triggered by background compaction — the in-progress SSTable file would be
incomplete. fjall's version history tracks which SSTable files are valid for the current
consistent version; orphaned partial files are ignored on open and cleaned up by the next
compaction cycle. The database opens cleanly.

(c) Neither — the write was fully in OS buffers but not yet journaled. With default
`PersistMode` (OS-buffer-only), this write is lost. But the file is not corrupted; fjall opens
cleanly at the prior checkpoint.

**In all three cases, fjall opens without an assertion failure and serves the last consistent
state.** The tradeoff: the last unflushed write batch may be lost (case c), which for
trusty-search means the last ~300–600 chunks of the most recent reindex batch are missing.
This is recoverable by triggering a fresh reindex — far better than the #694 outcome of losing
the entire 200k+ chunk corpus with no clean copy to restore from.

### 4.3 Recommended Hardening Regardless of Backend

Both redb and fjall benefit from explicit durability hardening:

1. **Explicit fsync per reindex batch**: call `persist(PersistMode::SyncAll)` (fjall) or
   ensure redb's `commit()` calls `fsync` (which it does by default, but confirm the
   `Durability::Immediate` setting is in use, not `Durability::None`).
2. **Atomic snapshot before S3 tar**: build the tarball from a quiesced copy (`cp --reflink`
   on btrfs/APFS, or a redb `into_transaction()` snapshot) rather than the live-being-written
   file.
3. **Crash-safe open fallback**: on redb open failure (the assertion in #694), catch the panic
   or the error and fall back to an empty corpus with a warning rather than silently serving 0
   results. This is a defensive measure regardless of backend.

---

## Part 5: Cross-Crate Impact

| Crate | Impact of fjall spike |
|---|---|
| `trusty-search` | Primary target. ~20 `spawn_blocking` sites removable for reads; write path simplifies. FD budget is the main risk. |
| `trusty-common` (memory-core) | Palace KV stores (kg.redb, payloads.redb, analytics.redb) are small-to-medium volume. ~10 `spawn_blocking` sites removable. Lower FD risk (fewer simultaneous open DBs). |
| `trusty-analyze` | `facts.redb` is lightly used; benefit is modest. `spawn_blocking` not systematically used here. Lower priority. |
| `trusty-review` | `dedup.redb` is very low-traffic. No practical benefit. Keep redb as-is. |
| `open-mpm` | `RedbUsearchStore` is pending migration to `HnswVectorIndex` (#692 Phase 1). Independent of the fjall spike; KV side stays redb until spike concludes. |

The spike should be scoped to **trusty-search's `CorpusStore` only**. The `KvStore` trait from
#692 Phase 1 means swapping the backend for memory-core and trusty-analyze requires only
changing the concrete type at construction time — the rest of the codebase is unaffected.

---

## Part 6: Risks and Open Questions

**Risk 1 — FD exhaustion at scale.** A daemon holding 200+ indexes with fjall's multi-file
SSTable layout could blow through the default `ulimit -n`. Mitigation: measure FD count in
the spike; if over budget, investigate fjall's FD cache configuration or consider fjall only
for `trusty-common` memory-core (far fewer simultaneous open DBs) and keep redb for
trusty-search.

**Risk 2 — Cold-start regression.** fjall's version manifest + journal replay is slower than
redb's single-file mmap on first open. If the trusty-search daemon restarts frequently
(e.g., rolling deploys in systemd), a 10–15 second cold-start for a large corpus is a
usability problem. Measure in the spike.

**Risk 3 — API stability.** fjall 3.0 had breaking API changes (keyspace/partition rename).
Pin the workspace to a specific minor version and watch the changelog before upgrading.

**Risk 4 — Write stall under compaction.** LSM-trees can stall writes when L0 fills faster than
compaction drains it ("write stall"). Under a large reindex this could cause the batch-commit
loop to block unexpectedly. Monitor L0 file count in the spike.

**Risk 5 — `spawn_blocking` is not inherently wrong.** The tokio documentation recommends
`spawn_blocking` for any I/O that cannot use async. Removing it for fjall reads saves thread
context switches but introduces fjall's own blocking internally (Bloom filter evaluation,
block decompression). Net latency benefit must be verified empirically, not assumed.

**Open question 1 — Does fjall's MVCC snapshot read ever block?** The docs describe non-blocking
readers, but snapshot creation under OCC with active compaction should be verified. If compaction
holds a short exclusive lock during SSTable file rotation, readers could stall briefly.

**Open question 2 — Single keyspace or per-table keyspace?** We could use one `Keyspace` with 6
`Partition`s (matching our one-file-per-index design), or one `Partition` per index (simpler,
higher FD count). The single-keyspace approach matches the redb design better and keeps
cross-partition atomic writes cheap.

**Open question 3 — redb 4.x.** The workspace currently pins redb 2.6. redb 4.1 (released
2026-04-19) claims ~15% concurrent read improvement and ~1.5× write speedup. Before migrating
to fjall, it is worth evaluating whether upgrading to redb 4.x addresses the concurrency concern
at lower migration cost. See the redb 4.1 changelog: the improvements are described as
"dynamic cache partitioning" and concurrent read optimisations, not removal of the global write
lock. The `spawn_blocking` requirement is architectural to redb's sync API and would remain.

---

## Recommendation

**Go-ahead for a bounded benchmark spike (1–2 engineer-days).** The motivation is strong:

1. The MVCC multi-reader story is a genuine structural improvement over redb's serialised
   `begin_read()` for the concurrent-search workload that is trusty-search's primary use case.
2. The #694 SIGKILL scenario exposes a real durability gap; fjall's WAL append-only journal
   handles abrupt kills more gracefully.
3. The MSRV check passes (fjall 1.90 < workspace 1.91).
4. The `KvStore` trait from #692 Phase 1 de-risks the spike: incumbents stay live behind their
   adapter; the fjall path is a plug-in, not a rip-out.

The spike must measure FD budget at 200-index scale before any production adoption. If FD
pressure is unmanageable, fjall is still the right choice for `trusty-common` memory-core
(where few DBs are open simultaneously) and redb stays for trusty-search's large-scale
multi-index daemon.

Top 3 metrics for the spike:
1. **Concurrent read p95 under writer load** (the spawn_blocking/MVCC motivator)
2. **Reindex batch-commit throughput** (write amplification check)
3. **FD count at 200 indexes** (the make-or-break feasibility gate)

---

## References

- Issue #692 — Storage adapter trait + candidate survey (this doc builds on that inventory)
- Issue #694 — redb SIGKILL truncation corruption (durability section grounded in this incident)
- `crates/trusty-search/src/core/corpus.rs` — redb CorpusStore, all seven table definitions
- `crates/trusty-search/src/core/indexer/ingest.rs:912,936` — spawn_blocking rationale comment
- `crates/trusty-search/src/core/indexer/search.rs:167` — hot-path spawn_blocking for get_chunks
- `crates/trusty-common/src/memory_core/store/kg.rs:12` — spawn_blocking rationale comment
- fjall GitHub: https://github.com/fjall-rs/fjall (v3.1.4, 2026-04-14)
- fjall 3.0 release post: https://fjall-rs.github.io/post/fjall-3/
- lsm-tree crate: https://github.com/fjall-rs/lsm-tree (v3.1.4, MSRV 1.90)
- redb 4.1 changelog: https://docs.rs/crate/redb/latest (concurrent read improvements)
- Issue #28 — redb cutover for corpus store (trusty-search)
- Issues #44–#47 — SQLite → redb migrations (trusty-memory/memory-core)
- Issue #534 — SIGTERM graceful request drain (connection-safe daemon restart)
