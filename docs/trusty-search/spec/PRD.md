# trusty-search — Product Requirements Document

> **Status:** Canonical · Living Document
> **Last reviewed:** 2026-06-01
> **Derived from:** code/docs/tickets audit (v0.22.2)

**Status legend:** ✅ Implemented · 🟡 Partial · 🔵 Designed-not-built · ⚪ Aspirational
Each requirement is framed **Vision / Current / Gap**.

---

## 1. Vision & Mission

### North-star vision

> **trusty-search is a machine-wide hybrid code-search service** — one install,
> one always-on daemon, unlimited named indexes — that fuses **lexical (BM25),
> semantic (HNSW vector), and structural (knowledge-graph) search** behind a
> single, parameter-free ranker, and serves that pipeline identically over
> **MCP, HTTP, and CLI**. It is **local, embedded, and service-free**: no cloud,
> no database server, no Python or Node runtime in the search path.

Where a developer today juggles `grep`/`ripgrep` for literals, an ad-hoc vector
tool for "what does this concept look like," and nothing at all for "who calls
this function," trusty-search collapses all three into one query whose intent it
classifies and routes automatically. The defining properties are: **hybrid
ranking that needs no per-query tuning** (RRF, k = 60), **zero cold-start**
(the HNSW graph is pinned hot and embeddings are LRU-cached), and a
**single-binary install that bundles its own embedder sidecar** so
`cargo install trusty-search` is genuinely all an operator runs.

### Mission

Deliver a single machine-wide daemon that any tool — a human at a CLI, an LLM
agent over MCP, or a host process like open-mpm over HTTP — can ask code
questions of, in lexical / semantic / structural terms, and get back ranked,
token-efficient code chunks in single-digit milliseconds, without standing up
any external service.

### Why this is novel

The closest prior art is `mcp-vector-search` (a Python vector-only MCP tool;
see [research/trusty-search-vs-mcp-vector-search](../research/trusty-search-vs-mcp-vector-search-2026-05-12.md)).
trusty-search's differentiator is the **three-lane hybrid** (lexical + vector +
KG) fused with RRF, the **intent classifier** that reweights lanes per query,
the **machine-wide multi-index daemon** model (one process, many projects), and
the **Rust single-binary + bundled sidecar** deployment that removes the runtime
and service dependencies vector-only tools carry. A `lexical_only` index further
makes trusty-search a *daemonized ripgrep* — BM25 + grep-parity matching with no
ONNX, no GPU, no model download (issue [#111](https://github.com/bobmatnyc/trusty-tools/issues/111)).

---

## 2. Goals & Non-Goals

### Goals

| # | Goal | Status |
|---|---|---|
| G1 | **Machine-wide multi-index daemon** — single install, one process, unlimited named indexes via `DashMap<IndexId, Arc<IndexHandle>>`. | ✅ |
| G2 | **Hybrid three-lane search** — BM25 + HNSW vector + KG expansion fused via RRF (k = 60, parameter-free). | ✅ |
| G3 | **Query-intent routing** — sub-ms regex classifier into 5 intents, per-intent α/β lane weights and KG gating. | ✅ |
| G4 | **MCP server** — JSON-RPC 2.0 over stdio + HTTP/SSE, drop-in for Claude Code. | ✅ |
| G5 | **REST HTTP API** — loopback-only axum daemon for integrators (e.g. open-mpm). | ✅ |
| G6 | **Single-binary install with bundled embedder** — `cargo install trusty-search` installs both `trusty-search` and the `trusty-embedderd` sidecar. | ✅ |
| G7 | **Zero cold-start queries** — HNSW pinned hot (`Duration::MAX` cool-after) + LRU embedding cache. | ✅ |
| G8 | **Auto-tuned memory footprint** — caps computed from detected RAM at startup; every cap env-overridable. | ✅ |
| G9 | **Incremental, crash-safe persistence** — redb corpus with per-batch atomic commits + sha2 content-fingerprint skip across restarts. | ✅ |
| G10 | **Embedded admin UI** — Svelte 5 UI compiled into the binary via `include_dir!`. | ✅ |
| G11 | **Lexical-only / skip-KG modes** — `daemonized ripgrep` (BM25, no embedder) and KG-suppressed indexes for resource-constrained or generated-code subtrees. | ✅ |
| G12 | **Cross-index fan-out search** — one query across all registered indexes (`POST /search`, `POST /grep`). | 🟡 |
| G13 | **Nested-index hierarchy & sub-index prioritization** — parent/child index graph with dedup and subtree-first fan-out ranking. | 🟡 (MVP ✅; full dedup of partial overlap 🔵) |
| G14 | **Co-located per-project storage + filesystem discovery** — index data in `.trusty-search/` next to the project, auto-discovered on walk. | ✅ (#403, v0.20.0) |
| G15 | **Cross-release performance regression CI gate** — MRR@5 / Recall@10 tracked across versions. | 🟡 |

### Non-Goals

| Non-Goal | Rationale |
|---|---|
| Cloud-hosted / multi-tenant search service | trusty-search is a **local, loopback-only** daemon; the ELv2 license explicitly forbids offering it as a hosted service. |
| Network-exposed daemon | The daemon binds `127.0.0.1` only and trusts every caller — **do not** bind a non-loopback interface; there is no auth layer by design. |
| Python or Node runtime in the search path | Rust single binary; Node is used only to *build* the embedded Svelte UI. |
| Code-quality metrics (complexity / smells / grades) | Moved to **trusty-analyze** (issue #71). `CodeChunk` no longer carries complexity/blame; those HTTP endpoints are not served here as of v0.2.0. |
| Windows daemon support | macOS 12+ / Linux only today; Windows paths in `trusty-common` are a future item. |
| Full LSP/compiler-grade symbol resolution | tree-sitter extraction is the baseline; SCIP ingest is a designed-not-built upgrade path (#105). |

---

## 3. Target Users / Personas

| Persona | Who | Primary need | Surface |
|---|---|---|---|
| **LLM coding agent** | Claude Code (and other MCP clients) | Token-efficient hybrid code search + call-chain navigation as a tool | **MCP** (stdio / HTTP-SSE) |
| **Host orchestrator** | open-mpm and other Rust processes embedding the daemon | A liveness-probable REST API + the `SearchMcpService` rlib surface | **HTTP API** / `rlib` |
| **Terminal developer** | Engineers who want a faster, semantic `grep` | Index a repo, run a query, see ranked chunks; manage the daemon | **CLI** |
| **Daemon operator** | Whoever runs the box | One install, predictable RAM, diagnostics, GPU/CPU control | **CLI** (`status` / `doctor` / `start`) + **admin UI** |

**Unifying need across all four:** ask a code question in whatever terms fit
(literal, conceptual, or "who calls/who is called by") and get ranked, accurate,
token-cheap results back fast, from a service that is already running.

---

## 4. Functional Requirements

Grouped by capability area. Each requirement carries Vision / Current / Gap and
an inline status tag. Source paths are cited where known.

### 4.1 Indexing pipeline (`src/core/chunker/`, `src/core/indexer/`, `src/service/reindex.rs`, `src/service/walker.rs`)

**FR-IDX-1 — AST-aware chunking** ✅
- *Vision:* One chunk per top-level declaration (not sliding windows), so
  `function_name`, `chunk_type`, and `calls` are accurate enough to drive both
  semantic search and KG CALLS edges.
- *Current:* `chunk_ast()` parses with tree-sitter (15 grammars: rust, python,
  js, ts, go, java, c, cpp, ruby, php, scala, c-sharp, kotlin, swift) and walks
  top-level declarations into chunks, splitting oversized ones into stable
  parent-ID sub-chunks (`src/core/chunker/mod.rs`). Unknown extensions and
  structured docs (md/yaml/toml/json) fall back to format-aware text chunkers.
- *Gap:* Build-file grammars (`.gradle`, `.groovy`) use the sliding-window
  fallback.

**FR-IDX-2 — Source-tree walk honouring `.gitignore`** ✅
- *Vision:* Enumerate indexable files while skipping VCS/build/dependency dirs
  and binary noise; never silently partial-index because of ignore rules.
- *Current:* `walk_source_files()` uses the `ignore` crate (ripgrep's engine) to
  honour `.gitignore`/`.ignore`/global gitignore, filters by `SOURCE_EXTS`, and
  applies minification/size guards at read time (`src/service/walker.rs`, #100).
  Walk diagnostics (`last_walk_files_seen/skipped/error`) are recorded per
  reindex to explain zero-chunk outcomes (#280).
- *Gap:* None material.

**FR-IDX-3 — Incremental, crash-safe reindex** ✅
- *Vision:* Re-running an index skips unchanged files; a crash mid-reindex never
  corrupts the corpus; large repos reindex in minutes, not tens of minutes.
- *Current:* sha2 content fingerprints skip unchanged files across restarts;
  `--force` clears the per-index hash cache. The redb `CorpusStore` commits each
  batch atomically (O(batch), not O(corpus)) (`src/core/corpus.rs`, #28). Parse +
  embed run outside the write lock; only the redb+HNSW commit holds it. SSE
  progress with a 500-event replay buffer; new `embedder_init` / `embedder_ready`
  SSE events make the ONNX cold-start visible (#572). The CLI renders a 4-phase
  MultiProgress bar (Crawl / Chunk / Loading model / Embed) and uses a
  stall-detection loop (120 s without progress) rather than a hard wall-clock
  timeout, fixing spurious "timed out" failures on large repos (#572).
- *Gap:* None material.

**FR-IDX-4 — File watching** ✅
- *Vision:* Edits on disk update the index without a manual reindex.
- *Current:* `notify` + `notify-debouncer-mini` (500 ms debounce) drive a watch
  loop per index (`src/service/watcher.rs`, `watch_loop.rs`).
- *Gap:* None material.

### 4.2 Lexical search — BM25 (`src/core/bm25.rs` → `trusty_common::bm25`, `src/service/grep.rs`)

**FR-LEX-1 — BM25 ranked lexical lane** ✅
- *Vision:* A zero-dependency BM25 scorer with code-aware tokenization
  (camelCase / snake_case splitting) shared across trusty tooling.
- *Current:* BM25 moved into `trusty-common` (#156) and is re-exported here as
  `Bm25Index`; the per-query index is built from the chunk corpus and capped by
  `TRUSTY_BM25_CORPUS_CAP`.
- *Gap:* Deferred memory optimizations (streaming sort-merge, etc.) tracked in
  [#340](https://github.com/bobmatnyc/trusty-tools/issues/340). 🟡

**FR-LEX-2 — grep-parity regex search (`/grep`)** ✅
- *Vision:* Exact, deterministic, line-accurate matching with ripgrep ergonomics
  (regex, `-i`, `-A`/`-B`/`-C`, `--include` globs, multiline) — without
  re-embedding.
- *Current:* `CompiledGrep` + `grep_file_content` is a pure matcher driven by the
  HTTP handler over the files the index already knows (`src/service/grep.rs`).
  Certified at grep parity: P50 8 ms vs ripgrep 9 ms on a 1,155-file workspace
  (#111, [v0.14.0 stage-1 cert](../regression-testing/v0.14.0-stage1-cert-2026-05-27.md)).
- *Gap:* None material.

**FR-LEX-3 — Lexical-only mode (daemonized ripgrep)** ✅
- *Vision:* An index that skips embedding entirely — BM25 + grep speed via a
  persistent HTTP/MCP daemon, no ONNX, no GPU, no model download.
- *Current:* `lexical_only: true` on index create. ~63× faster reindex; daemon
  fits in ~700 MB. Stage-1-minimal also skips symbol-graph construction
  ([#312](https://github.com/bobmatnyc/trusty-tools/issues/312)).
- *Gap:* None material.

### 4.3 Semantic search — HNSW vector (`src/core/store.rs`, `src/core/embed.rs`, `src/service/embedder_supervisor.rs`)

**FR-VEC-1 — HNSW vector lane** ✅
- *Vision:* Fast approximate-nearest-neighbour semantic recall over code-chunk
  embeddings, hot from daemon start.
- *Current:* usearch 2.25 HNSW wrapped in `Arc<RwLock<>>` for concurrent reads;
  pinned hot (`Duration::MAX` cool-after); INT8-quantized all-MiniLM-L6-v2
  (384-dim) via fastembed/ONNX. 4× top_k candidates feed RRF
  (`src/core/store.rs`, `src/core/indexer/search.rs`).
- *Gap:* None material.

**FR-VEC-2 — Embedding cache (zero re-embed on repeat)** ✅
- *Vision:* Repeated queries never re-pay the embedder cost.
- *Current:* LRU cache (`TRUSTY_EMBEDDING_CACHE`, 500–20 000 by tier) on query
  embeddings; cache miss falls back gracefully.
- *Gap:* None material.

**FR-VEC-3 — `search_semantic` / `search_similar` lanes** ✅
- *Vision:* A vector-only lane and code-to-code similarity from a seed
  file/function.
- *Current:* `search_semantic` MCP tool + `POST /indexes/:id/search_similar`
  (seed = named function or first chunk of a file).
- *Gap:* None material.

### 4.4 Knowledge-graph search (`src/core/symbol_graph.rs`, `src/service/call_chain.rs`)

**FR-KG-1 — Symbol graph & KG expansion** ✅
- *Vision:* Answer "who calls X / what does X call" and expand search around a
  hit with adjacent code at a discounted score.
- *Current:* petgraph `DiGraph<SymbolNode,()>` keyed by (qualified) symbol name,
  rebuilt cheaply from the corpus and held in `Arc<SymbolGraph>`; 1–2-hop
  `callers_of`/`callees_of` expansion scored at 70% of the trigger chunk's RRF
  score, **gated to Usage intent only**. `EdgeKind` (CALLS/IMPORTS/INHERITS/
  CONTAINS) multipliers. Node count capped by `TRUSTY_MAX_KG_NODES`.
- *Gap:* KG Phase B (cross-file IMPORTS/INHERITS propagation) not built.

**FR-KG-2 — `get_call_chain` annotated call tree** ✅
- *Vision:* A depth-1 caller/callee tree with `Why:`/`What:` doc annotations to
  improve multi-function edit quality.
- *Current:* `get_call_chain` MCP tool + `GET /indexes/:id/call_chain` render a
  plain-text tree resolved by exact/fuzzy/`file:line` symbol lookup
  (`src/service/call_chain.rs`, #76).
- *Gap:* None material.

**FR-KG-3 — `search_kg` with `refine_query`** ✅
- *Vision:* KG-first graph-walk search that does not compound a weak seed's error.
- *Current:* `search_kg` accepts optional `refine_query`; the daemon embeds it
  and discards KG neighbours below cosine 0.4, re-ranking survivors (#147).
- *Gap:* None material.

**FR-KG-4 — skip-KG mode** ✅
- *Vision:* Run BM25 + vector but skip the Phase-3 KG rebuild for
  documentation-heavy / generated-code subtrees.
- *Current:* `skip_kg: true` (CLI `--no-kg`, YAML, HTTP, or `TRUSTY_NO_KG=1`).
  Saves ~50–100 MB heap + ~400 ms/reindex. `call_chain` returns a structured 503
  (`kg_unavailable`) when skipped (#313).
- *Gap:* None material.

**FR-KG-5 — SCIP ingest for LSP-quality entities** 🔵
- *Vision:* Consume CI-produced SCIP indexes for cross-file symbol fidelity
  beyond tree-sitter.
- *Current:* `CodeEntityIndex` trait + `ScipIndex::from_refs` testable path
  exist; native protobuf decode is a TODO (`src/core/scip_ingest.rs`, #105).
- *Gap:* `ScipIndex::from_scip` (protobuf parse) not wired.

### 4.5 Hybrid ranking (`src/core/search/rrf.rs`, `src/core/classifier.rs`, `src/core/mmr.rs`)

**FR-RANK-1 — RRF fusion** ✅
- *Vision:* Combine heterogeneous ranked lists without per-query tuning.
- *Current:* `rrf_fuse()` sums `weight · 1/(k+rank)` per lane, k = 60 (Cormack et
  al.), rank-only (scores ignored) (`src/core/search/rrf.rs`).
- *Gap:* None material.

**FR-RANK-2 — Intent classification & lane routing** ✅
- *Vision:* Reweight lexical/vector lanes and gate KG per query, sub-ms.
- *Current:* `QueryClassifier` regex → `Definition | Usage | Conceptual |
  BugDebt | Unknown`, each mapping to `(α, β, use_kg_first)`
  (`src/core/classifier.rs`). Usage gates KG expansion on.
- *Gap:* None material.

**FR-RANK-3 — MMR diversity re-ranking** ✅
- *Vision:* Avoid near-duplicate top results.
- *Current:* `mmr_rerank` with cosine similarity (`src/core/mmr.rs`).
- *Gap:* None material.

**FR-RANK-4 — Branch-aware boosting** ✅
- *Vision:* Surface work on the current git branch first.
- *Current:* `branch_files`/`branch` on `POST .../search` boost matching chunks
  (default 1.5×, clamped [1.0, 3.0]); each result carries `on_branch` (#122).
  When only `branch` is given the daemon derives the file list via
  `git merge-base` + `git diff --name-only` (non-fatal on failure).
- *Gap:* None material.

**FR-RANK-5 — Documentation down-ranking** ✅
- *Vision:* Code queries should not be dominated by `.md`/changelog chunks.
- *Current:* `src/core/indexer/docs_penalty.rs` down-ranks doc/changelog chunks
  in code-intent queries (#72, #77).
- *Gap:* None material.

### 4.6 MCP server (`src/mcp/`)

**FR-MCP-1 — JSON-RPC 2.0 server over stdio + HTTP/SSE** ✅
- *Vision:* A drop-in MCP server Claude Code can wire with two lines of config.
- *Current:* `McpServer` dispatcher proxies tool calls to the daemon over HTTP;
  `stdio` line-delimited loop and `sse` axum router (`POST /mcp`, `GET /mcp/sse`)
  (`src/mcp/mod.rs`). Stdout reserved for JSON-RPC; logs to stderr.
- *Gap:* None material.

**FR-MCP-2 — Tool catalogue** ✅
- *Vision:* Expose every search lane + index management as MCP tools.
- *Current:* 19 tools — `search_code`, `search_kg`, `search_semantic`,
  `search_lexical`, `search_all`, `search_similar`, `grep`, `get_call_chain`,
  `index_file`, `remove_file`, `list_indexes`, `create_index`, `delete_index`,
  `reindex`, `index_status`, `list_chunks`, `search_health`, `chat`, `upgrade`
  (`src/mcp/tools.rs`). `grep` now accepts `max_count` as a ripgrep-parity
  alias for `max_results` (#447). `search_similar` re-embeds seed content on
  LRU miss for `skip_kg` indexes (#484).
- *Gap:* The crate README lists an older subset; the code is authoritative.

### 4.7 HTTP API (`src/service/server.rs`)

**FR-HTTP-1 — Loopback REST API** ✅
- *Vision:* A liveness-probable REST surface for integrators; localhost-only, no
  auth.
- *Current:* axum 0.8 + tower-http (CORS, trace, gzip), HTTP/2; per-index search,
  search_similar, index-file, remove-file, reindex (+ SSE stream), chunks,
  status, call_chain, graph (+ stats), grep; plus global `/health`, `/indexes`
  (with `?format=tree` hierarchy view and `?details=true` size reporting, #404/#312),
  `/search`, `/grep`, `/metrics`, `/status/stream`, `/logs/tail`, `/admin/stop`,
  `/api/chat/providers`, `/chat`, `/upgrade` (#537), `/ui/*` (`src/service/server.rs`).
  `/health` response now includes `update_available`, `embedder_error`, and
  `embedder_ready` fields.
- *Gap:* Facts store endpoints (`/facts`) are optional/may return 503 when
  unconfigured.

**FR-HTTP-2 — Cross-index fan-out** ✅
- *Vision:* One query across all indexes, weighted/skipped by per-project
  relevance.
- *Current:* `POST /search` and `POST /grep` fan out across the registry;
  `context_inference` scrapes project metadata (README/CLAUDE.md/manifests) to
  build a per-index relevance summary (`src/service/context_inference.rs`, #112).
  When a nested hierarchy is present (root_path prefix containment), sub-index
  hits are boosted (×1.5 lane weight) and exact-duplicate (file, start, end)
  results from parent+child overlap are deduplicated post-RRF (#404 MVP).
- *Gap:* Partial-overlap dedup is heuristic only. Worktree-aware delta overlay
  (#447) not yet implemented.

**FR-HTTP-3 — Prometheus metrics** ✅
- *Vision:* Production observability for multi-user deployments.
- *Current:* `/metrics` (request counters, latency histograms, queue/pool gauges)
  when the recorder is wired (`src/service/metrics.rs`, #41 Phase 1).
- *Gap:* None material.

### 4.8 CLI (`src/main.rs`, `src/commands/`)

**FR-CLI-1 — Index + query + manage from the terminal** ✅
- *Vision:* Zero-to-search in five commands; daemon lifecycle and diagnostics.
- *Current:* `start`/`stop`/`index`/`query`/`status`/`doctor`/`ui`/`convert`/
  `serve` + aliases (`init`, `reindex`). `index` auto-detects
  `./trusty-search.yaml`. `doctor --fix` runs a 6-check diagnostic with
  auto-repair. `convert` migrates from `mcp-vector-search` (`src/commands/`).
  New since v0.18: `port [--addr|--json]` (#526), `prune-orphans [--dry-run]`
  (#489), `upgrade [--check|--yes]` (#537), `migrate storage` (#403/#491),
  `service install|uninstall|status|logs` (macOS launchd). Auto-discovery
  now recognises `.trusty-tools/` project marker (#470).
- *Gap:* None material.

### 4.9 Daemon lifecycle (`src/service/daemon.rs`, `src/commands/start.rs`)

**FR-DMN-1 — Singleton, auto-port, graceful shutdown** ✅
- *Vision:* One daemon per machine, discoverable port, clean SIGTERM/SIGINT exit.
- *Current:* OS advisory lock on a PID lockfile enforces singleton; binds from a
  requested port walking forward to a free one and writes `port.lock`; graceful
  shutdown via axum `with_graceful_shutdown(shutdown_signal())` drains in-flight
  requests on SIGTERM (#534). The `mcp_bridge` reconnects with exponential backoff
  so daemon upgrades are transparent to running LLM sessions. `port` CLI subcommand
  exposes the live port in three machine-parsable formats (#526).
- *Gap:* None material.

**FR-DMN-2 — Warm-boot persistence** ✅
- *Vision:* Registered indexes (with HNSW graph + chunk corpus) survive restarts;
  no full re-index on every boot.
- *Current:* Registry TOML + per-index data dir (`hnsw.usearch` + redb corpus);
  `restore_indexes` on startup; forward-only idempotent schema migrations
  (`_meta.schema_version`) run in background (`src/service/persistence.rs`,
  `src/core/migration/`, #85).
- *Gap:* None material.

**FR-DMN-3 — Hard RAM check** ✅
- *Vision:* Fail fast on under-spec hosts rather than OOM mid-reindex.
- *Current:* 16 GB minimum hard-checked at `start` with an actionable error;
  `TRUSTY_SKIP_RAM_CHECK=1` bypass (`src/commands/start.rs`, #291).
- *Gap:* None material.

**FR-DMN-4 — GPU/CPU device control** ✅
- *Vision:* CoreML auto on Apple Silicon; CUDA opt-in; force-CPU/GPU at runtime.
- *Current:* CoreML EP auto-registered on aarch64-macOS (no feature flag since
  v0.3.13); CUDA via `--features cuda` (+ `--no-default-features` and
  `ORT_DYLIB_PATH` on glibc < 2.38); `--device cpu|gpu|auto` persisted to
  `daemon.env`.
- *Gap:* None material.

### 4.10 Embedder sidecar (`crates/trusty-embedderd`, `src/service/embedder_supervisor.rs`)

**FR-EMB-1 — Bundled single-install sidecar** ✅
- *Vision:* `cargo install trusty-search` installs both binaries; the operator
  manages one daemon.
- *Current:* `trusty-embedderd` is a workspace crate declared both as a Cargo dep
  and a second `[[bin]]` in trusty-search's manifest; the shim calls
  `trusty_embedderd::run()`. The supervisor spawns/supervises it
  (`src/service/embedder_supervisor.rs`, #110 Phase 2 / #187).
- *Gap:* None material.

**FR-EMB-2 — Lazy spawn + idle shutdown** ✅
- *Vision:* Don't pay the ONNX init cost until the first embed; reclaim it when
  idle (useful for lexical-only deployments).
- *Current:* `LazyEmbedderHandle` defers the spawn until the first embed request;
  binary discovery still runs at boot and fails fast with an install hint.
  `TRUSTY_EMBEDDERD_IDLE_SHUTDOWN_SECS` kills + resets the spawn gate (#315).
- *Gap:* None material.

**FR-EMB-3 — Pluggable embedder transports** ✅
- *Vision:* The sidecar is the default, but in-process / HTTP / UDS / Candle
  paths exist for tests and special hosts.
- *Current:* `TRUSTY_EMBEDDER` selects `auto/stdio` (default sidecar),
  `in-process`, `http://…`, `unix:/…`, or `candle` (with `--features candle`).
- *Gap:* None material.

**FR-EMB-4 — Priority embed pool** ✅
- *Vision:* Interactive search embeddings must not starve behind a long reindex.
- *Current:* `embed_pool` drains interactive before background via biased
  `select!`; worker count autotuned from RAM (`src/service/embed_pool.rs`, #41).
- *Gap:* None material.

### 4.11 Memory & auto-tuning (`src/core/memory_policy.rs`, `src/core/memguard.rs`)

**FR-MEM-1 — Tiered auto-tuned caps** ✅
- *Vision:* Defaults that fit the host: safe on a laptop, generous on a
  workstation, all overridable.
- *Current:* `MemoryPolicy::detect()` reads RAM (`hw.memsize` / `/proc/meminfo`),
  selects a tier (Tiny→XLarge), sets `MAX_CHUNKS`, `EMBEDDING_CACHE`,
  `MAX_BATCH_SIZE`, `BM25_CORPUS_CAP`, `MAX_KG_NODES`, and `MEMORY_LIMIT_MB`
  (= clamp(RAM×25%, 1–64 GB)); env vars override (precedence: shell >
  `daemon.env` > tier). Resolved tier logged at startup.
- *Gap:* README and CLAUDE.md disagree on whether Tiny/Small tiers are removed
  vs. retained — the code retains five tiers but `start` hard-checks 16 GB. 🟡

**FR-MEM-2 — Soft RSS ceiling during reindex** ✅
- *Vision:* Never OOM; degrade to a usable partial index instead.
- *Current:* `memguard` polls RSS; on `TRUSTY_MEMORY_LIMIT_MB` breach the reindex
  skips remaining batches (already-committed chunks stay searchable) and emits
  `memory_limit_hit: true` (`src/core/memguard.rs`, #75/#95).
- *Gap:* Between-batch poller cannot catch intra-call ORT spikes; mitigated by
  the batch-size formula and CoreML tripwire.

**FR-MEM-3 — CoreML batch tripwire** ✅
- *Vision:* Bound Apple-Silicon unified-memory spikes per embed batch.
- *Current:* `TRUSTY_COREML_TRIPWIRE_MB` (4 GB default) halves batch size on a
  > 4 GB RSS jump; default CoreML batch 32 (ANE-optimal).
- *Gap:* None material.

**FR-MEM-4 — Idle chunk-map eviction** ✅
- *Vision:* A quiet daemon should shrink to its durable baseline.
- *Current:* `TRUSTY_CHUNKS_IDLE_EVICT_SECS` (300 s) evicts the in-memory
  `RawChunk` map for durably-backed indexes; readers rehydrate from redb;
  BM25 + symbol graph stay hot.
- *Gap:* None material.

### 4.12 UI (`src/service/ui.rs`, `ui/`, `ui-dist/`)

**FR-UI-1 — Embedded Svelte admin UI** ✅
- *Vision:* Search, index management, and chat from a browser, served by the
  daemon itself.
- *Current:* Svelte 5 UI compiled into the binary via `include_dir!`; Collections,
  Search, Chat, Admin panels; opened with `trusty-search ui`; built by `build.rs`
  (`SKIP_UI_BUILD=1` to skip) (`src/service/ui.rs`).
- *Gap:* None material.

**FR-UI-2 — OpenRouter chat with auto-injected search context** ✅
- *Vision:* Ask natural-language questions answered with retrieved code context.
- *Current:* `POST /chat` (+ `chat` MCP tool) forwards to OpenRouter with
  search-context injection; requires `OPENROUTER_API_KEY` (503 otherwise).
- *Gap:* None material.

### 4.13 Self-update & daemon lifecycle

**FR-UPD-1 — In-place self-update with safe daemon restart** ✅
- *Vision:* An operator or LLM agent can update the daemon to the latest version
  with a single command, without manual `cargo install` or launchd plumbing.
- *Current:* `trusty-search upgrade [--check] [--yes]` checks crates.io for a
  newer version and, with confirmation (or `--yes`), installs it via
  `cargo install` and restarts the daemon (SIGTERM + launchd respawn). `POST
  /upgrade` exposes the same workflow over HTTP so the `upgrade` MCP tool can
  trigger it. `GET /health` includes `update_available` when a newer version is
  detected in the background (#537).
- *Gap:* None material.

### 4.14 Co-located storage & filesystem discovery

**FR-STORE-2 — Storage hygiene** ✅
- *Vision:* Dead registry entries and orphaned data directories don't accumulate
  silently.
- *Current:* `trusty-search prune-orphans [--dry-run] [--yes]` loads `indexes.toml`,
  identifies entries whose `root_path` doesn't exist on disk, and removes them
  after confirmation. Works offline (no daemon required) (#489, v0.20.3).
- *Gap:* Orphaned `.trusty-search/` data directories inside moved/deleted project
  trees are not automatically detected; operators must find and remove them manually.

### 4.16 Multi-index topology & storage (remaining roadmap)

**FR-NEST-1 — Nested-index hierarchy & sub-index prioritization** 🟡
- *Vision:* A DAG of indexes where sub-indexes are children of a parent; fan-out
  returns subtree results first, dedups overlapping coverage, and uses the parent
  as a backstop.
- *Current:* MVP implemented in v0.20.0 (#404 / PR #437). `IndexHierarchy`
  (`src/core/search/hierarchy.rs`) derives parent/child relationships from
  canonical `root_path` prefix containment. Fan-out via `POST /search` boosts
  sub-index hits (×1.5, clamped [1.0, 4.0]), includes threshold-safety-net
  children, and deduplicates exact (file, start, end) matches post-RRF.
  `GET /indexes?format=tree` returns the hierarchy-annotated registry.
- *Gap:* Partial-overlap dedup is heuristic. Relative chunk paths (#402) and
  explicit `parent_id` fields not implemented. Worktree-aware delta overlay (#447)
  still open.

**FR-STORE-1 — Co-located `.trusty-search/` storage + filesystem discovery** ✅
- *Vision:* Index data lives in a `.trusty-search/` dir next to the project;
  the walker discovers existing indexes on the filesystem.
- *Current:* Implemented in v0.20.0 (#403 / PR #440). Per-project data lives at
  `<root>/.trusty-search/`; tracked roots stored in `roots.toml`; `fs_discovery`
  scans recursively on startup. `migrate storage` provides the offline opt-in
  migration from the legacy central dir (#491). `prune-orphans` cleans up dead
  root entries (#489).
- *Gap:* Relative chunk paths (#402) not yet addressed — chunk ids embed absolute
  file paths, which break if the project root moves (workaround: re-index).

---

## 5. Success Criteria & Differentiators

| Criterion | Target | Status |
|---|---|---|
| Warm p50 query latency | < 10 ms on a 100k-chunk index | ✅ (8 ms `/grep` P50 certified on 1,155 files) |
| `/grep` vs ripgrep | parity | ✅ (8 ms vs 9 ms) |
| Full hybrid reindex | ~2–3 min for a 14k-file repo | ✅ |
| Lexical-only reindex | ~63× faster than hybrid; daemon ≤ ~700 MB | ✅ |
| Cold start | zero (HNSW hot + LRU embed cache) | ✅ |
| Install footprint | one command installs both binaries | ✅ |
| Retrieval quality | MRR@5 / Recall@10 tracked across releases | 🟡 (benchmarks exist; CI gate pending) |

**Differentiators vs. `mcp-vector-search` and bare ripgrep:** three-lane hybrid
(lexical + vector + KG) with parameter-free RRF and per-query intent routing;
machine-wide multi-index daemon (one process, many projects); Rust single-binary
+ bundled sidecar (no Python/Node runtime); zero cold-start; and a `lexical_only`
mode that is a daemonized ripgrep with MCP/HTTP integration.

---

## 6. Open Questions & Roadmap

| # | Question / item | Status | Tracking |
|---|---|---|---|
| Q1 | Fan-out dedup: exact (file, start, end) match implemented; partial-overlap merge pending | 🟡 | #404 |
| Q2 | Relative chunk paths so project moves don't break chunk ids | 🔵 | #402 |
| Q3 | Worktree-aware indexing: shared base + per-worktree delta overlay | 🔵 | #447 |
| Q4 | Wire a benchmark regression CI gate (MRR@5 / Recall@10) | 🟡 | #129 |
| Q5 | SCIP protobuf decode for LSP-quality entities | 🔵 | #105 |
| Q6 | KG Phase B: cross-file IMPORTS/INHERITS edge propagation | 🔵 | — |
| Q7 | Further BM25 / redb in-memory footprint reductions | 🟡 | #340 |
| Q8 | Reconcile the README/CLAUDE.md memory-tier description (Tiny/Small retained vs removed) | 🟡 | docs |
| Q9 | Windows daemon path support (via `trusty-common`) | ⚪ | — |
| Q10 | Split `reindex_engine.rs` (1 438 lines, over 500-line cap) | 🟡 | #571 |

For **cross-release performance tracking**, see
[#129](https://github.com/bobmatnyc/trusty-tools/issues/129), which accumulates
benchmark deltas across all measured versions.
