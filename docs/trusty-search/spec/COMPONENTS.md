# trusty-search — Component Specifications

> **Status:** Canonical · Living Document
> **Last reviewed:** 2026-05-29
> **Derived from:** code/docs/tickets audit

**Status legend:** ✅ Implemented · 🟡 Partial · 🔵 Designed-not-built · ⚪ Aspirational

One section per major subsystem. Each states **Responsibility**, **Key
types/modules** (with `src/` paths), **Current state**, and **Known gaps**,
framed Vision / Current / Gap. For the cross-cutting query pipeline see
[ARCHITECTURE.md §5](./ARCHITECTURE.md); for product framing see [PRD.md](./PRD.md).
All paths are under `crates/trusty-search/`.

---

## 1. Indexer / Pipeline — `src/core/indexer/`, `src/core/chunker/`, `src/service/reindex.rs`, `src/service/walker.rs`

**Responsibility.** Turn a project directory into a searchable index: walk files,
chunk them AST-aware, embed, commit durably, and build the symbol graph — staged
so any stage can be skipped, incrementally and crash-safely.

**Key types/modules.**
- `CodeIndexer` (`src/core/indexer/mod.rs`) — the orchestrator holding an
  `Embedder`, a `VectorStore`, and the in-memory chunk corpus; `search()` runs
  both lanes and fuses.
- `ingest.rs` — `parse_and_embed_files()` (outside the write lock) +
  `commit_parsed_batch()` (write lock only for the redb+HNSW commit); `ParsedBatch`,
  `CommitTimings`.
- `persist.rs` — snapshot/restore + background incremental persist.
- `files.rs` — remove/lookup/entity-exact-match.
- `search.rs` — the hybrid query pipeline (HNSW + BM25 + RRF + KG + MMR).
- `docs_penalty.rs` — down-rank docs/changelog on code intent (#72/#77).
- `chunk_ast()` / `chunk_text()` (`src/core/chunker/`) — tree-sitter AST chunker
  (15 grammars) with oversized-chunk splitting + format-aware doc fallback.
- `walk_source_files()` (`src/service/walker.rs`) — gitignore-aware walk.
- `spawn_reindex` + `ReindexProgress` (`src/service/reindex.rs`) — SSE progress.

**Current state.** ✅ Full staged pipeline with split-lock concurrency, sha2
incremental skip, atomic redb per-batch commits, gitignore-honouring walk (#100),
walk diagnostics (#280), and memory-bounded reindex with a soft RSS ceiling.

**Known gaps.**
- 🟡 Build-file grammars (`.gradle`, `.groovy`) fall back to sliding-window.
- 🔵 SCIP-quality cross-file entities (FR-KG-5 / #105).

---

## 2. Lexical Search — BM25 + grep — `src/core/bm25.rs`, `src/service/grep.rs`

**Responsibility.** Provide both ranked lexical retrieval (BM25) and exact,
deterministic regex matching (grep parity) with code-aware tokenization.

**Key types/modules.**
- `Bm25Index` / `tokenize` (`src/core/bm25.rs`, re-exported from
  `trusty_common::bm25`, #156) — zero-dep BM25 with camelCase/snake_case
  splitting; per-query index built from the corpus, capped by
  `TRUSTY_BM25_CORPUS_CAP`.
- `CompiledGrep` + `grep_file_content` (`src/service/grep.rs`) — pure, I/O-free
  matcher (regex, `-i`, `-A`/`-B`/`-C`, `--include` globs, multiline) driven by
  the HTTP handler over already-chunked files; never re-embeds.

**Current state.** ✅ BM25 lane in every hybrid query; `/grep` certified at
ripgrep parity (P50 8 ms vs 9 ms, #111). `lexical_only` mode is a daemonized
ripgrep (~63× faster reindex, ~700 MB daemon).

**Known gaps.**
- 🟡 Deferred BM25 in-memory footprint optimizations (streaming sort-merge, B.1/
  B.3/B.5) — [#340](https://github.com/bobmatnyc/trusty-tools/issues/340).

---

## 3. Vector / Semantic Search — `src/core/store.rs`, `src/core/embed.rs`

**Responsibility.** Approximate-nearest-neighbour semantic recall over code-chunk
embeddings, hot from daemon start, with repeat queries free.

**Key types/modules.**
- `VectorStore` / usearch HNSW wrapper (`src/core/store.rs`) in `Arc<RwLock<>>`
  for concurrent reads; `chunk_id ↔ u64 key` JSON sidecar for restore.
- `Embedder` trait + `FastEmbedder` (`src/core/embed.rs`) — fastembed/ONNX
  all-MiniLM-L6-v2 INT8 (384-dim).
- `QueryCache` — `LruCache<QueryHash, Vec<f32>>` (`TRUSTY_EMBEDDING_CACHE`).

**Current state.** ✅ HNSW pinned hot (`Duration::MAX` cool-after); 4× top_k
candidates feed RRF; LRU embedding cache skips re-embed on repeat;
`search_semantic` (vector-only lane) and `search_similar` (code-to-code from a
seed file/function) both implemented.

**Known gaps.** None material; embedding itself runs in the sidecar (§10).

---

## 4. Knowledge Graph — `src/core/symbol_graph.rs`, `src/service/call_chain.rs`

**Responsibility.** Answer structural questions ("who calls / what does X call")
and expand search around a hit with adjacent code at a discounted score.

**Key types/modules.**
- `SymbolGraph` / `SymbolNode` (`src/core/symbol_graph.rs`) — petgraph
  `DiGraph<SymbolNode,()>` keyed by (qualified) symbol name, rebuilt cheaply from
  the corpus, held in `Arc<SymbolGraph>`; `callers_of` / `callees_of` 1–2-hop;
  `EdgeKind` (CALLS/IMPORTS/INHERITS/CONTAINS) multipliers; capped by
  `TRUSTY_MAX_KG_NODES`.
- `get_call_chain` renderer (`src/service/call_chain.rs`, #76) — plain-text
  depth-1 caller/callee tree with `Why:`/`What:` doc annotations; resolves by
  exact/fuzzy/`file:line`, picking the most-connected candidate.
- `search_kg` `refine_query` filter (#147) — embeds the refinement, discards KG
  neighbours below cosine 0.4, re-ranks survivors.

**Current state.** ✅ KG expansion gated to Usage intent, scored at 0.7× the
trigger chunk's RRF score. `get_call_chain` and `search_kg` work. skip-KG mode
(`--no-kg` / `TRUSTY_NO_KG`) suppresses Stage 3 entirely and returns a structured
503 (`kg_unavailable`) from `call_chain` (#313).

**Known gaps.**
- 🔵 KG Phase B: cross-file IMPORTS/INHERITS edge propagation.
- 🔵 SCIP protobuf decode (`src/core/scip_ingest.rs`, #105).

---

## 5. Ranker — RRF + intent routing + MMR — `src/core/search/rrf.rs`, `src/core/classifier.rs`, `src/core/mmr.rs`

**Responsibility.** Fuse heterogeneous ranked lists without per-query tuning,
reweighted by classified intent, with diversity and branch/docs adjustments.

**Key types/modules.**
- `rrf_fuse()` + `RRF_K = 60` (`src/core/search/rrf.rs`) — rank-only fusion,
  `Σ weight·1/(k+rank)`.
- `QueryClassifier` / `QueryIntent` (`src/core/classifier.rs`) — sub-ms regex →
  `(α, β, use_kg_first)` per intent (see ARCHITECTURE §5).
- `mmr_rerank` / `cosine_similarity` (`src/core/mmr.rs`).
- Branch boost (`branch_files`/`branch` → ×`branch_boost`, default 1.5, #122);
  docs penalty (`src/core/indexer/docs_penalty.rs`).

**Current state.** ✅ All five ingredients (RRF, intent routing, MMR, branch
boost, docs penalty) implemented and wired into `search.rs`. Each result carries
a `match_reason` and `on_branch`.

**Known gaps.** None material.

---

## 6. MCP Server — `src/mcp/`

**Responsibility.** Adapt the daemon's REST API into MCP JSON-RPC 2.0 tool calls
over stdio and HTTP/SSE so an LLM client (Claude Code) can drive code search.

**Key types/modules.**
- `McpServer` (`src/mcp/tools.rs`) — pure dispatcher proxying tools to the daemon
  over HTTP; `Request`/`Response`/`JsonRpcError`/`error_codes`/`tool_descriptors`.
- `stdio` (`src/mcp/stdio.rs`) — line-delimited JSON-RPC loop.
- `sse` (`src/mcp/sse.rs`) — axum `POST /mcp` + `GET /mcp/sse`.
- `openrpc` (`src/mcp/openrpc.rs`) — OpenRPC descriptor.

**Current state.** ✅ 17 tools: `search_code`, `search_kg`, `search_semantic`,
`search_lexical`, `search_similar`, `grep`, `get_call_chain`, `index_file`,
`remove_file`, `list_indexes`, `create_index`, `delete_index`, `reindex`,
`index_status`, `list_chunks`, `search_health`, `chat`. stdout reserved for
JSON-RPC; logs to stderr.

**Known gaps.**
- 🟡 The crate `README.md` advertises an older 11–15-tool subset; `src/mcp/tools.rs`
  is authoritative.

---

## 7. HTTP API — `src/service/server.rs`

**Responsibility.** Serve the search pipeline + index management as a
loopback-only REST API (axum) for integrators and the UI, plus cross-index
fan-out and observability.

**Key types/modules.**
- `SearchAppState` (`Arc`-shared) over the `IndexRegistry`.
- Per-index handlers (search, search_similar, index-file, remove-file, reindex +
  SSE, chunks, status, call_chain, graph, grep).
- Global handlers: `global_search_handler` / `global_grep_handler` (fan-out),
  `/health`, `/indexes`, `/metrics`, `/status/stream`, `/logs/tail`, `/chat`,
  `/api/chat/providers`, `/ui/*`.
- `MetricsState` (`src/service/metrics.rs`), `context_inference`
  (`src/service/context_inference.rs`).

**Current state.** ✅ Full per-index API + global fan-out; HTTP/2; CORS/trace/gzip;
`{ "error": … }` error envelope with standard codes; Prometheus `/metrics` (#41).

**Known gaps.**
- 🟡 Cross-index fan-out has no dedup of overlapping indexes (FR-NEST-1, #404).
- Facts store (`/facts`) optional — 503 when unconfigured.

---

## 8. CLI — `src/main.rs`, `src/commands/`

**Responsibility.** Zero-to-search from the terminal plus daemon lifecycle and
diagnostics; testable handlers split out of `main()`.

**Key types/modules.**
- `src/main.rs` — clap parsing → `commands::*`; central friendly-error/exit (#104).
- Per-subcommand handlers (`src/commands/`): `start`, `stop`, `index`, `query`,
  `status`, `doctor`, `ui`, `convert`, `serve`, plus `init`/`reindex` aliases,
  `discover`, `monitor`, `dashboard`, `setup`, `integrate`, `cleanup`, `migrate`.
- Shared helpers: `daemon_utils`, `format`, `index_resolve`, `reindex_engine`,
  `doctor_checks`, `doctor_pipeline`, `log_rotation`.

**Current state.** ✅ `index` auto-detects `./trusty-search.yaml`; `doctor --fix`
is a 6-check diagnostic with auto-repair; `convert` migrates from
`mcp-vector-search`; `--data-dir` enables isolated daemon instances;
`--no-auto-discover` skips the startup scan.

**Known gaps.** None material.

---

## 9. Daemon Lifecycle — `src/service/daemon.rs`, `src/service/persistence*.rs`, `src/commands/start.rs`

**Responsibility.** One discoverable singleton daemon per machine that warm-boots
its registered indexes and degrades safely under memory pressure.

**Key types/modules.**
- `run_daemon` / `DaemonHandle` / `DaemonError` (`src/service/daemon.rs`) — PID
  lockfile (OS advisory exclusive lock), auto-port + `port.lock`, graceful
  shutdown, `daemon.env` (`save_daemon_env` / `PERSISTED_ENV_VARS`).
- `load_index_registry` / `save_index_registry` / `index_data_dir`
  (`src/service/persistence.rs`) + `persistence_loader.rs` (`restore_indexes`).
- `MigrationRegistry` / `run_migrations` (`src/core/migration/`) — background,
  forward-only, idempotent (#179).
- Hard 16 GB RAM check (`src/commands/start.rs`, #291).

**Current state.** ✅ Singleton enforcement, auto-port discovery, warm-boot
persistence (HNSW + redb survive restarts, #85), background schema migrations,
device-flag persistence.

**Known gaps.**
- 🔵 Co-located `.trusty-search/` storage + filesystem discovery
  ([#403](https://github.com/bobmatnyc/trusty-tools/issues/403)) — today data
  lives under a central daemon data dir.

---

## 10. Embedder Integration — `crates/trusty-embedderd`, `src/service/embedder_supervisor.rs`, `src/service/embed_pool.rs`

**Responsibility.** Keep the ONNX/CoreML arena out of the search daemon's address
space, install transparently, spawn lazily, and never starve interactive search.

**Key types/modules.**
- `EmbedderSupervisor` + `LazyEmbedderHandle` (`src/service/embedder_supervisor.rs`,
  re-exporting `trusty_common::embedder_client` supervisor types) — config from
  env, `default_socket_path()`, `locate_embedderd_binary()` (actionable install
  hint), deferred spawn (#315).
- `embed_pool` (`src/service/embed_pool.rs`) — fixed worker pool, interactive lane
  drained before background via biased `select!`, worker count autotuned from RAM
  (#41).
- `crates/trusty-embedderd/src/`: `protocol.rs`, `batch_queue.rs`,
  `stdio_server.rs`, `uds_server.rs`, optional `http-server` feature (#250); both
  the standalone binary and trusty-search's bundled shim call
  `trusty_embedderd::run()`.

**Current state.** ✅ One `cargo install` installs both binaries; sidecar spawned
on first embed request, supervised with backoff/restart limits, idle-shutdownable;
`TRUSTY_EMBEDDER` selects stdio (default) / in-process / http / uds / candle.

**Known gaps.** None material.

---

## 11. Embedded UI — `src/service/ui.rs`, `ui/`, `ui-dist/`

**Responsibility.** Browser-based search, index management, and chat served by the
daemon itself, with no separate frontend deploy.

**Key types/modules.**
- `src/service/ui.rs` — serves the bundle at `GET /ui`, `/ui/`, `/ui/{*path}`.
- `ui/` (Svelte 5 sources) → `ui-dist/` (compiled bundle embedded via
  `include_dir!`); built by `build.rs` (`SKIP_UI_BUILD=1` skips).
- `POST /chat` + `GET /api/chat/providers` — OpenRouter chat with search-context
  injection (503 without `OPENROUTER_API_KEY`).

**Current state.** ✅ Collections, Search, Chat, Admin panels compiled into the
binary; opened with `trusty-search ui`. CI fails if `ui-dist/` is stale
(`ui-dist-check`); `make release-prep` rebuilds + mirrors before publish.

**Known gaps.** None material; the UI is not part of the integration contract.

---

## 12. Cross-cutting: Memory Auto-Tuning — `src/core/memory_policy.rs`, `src/core/memguard.rs`

**Responsibility.** Bound RAM on long-running deployments: defaults sized to the
host, a soft RSS ceiling that degrades to a usable partial index, and an
Apple-Silicon batch tripwire.

**Key types/modules.**
- `MemoryPolicy` / `MemoryTier` (`src/core/memory_policy.rs`) — RAM detection,
  tier selection, env-override, write-back into process env;
  `resolve_coreml_batch_size`, `resolve_coreml_tripwire_mb`.
- `memguard` (`src/core/memguard.rs`) — `current_rss_mb` (per-PID, incl. sidecar),
  `index_memory_limit_mb`, soft-ceiling enforcement.

**Current state.** ✅ Five tiers (Tiny→XLarge); `MEMORY_LIMIT_MB =
clamp(RAM×25%, 1–64 GB)`; `MAX_BATCH_SIZE` auto-derived (#95); soft ceiling skips
remaining batches with `memory_limit_hit: true` (#75); CoreML tripwire halves
batch on > 4 GB RSS jumps; idle chunk-map eviction (300 s).

**Known gaps.**
- 🟡 README/CLAUDE.md describe the Tiny/Small tiers inconsistently vs. the 16 GB
  hard check at `start` (PRD Q8); the policy code still defines all five tiers.
