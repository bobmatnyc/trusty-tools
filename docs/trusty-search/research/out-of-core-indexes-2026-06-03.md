# Out-of-Core (Larger-than-RAM) Chunk Index Architecture — Options Analysis

- **Date:** 2026-06-03
- **Crate:** `trusty-search` (v0.22.3)
- **Author:** Research analyst (automated)
- **Status:** Decision-support / epic seed
- **Builds on:** [#692](https://github.com/bobmatnyc/trusty-tools/issues/692) (backing-store abstraction + eval), [#705](https://github.com/bobmatnyc/trusty-tools/issues/705) (memory management / cold-index offload), [#704](https://github.com/bobmatnyc/trusty-tools/issues/704) (durability / resilience)
- **In-flight dependency:** [#702](https://github.com/bobmatnyc/trusty-tools/issues/702) — redb 2.6 → 4.x workspace upgrade (open, sibling worktree `chore/702-redb-4x`)

---

## 1. Problem statement

trusty-search is a **machine-wide daemon that holds every registered index fully
resident in RAM**. The design was explicitly tuned for "zero cold-start" (HNSW stays
hot; see `crates/trusty-search/CLAUDE.md` "Performance Targets"). That assumption breaks
on the Duetto production fleet:

- **~0.5–2 GB resident per index** (HNSW arena + BM25 posting lists + petgraph KG +
  redb page cache + chunk-text map).
- **50+ registered indexes per host.**
- → **25–100 GB of working set** on hosts whose RAM tier tops out at a 64 GB ceiling
  for the steady-state daemon budget (`memory_policy.rs:101–105`,
  `MEMORY_LIMIT_CEIL_MB = 65_536`).

The daemon has **no global cross-index RAM ceiling** and **no whole-index eviction**
(only chunk-text idle eviction exists today — see §2). When the sum of resident indexes
exceeds host RAM, the OS either thrashes the page cache or the kernel OOM-kills the
daemon. On the EFS-backed fleet the failure mode is worse: cold mmap page faults become
**network round-trips of tens of ms to (worst case) seconds** (§2.6, §3.1), so even the
"memory-friendly" mmap-view path degrades badly under memory pressure.

This document inventories what is RAM-resident today (Part 1), surveys out-of-core
options against our exact access patterns and the EFS cold-page constraint (Part 2), and
recommends a phased plan sequenced behind the #692 adapter and #702 redb upgrade (Part 3).

---

## Part 1 — What is RAM-resident today (file:line grounded)

trusty-search keeps **four** per-index data structures hot, plus a redb-backed durable
corpus. Per the architecture diagram in `crates/trusty-search/CLAUDE.md`, an
`IndexHandle` owns a `CodeIndexer` which owns all four.

### 1.1 Vectors / HNSW — the central fact: mmap view is promoted to a heap copy on first search

This is the single biggest lever and the most subtle one.

- **Warm-boot opens the snapshot via `Index::view` (mmap, read-only).**
  `UsearchStore::load_from` (`store.rs:374–448`) deliberately calls usearch's `view()`
  instead of `load()` so the snapshot is memory-mapped and the OS page cache services
  reads, "dropping warm-boot RSS from ~40 GB to a small fraction of that"
  (`store.rs:188–197` doc comment). The store is marked `is_view = true` and remembers
  its source path (`store.rs:432–433`).

- **The first *write* promotes the view to a full heap copy via `ensure_mutable()`.**
  `ensure_mutable` (`store.rs:470–478`) fast-paths on `is_view == false`; otherwise it
  calls `promote_view_to_mutable` (`store.rs:482–516`), which runs `Index::load(path)`
  — deserializing the entire graph + vector arena into heap RAM — then clears `is_view`.
  "From that point on this store behaves identically to one built via `new` and never
  enters view mode again" (`store.rs:459–462`).

- **Critically, promotion is wired into the *write* paths (`upsert` `store.rs:557`,
  `upsert_batch` `store.rs:770`, `remove` `store.rs:628`), NOT the read path.**
  `search` (`store.rs:589–624`) takes only a read lock on the index and never calls
  `ensure_mutable`. `test_view_promotes_to_mutable_on_write` (`store.rs:1180–1238`)
  asserts exactly this: *"search must not promote view → mutable"* (`store.rs:1214–1217`).

  **So pure-read indexes stay in mmap-view mode indefinitely.** Promotion to a heap copy
  is triggered by the **first incremental write** — which in practice happens the moment
  the `FileWatcher` (500 ms debounce) observes an edit in that project, or a `reindex` /
  `index_file` call lands. On a developer host the active project's index is therefore
  almost always promoted to heap; on a server fleet that only serves read queries, indexes
  can remain mmap-resident. **The #705 framing ("once queried it promotes and stays
  there") is true for any host where the watcher is live or files are re-ingested; the
  precise trigger is the first mutation, not the first query.**

- **The "Duration::MAX cool-after" phrasing in CLAUDE.md is a design intent, not a literal
  timer.** The only literal `Duration::MAX` in the crate is a reindex HTTP client timeout
  (`commands/reindex_engine.rs:337`). The mechanism that keeps HNSW hot is the absence of
  whole-index eviction (§1.5) plus the one-way view→heap promotion above.

**Per-index HNSW cost:** all-MiniLM-L6-v2 produces 384-dim f32 vectors. A promoted index
holds `N × 384 × 4 bytes` of vector arena plus graph overhead. At the 1M-element cap
(`DEFAULT_HNSW_MAX_ELEMENTS = 1_000_000`, `store.rs:48`) that is ~1.5 GB of raw vectors
plus graph — the comment at `store.rs:47` estimates "~6 GB at 1M × 384-dim × 4 bytes plus
graph overhead". A typical Duetto index (tens of thousands of chunks) is in the 0.1–0.5 GB
range **when promoted**, and a small fraction of that when left in mmap-view.

### 1.2 Lexical / BM25 — fully in RAM, never evicted

- `bm25: Arc<RwLock<Bm25Index>>` is held resident on the indexer
  (#705 cites `indexer/mod.rs:857`). `core/bm25.rs` is now a thin re-export of
  `trusty_common::bm25::BM25Index` (`bm25.rs:16`).
- Posting lists live entirely on the heap; chunk-text idle eviction (§1.4) **does not
  touch BM25** — #705 explicitly lists `bm25` among the structures the current eviction
  "does NOT touch". Capped per index by `TRUSTY_BM25_CORPUS_CAP`
  (100k/200k/400k docs by tier — `memory_policy.rs:409–411`).

### 1.3 KG / Symbol graph — fully in RAM, but already persisted in redb

- `symbol_graph: Arc<RwLock<Arc<SymbolGraph>>>` is a `petgraph::DiGraph<SymbolNode,
  EdgeKind>` (`symbol_graph.rs:80–87`) held resident (#705 cites `indexer/mod.rs:865`).
  Each node clones three `String`s and the graph maintains two side maps (`by_symbol`,
  `chunk_to_symbol`); the doc warns "on a 1M-chunk monorepo this graph can pin 3–5 GB"
  (`symbol_graph.rs:33–35`). Capped by `TRUSTY_MAX_KG_NODES` (default 100k —
  `symbol_graph.rs:37`).
- **The graph is *also* persisted in redb** in four tables: `kg_nodes`
  (`corpus.rs:109`), `kg_edges` forward adjacency (`corpus.rs:118`), `kg_edges_rev`
  reverse adjacency (`corpus.rs:127–128`), plus community tables (`corpus.rs:139–148`).
  The forward/reverse split is by design: "`callers_of` expansions walk *incoming* edges
  by symbol" (`corpus.rs:122`). `load_kg_graph` (`corpus.rs:627–651`) and the helper
  `load_adjacency` (`corpus.rs:872–891`) already stream these tables back. **This is the
  enabling fact for KG out-of-core (§2.7): on-demand per-symbol neighbour lookups are a
  single keyed redb read away.**

### 1.4 Corpus / chunk text — redb-backed (durable, mmap), in-mem map idle-evicts

- The durable corpus is `CorpusStore` over a `redb::Database` (`corpus.rs:24–30`), one
  `index.redb` file per index, mmap CoW B-tree (per #692 inventory). Chunks live in
  `CHUNKS_TABLE` (`corpus.rs:91`), serialized as JSON values.
- redb's own page cache is a **ceiling** (`set_cache_size`), default 64 MB
  (`DEFAULT_REDB_CACHE_MB`, `corpus.rs:52`; tunable via `TRUSTY_REDB_CACHE_MB`,
  `corpus.rs:67`). Empirically the redb working set on the trusty-tools corpus (~23.5k
  chunks) is ~87 MB (`corpus.rs:38–45`).
- **The only thing that idle-evicts today is the in-memory `chunks: HashMap<String,
  RawChunk>` text map.** `evict_chunks_if_idle` (`indexer/mod.rs:1053`) clears it after
  `TRUSTY_CHUNKS_IDLE_EVICT_SECS` (default 300 s); a 60 s background ticker drives it
  (#705 cites `service/server.rs:1006–1035`). Reads lazily rehydrate from redb via
  `ensure_chunks_loaded` (`indexer/mod.rs:1101`), called on every file/search/ingest path
  (`indexer/files.rs`, `indexer/ingest.rs`, `indexer/search.rs`). Guarded to durable
  corpora only.

### 1.5 Whole-index residency & memory tiers

- `IndexRegistry` is a `DashMap<IndexId, Arc<IndexHandle>>` (#705 cites
  `registry.rs:655–711`). **Every registered index holds a live `Arc<IndexHandle>` for the
  daemon's entire uptime; nothing drops a handle for idleness** — only an explicit
  `DELETE /indexes/:id`. This is the structural root cause of the 25–100 GB fleet problem.
- `MemoryPolicy` (`memory_policy.rs:464–597`) auto-tunes caps from detected RAM into three
  tiers (Medium 16–31 GB, Large 32–63 GB, XLarge ≥64 GB — `memory_policy.rs:324–332`).
  All caps are **per index**; there is no cross-index budget. Steady-state daemon limit is
  25 % of RAM clamped to **[1 GB, 64 GB]** (`memory_policy.rs:130–133`); the transient
  index-pipeline limit is 75 % clamped to [2 GB, 96 GB] (`memory_policy.rs:148–151`).

**Summary — what is hot per index, today:**

| Component | Residency | Already on disk? | Evictable today? | Cite |
|---|---|---|---|---|
| HNSW vectors+graph | heap (after 1st write) / mmap (read-only) | yes (`.usearch`) | no whole-index path | `store.rs:374–516` |
| BM25 posting lists | heap | no (rebuilt) | no | `indexer/mod.rs:857` |
| KG petgraph | heap | **yes** (`kg_*` redb tables) | no | `symbol_graph.rs:80`, `corpus.rs:109–148` |
| Chunk-text map | heap | yes (`chunks` redb table) | **yes** (idle 5 min) | `indexer/mod.rs:1053` |
| redb page cache | mmap, capped 64 MB | yes | bounded, not evicted | `corpus.rs:52` |

---

## Part 2 — Options survey

Each option is assessed against our access patterns (hybrid BM25+HNSW+KG, RRF-fused,
4×top_k HNSW candidates per query — see CLAUDE.md "Query Pipeline") and the **EFS
cold-page constraint** (§2.6). All ANN options must fit behind the #692 `VectorStore` /
`KvStore` adapter (`store.rs:107–163` defines `trait VectorStore` today — the natural
seam).

### 2.6 The EFS cold-page constraint (sets the bar for every mmap option)

mmap turns a memory access into **blocking IO**; on EFS each major page fault is a network
round-trip. EFS Standard (SSD) first-byte latency is single-digit-to-tens of ms; EFS IA /
Archive is "tens of milliseconds" first-byte and **per-file-operation overhead** because
EFS replicates to all mount points
([AWS EFS troubleshooting](https://repost.aws/knowledge-center/efs-troubleshoot-slow-performance), retrieved 2026-06-03).
A random-access HNSW search touches many non-contiguous graph pages, so a **cold** mmap-view
search on EFS can stack dozens of faults → the resilience report's cited **10–30 s
fault-in** is plausible worst-case (cold class, burst-credit-starved, or large graph).
Mitigations are well known
([Huon Wilson, "mmap is secretly blocking IO", 2024-08](https://huonw.github.io/blog/2024/08/async-hazard-mmap/);
[Erik Rigtorp, "Latency Implications of Virtual Memory"](https://rigtorp.se/virtual-memory/)):
`MAP_POPULATE` / `madvise(MADV_WILLNEED)` prefault, keep files on EFS Standard, or **avoid
mmap entirely and use explicit `pread`** over the network FS. **This is why a managed,
network-API store (§2.9) is strictly better than mmap-on-EFS for the worst hosts: a
deterministic ~100 ms RPC beats an unbounded fault storm.**

### Vector / ANN out-of-core

#### 2.1 mmap-view HNSW with **no** heap promotion (cheapest win)

- **How:** usearch natively supports serving searches directly from an mmap'd file.
  `view()` memory-maps read-only without allocating the full index in RAM; this is a
  headline usearch feature explicitly aimed at cost/memory efficiency and validated in
  production (YugabyteDB, billions of vectors)
  ([usearch DeepWiki](https://deepwiki.com/unum-cloud/usearch),
  [USearch docs](https://unum-cloud.github.io/USearch/), retrieved 2026-06-03).
  We already open in view mode (`store.rs:420`).
- **What changes:** stop the **read-triggered** heap promotion entirely (already true —
  `search` never promotes), and **decouple writes from forcing a permanent heap copy**.
  Concretely: gate `ensure_mutable` so a read-mostly index that takes an occasional write
  promotes, serves the write, and is allowed to **demote back to view** when idle (or
  route writes to a staged delta + periodic rebuild so the serving copy stays mmap'd).
  The cleanest variant for a read-only fleet: a `serve_read_only` mode where the
  `FileWatcher` is disabled and `index_file`/`reindex` are rejected, so promotion never
  fires.
- **RAM reduction:** large — from heap-resident (`N×384×4` + graph) to only the OS-paged
  working set. **Limits:** view mode is strictly read-only (writes UB/error on a view —
  `store.rs:455–462`); usearch's mmap path doesn't shrink the *logical* size, only RSS.
- **Cold/warm latency:** warm (page-cached) is near-heap. **Cold on EFS is the risk** (§2.6).
- **Recall:** unchanged (identical graph). **Rust maturity:** already in tree (usearch 2.25).
  **Adapter fit:** native — it *is* the current `VectorStore`. **Build cost:** near-zero.

#### 2.2 Quantization (orthogonal — stacks with everything)

- **How:** store vectors at lower precision. usearch supports `f16`/`i8` scalar quant and
  product quantization via `ScalarKind` (we currently hard-code `ScalarKind::F32` at
  `store.rs:232`). pgvector offers `halfvec` (f16). Memory factor and recall:

  | Scheme | Memory factor | Recall impact |
  |---|---|---|
  | f16 / `halfvec` | **2×** | negligible — "very little impact on load or recall"; recommended default ([Jonathan Katz](https://jkatz05.com/post/postgres/pgvector-scalar-binary-quantization/), [Neon halfvec](https://neon.com/blog/dont-use-vector-use-halvec-instead-and-save-50-of-your-storage-cost)) |
  | int8 SQ | **4×** | very low — "~99.99 % accuracy retained" in cited benchmarks |
  | PQ | **8–32×** | moderate, recoverable with a re-rank/refine pass |
  | binary | 32× | significant; only for high-dim, distinguishable-bit corpora |

  (all retrieved 2026-06-03)
- **RAM reduction:** 2×–4× essentially for free (f16/int8); compounds with §2.1 mmap and
  with a managed store. **Cold/warm latency:** smaller pages → fewer faults → *helps* EFS.
  **Recall:** f16 ≈ lossless; int8 ≈ lossless; PQ needs a refine pass over uncompressed
  vectors (LanceDB `refine_factor` pattern). **Rust maturity:** usearch quant is a
  constructor flag — trivial. **Adapter fit:** internal to `UsearchStore::with_capacity_hint`.
  **Build cost:** low; main work is a re-embed/rebuild migration and a recall regression gate.

#### 2.3 DiskANN (SSD-resident graph ANN)

- **How:** Vamana graph on SSD + in-RAM PQ for candidate selection; bounded RAM, I/O-bound
  search. Rust landscape (retrieved 2026-06-03): `infinilabs/diskann` (pure Rust, MIT,
  forked from MS partial port), `diskann-rs`/`rust-diskann` (memmap2-based, "6–10× lower
  memory… 15× faster incremental updates", but benched only at ~200k×128), and Microsoft's
  trait-based **DiskANN3** (real-time updates, in-memory + disk providers).
  Sources: [infinilabs/diskann](https://github.com/infinilabs/diskann),
  [rust-diskann](https://github.com/jianshu93/rust-diskann),
  [diskann-rs](https://crates.io/crates/diskann-rs),
  [MS DiskANN](https://github.com/microsoft/DiskANN).
- **RAM reduction:** large and *bounded* (the design goal). **Cold/warm latency:** built
  for SSD random reads; on EFS the same I/O-bound profile applies but each read is a
  network hop — **DiskANN's beam search issues many small random reads, the exact pattern
  EFS punishes**, so DiskANN-on-EFS is *not* obviously better than managed-API (§2.9).
- **Recall:** high at scale (its raison d'être). **Rust maturity:** ⚠️ **weakest link** —
  no pure-Rust crate has a named large-scale production track record; MS DiskANN3 couples
  you to its `DataProvider` trait. **Adapter fit:** clean as a new `VectorStore` impl, but
  it's a *full engine swap*, not a tweak. **Build cost:** high (FFI or large pure-Rust dep,
  index-format migration, new tuning surface). Our per-index scale (tens of thousands of
  chunks) is **well below** where DiskANN's billion-scale advantages pay for its complexity.

#### 2.4 Lance / LanceDB (columnar IVF-PQ, object-store-native)

- **How:** Lance columnar format (Rust) + LanceDB; **disk-based IVF-PQ by design**, builds
  sub-HNSW per IVF partition for larger-than-RAM, and is **S3/object-store native** (the
  Metagenomi 1B-vector-on-S3 + Lambda map-reduce deployment is the reference). Newer
  IVF_SQ (4×), IVF_RQ/RabitQ (≈32×), IVF_HNSW_PQ variants. `nprobes` + `refine_factor`
  tune recall/latency. Caveat: **IVF-PQ training is slow and memory-intensive**.
  Sources: [LanceDB indexing docs](https://docs.lancedb.com/indexing),
  [lancedb Rust crate](https://docs.rs/lancedb/latest/lancedb/),
  [AWS 1B-vector-on-S3 blog](https://aws.amazon.com/blogs/architecture/a-scalable-elastic-database-and-search-solution-for-1b-vectors-built-on-lancedb-and-amazon-s3/)
  (retrieved 2026-06-03).
- **RAM reduction:** large; storage can live on object store, decoupled from compute.
  **Cold/warm latency:** IVF restricts the search to `nprobes` partitions → far fewer
  random reads than a full HNSW graph walk → **structurally friendlier to high-latency
  backing stores (EFS/S3) than raw mmap-HNSW or DiskANN.** **Recall:** tunable
  (`nprobes`/`refine_factor`), PQ-lossy without refine. **Rust maturity:** ✅ strongest of
  the swaps — Lance is Rust-native, $30M Series A (June 2025), active. **Adapter fit:** good
  behind #692 (it's also a `KvStore`-ish columnar store, cross-refs the #692 backing-store
  question directly). **Build cost:** medium-high (new dep, IVF-PQ training pipeline, format
  migration). Best strategic fit **if** Duetto wants object-store-backed indexes.

#### 2.5 Qdrant on-disk + quantization (noted, not embedded)

Server option: on-disk payload/vectors + scalar/PQ quantization. Out of scope because
trusty-search is a single embedded daemon (`cargo install trusty-search` standalone — no
external service). Listed for completeness; if a managed path is chosen, §2.9 services are
the AWS-native equivalents.

### Lexical out-of-core

#### 2.7 Tantivy (mmap segment-based Rust FTS) — replace in-RAM BM25

- **How:** Tantivy is a Lucene-class Rust FTS with an `MmapDirectory` and segment-based
  layout; **out-of-core by design** — query memory is delegated to the OS page cache, with
  FST term dictionaries + SIMD integer compression. **It implements real BM25** ("BM25
  scoring (the same as Lucene)"). Indexing/merging has in-memory peaks, not querying.
  Sources: [tantivy crate](https://crates.io/crates/tantivy),
  [quickwit-oss/tantivy](https://github.com/quickwit-oss/tantivy),
  [Spice AI Tantivy overview](https://spice.ai/learn/tantivy) (retrieved 2026-06-03).
- **RAM reduction:** large — posting lists move from heap (`Arc<RwLock<Bm25Index>>`) to
  mmap'd segments. **Could subsume both the BM25 lane and the chunk store** (Tantivy can
  store the chunk text as a stored field), collapsing two structures.
- **Cold/warm latency:** warm near-heap; cold faults on EFS apply (§2.6) but Tantivy's
  term-dictionary FST localizes the touched pages better than a full posting-list scan.
- **Scoring parity:** ✅ real BM25 — but **our RRF fusion is rank-based, not score-based**
  (RRF k=60, parameter-free), so exact BM25 score parity with `trusty_common::bm25` is
  *not even required*; matching the produced ranking closely enough is. **Rust maturity:**
  ✅ very high (Quickwit/ParadeDB production). **Adapter fit:** needs a `LexicalStore` trait
  (sibling to #692's `VectorStore`/`KvStore`); larger surface than a drop-in. **Build cost:**
  medium — new dep, schema design, reindex, and a ranking regression gate.

### KG out-of-core

#### 2.8 Query the symbol graph from redb (LRU hot subgraphs) vs full petgraph

- **How:** the KG is **already persisted** in `kg_nodes` / `kg_edges` / `kg_edges_rev`
  (`corpus.rs:109–128`), keyed by symbol, with O(1) load of a node's outgoing/incoming
  adjacency (`load_adjacency`, `corpus.rs:872`). KG expansion only ever does 1–2 hop
  `callers_of` / `callees_of` around the top RRF hits (CLAUDE.md "Query Pipeline" step 5).
  So instead of holding the whole `petgraph::DiGraph` resident, do **on-demand keyed redb
  reads** for just the touched symbols, fronted by a small **LRU of hot subgraphs**.
- **Feasibility:** ✅ high — the redb schema is purpose-built for exactly this lookup
  pattern (forward + reverse adjacency split precisely so each direction is one keyed read).
  A 1–2 hop expansion is a handful of point reads against an already-warm redb file.
- **RAM reduction:** medium-large on KG-heavy monorepos (the graph that "can pin 3–5 GB",
  `symbol_graph.rs:33`, drops to an LRU bound). **Cold/warm latency:** a few extra redb
  point reads per query — cheap on local NVMe, and redb's own page cache absorbs the hot
  set; on EFS each read is a network hop but bounded (a handful, not a graph walk).
  **Recall/quality:** identical traversal results (same edges). **Rust maturity:** in-tree
  (redb). **Adapter fit:** internal to `SymbolGraph` + `CorpusStore`. **Build cost:** low-medium.

### Whole-index tiering / managed offload (cross-ref; not re-derived)

#### 2.9 Managed services (cross-ref #692) — zero local RAM, network latency

For the **EFS fleet where mmap cold-paging is worst**, pushing vectors to a managed,
network-API store removes the local RAM pressure *and* replaces unbounded fault storms with
deterministic RPCs:

- **AWS S3 Vectors** (GA Dec 2025): storage-first, ~100 ms for frequent queries / sub-second
  infrequent, **pay-per-GB+per-query, ~90 % cheaper** than always-on engines. **The
  benchmarked example is almost exactly our shape: 250k vectors across 40 indexes, 1M
  queries/month → ~$11/mo vs ~$350/mo OpenSearch minimum.** Throughput ceiling: hundreds
  req/s per index — fine for a dev-tool query load.
  ([AWS S3 Vectors GA](https://aws.amazon.com/blogs/aws/amazon-s3-vectors-now-generally-available-with-increased-scale-and-performance/),
  [S3 Vectors vs OpenSearch benchmark](https://medium.com/@shaileshkumarmishra/benchmarked-aws-s3-vectors-against-opensearch-and-postgresql-the-results-will-surprise-you-9ffe6c394da7),
  retrieved 2026-06-03).
- **Aurora pgvector** (`halfvec` for 2× memory): best when relational queries co-exist;
  per-instance cost.
- **OpenSearch Service**: 10–100 ms (sub-10 ms in-memory), high QPS, ~$350/mo floor; disk
  mode trades 100–200 ms for the same recall.
  ([AWS vector DB guidance](https://docs.aws.amazon.com/prescriptive-guidance/latest/choosing-an-aws-vector-database-for-rag-use-cases/vector-db-comparison.html)).

These sit behind the **same #692 `VectorStore` adapter** as a remote impl — the trait
already anticipates this ("possibly remote tomorrow", `store.rs:97–106`).

#### 2.10 #705 idle-index offload (cross-ref)

#705 Track B1–B4: extend idle eviction down to HNSW+BM25+KG (full cold-index offload with
lazy reload) and add a **global RAM budget with LRU whole-index eviction**. This is the
*orthogonal* lever to out-of-core: even with mmap/quantization, a hard cross-index ceiling
is needed so 200 indexes can't collectively blow the host. **Out-of-core (this doc) lowers
the per-index cost; #705 caps the aggregate.** They compose.

### Options matrix

| Option | RAM reduction | Warm p95 | Cold p95 (esp. EFS) | Recall | Rust maturity | #692 adapter fit |
|---|---|---|---|---|---|---|
| 2.1 mmap-view HNSW, no promote | High | ≈ heap | ⚠️ fault storm (10–30 s worst) | unchanged | in-tree | native (`VectorStore`) |
| 2.2 Quantization (f16/int8/PQ) | 2×/4×/8–32× | ≈ / faster | better (smaller pages) | ≈lossless→tunable | in-tree (usearch flag) | internal |
| 2.3 DiskANN | High, bounded | good (SSD) | ⚠️ many small reads on EFS | high | ⚠️ low (no prod track record) | new engine swap |
| 2.4 Lance/LanceDB IVF-PQ | High (object-store) | good | better (nprobes-bounded reads) | tunable | ✅ Rust-native | good (also KvStore) |
| 2.7 Tantivy (lexical) | High | ≈ heap | ⚠️ faults, FST-localized | real BM25; RRF rank-safe | ✅ high | needs `LexicalStore` trait |
| 2.8 KG-from-redb + LRU | Med-large | +few redb reads | bounded point reads | identical | in-tree (redb) | internal |
| 2.9 Managed (S3 Vectors/Aurora/OS) | **Zero local** | 10–100 ms net | **deterministic RPC** | tunable | client only | remote `VectorStore` |
| 2.10 #705 LRU whole-index evict | Aggregate cap | reload on miss | reload on miss | n/a | in-tree | orthogonal |

---

## Part 3 — Recommendation & phased plan

### 3.1 Opinionated recommendation

**Two near-term cheap wins (ship behind the existing `VectorStore`, no engine swap):**

1. **Quantization to int8 (4×), f16 fallback (2×) — §2.2.** Highest RAM-per-line-of-code
   payoff in the codebase. It is a constructor flag on `UsearchStore` plus a rebuild
   migration and a recall gate. It **compounds with every other option** (smaller pages
   even help EFS) and is the one change that helps *all* hosts, EFS or not.
2. **Stop promoting read-mostly HNSW to heap — §2.1.** Add a `serve_read_only` / "no-promote"
   mode so server-fleet indexes stay in mmap-view: gate `ensure_mutable`, disable the
   `FileWatcher` on read-only indexes, and (optionally) allow demote-to-view on idle. On a
   read-serving host this is the difference between `N × (0.1–0.5 GB heap)` and `N × (paged
   working set)`.

**Plus one near-term internal win that needs no new dep:**

3. **KG-from-redb with an LRU of hot subgraphs — §2.8.** The redb schema already supports
   it; removing the always-resident petgraph reclaims up to 3–5 GB on KG-heavy indexes.

**Strategic engine choice — be opinionated:**

- **For the EFS fleet specifically: go managed (AWS S3 Vectors) behind the #692 remote
  `VectorStore` adapter.** This is the *only* option that turns the worst-case EFS cold-page
  fault storm into a bounded ~100 ms RPC, and the cost math is decisive for our exact shape
  (~$11/mo vs ~$350/mo for 40 indexes / 1M queries). It also removes local vector RAM
  entirely, directly attacking the 25–100 GB problem on the hosts that suffer most.
- **For the lexical lane, adopt Tantivy (§2.7)** as the second strategic move — it is the
  highest-maturity out-of-core swap, our RRF fusion is rank-based so exact BM25 parity isn't
  required, and it can subsume the chunk store. **Prefer Tantivy over DiskANN.**
- **Defer DiskANN (§2.3).** Our per-index scale is far below where it pays for its low Rust
  maturity. **Consider Lance/LanceDB (§2.4) only if** Duetto wants self-hosted
  object-store-backed indexes instead of a managed AWS service — it's the strongest *Rust*
  larger-than-RAM engine and IVF's bounded reads are EFS-friendlier than DiskANN, but it's a
  bigger lift than going managed.

**Why this ordering:** wins 1–3 are individually shippable, reversible, need no new
dependency, and each lands behind seams that already exist (`VectorStore`, `SymbolGraph`,
`CorpusStore`). The strategic moves (managed vectors, Tantivy lexical) are then de-risked by
landing *on top of* the #692 adapter once it exists, and after #702 stabilizes the redb
format the KG and corpus work depends on.

### 3.2 Phased, individually-shippable sub-tickets

Sequenced against the **#692 adapter** and the in-flight **#702 redb 4.x** work.

| # | Suggested title | Depends on | Ships independently? |
|---|---|---|---|
| P0 | **bench: out-of-core measurement harness (RAM, warm p95, cold page-fault p95: local NVMe vs EFS)** | — | yes |
| P1 | **feat(store): int8/f16 vector quantization behind `ScalarKind` flag + rebuild migration + recall gate** | P0 | yes |
| P2 | **feat(store): `serve_read_only` / no-promote HNSW mode (keep mmap-view; gate `ensure_mutable`; disable watcher)** | P0 | yes |
| P3 | **feat(kg): on-demand `callers_of`/`callees_of` from redb adjacency + hot-subgraph LRU; drop always-resident petgraph** | #702 (redb 4.x) | yes |
| P4 | **refactor(#692): land `VectorStore`/`KvStore` adapter seam (formalize existing trait; add `LexicalStore`)** | #692 | yes (enabler) |
| P5 | **feat(store): remote `VectorStore` impl — AWS S3 Vectors backend for the EFS fleet (zero local vector RAM)** | P4 | yes |
| P6 | **feat(lexical): Tantivy `LexicalStore` impl behind the adapter; RRF ranking regression gate; optional chunk-store subsumption** | P4 | yes |
| P7 | **(stretch) spike: Lance/LanceDB IVF-PQ as an object-store `VectorStore` — only if self-hosted object store is preferred over managed** | P4, P1 | yes (spike) |
| P8 | **(cross-ref #705) global RAM budget + LRU whole-index eviction — caps the aggregate after per-index cost is lowered** | #705 | yes |

### 3.3 Acceptance criteria

- **Quantization (P1):** an int8-quantized index uses **≤ 1/3 the RSS** of the f32 baseline
  for the same corpus, with **Recall@10 ≥ 0.97 × f32 baseline** on the benchmark corpus
  (MRR@5 / Recall@10 harness in `tests/benchmark_harness.rs`). Per-index RSS is **bounded**
  and reported at startup.
- **No-promote read mode (P2):** a 10 GB index (on disk) served on a host with **4 GB free
  RAM** stays resident in mmap-view; **warm p95 ≤ X ms** (X = local-NVMe baseline + 25 %)
  and **cold p95 ≤ Y ms** (Y measured per backing store in P0; the criterion is "no OOM-kill
  and no unbounded stall", with EFS cold p95 quantified and documented, not necessarily met
  against a fixed target).
- **KG-from-redb (P3):** identical `callers_of`/`callees_of` results vs the in-memory graph
  on the test corpus; resident KG heap bounded by the LRU cap; added per-query latency
  **≤ 5 ms p95 on local NVMe**.
- **Managed backend (P5):** zero local vector arena RAM for offloaded indexes; **cold p95 on
  the EFS fleet ≤ 200 ms** (vs the multi-second mmap-fault worst case); cost projected
  ≤ $50/mo for the production index/query volume.
- **Tantivy (P6):** RRF top-10 ranking overlap **≥ 0.95** vs the current BM25 lane on the
  benchmark queries; lexical lane RSS reduced; no regression in end-to-end MRR@5.

### 3.4 Benchmark plan (P0 — the prerequisite)

Measure on a **real corpus** (reuse `baseline_trusty_tools` /
`tests/benchmark_harness.rs`; the regression suite already exists — see CLAUDE.md
"Run trusty-search performance regression suite"). For each configuration measure **three**
numbers, not one:

1. **RSS** (steady-state resident, per index and aggregate) — confirms the RAM-reduction claim.
2. **Warm p95** query latency (page cache hot) — confirms no serving regression.
3. **COLD page-fault p95** — drop caches / fresh mount, first query after open. **Measure on
   both local NVMe and EFS** (Standard *and* IA, since lifecycle transitions are the cliff).
   This is the number that decides mmap-view (§2.1) vs managed (§2.9) for the fleet.

Configurations to sweep: `{f32, f16, int8}` × `{heap, mmap-view}` × `{local NVMe, EFS-Std,
EFS-IA}`, on a representative Duetto-sized index (tens of thousands of chunks), plus one
synthetic large index near the 1M cap. Record results as a versioned snapshot under
`docs/trusty-search/regression-testing/` per the doc conventions, and feed the deltas into
the cross-release tracker (#129).

---

## Appendix — primary code citations

| Claim | File:line |
|---|---|
| `view()` mmap open on warm-boot | `crates/trusty-search/src/core/store.rs:420` |
| view→heap promotion (`ensure_mutable`) | `store.rs:470–478` |
| promotion slow path (`promote_view_to_mutable`, `Index::load`) | `store.rs:482–516` |
| search never promotes (read-only stays mmap) | `store.rs:589–624`, test `store.rs:1214–1217` |
| writes trigger promotion | `store.rs:557` (`upsert`), `store.rs:770` (`upsert_batch`), `store.rs:628` (`remove`) |
| HNSW size cap / RAM estimate | `store.rs:48`, `store.rs:47` |
| `VectorStore` trait (the #692 seam) | `store.rs:107–163` |
| `ScalarKind::F32` hard-coded (quant lever) | `store.rs:232` |
| KG persisted in redb (forward/reverse adjacency) | `corpus.rs:109`, `:118`, `:127–128` |
| `load_kg_graph` / `load_adjacency` (on-demand reads) | `corpus.rs:627–651`, `:872–891` |
| redb corpus + cache ceiling (64 MB default) | `corpus.rs:24–30`, `:52`, `:67` |
| chunk-text idle eviction (the only eviction today) | `indexer/mod.rs:1053`, `:1101` |
| memory tiers / per-index caps / 64 GB daemon ceiling | `memory_policy.rs:324–332`, `:101–105`, `:130–151` |
| `Duration::MAX` is a reindex HTTP timeout, not a cool-after | `commands/reindex_engine.rs:337` |
