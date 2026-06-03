# Changelog

All notable changes to trusty-search are documented in this file.

Format follows [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).
Versions correspond to `Cargo.toml` patch releases.

---

## [0.23.0] — 2026-06-03

### Changed

- **redb 4.x + incompatible-corpus backup/rebuild on open** (#702) — index.redb
  and kg.redb are upgraded to redb 4.x. Existing redb 2.x files are detected as
  incompatible, backed up to `*.v2-incompatible`, and rebuilt (reindex triggered
  automatically). Possible multi-minute reindex window on first start after upgrade.

- **TRUSTY_HNSW_MMAP_SERVE (default on)** (#709) — warm-booted HNSW snapshots
  are now served directly from the mmap page cache, significantly reducing RSS.
  Promotion to a heap-resident copy is deferred until the first write. Disable with
  `TRUSTY_HNSW_MMAP_SERVE=0` on NFS/EFS-backed storage where cold page-fault
  latency matters more than RSS.

- **TRUSTY_VECTOR_QUANT (f16/i8)** (#712) — optional vector quantization for new
  HNSW indexes: `f16` (≈2× smaller, small recall cost) or `i8` (≈4× smaller,
  larger recall cost). Requires a forced reindex to take effect on existing indexes.

- **Persistent reindex hash cache** (#662) — content-hash cache for incremental
  reindex is now stored on disk and survives daemon restarts, avoiding unnecessary
  re-embedding on startup.

- **Dashboard auto-start** (#686) — the web UI dashboard auto-starts on first
  daemon launch without requiring a manual `trusty-search ui` invocation.

- **Bulk select/delete/reindex + Documents=0 fix** (#683) — UI and API support
  bulk operations; fixed a regression where new indexes incorrectly reported 0
  documents.

- **GET /indexes?details=true root_path** (#661) — the index list endpoint now
  accepts `details=true` to include `root_path` for each index.

- **Portable-paths fix + migration M004 schema 3→4** (#674) — index paths are
  now stored in a platform-portable form; M004 migration runs automatically on
  first start (non-destructive and idempotent).

> **OPERATOR NOTES:**
> 1. Existing `index.redb` and `kg.redb` files are redb 2.x and will be backed up
>    to `*.v2-incompatible` and rebuilt (reindex) on first start after upgrade.
>    Expect a multi-minute reindex window for large indexes.
> 2. Migration M004 runs automatically, is non-destructive, and is idempotent.

## [0.22.3] - 2026-06-02

### Fixed

- **CUDA arena VRAM OOM prevention (issue #600)** — via trusty-common 0.11.1:
  ORT's BFCArena is now configured with `arena_extend_strategy = kSameAsRequested`
  and an explicit `gpu_mem_limit` (default 12 GiB, tunable via
  `TRUSTY_GPU_MEM_LIMIT_BYTES` / `TRUSTY_GPU_MEM_LIMIT_MB`). Eliminates VRAM OOM
  on 16 GB Tesla T4 GPUs without requiring the `TRUSTY_MAX_BATCH_SIZE=32` workaround.

- **Accurate `/health` provider reporting (issue #604)** — the `provider` field in
  `/health` responses now reports the actual ORT execution provider in use (CUDA,
  CoreML, CPU) rather than always reporting CPU.

- **Non-destructive reindex with atomic swap (issue #603)** — `POST
  /indexes/:id/reindex` now builds a new corpus in a temporary database and swaps
  it atomically on completion, so the existing index stays fully searchable while
  the rebuild runs. Partial or failed reindex jobs no longer corrupt the live index.

- **Portable data paths and migration (issue #602)** — data-directory paths stored
  in persisted index metadata are now normalised at restore time so indexes survive
  machine renames, home-directory changes, and cross-machine copy. A forward
  migration updates stale absolute paths automatically.

- **Non-empty index validation (issue #601)** — the daemon now rejects a reindex
  swap if the freshly built corpus contains zero chunks, preventing an accidental
  wipe of a healthy index caused by a transient file-system or embedder failure.

---

## [0.18.0] - 2026-05-28

### Changed

- **Reduced default redb page-cache ceiling from 512 MB to 64 MB** (#329).
  Empirical profiling showed the actual redb working set for the trusty-tools
  corpus (23,513 chunks) is ~87 MB: a 512 MB cap run peaked at 557 MB RSS while
  an 8 MB cap run peaked at 470 MB — a difference of exactly 87 MB. The 512 MB
  ceiling was massively over-provisioned. The new 64 MB default captures the full
  working set with ~27 MB of headroom for B-tree internal nodes and future corpus
  growth, without the 33% indexing speed penalty observed at 8 MB (where I/O
  pressure becomes the bottleneck). Peak RSS during `--force` reindex of the
  trusty-tools corpus drops from 571 MB (v0.17.0 baseline) to 518 MB median
  (3-run distribution: 515/518/522 MB) — a 53 MB / 9.3% reduction with
  negligible timing impact (+1.6%, within noise). Override via
  `TRUSTY_REDB_CACHE_MB=<MB>` env var if needed.

### Performance

- See `docs/trusty-search/regression-testing/v0.18.0-redb-cap-reduction-cert-2026-05-28.md`
  for full cert numbers (3-run peak RSS distribution and reindex time comparison).

### Notes

- This is the B.2 quick-win from #329. The deferred B.1 (eliminate doc_terms),
  B.3 (lazy chunk LRU), and B.5 (posting compression) optimizations are tracked
  in the #329 follow-up work.
- Warm reindex is unchanged (empirically free — see profiling doc §9 M2).
- The `TRUSTY_REDB_CACHE_MB` env var override was already present; no API change.

---

## [0.17.0] - 2026-05-27

### Added

- **Issue #313 — Stage-1-minimal (`skip_kg`) mode.** A new additive flag
  `skip_kg: bool` on `PersistedIndex`, `IndexHandle`, and `IndexConfig` lets
  operators permanently suppress the Phase 3 Knowledge Graph rebuild for a
  specific index without disabling the embedder / vector search.

  **Three surfaces (D3):**
  - CLI: `trusty-search index --no-kg`
  - YAML: `skip_kg: true` in `trusty-search.yaml`
  - Env: `TRUSTY_NO_KG=1` (machine-wide default applied at `POST /indexes`)

  **Orthogonality (D1):** `skip_kg` and `lexical_only` are independent flags.
  Both can be set simultaneously. `lexical_only` suppresses Stages 2 and 3;
  `skip_kg` suppresses Stage 3 only, leaving vector embeddings intact.

  **503 contract (D2):** `GET /indexes/:id/call_chain` returns a structured
  503 JSON error `{ "error": "kg_unavailable", "reason": "skipped_by_config",
  "index": "…" }` when `skip_kg=true`. Callers must handle this status and
  not treat it as an index-absent 404.

  **Warm-boot:** on daemon restart, indexes with `skip_kg=true` have their
  graph stage initialised as `Skipped` rather than `Pending`, so no spurious
  KG-rebuild attempt is triggered.

  **Performance savings (per index):** ~50–100 MB heap (symbol graph), ~400 ms
  per reindex (tree-sitter extraction pass). Recommended for large
  documentation-only or generated-code sub-indexes in polyrepos.

---

## [0.16.0] - 2026-05-27

### Changed

- **Issue #315 — Lazy `trusty-embedderd` spawn with single-flight + optional
  idle shutdown.** `trusty-search start` no longer spawns the `trusty-embedderd`
  subprocess at daemon boot. Instead, a `LazyEmbedderHandle` is armed at
  startup and the child process starts on the first call to `embed` or
  `embed_batch` (reindex, hybrid search, `context_inference`). For
  `lexical_only` deployments with no semantic workloads the sidecar is never
  spawned, saving ~123 MB RSS.

  **Startup log change:** the boot log now contains
  `"embedderd supervisor armed, deferred spawn enabled"` instead of the
  previous "spawning sidecar" message. The first embed request logs
  `"LazyEmbedderHandle: first embed request — spawning trusty-embedderd"`.

  **Single-flight guarantee:** concurrent first callers serialise on an
  internal `Mutex`; exactly one spawn attempt is made regardless of how many
  embed calls arrive simultaneously.

  **Optional idle shutdown** (`TRUSTY_EMBEDDERD_IDLE_SHUTDOWN_SECS`, default
  `0` = disabled): when set to a non-zero value, the sidecar is killed after
  that many seconds of inactivity and the spawn gate is reset so the next
  embed request triggers a fresh spawn. Useful for `lexical_only` deployments
  that occasionally run a reindex.

  **Escape hatches unaffected:**
  - `TRUSTY_EMBEDDER=in-process` — no supervisor, no change.
  - `TRUSTY_EMBEDDER=http://...` or `unix://...` — no spawn, no change.
  - Binary discovery (`TRUSTY_EMBEDDERD_BIN`, PATH) still runs at daemon boot
    and fails fast if the binary is missing, preserving the existing install-hint
    error for misconfigured deployments.

  ```bash
  # Arm idle-shutdown for a lexical_only deployment:
  TRUSTY_EMBEDDERD_IDLE_SHUTDOWN_SECS=300 trusty-search start
  ```

---

## [0.15.1] - 2026-05-27

### Added

- **Issue #314 — `--no-auto-discover` flag and `TRUSTY_NO_AUTO_DISCOVER` env
  var for `trusty-search start`.** When either is set, the post-hydration
  auto-discovery scan (which walks `scan_paths` and indexes any unregistered
  project) is skipped entirely. The daemon starts with only the indexes already
  present in `indexes.toml` or registered at runtime.

  Precedence: CLI flag > env var > default (auto-discover enabled).

  Useful for CI/CD environments that must not discover arbitrary repositories,
  when the scan-paths tree is very large, or when reproducible startup
  behaviour is required.

  ```bash
  # Suppress auto-discovery via flag:
  trusty-search start --no-auto-discover

  # Suppress via env var (e.g. in a systemd unit or launchd plist):
  TRUSTY_NO_AUTO_DISCOVER=1 trusty-search start
  ```

---

## [0.15.0] - 2026-05-27

### Added

- **Issue #317 — Three-phase reindex progress bar (Walking → Chunking →
  Embedding).** The CLI reindex progress bar now shows file enumeration
  explicitly instead of several silent seconds before the first bar appeared.
  A single `ProgressBar` is reused across all three phases — the bar resets
  its position to 0 and updates its label at each phase boundary, "quickly
  filling to 100% then restarting" exactly as requested:

  - **Walking files…** — the daemon emits a new `walk_complete` SSE event
    after the file-system walk finishes. The bar fills instantaneously (the
    walk is synchronous on the daemon; the event arrives the moment it's done).
  - **Chunking…** — the `start` event (emitted immediately after
    `walk_complete`) triggers this brief label while the daemon begins the
    parse/embed pipeline. On large repos this handoff is visible for a fraction
    of a second before the first `batch` event arrives.
  - **Embedding chunks…** — the first `batch` event flips the bar into this
    phase and it fills as batches arrive, exactly as the old `ParseEmbed` phase
    did. For `lexical_only` indexes the embed phase is skipped; the bar stays
    on **Chunking** (there are no `batch` events for BM25-only indexes).

  **Daemon side:** a new `walk_complete` SSE event is emitted before the
  existing `start` event. Shape: `{"event":"walk_complete","total_files":1155}`.
  Old CLI clients that don't recognise `walk_complete` simply ignore it and
  wait for `start` — fully backward-compatible. New CLI clients talking to an
  old daemon (no `walk_complete`) fall back to the legacy two-phase flow
  (`start` → Embedding) automatically.

  **Decision on chunk+embed split (3 phases vs 2):** the daemon's pipelined
  orchestrator fuses parse+embed per batch — there is no clean "all chunks,
  then all embeds" split. `Chunking` is therefore a synthetic brief phase
  (the label shown between `walk_complete` and the first `batch` event, which
  is typically under one second). `Embedding` covers the rest of the pipeline
  exactly as the old `ParseEmbed` variant did. This matches Option 2 from the
  design spec and delivers the three visible phase labels the user asked for.

- **Bundled install** — `cargo install trusty-search` now produces **both**
  `trusty-search` and `trusty-embedderd` binaries from a single command.
  A second `[[bin]]` entry in `trusty-search/Cargo.toml` delegates to the
  `trusty-embedderd` library crate (`trusty_embedderd::run()`), so the
  sidecar binary is built and installed alongside the search daemon with
  zero extra steps. The standalone `cargo install trusty-embedderd` still
  works for advanced users who want only the embedding daemon.

  **Upgrade action (users coming from Phase 2):** simply run:
  ```
  cargo install trusty-search --locked --force
  ```
  No separate `cargo install trusty-embedderd` required.

### Changed (BREAKING)

- **#110 Phase 2 — `trusty-embedderd` is now a required runtime dependency.**
  When `TRUSTY_EMBEDDER` is unset, `trusty-search start` auto-spawns
  `trusty-embedderd --stdio` as a supervised child process and communicates via
  piped stdin/stdout (JSON-RPC 2.0). The child is restarted automatically on
  crash (up to `TRUSTY_EMBEDDERD_MAX_RESTARTS`, default 5) and is killed when
  the parent exits (via `kill_on_drop`).

  **BREAKING:** If `trusty-embedderd` is not found on PATH and
  `TRUSTY_EMBEDDERD_BIN` is unset, `trusty-search start` now **exits with an
  error** rather than silently falling back to in-process embedding. This is a
  deployment error — the sidecar architecture is a core design commitment, not
  an optional feature.

  **Upgrade action required:** install both binaries in one command:
  ```
  cargo install trusty-search --locked
  ```
  `cargo install trusty-search` now installs `trusty-embedderd` automatically —
  no second install command needed (bundled install, see above).
  To run without the sidecar (CI, debugging), set `TRUSTY_EMBEDDER=in-process`
  explicitly. The in-process path is an escape hatch, not a default.

### Added

- New `service/embedder_supervisor.rs` façade module: `SupervisorConfig` (with
  `from_env()` / `into_common()`), `locate_embedderd_binary()`, and
  `default_socket_path()`.
- Four `TRUSTY_EMBEDDER` modes: `auto`/unset (default stdio-sidecar),
  `in-process`, `http://...`, `unix:/path`.
- New `UdsEmbedderAdapter` for the `unix:` transport mode.
- New `SlotEmbedderAdapter` for the stdio-sidecar default: reads through the
  supervisor's `Arc<RwLock<Arc<dyn EmbedderClient>>>` slot so crash-restart
  swaps are transparent to all call sites.
- Integration test file `tests/embedder_supervisor_e2e.rs` with 7 `#[ignore]`-
  tagged lifecycle tests (spawn, batch, concurrency, crash-restart, empty batch,
  bit-identical, bad-path).
- New environment variables:
  - `TRUSTY_EMBEDDERD_STARTUP_TIMEOUT_SECS` (default 30)
  - `TRUSTY_EMBEDDERD_RESTART_BACKOFF_MAX_SECS` (default 60)
  - `TRUSTY_EMBEDDERD_MAX_RESTARTS` (default 5)
  - `TRUSTY_EMBEDDERD_BIN` — explicit path to the binary (overrides PATH search)

- **Schema migration framework.** Daemon startup now auto-migrates existing
  redb indexes when the schema version changes between releases. Migrations are
  non-blocking — the daemon serves queries at the pre-migration schema quality
  while each per-index task runs in the background. The schema version is
  persisted in a new `_meta` redb table after each successful migration step
  (crash-safe: a crash before the version write triggers a retry on next
  startup; idempotent `apply` implementations make retries safe).
  Set `TRUSTY_DISABLE_MIGRATIONS=1` to skip auto-migrations (debugging /
  one-off restore scenarios).

- **Migration M001: per-`pub const`/`pub static` Rust re-chunking (issue #143).**
  Indexes created before v0.11.1 had one `ChunkType::Code` chunk per Rust file
  instead of one `ChunkType::Constant` chunk per `pub const`/`pub static`
  declaration. M001 re-indexes every affected Rust file on first startup after
  upgrade, bringing those indexes up to v0.11.1 search quality. Idempotency is
  guaranteed by a "has Constant chunks?" pre-check; a regex pre-filter
  (`\bpub\s+(const|static)\b`) skips files that have no qualifying declarations
  without incurring the ~10 ms/file tree-sitter parse cost.

---

## [0.14.0] — 2026-05-27

### Added

- **`--data-dir <PATH>` flag on `trusty-search start`** (with `TRUSTY_DATA_DIR` env
  var) — overrides the platform default data directory for redb index storage,
  PID/port lockfiles, and `indexes.toml`. Enables multiple isolated daemon
  instances on the same machine; each instance gets its own data dir, binds its
  own port, and has no knowledge of the others.

  This flag was the key enabler for the Stage-1 cert methodology (issue #281):
  launching a fresh isolated daemon with `--data-dir /tmp/ts-stage1-cert` and
  a `HOME` override to suppress auto-discovery let us measure a clean reindex
  against a known-empty data dir without touching the production daemon on 7878.

  ```bash
  # Launch isolated cert daemon on a different port with its own data dir
  HOME=/tmp/ts-cert-home RUST_LOG=info trusty-search start \
      --data-dir /tmp/ts-stage1-cert \
      --port 7980 \
      --foreground
  ```

  The env var form is convenient for CI and container deployments:
  ```bash
  TRUSTY_DATA_DIR=/ci/search-data trusty-search start
  ```

  See `docs/trusty-search/regression-testing/v0.14.0-stage1-cert-2026-05-27.md`
  for the Stage-1 certification run that motivated this feature (issue #281).

---

## [0.12.1] — 2026-05-26

### Changed

- **Internal dep refactor (no behaviour change).** The `trusty-embedder-client`
  crate dependency has been removed. `EmbedderClient`, `RemoteEmbedderClient`,
  `EmbedRequest`, and `EmbedResponse` are now re-exported from
  `trusty_common::embedder_client` (feature `embedder-client`). All call sites
  updated from `trusty_embedder_client::` to `trusty_common::embedder_client::`.
  The remote-embedder opt-in path (`TRUSTY_EMBEDDER=http://...`) is fully
  functional and unchanged.

---

## [0.12.0] — 2026-05-26

### Added

- **#110 Phase 1** **Optional remote embedder via `TRUSTY_EMBEDDER` env var.**
  Set `TRUSTY_EMBEDDER=http://127.0.0.1:7890` to route all embed calls to a
  running `trusty-embedderd` instance instead of running ONNX in-process.
  Default behaviour (unset, `local`, or `in-process`) is unchanged.
  The startup log now always prints `embedder: in-process` or
  `embedder: remote <url>` so operators can confirm the active mode.

  New companion crates (v0.1.0, MIT):
  - `trusty-embedder-client` — `EmbedderClient` trait + JSON/HTTP wire types,
    `InProcessEmbedderClient` (default), and `RemoteEmbedderClient`
  - `trusty-embedderd` — standalone daemon that loads `AllMiniLML6V2(Q)` once
    and serves `POST /embed` + `GET /health` (clap CLI + axum HTTP, stderr logging)

---

## [0.11.1] — 2026-05-26

### Added

- **#143** `ChunkType::Constant` chunks per `pub const` / `pub static` Rust declaration.
  The Rust tree-sitter chunker now emits one `Constant` chunk per top-level public
  constant/static, with `function_name = Some(<identifier>)` (e.g. `BRUSILOV_EPOCH`).
  Previously a file containing only `pub const` declarations produced a single whole-file
  `Code` chunk with null `function_name`, making every constant invisible to symbol-name
  queries and the Definition-intent boost. Phase 1 covers Rust only; Python /
  TypeScript / Go / Java follow-up noted via TODO comment in the chunker.
- **#142** SCREAMING_SNAKE_CASE pattern in `QueryClassifier` — queries that are a
  single ALL_CAPS_WITH_UNDERSCORES identifier (e.g. `MAX_BATCH_SIZE`, `BRUSILOV_EPOCH`,
  `KIKUCHI_MAX_DEPTH`) now classify as `Intent::Definition` instead of `Unknown`.
  This was a gap in the priority chain: `SNAKE_IDENT_RE` matched lowercase snake_case
  but not SCREAMING_SNAKE; `ACRONYM_HINT_RE` fired on ALL_CAPS tokens *inside*
  multi-word queries but not on a whole-query constant name.

### Fixed

- **#142 + #143** Together these two fixes unblock the Definition-intent boost (#122)
  for constant lookups: the classifier correctly recognises SCREAMING_SNAKE queries,
  and the corpus now has per-constant chunks with non-null `function_name` for the
  structural lane to surface.

---

## [0.11.0] — 2026-05-26  **BREAKING**

### Removed

- **#152 / #145 PROVENANCE-ONLY decision** — Louvain community detection and
  `community_cohesion` ranking have been deleted. Empirical data showed the KG
  ranking lane lost Hit@1 by 16.7 pp vs semantic-only on KG-targeted queries
  (7/18 vs 10/18). The symbol-graph infrastructure is preserved — `get_call_chain`
  and `search_kg` MCP tools continue to work.

  BREAKING CHANGES:
  - `CodeChunk.community_id` field removed from schema (read tolerance preserved
    via `#[serde(default)]` — existing serialised chunks are tolerated on
    deserialise).
  - Post-RRF reranker no longer applies `community_cohesion` blending. The
    `meta.graph_scoring` and `meta.community_cohesion` fields are gone from
    search response JSON.
  - `GET /indexes/:id/communities` and `GET /indexes/:id/communities/:symbol`
    endpoints return 404 (removed, not deprecated).
  - `spawn_community_detection` removed from the reindex pipeline.

  Deleted components:
  - `src/core/community.rs` — entire Louvain implementation (673 lines)
  - `src/core/indexer/graph_score.rs` — `GraphScorer` / centrality bonus table (309 lines)
  - `SearchAppState::graph_scorer()` and `invalidate_graph_scorer()` methods
  - `GraphScorerCache` type alias and `spawn_community_detection` reindex task
  - `CodeChunk::community_id` field
  - `GET /indexes/:id/communities` and `GET /indexes/:id/communities/:symbol` endpoints
  - `meta.graph_scoring` and `meta.community_cohesion` fields from search response

  Migration notes for callers:
  - `CodeChunk` serialisations with `community_id` are tolerated (ignored on
    deserialise via `#[serde(default)]`). No schema migration required.
  - Old redb community tables (`KG_COMMUNITIES_TABLE`, `kg_symbol_community`)
    remain defined in `corpus.rs` for migration tolerance; they are no longer
    written or read by the active search path.
  - Remove any code polling `meta.graph_scoring` or `meta.community_cohesion`
    from search responses.
  - Remove any calls to the `/communities` or `/communities/:symbol` endpoints.

---

## [0.10.0] — 2026-05-25

### Added

- **#138** **Per-lane MCP tools — push intent classification to the LLM.**
  Four new MCP tools — `search_lexical`, `search_semantic`, `search_kg`,
  `search_all` — let the calling LLM pick the right lane combination
  instead of relying on the server-side regex intent classifier.
  - `search_lexical` — BM25 + grep only, ripgrep-equivalent latency.
    Always available.
  - `search_semantic` — BM25 + HNSW via RRF, no KG. Requires Stage 2
    (`vector`) on the index.
  - `search_kg` — BM25 + HNSW + KG expansion, forced ON. Requires Stage 3
    (`kg`).
  - `search_all` — full hybrid (lexical + semantic + KG), adaptive
    routing. Polymorphic: with `index_id` it's per-index hybrid (ticket
    spec); without, it falls back to legacy cross-project fan-out
    (issue #10) for back-compat.

  The legacy `search` tool stays as a back-compat alias for the
  per-index full hybrid. The MCP `tools/list` response now surfaces
  five lane-related search tools.

  When a per-lane tool is called against an index whose prerequisite
  stage isn't `Ready`, the daemon returns a structured `STAGE_NOT_READY`
  error (JSON-RPC code `-32010` or, via `tools/call`, `isError: true`
  with `_meta.error_code = "STAGE_NOT_READY"`). The error carries the
  full `current_stages` snapshot and a `suggested_tools` retry hint so
  the LLM can pick a fallback without a second status probe.

  `SearchStage` gains `Semantic` and `Graph` variants alongside the
  existing `Lexical`. The search dispatcher routes each variant to its
  fixed lane combination: `Lexical` skips HNSW + KG; `Semantic` runs
  BM25 + HNSW but skips KG; `Graph` forces KG expansion even on
  Definition-intent seed queries. `stage = None` keeps the legacy
  adaptive routing.

  Tool descriptions follow the ticket's authoring guide (when-to-use
  hook, fit/don't-fit examples, cost class, failure-mode hint) and
  carry `examples` arrays in their JSON schemas to nudge LLM tool
  selection. The classifier and per-stage gating remain in place as
  defensive fallbacks for non-MCP HTTP callers.

### Changed

- The `search_all` MCP tool is now polymorphic: when invoked with an
  `index_id`, it dispatches the per-index full hybrid (matching the
  #138 spec); when invoked without one, it preserves the legacy
  cross-project fan-out behaviour. Callers using either form keep
  working without code changes.

---

## [0.9.2] — 2026-05-25

### Fixed

- **#122** Definition boost regresses Hit@1 on function-name queries with
  descriptor / string-literal matches. The struct-definition boost added in
  v0.8.x (#117) covered `Struct`/`Enum`/`Class`/`Trait`/`TypeAlias` chunks
  but deliberately excluded `Function`/`Method` because we assumed the
  `inject_entity_exact_match` lane would carry function-name queries. The
  synthetic-corpus baseline (#123) reproduced a clean failure for Q04
  `BRUSILOV_EPOCH`, where a usage site (`calibration.rs`) out-ranked the
  canonical declaration (`constants.rs`) across all three search modes.

  The fix extends `apply_score_adjustments` to also apply
  `STRUCT_DEFINITION_BOOST` (2.0×) to `Function`/`Method` chunks whose
  `function_name` matches a query token. The chunk_type filter is the
  natural defense against the JSON-descriptor false-positive case (a
  `Constant` chunk containing `"get_call_chain"` as a string literal in an
  MCP tool descriptor): JSON-descriptor chunks are typed `Constant` or
  `Statement`, not `Function`, so they are never boosted.

  Four regression tests pin the new behavior:
  `test_function_definition_boost_surfaces_function_over_string_literal_usage`,
  `test_method_definition_boost_fires`,
  `test_function_boost_skipped_on_conceptual_intent`, and
  `test_function_boost_no_op_when_function_name_missing`.

---

## [0.9.1] — 2026-05-25

### Fixed

- **#135** Warm-boot stages restoration — fixes silent BM25-only fallback on
  existing indexes. The v0.9.0 staged-pipeline refactor introduced a regression
  in the daemon's warm-boot path: every index restored from `indexes.toml`
  came back with `stages = Pending` for lexical / semantic / graph, regardless
  of what was on disk. Because the search handler now derives
  `search_capabilities` from `stages` (not the legacy top-level `status`), the
  hybrid pipeline was silently disabled on every fully-indexed registered
  project until the operator force-reindexed.

  The fix inspects each index's on-disk artifacts after warm-boot:
  `corpus.chunk_count()` (lexical readiness), `hnsw.usearch` presence
  (semantic readiness), and the rehydrated symbol graph's `node_count()`
  (graph readiness). A `lexical_only` index forces semantic + graph to
  `Skipped` regardless of on-disk state. An index with `chunk_count == 0`
  but a registered entry is treated as mid-reindex recovery (lexical →
  `InProgress`) so the next reindex resumes via the hash-skip path.

  No schema change: the existing on-disk artifacts are authoritative, so
  `indexes.toml` did not need a `stages_marker` field. Existing daemons
  pick up the fix on the next restart with no migration step.

---

## [0.9.0] — 2026-05-25

### Added — staged-pipeline (Phase 1)

- **#109 (Phase 1)** Staged indexing pipeline — initial cut. The reindex
  pipeline now exposes per-stage progress so searches can run as soon as
  the lexical lane (Stage 1) is ready, without blocking on the embedder
  (Stage 2) or symbol-graph build (Stage 3).

  - **Status surface.** `GET /indexes/{id}/status` gains two additive
    fields (back-compat preserved):
    - `stages: { lexical: …, semantic: …, graph: … }` carrying per-stage
      `status` (`pending` | `in_progress` | `ready` | `skipped`),
      timestamps, and counters.
    - `search_capabilities: ["bm25", "literal", "exact_match", …]`
      growing as each stage flips to `ready` (`+ ["vector"]` when
      semantic ready, `+ ["kg"]` when graph ready).
    The legacy top-level `status` field is unchanged for existing API
    consumers.

  - **Search handler graceful degradation.** The handler now consults
    `search_capabilities` (not the top-level `status`) to decide which
    lanes to run. Searches during a reindex hit only the BM25 lane until
    the embedder catches up — the response carries
    `meta.search_capabilities` so clients can show "lexical-only" badges
    or retry once the semantic lane lands.

  - **`?stage=lexical` query param.** Per-query opt-in to Stage-1-only
    routing even on a fully-indexed index. Useful for
    grep-replacement use cases that don't want semantic noise.

  - **`--lexical-only` CLI flag and `lexical_only: true` API field.**
    Permanent opt-out from Stage 2 and Stage 3 at index-create time.
    The index stays at `status: indexed_lexical` forever; the reindex
    pipeline skips the embedder entirely. Persisted to `indexes.toml`
    so the choice survives daemon restarts. Useful for callers who
    explicitly want a "daemonized ripgrep" without the embedder
    overhead.

  - **Backpressure stub.** Search calls ping a per-index
    `tokio::sync::Notify` so the background Stage-2 task can yield
    briefly. Phase-2 work will tune the policy.

  Out of scope for Phase 1 (deferred to Phase 2): Stage 3 (Louvain) /
  KG-edge resolution async split — they remain in the synchronous
  reindex tail; file-watcher debouncing; full backpressure tuning.

  Pinned by `service::reindex::tests::stage_1_completes_and_search_works_before_embedding`,
  `lexical_only_index_never_runs_stage_2`,
  `search_capabilities_grows_as_stages_complete`, and the per-stage
  registry tests in `core::registry::tests::stage_status_*`.

### Changed

- **`CodeIndexer`** gains a `parse_files_only` method that mirrors
  `parse_and_embed_files` but skips the ONNX embed step entirely. Used
  exclusively by the `lexical_only` reindex path so a BM25-only index
  never pays the embedder's session-arena cost. Existing callers are
  untouched.
- **`SearchQuery`** gains an optional `stage: Option<SearchStage>`
  field. Defaults to `None` so existing callers see no behaviour
  change; setting `Some(SearchStage::Lexical)` forces the
  Stage-1-only lane routing.
- **`PersistedIndex`** gains a `lexical_only: bool` field with
  `#[serde(default)]` so legacy `indexes.toml` files load as `false`
  (full pipeline). Only explicit-`true` is written to disk to keep
  the on-disk format compact.

---

## [0.8.3] — 2026-05-25

### Fixed
- **#118** `mode=text` searches no longer return silently empty result
  sets. The walker's `include_docs` default flipped from `false` to
  `true`: prose docs (`*.md`, `*.mdx`, `*.rst`, `*.txt`, `*.adoc`) and
  `CHANGELOG*` / `LICENSE*` / `NOTICE*` files (with extensions) are now
  indexed alongside source. The per-mode hard filter
  (`is_allowed_for_mode`) is the single source of truth for which file
  types each mode returns — code-mode results never include `.md` chunks
  because the post-RRF filter rejects them, regardless of what the
  walker indexed.

  Migration: an `indexes.toml` entry written by v0.8.2 (where
  `include_docs = false` was the default and omitted via
  `skip_serializing_if`) now deserialises as `true` under v0.8.3 —
  `mode=text` searches start working on the next daemon restart with no
  explicit migration step. Indexes that PERSISTED an explicit
  `include_docs = false` (e.g. via `trusty-search.yaml`) keep their
  opt-out. Pinned by
  `service::persistence::tests::include_docs_defaults_true_and_round_trips`.

  The file watcher (`watch_loop`) follows the new default so live `.md`
  edits flow into the index too; the v0.8.2 `is_default_doc_excluded`
  guard there was removed.

  Acceptance pinned by
  `service::walker::tests::test_issue_118_acceptance_walks_both_source_and_docs`
  (walk side) plus the existing
  `core::indexer::tests::test_mode_filter_code_returns_only_source` /
  `test_mode_filter_text_returns_only_prose_and_named_docs` (search side).
- **#117** Definition-intent searches now surface struct/enum/class/trait
  declarations above usage sites. On the v0.8.1 benchmark the query
  `HNSW vector similarity search` placed `hnsw_store.rs` at rank 8 behind
  `retrieval.rs` and `mmr.rs` because the BM25 lane couldn't distinguish
  "file mentions HNSW many times" from "file IS the HNSW declaration". Two
  layered fixes:
  - #119's classifier upgrade routes the query to `Definition` (was
    `Unknown`), which already demotes docs and runs the grep lane.
  - The post-RRF reranker (`apply_score_adjustments`) now multiplies the
    score of any `Struct`/`Enum`/`Class`/`Trait`/`TypeAlias` chunk by
    `STRUCT_DEFINITION_BOOST = 2.0` when the chunk's `function_name`
    contains (case-insensitive) at least one query token. Substring
    rather than exact match so `HnswStore`/`hnswstore` matches the
    `hnsw` token; `is_struct_definition_chunk_type` enforces that only
    declaration-shaped chunks qualify (free code and methods don't).

  Acceptance pinned by `test_struct_definition_boost_surfaces_struct_over_usage`:
  a corpus with one Struct declaration (`HnswStore` in `hnsw_store.rs`)
  and three usage chunks now ranks the declaration in top-3 for the
  canonical query.
- **#119** `QueryClassifier` now recognises three additional query shapes
  that were silently returning `intent: "Unknown"` on the v0.8.1
  grep-equivalency benchmark — keeping the existing intent-aware lane
  weighting, RRF balance, and effective-mode override dormant on 12 of
  14 real queries. The new rules:
  - **Single `snake_case` identifier** (e.g. `apply_archive_downrank`,
    `is_default_doc_excluded`, `get_call_chain`, `bm25_search`) →
    `Definition`. Token must be the whole query and must contain at
    least one underscore so a bare `foo` is not pulled into the rule.
  - **ALL-CAPS acronym hint** (e.g. `HNSW`, `BM25`, `RRF`, `ORT`, `API`,
    `LRU`) anywhere in the query → `Definition`. These almost always
    refer to a struct, module, or type name in the codebase, so routing
    them to Definition lets the structural lane surface the canonical
    declaration over usage sites. This also closes #117 (see below).
  - **≥4-word natural-language query with no identifier tokens** (e.g.
    `axum middleware concurrency limiter`,
    `Louvain community detection modularity`,
    `redb persistence write transaction`,
    `embed batch async worker pool`) → `Conceptual`. Lower bar than the
    existing 6-word `LONG_NL_RE` so short concept queries also route to
    the vector lane.

  Benchmark impact: ≥13/14 of the canonical queries now classify as
  non-`Unknown` (was 2/14). Pinned by
  `core::classifier::tests::test_canonical_benchmark_at_least_12_of_14_classified`.

---

## [0.8.2] — 2026-05-25

### Fixed
- **#100 follow-up** Clarified the `reindex complete:` daemon log line so a
  hash-skip-only run no longer looks like a walker → chunker regression. The
  log now includes `indexed_new=` (files that actually re-chunked this run,
  derived as `indexed - skipped`) alongside the existing counters. Previously
  a second reindex of an unchanged workspace logged `files=N chunks=0` —
  textually identical to a hypothetical walker bug that yields paths but
  drops them — so operators kept misreading the fast path as a failure
  (extensive investigation in the v0.8.1 issue thread). The same
  `indexed_new` field is now surfaced on the reindex `complete` SSE event so
  external callers (CLI, dashboard, open-mpm) read the same signal.

### Added
- New end-to-end integration test `reindex_persists_chunks_end_to_end` in
  `service::reindex::tests` that runs the FULL pipeline (walker → chunker →
  corpus) twice against a staged tempdir. The first reindex asserts
  `total_chunks > 0`, `chunk_count() > 0`, and that a search for a unique
  function name returns a chunk whose `file` field equals the canonical
  `lib.rs` path. The second reindex asserts `total_chunks == 0` AND
  `skipped == 1` — pinning the hash-skip fast path so the next bisection
  doesn't waste another round chasing a non-existent walker bug. The
  walker-only unit tests added in v0.8.1 catch the walker yield but not the
  chunker / corpus end of the pipeline; this test closes that gap.

### Internal
- `apply_successful_commit` and `emit_complete_event` derive `indexed_new`
  from the existing `indexed` and `skipped` counters — no new tracked state.

---

## [0.8.1] — 2026-05-25

### Fixed
- **#100** Walker now honours `.gitignore`. Previously the walker used
  `walkdir` directly, which ignores all VCS-aware ignore files; combined with
  the per-index chunk budget (`TRUSTY_MAX_CHUNKS`, auto-tuned from the memory
  policy), this caused silent partial-index failures where a gitignored
  subtree (e.g. `claude-mpm-patch/` full of minified bundles) dominated or
  exhausted the budget before the walker reached the actual project source.
  The walker now delegates to the `ignore` crate — the same engine ripgrep
  uses — and respects `.gitignore`, `.git/info/exclude`, the global git
  ignore, `.ignore`, `.rgignore`, and parent-directory ignore files. The
  hardcoded `SKIP_DIRS` / `should_skip_path` filters still apply as
  defence-in-depth for projects without a `.gitignore` (closes #100).

### Added
- **#100** `respect_gitignore` opt-out for indexes that intentionally walk a
  gitignored / vendored subtree. The flag rides on `WalkOptions`, `IndexConfig`
  (`trusty-search.yaml`), `IndexHandle`, `PersistedIndex` (with serde default
  for back-compat with existing `indexes.toml` files), and the
  `POST /indexes` `respect_gitignore` request field. Default `true` so every
  existing caller picks up the fix automatically without a wire change.
- **#100** `walk_truncated_by_budget` (boolean) and `chunks_dropped_by_cap`
  (count) surfaced in `GET /indexes/:id/status` and the reindex `complete` SSE
  event. Non-zero ⇒ the index is incomplete because the per-index chunk cap
  was reached during the walk; operators previously had no way to distinguish
  a clean index from one whose tail was silently dropped. Defaults to
  `false` / `0` for indexes warm-booted from disk that haven't been reindexed
  since the daemon started.

### Internal
- New `ignore = "0.4"` direct dependency in `crates/trusty-search/Cargo.toml`
  (previously a transitive dep via globset / notify).
- `CommitTimings.chunks_dropped_by_cap` plumbs the per-batch cap-drop count
  from `core::indexer::ingest` up through `service::reindex` to
  `ReindexProgress`.
- 4 new walker unit tests pin the behaviour:
  `test_walker_honors_gitignore`, `test_walker_respects_disable_flag`,
  `test_walker_honors_dot_ignore`, `test_walker_still_skips_hardcoded_dirs`.

---

## [0.3.57] — 2026-05-21

### Changed
- Granular per-phase progress for `trusty-search index` / `reindex`. The live
  progress display now carries a phase label on its header line
  (`Connecting → Parsing & embedding files → Done`) and the stats line shows
  embedding throughput (`<N> cps`) and a file-derived ETA. The post-reindex
  timing breakdown is reorganised into five named phases — Parse/chunk, Embed,
  Upsert vectors, BM25 index, Knowledge graph — and now includes the
  vector-upsert timing. Progress draws to **stderr** only and is suppressed
  entirely when stdout is not a TTY (piped / redirected output).

---

## [0.3.56] — 2026-05-21

### Fixed
- **#127** `TRUSTY_INDEX_MEMORY_LIMIT_MB` auto-tune raised from 40% to 75% of system RAM. The old 40% fraction yielded only a ~52 GB ceiling on a 128 GB host, but large repos (e.g. 114k chunks) peak at ~76 GB RSS during reindex on Apple Silicon — the pipeline hit the limit and skipped batches, leaving the index incomplete. 75% of RAM gives the transient indexing pipeline enough headroom while still reserving 25% for the OS and other processes. The `TRUSTY_INDEX_MEMORY_LIMIT_MB` env override is unchanged; the startup log now reports "75% of RAM".
- **#128** Batch HNSW upsert no longer silently drops a whole 128-file batch when one embedding fails. `UsearchStore::upsert_batch` now screens each vector for NaN / infinity / all-zero (degenerate-for-cosine) components and isolates per-item `add` failures: the offending chunk id is logged at `warn`, its key-map entry is rolled back, and the remaining vectors index normally. The call only returns `Err` when *every* vector fails (a systemic problem).

---

## [0.3.36] — 2026-05-15

### Added
- **#122** Branch-aware scoring: `branch_files` request field boosts chunks from the current branch by a configurable multiplier (default 1.5×, clamped to `[1.0, 3.0]`); results carry `on_branch: bool`; when `branch_files` is absent, the daemon shells out to `git merge-base` + `git diff --name-only` to derive the file list automatically

### Fixed
- **#121** Embedder init hang: ORT initialization now runs on a blocking thread with a configurable timeout (`TRUSTY_EMBEDDER_INIT_TIMEOUT_SECS`); a timeout surfaces as an error state rather than hanging forever
- **#120** `MEMORY_LIMIT_MB` recomputed as 25% of system RAM instead of a fixed tier cap; `TRUSTY_MEMORY_LIMIT_MB` still overrides

### Changed
- Makefile: `CLOSES` variable support in `patch` target; surgical daemon stop in `deploy` (PID lockfile + `pkill -x`) instead of broad pattern match; `kill` before deploy prevents OOM during compile; `launchctl unload` in deploy target prevents dual-daemon OOM
- Workflow: `closes #N` now required in all resolution commits

---

## [0.3.35] — 2026-05-14

### Fixed
- **#119** CoreML jetsam crash on Apple Silicon (via trusty-embedder v0.1.5 bump)
- **#118** `DELETE /indexes/:id` now persisted to `indexes.toml` so removals survive daemon restart
- Daemon now detaches from terminal when started without `--foreground`, fixing crash when the parent tmux session is killed
- ORT batch size default lowered from 200 MB/slot estimate; clamp changed to `[8, 64]` to prevent 94 GB reindex spikes

### Changed
- `TRUSTY_DEVICE` persisted to `daemon.env` so `--device cpu` survives daemon restarts
- Makefile: `deploy` target added with `CARGO_BUILD_JOBS=2` to prevent OOM kills; `cargo install` removed from `patch` target

---

## [0.3.34] — 2026-05-13

_(version bump only; internal release pipeline fix)_

---

## [0.3.33] — 2026-05-13

### Added
- OpenRPC `rpc.discover` endpoint exposed via trusty-mcp-core helpers
- `SearchMcpService` implements `ServiceDescriptor` (#115)
- Migration script for mcp-vector-search → trusty-search

### Fixed
- **#117** `serve --http` no longer clobbers the daemon's `http_addr` discovery file
- **#116** tree-sitter upgraded to 0.26 for direct linking compatibility with open-mpm
- **#114** glibc 2.34 compatibility for CUDA builds on Amazon Linux 2023
- Test flakiness in file-watcher test on macOS (stray tmpdir events)

### Changed
- trusty-mcp-core bumped to v0.1.1 for OpenRPC support
- trusty-embedder bumped to v0.1.4 for bundled-ort support

---

## [0.3.32] — 2026-05-12

### Fixed
- **#117** `serve --http` flag no longer overwrites the daemon's HTTP address discovery file, preventing the CLI from connecting to the wrong process

---

## [0.3.31] — 2026-05-12

### Added
- **#112** Index context inference and smart fan-out routing: queries against unknown or multi-index contexts are routed to the best-matching indexes automatically
- **#113** Runtime CUDA auto-detection with GPU batch size tuning: when a CUDA-capable GPU is detected, `TRUSTY_MAX_BATCH_SIZE` is auto-bumped to 512; set `TRUSTY_MAX_BATCH_SIZE_EXPLICIT=1` to keep a manually configured value

---

## [0.3.30] — 2026-05-12

### Added
- **#110** `POST /search` fan-out endpoint: search across all registered indexes in a single call, results merged by RRF score
- **#111** `path_filter` field on index registration: restrict which file paths are indexed for a given `IndexId`
- **#91** Classifier extended to match leading-acronym identifiers (`BM25Index`, `IOError`, `URLParser`)

---

## [0.3.29] — 2026-05-12

### Fixed
- `colored::Colorize` import gated to macOS only, fixing compilation on Linux

---

## [0.3.28] — 2026-05-12

### Changed
- **#97** Extracted 52 functions from `main.rs` into `commands/` modules for improved maintainability
- **#98, #109** Extracted helpers in `build.rs` and `spawn_reindex` into focused async helpers
- **#98** Reindex phases extracted into focused async helpers
- **#103** `symbol_graph` helpers extracted to reduce cyclomatic complexity
- **#101, #104** Replaced `unwrap`/`panic`/`process::exit` with proper error handling throughout

### Tests
- **#99** Unit tests added for CLI command handlers and daemon-guard paths

---

## [0.3.27] - 2026-05-12

### Fixed
- **#87** macOS SIGKILL on binary replace: `trusty-search start` now exits with an error if a daemon is already running; `make install` and `make patch` stop the daemon before reinstalling the binary
- **#82** Memory limit enforcement during reindex: tier-based hard caps on `TRUSTY_MAX_BATCH_SIZE` env-var overrides (Medium=64, Large=128, XLarge=256) prevent RSS spikes from misconfigured batch sizes; existing background RSS poller confirmed active
- **#89** ORT ONNX arena pre-allocation: confirmed mitigated by `with_arena_allocator(false)`; tier hard caps add defense-in-depth

### Improved
- **#88** Intent classifier now recognises domain-term Definition queries: PascalCase/CamelCase identifiers, and standalone "definition"/"interface"/"schema"/"type"/"enum"/"model" trigger Definition intent
- **#91** Compound noun classifier: CamelCase compound noun queries (e.g. "QueryClassifier intent classification") now route to Definition intent instead of Unknown
- **#92** Definition-intent ranking: `.md`/`.toml`/`.json`/`.yaml` files scored at 0.5× in RRF fusion for Definition intent only; source files rank first for symbol lookups
- **#94** KG expansion: results merged by score before `take(top_k)`; `hybrid+kg` match_reason now surfaces on large indexes

---

## [0.1.46] — 4 indexing speed optimizations

### Performance
- **INT8 quantized model**: switch fastembed model to `AllMiniLML6V2Q` (INT8 quantized); same 384-dim output, ~30% faster ONNX inference
- **Batch upsert**: accumulate HNSW vectors across all chunks in a reindex pass and call a single `UsearchStore::upsert_batch` instead of N individual inserts; eliminates per-chunk lock overhead
- **Split lock** (`parse_and_embed_files` / `commit_parsed_batch`): parsing + embedding now runs outside the write lock; the write lock is held only for the final redb + HNSW commit, enabling higher concurrency
- **Batch size 512**: increase ONNX batch size 256 → 512 for better GPU/NEON/AVX2 saturation
- Combined target: **< 2 min on a 14k-file repo** (down from ~2–4 min after v0.1.34)

---

## [0.1.45] — multi-line progress + blue-green verify + incremental index

### Added
- **Multi-line progress display**: `indicatif::MultiProgress` shows concurrent bars — one per active reindex stream — plus a summary line with aggregate `chunks/s`
- **Blue-green verify**: after a reindex completes, a lightweight verification pass confirms the new HNSW index answers a canary query before swapping the live handle; prevents silent corruption on large repos
- **Incremental index flag** (`--incremental`, default on): skips files whose sha2 fingerprint matches the stored value even across daemon restarts; `--force` still triggers a full rebuild

---

## [0.1.44] — async HTTP server in trusty-common

### Added
- `trusty-common`: `server` module with `with_standard_middleware` (axum-server feature) and `daemon_http_client` helper
- `trusty-search-service`: `build_router` uses `with_standard_middleware`
- `main.rs`: all daemon HTTP call sites use `trusty_common::server::daemon_http_client`

---

## [0.1.43] — HTTP timeouts (fix status hang)

### Fixed
- Add 2s connect / 5s request timeouts to all daemon HTTP calls via `daemon_client()` helper; `status`, `health`, `doctor`, `query`, and `reindex` now fail fast with a clear error instead of hanging when the daemon is not running

---

## [0.1.42] — status/health unified + doctor command

### Added
- `status` and `health` are now aliases for the same `run_status()` handler; output shows daemon version, port, and per-index chunk counts
- `trusty-search doctor`: 6-check diagnostic (daemon liveness, model cache, data-dir writability, stale lockfile, empty indexes, port reachability) with colored ✓/⚠/✗ output
- `doctor --fix`: auto-repairs stale lockfile and empty indexes via `run_reindex` with progress bar; exits 1 on any error

---

## [0.1.41] — `index` primary command + indicatif progress bar

### Added
- `trusty-search index [PATH] [--name <id>] [--force]`: auto-registers the index if absent, skips if already indexed, `--force` triggers full reindex; replaces the awkward `init` + `reindex` two-step
- `indicatif` progress bar during reindex: `⟳ Indexing {id} [████░░] {pos}/{len} files — {eta} remaining`; updates on each SSE batch event, finishes with chunk count and elapsed time
- `register_index_with_daemon()` and `fetch_chunk_count()` helpers shared between `Init` and `Index` commands
- `init` and `reindex` preserved as backward-compatible aliases

---

## [0.1.40] — wire shared crates throughout

### Refactored
- `ui.rs`: replace inline OpenRouter HTTP client with `trusty_common::openrouter_chat` (~50 lines removed)
- All three shared crates (`trusty-mcp-core`, `trusty-embedder`, `trusty-common`) fully wired into every consumer
- Pin shared crates to public git tags on `bobmatnyc/trusty-common` (v0.1.0); remove in-tree copies (net −985 LOC)

---

## [0.1.39] — exclude minified JS/build dirs from indexing

### Added
- `should_skip_path()`: skips `*.min.js/css`, `*.bundle.js`, `*.chunk.js`, hashed bundles, binary extensions, files > 1 MB
- `should_skip_content()`: heuristic minification detection for `.js/mjs/cjs` (< 5 lines with any line > 500 chars)
- `SKIP_DIRS`: `node_modules`, `dist`, `build`, `target`, `.git`, `__pycache__`, `.next`, `.nuxt`, `.svelte-kit`, `vendor`, `.gradle`, `.m2`, `coverage`, `.nyc_output`
- Reindex emits SSE `"skip"` event with `reason:"minified"` for content-skipped files
- 14 new tests covering all skip patterns

---

## [0.1.38] — shared crates (trusty-mcp-core, trusty-embedder, trusty-common)

### Added
- `trusty-mcp-core`: `McpRequest`/`McpResponse`/`JsonRpcError`, error code constants, `run_stdio_loop` generic async stdio handler, CORS/Trace axum helpers
- `trusty-embedder`: `Embedder` trait, `FastEmbedder` with LRU cache + persistent model cache dir, `EMBED_DIM=384`, `MockEmbedder` for tests, `embed_one` helper
- `trusty-common`: `bind_with_auto_port`, `resolve_data_dir`/`cache_dir`, `ConcurrentRegistry<K,V>`, `init_tracing`, `maybe_disable_color`
- All three registered in workspace; 163 tests passing

### Refactored
- Adopt shared crates and delete inlined equivalents; `trusty-search-core::embed` becomes a thin facade over the shared `Embedder` trait
- Daemon port binding goes through shared async helper; `main.rs` uses `init_tracing`/`maybe_disable_color`

---

## [0.1.37] — daemon early-exit + model cache

### Fixed
- `is_already_running()` checks lockfile before `FastEmbedder::new()` so "another daemon running" exits in < 1 ms instead of after an 86 MB model download

### Added
- `model_cache_dir()` resolves `~/Library/Caches/trusty-search/models/`; model downloads once and loads from disk on all subsequent daemon starts
- `serial_test` on embed tests prevents `hf_hub` lock-file races in parallel test runs

---

## [0.1.36] — HTTP ↔ MCP functional parity

### Added
- Four missing MCP tools added for full HTTP endpoint coverage:
  - `delete_index` ← `DELETE /indexes/:id`
  - `reindex` ← `POST /indexes/:id/reindex`
  - `index_status` ← `GET /indexes/:id/status`
  - `chat` ← `POST /chat` (OpenRouter proxy)
- `test_tools_list_complete` asserts HTTP/MCP parity; 151 tests passing

---

## [0.1.35] — Svelte admin UI + MCP stdio server

### Added
- **Web management UI** served at `GET /ui`:
  - Collections panel: list/create/delete indexes, reindex with live SSE progress
  - Search panel: single and cross-collection hybrid search, `match_reason` badges, compact/full snippet toggle
  - Chat panel: OpenRouter-backed conversational Q&A (gated by `OPENROUTER_API_KEY`)
  - Admin panel: daemon info, per-file index/remove ops, danger zone
  - Static assets embedded at compile time via `include_dir`
- `POST /chat` endpoint proxies to OpenRouter with search context injection
- `DELETE /indexes/:id` endpoint
- `trusty-search ui` subcommand: start daemon + open browser
- **MCP stdio JSON-RPC server** (full JSON-RPC 2.0 over stdin/stdout, protocol 2024-11-05):
  - `initialize` handshake, `notifications/initialized` suppressed correctly
  - `tools/list`: all 6 tools (`search_code`, `index_file`, `remove_file`, `list_indexes`, `create_index`, `search_health`)
  - `tools/call`: MCP-spec content envelope with `isError` flag
  - Graceful shutdown on stdin EOF; errors to stderr only

---

## [0.1.34] — 4× faster indexing

### Performance
- Eliminate 452 symbol-graph rebuilds per reindex: `index_files_batch_no_rebuild` defers graph rebuild to once at completion
- `resolve_callee` O(N×S) linear suffix scan replaced with O(1) hash lookup using precomputed simple-name → `NodeIndex` map
- Batch size 32 → 128 for better ONNX saturation
- `drain` RawChunk corpus instead of cloning (saves ~115k allocations per reindex)
- Expected reduction on large monorepos: ~46 min → 2–4 min

---

## [0.1.33] — hot BM25/HNSW/LRU fixes + CLI stubs

### Fixed
- **Bug A**: Wire `FastEmbedder` + `UsearchStore` in `create_index_handler`; HNSW now actually stores and returns vector results
- **Bug B**: Replace per-query BM25 rebuild with persistent `Arc<RwLock<Bm25Index>>` maintained incrementally at index time; search is O(df_i) not O(corpus)
- **Bug C**: LRU embedding cache now deduplicates across requests (was masked by Bug B)

### Added
- `status` CLI: daemon health + per-index chunk counts
- `query` CLI: `POST /indexes/:id/search` with ranked output or `--json`
- `init` now calls `POST /indexes` on the daemon (fixes misleading "Registered" message)

---

## [0.1.32] — convert command

### Added
- `trusty-search convert project|all`: migrate indexes from mcp-vector-search by reading `.mcp-vector-search/config.json` files
  - `convert project`: git-style upward discovery from CWD
  - `convert all`: scans `~` at depth 6, skipping noise dirs
  - `--dry-run`: preview without contacting the daemon
  - `--concurrency`: bounds parallel migrations via `tokio::Semaphore`
  - Idempotent: existing indexes detected via daemon `{created: false}` response

---

## [0.1.31] — large codebase performance

### Performance
- `CodeIndexer::index_files_batch`: parses N files in parallel via rayon, embeds all chunks in 256-chunk ONNX batches, takes corpus write lock once per batch
- Incremental hash skip: files whose content hash matches the previous reindex are skipped; new SSE events: `"skip"`, `"batch"` (with `chunks_per_sec`), `"complete"` now carries skipped count
- `UsearchStore::with_capacity_hint`: tunes HNSW (connectivity=32, expansion_add=128, expansion_search=64) when expected chunk count > 50k
- `.gradle`/`.groovy`/`.kts`/`.mjs`/`.cjs` added to `SOURCE_EXTS`; Java/Gradle build dirs pruned from walker

---

## [0.1.30] — start/stop CLI

### Added
- `trusty-search start`: starts the HTTP daemon (replaces `daemon`)
- `trusty-search stop`: reads PID from fs4 lockfile, sends SIGTERM, polls up to 5s for port file to disappear

---

## [0.1.29] — reindex + SSE progress streaming

### Added
- `walker::walk_source_files`: walkdir-based, skips `.git`/`target`/`node_modules`/etc.
- `POST /indexes/:id/reindex`: spawns background reindex task with optional `{root_path}` body
- `GET /indexes/:id/reindex/stream`: SSE endpoint emitting `start`/`progress`/`complete`/`error` events with replay buffer for late subscribers
- `trusty-search reindex [PATH]` CLI: connects to SSE stream, renders live percentage/file progress
- `trusty-search add <PATH>`: walks directories and indexes every source file match
- `trusty-search remove <FILE>`: calls `/indexes/:id/remove-file`
- `trusty-search list`: calls `/indexes` and renders registry

---

## [0.1.28] — SCIP ingest interface

### Added
- SCIP ingest interface with `CodeEntityIndex` trait and `from_refs` constructor ([#24])

---

## [0.1.27] — ONNX NER gated

### Added
- ONNX NER for doc comment NLP entity extraction, gated by model file presence ([#23])

---

## [0.1.26] — ConceptCluster k-means

### Added
- `ConceptCluster` entities via fastembed + linfa k-means ([#22])

---

## [0.1.25] — complexity metrics

### Added
- Complexity and code quality metrics per chunk ([#32])

---

## [0.1.24] — search_similar

### Added
- Code-to-code similarity search and `search_similar` MCP tool ([#31])

---

## [0.1.23] — git blame integration

### Added
- Git blame integration per-chunk with temporal decay scoring ([#30])

---

## [0.1.22] — benchmark harness

### Added
- Benchmark harness: MRR@5 and Recall@10 evaluation ([#25])

---

## [0.1.21] — canonical facts table

### Added
- Canonical facts table with provenance tracking and HTTP query API ([#26])

---

## [0.1.20] — MMR diversity

### Added
- MMR (Maximal Marginal Relevance) diversity pass after RRF fusion ([#28])

---

## [0.1.19] — entity-match RRF lane

### Added
- Entity-match RRF lane for exact symbol name queries ([#20])

---

## [0.1.18] — KG rich edge types

### Added
- Knowledge Graph CALLS/IMPORTS/INHERITS/CONTAINS edges derived from chunk AST data ([#33])

---

## [0.1.17] — virtual_terms in BM25

### Added
- Populate `virtual_terms` from entities and append to BM25 documents for enriched lexical matching ([#19])

---

## [0.1.16] — intent-gated KG traversal

### Added
- Intent-gated KG traversal with `EdgeKind` score multipliers ([#18])

---

## [0.1.15] — EntityExtractor Phase A

### Added
- `EntityExtractor` Phase A: structural entities (functions, classes, imports) ([#17])

---

## [0.1.14] — CodeChunk extended fields

### Added
- Extend `CodeChunk` with `chunk_type`, `calls`, `inherits_from`, `complexity_score`, `chunk_depth` ([#29])

---

## [0.1.13] — BM25 three-pass tokenizer

### Added
- Three-pass BM25 tokenizer with camelCase and snake_case splitting ([#27])

---

## [0.1.12] — QueryClassifier entity keywords

### Added
- Extend `QueryClassifier` with entity-type keyword recognition ([#21])

---

## [0.1.11] — RawEntity + EdgeKind schema

### Added
- Canonical `RawEntity` schema and `EdgeKind` enum ([#16])

---

## [0.1.10] — CI + Dependabot

### Added
- GitHub Actions CI workflow and Dependabot config ([#9])

---

## [0.1.9] — daemon + graceful shutdown

### Added
- Daemon with PID lockfile (fs4), auto-port binding, graceful shutdown ([#8])

---

## [0.1.8] — MCP server

### Added
- MCP server with stdio and HTTP/SSE transport ([#7])

---

## [0.1.7] — FileWatcher

### Added
- `FileWatcher` with notify-debouncer-mini, 500ms debounce, fsevent backend ([#6])

---

## [0.1.6] — SymbolGraph KG expansion

### Added
- Build `SymbolGraph` from tree-sitter parse output; wire KG expansion (callers_of/callees_of) into the query pipeline ([#5])

---

## [0.1.5] — AST chunker + entity extraction

### Added
- Replace sliding-window chunker with tree-sitter AST-aware chunker ([#4])
- Initial `EntityExtractor` ([#17])

---

## [0.1.4] — search pipeline

### Added
- `CodeIndexer::search` end-to-end: HNSW + BM25 + RRF fusion ([#3])

---

## [0.1.3] — CLI redesign with auto-detection

### Added
- Project auto-detection and clean CLI help structure ([#14])

---

## [0.1.2] — UsearchStore HNSW wiring

### Added
- Wire `UsearchStore` to real usearch HNSW `Index` for add/search/remove ([#2])

---

## [0.1.1] — FastEmbedder implementation

### Added
- `FastEmbedder` with fastembed-rs + LRU cache ([#1])

---

## [0.1.0] — initial scaffold

### Added
- Workspace scaffold: `trusty-search-core`, `trusty-search-service`, `trusty-search-mcp`, CLI binary
- Query classifier (regex-based intent detection)
- BM25 lexical index (ported from open-mpm)
- `IndexRegistry` with `DashMap` + `Arc<RwLock<CodeIndexer>>`
- axum router skeleton

[Unreleased]: https://github.com/bobmatnyc/trusty-search/compare/v0.3.36...HEAD
[0.3.36]: https://github.com/bobmatnyc/trusty-search/compare/v0.3.35...v0.3.36
[0.3.35]: https://github.com/bobmatnyc/trusty-search/compare/v0.3.34...v0.3.35
[0.3.34]: https://github.com/bobmatnyc/trusty-search/compare/v0.3.33...v0.3.34
[0.3.33]: https://github.com/bobmatnyc/trusty-search/compare/v0.3.32...v0.3.33
[0.3.32]: https://github.com/bobmatnyc/trusty-search/compare/v0.3.31...v0.3.32
[0.3.31]: https://github.com/bobmatnyc/trusty-search/compare/v0.3.30...v0.3.31
[0.3.30]: https://github.com/bobmatnyc/trusty-search/compare/v0.3.29...v0.3.30
[0.3.29]: https://github.com/bobmatnyc/trusty-search/compare/v0.3.28...v0.3.29
[0.3.28]: https://github.com/bobmatnyc/trusty-search/compare/v0.3.27...v0.3.28
[0.3.27]: https://github.com/bobmatnyc/trusty-search/compare/v0.3.26...v0.3.27
[0.1.46]: https://github.com/bobmatnyc/trusty-search/compare/v0.1.45...v0.1.46
[0.1.45]: https://github.com/bobmatnyc/trusty-search/compare/v0.1.44...v0.1.45
[0.1.44]: https://github.com/bobmatnyc/trusty-search/compare/v0.1.43...v0.1.44
[0.1.43]: https://github.com/bobmatnyc/trusty-search/compare/v0.1.42...v0.1.43
[0.1.42]: https://github.com/bobmatnyc/trusty-search/compare/v0.1.41...v0.1.42
[0.1.41]: https://github.com/bobmatnyc/trusty-search/compare/v0.1.40...v0.1.41
[0.1.40]: https://github.com/bobmatnyc/trusty-search/compare/v0.1.39...v0.1.40
[0.1.39]: https://github.com/bobmatnyc/trusty-search/compare/v0.1.38...v0.1.39
[0.1.38]: https://github.com/bobmatnyc/trusty-search/compare/v0.1.37...v0.1.38
[0.1.37]: https://github.com/bobmatnyc/trusty-search/compare/v0.1.36...v0.1.37
[0.1.36]: https://github.com/bobmatnyc/trusty-search/compare/v0.1.35...v0.1.36
[0.1.35]: https://github.com/bobmatnyc/trusty-search/compare/v0.1.34...v0.1.35
[0.1.34]: https://github.com/bobmatnyc/trusty-search/compare/v0.1.33...v0.1.34
[0.1.33]: https://github.com/bobmatnyc/trusty-search/compare/v0.1.32...v0.1.33
[0.1.32]: https://github.com/bobmatnyc/trusty-search/compare/v0.1.31...v0.1.32
[0.1.31]: https://github.com/bobmatnyc/trusty-search/compare/v0.1.30...v0.1.31
[0.1.30]: https://github.com/bobmatnyc/trusty-search/compare/v0.1.29...v0.1.30
[0.1.29]: https://github.com/bobmatnyc/trusty-search/compare/v0.1.28...v0.1.29
[0.1.28]: https://github.com/bobmatnyc/trusty-search/compare/v0.1.27...v0.1.28
[0.1.27]: https://github.com/bobmatnyc/trusty-search/compare/v0.1.26...v0.1.27
[0.1.26]: https://github.com/bobmatnyc/trusty-search/compare/v0.1.25...v0.1.26
[0.1.25]: https://github.com/bobmatnyc/trusty-search/compare/v0.1.24...v0.1.25
[0.1.24]: https://github.com/bobmatnyc/trusty-search/compare/v0.1.23...v0.1.24
[0.1.23]: https://github.com/bobmatnyc/trusty-search/compare/v0.1.22...v0.1.23
[0.1.22]: https://github.com/bobmatnyc/trusty-search/compare/v0.1.21...v0.1.22
[0.1.21]: https://github.com/bobmatnyc/trusty-search/compare/v0.1.20...v0.1.21
[0.1.20]: https://github.com/bobmatnyc/trusty-search/compare/v0.1.19...v0.1.20
[0.1.19]: https://github.com/bobmatnyc/trusty-search/compare/v0.1.18...v0.1.19
[0.1.18]: https://github.com/bobmatnyc/trusty-search/compare/v0.1.17...v0.1.18
[0.1.17]: https://github.com/bobmatnyc/trusty-search/compare/v0.1.16...v0.1.17
[0.1.16]: https://github.com/bobmatnyc/trusty-search/compare/v0.1.15...v0.1.16
[0.1.15]: https://github.com/bobmatnyc/trusty-search/compare/v0.1.14...v0.1.15
[0.1.14]: https://github.com/bobmatnyc/trusty-search/compare/v0.1.13...v0.1.14
[0.1.13]: https://github.com/bobmatnyc/trusty-search/compare/v0.1.12...v0.1.13
[0.1.12]: https://github.com/bobmatnyc/trusty-search/compare/v0.1.11...v0.1.12
[0.1.11]: https://github.com/bobmatnyc/trusty-search/compare/v0.1.10...v0.1.11
[0.1.10]: https://github.com/bobmatnyc/trusty-search/compare/v0.1.9...v0.1.10
[0.1.9]: https://github.com/bobmatnyc/trusty-search/compare/v0.1.8...v0.1.9
[0.1.8]: https://github.com/bobmatnyc/trusty-search/compare/v0.1.7...v0.1.8
[0.1.7]: https://github.com/bobmatnyc/trusty-search/compare/v0.1.6...v0.1.7
[0.1.6]: https://github.com/bobmatnyc/trusty-search/compare/v0.1.5...v0.1.6
[0.1.5]: https://github.com/bobmatnyc/trusty-search/compare/v0.1.4...v0.1.5
[0.1.4]: https://github.com/bobmatnyc/trusty-search/compare/v0.1.3...v0.1.4
[0.1.3]: https://github.com/bobmatnyc/trusty-search/compare/v0.1.2...v0.1.3
[0.1.2]: https://github.com/bobmatnyc/trusty-search/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/bobmatnyc/trusty-search/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/bobmatnyc/trusty-search/releases/tag/v0.1.0
