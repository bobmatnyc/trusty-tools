# trusty-search — System Architecture

> **Status:** Canonical · Living Document
> **Last reviewed:** 2026-06-01
> **Derived from:** code/docs/tickets audit (v0.22.2)

**Status legend:** ✅ Implemented · 🟡 Partial · 🔵 Designed-not-built · ⚪ Aspirational

This document describes how trusty-search fits together: the daemon/registry
process model, the staged indexing pipeline, the bundled embedder sidecar, the
storage layer, the query-dispatch pipeline, the MCP/HTTP framing rules, the
memory auto-tuning knobs, and the execution-provider paths. It closes with the
source-module map.

---

## 1. Process Model

trusty-search is a **single Rust crate** (`crates/trusty-search/`) producing two
binaries plus a library:

- **`trusty-search`** (`src/main.rs`) — the CLI + daemon + MCP server.
- **`trusty-embedderd`** (`src/bin/trusty-embedderd.rs`) — a thin shim calling
  `trusty_embedderd::run()`; the actual sidecar logic lives in the
  `crates/trusty-embedderd/` library crate. Declaring it as both a Cargo
  dependency *and* a `[[bin]]` is what makes `cargo install trusty-search`
  install **both** binaries in one command (Cargo.toml `[[bin]]` + dep, #187).
- **`trusty_search`** (`src/lib.rs`) — an rlib re-publishing `core`, `service`,
  and `mcp` so integration tests and host crates (open-mpm) can `use
  trusty_search::SearchMcpService` directly (#115).

```
cargo install trusty-search
        │  installs two binaries
        ├─ trusty-search        (CLI / daemon / MCP)
        └─ trusty-embedderd     (ONNX/CoreML embedder sidecar)

trusty-search start  ──►  HTTP daemon (singleton, loopback)
    │
    ├─ IndexRegistry: DashMap<IndexId, Arc<IndexHandle>>     ← many projects, one process
    │     └─ IndexHandle
    │           ├─ CodeIndexer: Arc<RwLock<…>>  (usearch HNSW + redb corpus)
    │           ├─ Bm25Index   (per-query, built from corpus)
    │           ├─ SymbolGraph: Arc<…>  (petgraph, tree-sitter derived)
    │           ├─ FileWatcher (notify, 500 ms debounce)
    │           └─ QueryCache  (LruCache<QueryHash, Vec<f32>>)
    │
    ├─ axum router (HTTP/2, CORS/trace/gzip)  ← REST + SSE + /metrics + /ui
    ├─ EmbedPool (priority lanes)  ──►  EmbedderSupervisor
    │                                       └─ spawns trusty-embedderd (lazy)
    └─ MCP server (when `serve`): stdio loop or HTTP/SSE router → proxies to daemon
```

### Key properties

- **Singleton daemon per machine.** An OS advisory exclusive lock on a PID
  lockfile in the user data dir enforces it; a second `start` exits with
  `DaemonError::AlreadyRunning` (`src/service/daemon.rs`). ✅
- **Auto-port + discoverable.** The daemon binds from a requested port walking
  forward to the first free one, then writes `port.lock`
  (`~/Library/Application Support/trusty-search/port.lock` on macOS,
  `$XDG_DATA_HOME/trusty-search/port.lock` on Linux). The CLI and MCP server
  resolve the live port from that file. The `port` subcommand reports the live
  port in three machine-parsable formats (`--addr`, `--json`, default bare). ✅
- **Loopback-only, no auth.** The daemon binds `127.0.0.1` and trusts every
  caller by design — **never** bind a non-loopback interface (PRD non-goal). ✅
- **Concurrent multi-index reads.** `DashMap` shard-locks per index (different
  indexes never contend); `Arc<RwLock<CodeIndexer>>` is reader-priority so many
  searches against one index never block. Indexing uses a `tokio::Semaphore` to
  avoid thread-pool starvation. ✅
- **MCP is a thin proxy.** `serve` does **not** re-implement search; the
  `McpServer` dispatcher proxies each tool call to the running daemon over HTTP
  (`src/mcp/`). The daemon must already be running. ✅
- **Graceful shutdown.** SIGTERM (e.g. from `launchctl bootout`) drains in-flight
  requests via `axum::serve(…).with_graceful_shutdown(shutdown_signal())` before
  the process exits. The `mcp_bridge` reconnects automatically with exponential
  backoff, making upgrades transparent to running LLM sessions. ✅

---

## 2. Indexing Pipeline (staged)

A reindex walks the project root, chunks each file, embeds the chunks, commits
them durably, and builds the symbol graph. The pipeline is **staged** so each
stage can be independently skipped:

```
walk_source_files(root)            # ignore-crate gitignore-aware, SOURCE_EXTS filter, size/minify guards
   │   (src/service/walker.rs)
   ▼
Stage 1 — chunk + BM25             # chunk_ast() tree-sitter → RawChunk[] + RawEntity[]
   │   (src/core/chunker/)          # BM25 index built from the chunk corpus
   │                                # `lexical_only` stops here (daemonized ripgrep)
   ▼
Stage 2 — embed (vector)           # parse_and_embed_files() OUTSIDE the write lock
   │   (src/core/indexer/ingest.rs) # batched ONNX embed via the embedder sidecar
   │                                # `skip_kg`/`lexical_only` independence (orthogonal)
   ▼
commit_parsed_batch()              # holds the write lock ONLY for the redb + HNSW commit
   │   (atomic per-batch; O(batch) not O(corpus))
   ▼
Stage 3 — knowledge graph          # SymbolGraph rebuilt from corpus (CALLS/IMPORTS/…)
       (src/core/symbol_graph.rs)   # skipped when skip_kg / lexical_only / Stage-1-minimal
```

- **Split-lock design.** Parsing and embedding (the slow part) run outside the
  `RwLock`; only the redb+HNSW commit takes the write lock, keeping searches
  responsive during a reindex. ✅
- **Incremental skip.** sha2 content fingerprints skip unchanged files across
  daemon restarts; `--force` clears the per-index hash cache. ✅
- **Memory-bounded.** The reindex orchestrator polls RSS via `memguard`; on a
  `TRUSTY_MEMORY_LIMIT_MB` breach it skips remaining batches (already-committed
  chunks stay searchable) and reports `memory_limit_hit: true`. ✅
- **Sidecar stall hardening.** `StdioEmbedderClient::embed_batch` applies a
  per-call timeout (`TRUSTY_EMBEDDERD_CALL_TIMEOUT_SECS`, default 120 s) around
  the sidecar `read_line` call. On a stall the supervisor respawns the sidecar
  and the reindex continues rather than hanging the SSE stream. ✅
- **Progress.** SSE events (`start`/`progress`/`complete`/`error`,
  plus `embedder_init`/`embedder_ready` when the sidecar cold-starts, and
  per-file chunking-progress events during Stage 1) on a `tokio::broadcast`
  channel with a 500-event replay buffer so late subscribers still see `start`
  (`src/service/reindex.rs`). ✅
- **Progress-aware foreground wait.** The `reindex_engine` CLI event loop treats
  any forward-progress SSE event as a stall-timer reset; `--timeout` is now an
  optional flag (`Option<u64>`) — when omitted the wait is open-ended and only
  the absence of SSE events for `stall_secs` (default 120) triggers a timeout.
  `ReindexOptions` gains `timeout_explicit: bool` and `stall_secs: u64`. ✅
- **Walk diagnostics.** `last_walk_started_at`/`files_seen`/`files_skipped`/
  `error` are recorded per reindex so a zero-chunk outcome is explainable (#280). ✅

### Stage-skipping modes

| Mode | Stage 1 (BM25) | Stage 2 (vector) | Stage 3 (KG) | Trigger |
|---|---|---|---|---|
| Full hybrid | ✅ | ✅ | ✅ | default |
| `skip_kg` | ✅ | ✅ | — | `--no-kg` / YAML / HTTP / `TRUSTY_NO_KG=1` (#313) |
| `lexical_only` | ✅ | — | — | `--lexical-only` / `lexical_only: true` (#111) |
| Stage-1-minimal | ✅ | — | — | lexical_only + symbol-graph skip (#312) |

`skip_kg` and `lexical_only` are **orthogonal**: setting both leaves only BM25.

---

## 3. Embedder Sidecar (`trusty-embedderd`)

The ONNX/CoreML embedding arena is the daemon's largest, spikiest memory
consumer. trusty-search moves it into a **separate supervised subprocess**
(`crates/trusty-embedderd/`), mirroring industry ML-serving topology (Triton,
vLLM, TEI, ollama) and substantially reducing the search daemon's RSS (#110).

```
trusty-search daemon
   │
   ├─ EmbedPool (interactive/background priority lanes, src/service/embed_pool.rs)
   │
   └─ EmbedderSupervisor (src/service/embedder_supervisor.rs)
         │  re-exports trusty_common::embedder_client supervisor types
         │  + trusty-search defaults, default_socket_path(), locate_embedderd_binary()
         │
         └─ LazyEmbedderHandle  ──spawn on first embed request──►  trusty-embedderd
                                                                      (--stdio | --socket | http)
```

- **Single install, two binaries.** See §1: the `[[bin]]` + Cargo-dep trick. ✅
- **Lazy spawn (#315).** Binary discovery runs at boot (fails fast with an
  install hint if missing), but the sidecar process is spawned on the **first
  embed request** (reindex, hybrid search, or `context_inference`), not at
  startup. `TRUSTY_EMBEDDERD_IDLE_SHUTDOWN_SECS` kills it after idle and resets
  the spawn gate. ✅
- **Transport selection (`TRUSTY_EMBEDDER`).** `auto`/`stdio` (default,
  supervised stdio subprocess), `in-process` (explicit ONNX in the daemon — never
  silent), `http://…`, `unix:/…`, or `candle` (Metal via `--features candle`). ✅
- **Supervisor tuning.** `TRUSTY_EMBEDDERD_BIN`,
  `*_STARTUP_TIMEOUT_SECS` (30), `*_RESTART_BACKOFF_MAX_SECS` (60),
  `*_MAX_RESTARTS` (5), `*_IDLE_SHUTDOWN_SECS` (0 = off),
  `TRUSTY_EMBEDDER_INIT_TIMEOUT_SECS` (60),
  `TRUSTY_EMBEDDERD_CALL_TIMEOUT_SECS` (120 — per-batch stdio read deadline). ✅
- **Sidecar internals.** `crates/trusty-embedderd/src/`: `protocol.rs`
  (JSON-RPC embed protocol), `batch_queue.rs` (request batching),
  `stdio_server.rs`, `uds_server.rs`, and an optional `http-server` feature
  (axum, #250). Both the standalone binary and the bundled shim call
  `trusty_embedderd::run()` — zero divergence.

---

## 4. Storage Layer

Three storage substrates, all local and embedded:

| Substrate | Backend | Holds | Module |
|---|---|---|---|
| **Chunk corpus** | redb 2.6 | `chunk_id → RawChunk`, `file → RawEntity[]`, `_meta.schema_version` | `src/core/corpus.rs`, `src/core/store.rs` |
| **Vector index** | usearch 2.25 (HNSW, in-memory) | 384-dim INT8 vectors + `chunk_id ↔ u64 key` sidecar | `src/core/store.rs` |
| **Symbol graph** | petgraph 0.6 (in-memory) | `DiGraph<SymbolNode,()>` keyed by symbol name | `src/core/symbol_graph.rs` |

- **Why redb (#28).** The prior `chunks.json` full-rewrite-per-batch caused a
  memory explosion (~400 MB blob on a 200k corpus). redb gives crash-safe atomic
  **per-batch** commits (O(batch)), incremental writes, and streamed startup
  reads without holding two copies in RAM. ✅
- **HNSW persistence.** usearch persists vectors + graph keyed by `u64`; a
  JSON sidecar maps `chunk_id → u64 key` (and `next_key`) so a restored index
  can translate HNSW matches back to chunk ids. The graph is **pinned hot**
  (`Duration::MAX` cool-after) for zero cold-start. ✅
- **Warm-boot persistence (#85).** A registry TOML (`<data_dir>/indexes.toml`,
  `IndexId → root_path`) plus per-index dirs (`<data_dir>/indexes/<id>/` with
  `hnsw.usearch` + the redb corpus) survive restarts; `restore_indexes` reloads
  them on startup (`src/service/persistence.rs`, `persistence_loader.rs`). ✅
- **Idle eviction.** The in-memory `RawChunk` map for durably-backed indexes is
  evicted after `TRUSTY_CHUNKS_IDLE_EVICT_SECS` (300 s default); readers
  rehydrate from redb; BM25 + symbol graph stay hot. ✅

### Schema migrations

A forward-only, idempotent migration framework evolves the redb schema without
manual reindexing (`src/core/migration/`, shared kernel from `trusty-common`,
#179). The `_meta` table stores a `u32 schema_version` (legacy DBs = 0). On
startup `run_migrations` computes the chain `source_version >= current`, applies
each in sequence, and writes the new version **after** each successful (idempotent)
`apply`, in a background `tokio::spawn` so queries keep serving.
`TRUSTY_DISABLE_MIGRATIONS=1` skips it. Registered: `m001` (re-chunk Rust
`pub const`/`static`, #143), `m002`.

---

## 5. Query-Dispatch Pipeline

Every hybrid search runs the same five-step pipeline (`src/core/indexer/search.rs`):

```
1. Classify intent      QueryClassifier (sub-ms regex)  → Definition|Usage|Conceptual|BugDebt|Unknown
   (src/core/classifier.rs)
2. Route weights        intent → (α vector, β BM25, use_kg_first)
3. Search lanes         4×top_k HNSW candidates  +  per-query BM25 over the corpus
4. Fuse                 rrf_fuse(): Σ weight·1/(k+rank), k=60, rank-only   (src/core/search/rrf.rs)
5. KG expand            (Usage only) 1–2 hop callers_of/callees_of @ 0.7× trigger RRF score
   + MMR rerank         mmr_rerank() cosine-diversity                       (src/core/mmr.rs)
   + branch boost       branch_files / branch → ×branch_boost (default 1.5) (#122)
   + docs penalty       down-rank .md/changelog on code intent             (#72/#77)
   → return CodeChunk[] with per-result match_reason ("hybrid","hybrid+kg","bm25","vector","fallback:ripgrep")
```

### Intent → routing weights

| Intent | α (vector) | β (BM25) | KG-first |
|---|---|---|---|
| Definition | 0.3 | 0.7 | false |
| Usage | 0.5 | 0.5 | **true** |
| Conceptual | 0.8 | 0.2 | false |
| BugDebt | 0.1 | 0.9 | false |
| Unknown | 0.6 | 0.4 | false |

The classifier is a sub-ms regex over the query text; KG expansion is gated to
**Usage** intent only.

### `CodeChunk` (result shape)

`{ id ("{path}:{start}:{end}"), file, start_line, end_line, content,
function_name?, score, compact_snippet? (7-line, token-efficient),
match_reason }`. Complexity/blame fields were removed when code-quality moved to
**trusty-analyze** (#71).

---

## 6. MCP / HTTP Framing

🔴 **stdout is reserved.** When running as an MCP stdio server, stdout carries
line-delimited JSON-RPC 2.0 frames; a stray `println!` corrupts the protocol.
All logging goes to **stderr** via `tracing` (`init_tracing`). This is a hard
project rule.

### MCP server (`src/mcp/`)

- `McpServer` — pure dispatcher: takes a JSON-RPC `Request`, proxies the named
  tool to the daemon over HTTP, returns a `Response`.
- `stdio` — line-delimited JSON-RPC loop on stdin/stdout (default `serve`).
- `sse` — axum router exposing `POST /mcp` and `GET /mcp/sse` (`serve --http`).
- `tools` — the 19-tool catalogue + JSON-RPC error codes (`src/mcp/tools.rs`).
- `openrpc` — an OpenRPC service descriptor.

### HTTP API (`src/service/server.rs`)

axum 0.8 + tower-http (CORS permissive `*`, trace, gzip), HTTP/2. All bodies are
`application/json` except SSE (`text/event-stream`). Errors return
`{ "error": "<message>" }` with standard codes (404 unknown index, 503 subsystem
disabled, 500 internal).

| Route | Purpose |
|---|---|
| `GET /health` | liveness/readiness (`{status, version, indexes, update_available?, embedder_error?}`) |
| `GET /indexes[?format=tree][?details=true]`, `POST /indexes`, `DELETE /indexes/{id}` | registry management; `?details=true` returns `{id, size_bytes}` objects; `?format=tree` reflects the nested hierarchy (#404) |
| `GET /indexes/{id}/status` | chunk count + walk diagnostics (#280) |
| `POST /indexes/{id}/search` | hybrid search (+ branch boost) |
| `POST /indexes/{id}/search_similar` | code-to-code similarity (re-embeds seed on LRU miss for skip_kg indexes, #484) |
| `POST /indexes/{id}/index-file`, `/remove-file` | single-file add/remove |
| `POST /indexes/{id}/reindex`, `GET …/reindex/stream` | fire-and-forget + SSE |
| `GET /indexes/{id}/chunks` | paginated chunk enumeration |
| `GET /indexes/{id}/call_chain` | annotated call tree (503 if skip_kg, #313) |
| `GET /indexes/{id}/graph`, `…/graph/stats` | symbol-graph export |
| `POST /indexes/{id}/grep`, `POST /grep` | per-index / cross-index grep |
| `POST /search` | cross-index fan-out (context-inference weighted + nested hierarchy boost #404) |
| `POST /upgrade` | check/install a new version via crates.io + cargo-install (#537) |
| `GET /metrics` | Prometheus text (#41) |
| `GET /status/stream`, `GET /logs/tail` | live status / log tail |
| `POST /admin/stop` | graceful daemon stop |
| `POST /chat`, `GET /api/chat/providers` | OpenRouter chat proxy |
| `GET /ui`, `/ui/`, `/ui/{*path}` | embedded Svelte admin UI |

---

## 7. Memory Auto-Tuning

`MemoryPolicy::detect()` reads total RAM (`hw.memsize` on macOS, `/proc/meminfo`
on Linux) at startup, classifies into a tier, sets default caps, overrides any
field whose env var is set, and writes resolved values back into the process env
so module-level readers pick them up (`src/core/memory_policy.rs`). Precedence:
**shell env > `daemon.env` > tier default**. The resolved tier is logged at boot.

| Tier | RAM | MAX_CHUNKS | EMBEDDING_CACHE | MAX_BATCH_SIZE | BM25_CORPUS_CAP | MAX_KG_NODES |
|---|---|---|---|---|---|---|
| Tiny | < 8 GB | 50 000 | 500 | 64 | 20 000 | 30 000 |
| Small | 8–15 GB | 100 000 | 1 000 | 128 | 50 000 | 75 000 |
| Medium | 16–31 GB | 200 000 | 5 000 | 256 | 100 000 | 150 000 |
| Large | 32–63 GB | 400 000 | 10 000 | 512 | 200 000 | 300 000 |
| XLarge | ≥ 64 GB | 800 000 | 20 000 | 512 | 400 000 | 500 000 |

`MEMORY_LIMIT_MB` is `clamp(RAM × 25%, 1 GB, 64 GB)`. `MAX_BATCH_SIZE` is
auto-derived as `floor(limit_mb × 0.75 / 55)` clamped `[32, 512]` (55 MB =
empirical ORT arena cost per batch slot, #95).

> **Caveat (PRD Q8):** `start` hard-checks a 16 GB minimum (#291), so the Tiny/
> Small tiers are effectively gated off at runtime even though the policy code
> still defines five tiers. README and CLAUDE.md describe this inconsistently.

**Env knobs:** `TRUSTY_MAX_CHUNKS`, `TRUSTY_EMBEDDING_CACHE`,
`TRUSTY_MAX_BATCH_SIZE`, `TRUSTY_BM25_CORPUS_CAP`, `TRUSTY_MAX_KG_NODES`,
`TRUSTY_MEMORY_LIMIT_MB`, `TRUSTY_CHUNKS_IDLE_EVICT_SECS`,
`TRUSTY_COREML_BATCH_SIZE`, `TRUSTY_COREML_TRIPWIRE_MB`, `TRUSTY_SKIP_RAM_CHECK`.

`memguard` (`src/core/memguard.rs`) polls RSS during reindex (per-PID, including
the sidecar) and enforces the soft ceiling; non-fatal probe failures return 0
and disable the tripwire gracefully rather than crashing.

---

## 8. Execution-Provider Paths (ONNX / CoreML / CUDA / Candle)

The embedder runs all-MiniLM-L6-v2 (INT8, 384-dim) through one of several
execution providers, resolved at sidecar startup and reported in the log
(`embedder initialized: model=AllMiniLML6V2(Q) dim=384 provider=…`):

| Provider | Activation | Notes |
|---|---|---|
| **CPU** (default) | always available | `bundled-ort` static ORT (glibc ≥ 2.38 / macOS) |
| **CoreML** (Metal GPU / ANE) | auto on `aarch64`-macOS since v0.3.13 | no feature flag; `coreml` feature is a no-op alias; batch 32 ANE-optimal + tripwire |
| **CUDA** | `--features cuda` (+ `--no-default-features`) | activates `ort/cuda` + `ort/load-dynamic`; auto-bumps batch to 512; `--device gpu` fails fast if absent |
| **Candle (Metal)** | `--features candle` + `TRUSTY_EMBEDDER=candle` | bypasses ONNX jetsam-kill risk on Apple Silicon (`src/service/candle_embedder.rs`, #41 phase 4) |

`--device cpu|gpu|auto` (persisted to `daemon.env`) forces/relaxes provider
selection at runtime. On glibc < 2.38 + CUDA, set `ORT_DYLIB_PATH` to a host
`libonnxruntime.so` (the load-dynamic path; #114). Feature chain:
`trusty-search/cuda → trusty-common/embedder-cuda → {fastembed/cuda,
fastembed/ort-load-dynamic, ort/cuda, ort/load-dynamic}` — `ort/cuda` is the
load-bearing GPU link.

---

## 9. Source Module Map

Single crate; three module trees (`core`, `service`, `mcp`) plus `commands`,
`bin`, and top-level files. Paths under `crates/trusty-search/`.

| Module | Responsibility |
|---|---|
| `src/main.rs` | CLI entry: clap parsing → `commands::*` dispatch; friendly error/exit handling (#104). |
| `src/lib.rs` | Re-publishes `core` / `service` / `mcp`; surfaces `SearchMcpService` (#115). |
| `src/bin/trusty-embedderd.rs` | Shim → `trusty_embedderd::run()` (bundled sidecar). |
| `src/config.rs`, `src/detect.rs`, `src/doctor.rs` | User config, project auto-detection, diagnostics. |
| **`src/core/`** | Search engine internals (no I/O server code). |
| `core/chunker/` | tree-sitter AST chunker (`mod.rs`), `classify.rs`, `inherits.rs`, `walk.rs`. |
| `core/indexer/` | `CodeIndexer` orchestrator: `mod.rs`, `ingest.rs`, `persist.rs`, `files.rs`, `search.rs`, `docs_penalty.rs`, `archive.rs`, `migrations.rs`, `tests.rs`. |
| `core/search/rrf.rs` | Reciprocal Rank Fusion (k = 60). |
| `core/search/hierarchy.rs` | `IndexHierarchy` — parent/child maps from root_path prefix containment; sub-index lane-weight boost + post-RRF dedup (#404 MVP). |
| `core/classifier.rs` | `QueryClassifier` / `QueryIntent` + lane weights. |
| `core/bm25.rs` | Re-export of `trusty_common::bm25` (`Bm25Index`, `tokenize`, #156). |
| `core/store.rs` | `VectorStore` / usearch HNSW wrapper + key sidecar. |
| `core/corpus.rs` | redb-backed durable `CorpusStore` (#28). |
| `core/symbol_graph.rs` | petgraph `SymbolGraph` (callers_of / callees_of). |
| `core/mmr.rs` | MMR diversity rerank + cosine similarity. |
| `core/memory_policy.rs` | RAM detection + tier caps (`MemoryPolicy`/`MemoryTier`). |
| `core/memguard.rs` | RSS polling + soft memory ceiling. |
| `core/migration/` | Forward-only schema migrations (`mod.rs`, `m001`, `m002`). |
| `core/entity.rs`, `ner.rs`, `concept_cluster.rs` | Entity extraction; optional ONNX NER; optional k-means concept clusters (`clustering` feature). |
| `core/registry.rs` | `IndexRegistry`, `IndexHandle`, `IndexId`, `WalkDiagnostics`. |
| `core/repo_config.rs`, `project_config.rs` | `trusty-search.yaml` multi-index config. |
| `core/scip_ingest.rs` | SCIP entity ingest trait (`from_refs` ✅, protobuf 🔵 #105). |
| `core/git.rs`, `output.rs`, `embed.rs` | git diff helpers, result formatting, `Embedder` trait/`FastEmbedder`. |
| **`src/service/`** | The HTTP daemon and all server-side machinery. |
| `service/server.rs` | axum router + `SearchAppState` (all HTTP handlers). |
| `service/daemon.rs` | PID lockfile, auto-port, graceful shutdown, `daemon.env`. |
| `service/reindex.rs` | Reindex orchestration + SSE progress/replay. |
| `service/walker.rs` | gitignore-aware source walk (`ignore` crate, #100). |
| `service/embedder_supervisor.rs` | Sidecar supervisor + `LazyEmbedderHandle` (#315). |
| `service/embed_pool.rs` | Priority embed worker pool (#41). |
| `service/context_inference.rs` | Per-index relevance summary for fan-out (#112). |
| `service/grep.rs` | grep-parity regex matcher (#111). |
| `service/call_chain.rs` | Annotated call-tree renderer (#76). |
| `service/persistence.rs`, `persistence_loader.rs` | Registry/index (de)serialization + restore. |
| `service/colocated_storage.rs` | `.trusty-search/` per-project layout helpers + `.gitignore` management (#403). |
| `service/fs_discovery.rs` | `scan_roots_for_colocated_indexes` — recursive discovery of `.trusty-search/` dirs from tracked roots (#403/#404). |
| `service/roots_registry.rs` | `roots.toml` — lightweight TOML list of tracked project roots for colocated-storage scanning. |
| `service/watcher.rs`, `watch_loop.rs` | File watching (notify, 500 ms debounce). |
| `service/metrics.rs` | Prometheus `/metrics` (#41). |
| `service/candle_embedder.rs` | Candle/Metal embedder (`candle` feature). |
| `service/concurrency.rs`, `config.rs`, `constants.rs`, `client.rs`, `indexed_files.rs`, `mcp_descriptor.rs`, `ui.rs` | Concurrency limits, user config, defaults, daemon HTTP client, indexed-file set, `SearchMcpService`, embedded UI serving. |
| **`src/mcp/`** | MCP server: `mod.rs`, `tools.rs` (19 tools), `stdio.rs`, `sse.rs`, `openrpc.rs`. |
| **`src/commands/`** | One handler per CLI subcommand + shared helpers. Key new additions: `port.rs` (daemon port reporting), `prune_orphans.rs` (offline orphan cleanup), `upgrade.rs` (self-update + restart), `reindex_ui.rs` (4-phase MultiProgress UI, #401), `migrate_storage/` (colocated-storage migration), `discover/` (submodule split: `mod.rs`, `http.rs`, `marker.rs`, #473). |

Supporting trees: `ui/` (Svelte 5 sources), `ui-dist/` (compiled bundle embedded
via `include_dir!`), `build.rs` (UI build wrapper, `SKIP_UI_BUILD=1` to skip),
`tests/` (integration + benchmark harnesses).

---

## 10. Multi-Index Topology (current vs. remaining roadmap)

**Current (✅).** The nested-index fan-out MVP landed in v0.20.0 (#404 / PR #437).
`IndexHierarchy` (`src/core/search/hierarchy.rs`) derives parent/child
relationships from canonical `root_path` prefix containment — no explicit
`parent_id` field required. Fan-out via `POST /search` now:
- **Boosts sub-index hits** (×1.5 lane-weight, clamped [1.0, 4.0]) so higher-
  fidelity subtree results rank above the parent.
- **Includes threshold safety-net children** even when the threshold router would
  otherwise exclude them.
- **Deduplicates overlapping chunks** post-RRF using (file, start, end) identity.

`GET /indexes?format=tree` returns the registry with hierarchy annotations.
Co-located `.trusty-search/` storage (#403) is also implemented: per-project
data lives under `<root>/.trusty-search/` (managed by `service/colocated_storage.rs`),
with tracked roots in `roots.toml` and recursive discovery via `service/fs_discovery.rs`.
`trusty-search migrate storage` provides the opt-in path from the legacy central
data dir.

**Remaining gaps (🟡/🔵).** Full dedup across all possible overlap patterns is a
best-effort heuristic (exact (file, start, end) match) rather than a proper
merge of partially-overlapping chunks. Relative chunk paths (#402) and cross-file
IMPORTS/INHERITS KG edges remain designed-not-built. Worktree-aware indexing
(shared base + per-worktree delta overlay, #447) is still open.
