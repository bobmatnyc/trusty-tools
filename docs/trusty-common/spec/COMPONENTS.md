# trusty-common — Component Specifications

> **Status:** Canonical · Living Document
> **Last reviewed:** 2026-05-29
> **Derived from:** code/docs/tickets audit

**Status legend:** ✅ Implemented · 🟡 Partial · 🔵 Designed-not-built · ⚪ Aspirational

One section per subsystem. Each states **Responsibility**, **Key types/modules**
(with `src/` paths), **Current state**, and **Known gaps**, framed Vision /
Current / Gap. For the feature-flag matrix see
[ARCHITECTURE.md §2](./ARCHITECTURE.md#2-the-feature-flag-model-the-load-bearing-decision);
for product framing see [PRD.md](./PRD.md).

---

## 1. Core utilities — `src/lib.rs` *(default)*

**Responsibility.** The always-on daemon-lifecycle primitives every trusty-*
binary shares: bind a port, find a data directory, record/discover the daemon
address, refuse to start a duplicate, initialise tracing, control colour.

**Key types/modules.**
- `bind_with_auto_port(addr, max_attempts)` — `src/lib.rs:283`.
- `resolve_data_dir(app_name)` + `DATA_DIR_OVERRIDE_ENV` — `src/lib.rs:323`, `:352`.
- `write_daemon_addr` / `read_daemon_addr` — `src/lib.rs:390`, `:406`.
- `probe_health` / `check_already_running` — `src/lib.rs:433`, `:470`.
- `maybe_disable_color`, `is_dir` — `src/lib.rs:589`, `:809`.
- `ChatMessage` + deprecated `openrouter_chat` / `openrouter_chat_stream` — `src/lib.rs:621`, `:665`, `:721`.

**Current state.** ✅ All functions implemented and unit-tested
(`auto_port_walks_forward`, `resolve_data_dir_creates_directory`,
`daemon_addr_round_trips`, `check_already_running_*`). Logs nothing to stdout;
returns `anyhow::Result` because every caller is a binary.

**Known gaps.**
- 🟡 `openrouter_chat` / `openrouter_chat_stream` are deprecated (0.3.1) — use
  `OpenRouterProvider` (§4). Retained for back-compat.
- ⚪ Windows data-dir paths are unverified in CI.

---

## 2. Tracing & log buffer — `src/lib.rs`, `src/log_buffer.rs` *(default)*

**Responsibility.** One verbosity ladder + `RUST_LOG` override for every binary,
always to stderr, plus an in-memory ring so daemons can serve `GET /logs/tail`.

**Key types/modules.**
- `init_tracing(verbose_count)` — `src/lib.rs:504` (`0→warn,1→info,2→debug,3+→trace`).
- `init_tracing_with_buffer(verbose, capacity)` — `src/lib.rs:538` (dual-layer registry).
- `LogBuffer` (capped `VecDeque<String>`) + `LogBufferLayer` — `src/log_buffer.rs`.

**Current state.** ✅ `try_init` makes both idempotent across test binaries. The
buffer layer filters via `RUST_LOG_BUFFER` (default `info`) independently of the
stderr filter, so lifecycle events reach `/logs/tail` even when stderr is at
`warn`.

**Known gaps.** None material.

---

## 3. System metrics — `src/sys_metrics.rs` *(default)*

**Responsibility.** Per-process RSS + CPU sampling for `/health`; directory byte
sizing.

**Key types/modules.** `SysMetrics` (wraps `sysinfo::System` scoped to current
PID), `SysMetrics::sample() -> (rss_mb, cpu_pct)`, `dir_size_bytes`.

**Current state.** ✅ CPU is delta-based, so the first sample reports `0.0` and
subsequent samples reflect usage since the prior call — designed for a ~2 s
`/health` poll without a background task. Tests: `sample_does_not_panic`,
`rss_is_plausible`.

**Known gaps.** None material.

---

## 4. Chat / providers — `src/chat.rs` *(default)*

**Responsibility.** Provider-agnostic streaming chat with OpenAI-style tool-use,
across cloud (OpenRouter) and local (Ollama/LM Studio) backends.

**Key types/modules.**
- `ChatProvider` trait (`chat_stream`).
- `OpenRouterProvider` / `OllamaProvider` — both speak OpenAI-compatible
  `/v1/chat/completions` SSE, including streamed `tool_calls`.
- `ChatEvent` (`Delta`/`Done`/`ToolCall`), `ToolDef`, `ToolCall`, `LocalModelConfig`.
- `auto_detect_local_provider` — probes `{base_url}/v1/models`, 1 s timeout.

**Current state.** ✅ Re-exported from the crate root (`src/lib.rs:259`). **API
key is a constructor argument** (`OpenRouterProvider::new(api_key, model)`) — the
library never reads `OPENROUTER_API_KEY`. Tests cover default config, the
unreachable-server path, SSE delta streaming, and tool-call fragment
accumulation.

**Known gaps.** None material; the deprecated free-function path lives in
`lib.rs` (§1).

---

## 5. MCP / JSON-RPC primitives — `src/mcp/` *(`mcp`)*

**Responsibility.** The shared JSON-RPC 2.0 / MCP envelope surface so every MCP
server in the workspace imports identical types.

**Key types/modules.** `Request`, `Response`, `JsonRpcError`, `error_codes`
(spec i32 constants), `initialize_response`, `run_stdio_loop` (accepts any
`Fn(Request) -> Future<Output=Response>`); `openrpc.rs` (`rpc.discover`),
`service.rs` (`ServiceDescriptor`). Formerly `trusty-mcp-core`.

**Current state.** ✅ Adds no deps beyond `serde`/`tokio`. Tests cover envelope
round-trips, error-code values, and the stdio loop with an in-memory dispatcher.

**Known gaps.** None material.

---

## 6. RPC client + transports — `src/rpc/` *(`rpc`)*

**Responsibility.** A general-purpose JSON-RPC 2.0 client with stdio-subprocess
and HTTP transports + pretty-printers (the library half of `trusty-rpc`).

**Key types/modules.** `RpcClient`, `new_id` (UUID-v4), `extract_result`;
`Transport` trait + `StdioTransport` + `HttpTransport`; `print_json`,
`print_server_info`, `print_tool_result`, `print_tools_list`.

**Current state.** ✅ Requires `uuid`; HTTP transport reuses workspace `reqwest`.
Per-submodule unit tests; integration tests live in the `trusty-rpc` crate.

**Known gaps.** None material.

---

## 7. Embedder — `src/embedder/` *(`embedder` + variants)*

**Responsibility.** One shared embedding abstraction so trusty-memory and
trusty-search stop shipping divergent `Embedder`/`FastEmbedder` copies.

**Key types/modules.**
- `Embedder` async trait (`embed_batch` primitive; single-text embed is a helper).
- `FastEmbedder` — fastembed-rs, all-MiniLM-L6-v2 (INT8, 384-d) with LRU cache +
  ORT warmup; fallback to full-precision when quantized unavailable.
- `MockEmbedder` (`embedder-test-support`) — deterministic test double.
- `candle_embedder::CandleEmbedder` (`embedder-candle`, #54) — Metal/CPU BERT.
- `rss.rs` — portable RSS measurement for the validation harness.

**Current state.** ✅ ORT linking variants: `embedder-bundled-ort` (static),
`embedder-cuda`, `embedder-load-dynamic`; `embedder-coreml` is a deprecated
no-op (CoreML auto-detected on Apple Silicon). CoreML safety hardened to require
a double opt-in (#85).

**Known gaps.**
- 🟡 ONNX-backed tests are `#[ignore]` (CI-fast); run with `--include-ignored`.
- ⚠️ ORT variants are mutually exclusive at the `ort-sys` level — pick one.

---

## 8. Embedder client — `src/embedder_client/` *(`embedder-client`)*

**Responsibility.** One client trait over every `trusty-embedderd` deployment
mode, so call sites are transport-agnostic (#110, #164).

**Key types/modules.** `EmbedderClient` trait; `InProcessEmbedderClient` (wraps
`FastEmbedder`), `RemoteEmbedderClient` (HTTP `POST /embed`), `UdsEmbedderClient`
(newline-framed JSON-RPC over UDS), `StdioEmbedderClient` (piped child process),
`EmbedderSupervisor` (auto-spawn lifecycle); `EmbedRequest`/`EmbedResponse` wire
types, `EmbedderError` (`thiserror`). Files: `mod.rs`, `in_process.rs`,
`remote.rs`, `uds.rs`, `stdio.rs`, `supervisor.rs`, `types.rs`, `error.rs`.

**Current state.** ✅ Consolidated the former `trusty-embedder-client` (HTTP, PR
#163) and `embed_client` (UDS, PR #157) into one module; the `embed-client`
feature/module are retired (#164). The `embedder` feature is **not** implied so
HTTP/UDS-only callers skip fastembed/ORT.

**Known gaps.** 🟡 Residual `embed_client`↔`embedder_client` naming reconciliation
(#164).

---

## 9. BM25 lexical index + client — `src/bm25.rs` *(`bm25`)*, `src/bm25_client.rs` *(`bm25-client`)*

**Responsibility.** A shared, zero-dependency BM25 index + code-aware tokenizer,
and a UDS client for the per-palace `trusty-bm25-daemon`.

**Key types/modules.** `tokenize` (camelCase/PascalCase/alpha↔digit splits),
`BM25Index` (incremental insert/update/remove, corpus cap); `Bm25Client`
(`index`/`search`/`delete` over a fresh `UnixStream` per call).

**Current state.** ✅ `bm25` is pure `std`+`tracing`, ported from open-mpm
(#156). `bm25_client` is pure tokio/serde; end-to-end coverage lives in
`trusty-bm25-daemon/tests/`. `rebuild` is intentionally not exposed on the
client (the dream subprocess calls it directly).

**Known gaps.** None material.

---

## 10. Memory-palace engine — `src/memory_core/` *(`memory-core` + sub-features)*

**Responsibility.** The complete Memory-Palace storage engine consumed by
`trusty-memory` — data model, layered stores, progressive retrieval, and the
dream/decay/analytics surfaces. Absorbed from `trusty-memory-core` (#5 phase 2d).

**Key types/modules.**
- **Model:** `Palace`/`Wing`/`Room`/`Drawer` + `PalaceId` (`palace.rs`),
  `PalaceRegistry` (`registry.rs`).
- **Retrieval:** `PalaceHandle` — 4-layer progressive retrieval L0→L3
  (`retrieval.rs`); process-wide shared `FastEmbedder` `OnceCell` (#57).
- **Stores (`store/`):** `palace_store` (redb rows), `vector`/`hnsw_store`
  (HNSW via hnsw_rs), `kg_redb`/`kg_store`/`kg_writer`/`kg` (KnowledgeGraph),
  `kg_sqlite` (legacy read, `sqlite-kg`), `payload_store` (postcard),
  `chat_sessions` (redb, #56), `l1_cache`, `kuzu` (`memory-core-kuzu`),
  `concurrent_open`.
- **Analytics:** `analytics` (RecallLog, redb #57), `decay`, `dream`,
  `community` (Louvain on petgraph, #52), `semantic_consolidation`
  (Mock/Ollama/OpenRouter inference, #87/#222), `git` (git2 history),
  `embed`, `filter` (content-quality gate, #222).

**Current state.** ✅ All stores migrated SQLite→redb (#43–#47, #56, #57). Auto-
enables `embedder` + `embedder-bundled-ort`.

**Known gaps.**
- 🟡 `sqlite-kg` (legacy SQLite read) and `usearch-migrate` (one-shot `.usearch`
  drain, #51) are transitional, slated for removal once all production palaces
  upgrade (#47).
- 🟡 The in-crate `palace.rs`/`retrieval.rs` doc comments still reference
  `cargo test -p trusty-memory-core` test paths (pre-absorption).

---

## 11. Symbol graph — `src/symgraph/` *(`symgraph` / `-parser` / `-server`)*

**Responsibility.** A tree-sitter symbol-graph engine with a pure-data contracts
surface separable from the parser (absorbed from `trusty-symgraph`, #5 phase 2c).

**Key types/modules.**
- **Contracts (`symgraph`):** `EntityType`, `RawEntity`, `EdgeKind`,
  `fact_hash_str`, tables — pure data, `thiserror`+`sha2`, **no** tree-sitter
  (`contracts.rs`).
- **Parser (`symgraph-parser`):** `SymbolGraph`, `SymbolRegistry`, parse →
  registry → emit + editor primitives, tree-sitter grammars for Rust/Python/JS/
  TS/Go/Java/C/C++ (`parser.rs`, `registry.rs`, `graph.rs`, `emitter.rs`,
  `editor.rs`, `symbol.rs`, `strategy.rs`, `locality.rs`).
- **Server (`symgraph-server`):** HTTP frontend (`server.rs`, implies `axum-server`).

**Current state.** ✅ The contracts/parser split lets non-parser consumers
(trusty-search, trusty-analyze) take `EntityType`/`RawEntity`/`EdgeKind` without
the tree-sitter `links` slot.

**Known gaps.** ⚠️ `symgraph-parser` claims `links = "tree-sitter"` — enable in
**at most one** crate per build graph (typically open-mpm).

---

## 12. Migration kernel — `src/migrations/` *(`migrations`)*

**Responsibility.** One ordered schema-migration runner replacing ad-hoc
"if schema_version < N" branches (#179).

**Key types/modules.** `SchemaVersion` (u32 newtype, `UNVERSIONED = 0`),
`Migration<S>` trait (`from_version`/`label`/`apply`), `MigrationRunner` (applies
pending steps in order, stamps after each); `file_stamp` (atomic JSON sidecar
`{ "schema_version": N }`), `redb_stamp` (documentation-only recipe so the
feature adds zero deps).

**Current state.** ✅ Pure `serde`/`anyhow`/`tracing` — no new deps. Tests cover
runner ordering, crash resumption, write-stamp failure propagation, and the
file-stamp round-trip.

**Known gaps.** None material; redb-backed stores follow the doc-only recipe
rather than a shared helper.

---

## 13. Ticketing MCP server — `src/tickets/` *(`tickets`)*

**Responsibility.** One MCP surface over GitHub Issues / JIRA / Linear (absorbed
from `trusty-tickets`).

**Key types/modules.** `api::config`, `api::models`, `api::client`, the
`Backend` trait + `backends::{github,jira,linear}`; `server` (MCP dispatch +
`run_stdio`); `tools` (tool-list schema). Driven by the `tickets-mcp` binary shim
(`src/bin/tickets_mcp.rs`). Requires `mcp`.

**Current state.** ✅ Module unit tests cover dispatch, tool-list counts, config
parsing, and serde round-trips.

**Known gaps.** 🟡 Live-backend tests require env-var credentials (not run in CI).

---

## 14. CLI help — `src/help.rs` *(`cli-help`)*

**Responsibility.** One declarative help model + renderer + "did you mean?" so
all six standalone binaries share a user-facing voice (#216).

**Key types/modules.** `HelpConfig`/`CommandDef`/`FlagDef`/`Example` (parsed from a
per-binary `help.yaml`, typically `include_str!`-bundled), `load_help`,
`render_help` (top-level + subcommand), `suggest` (Jaro-Winkler threshold). All
pure functions — no global state, no I/O. Deps: serde_yaml, strsim, indexmap.

**Current state.** ✅ Tests cover YAML parsing, top-level vs subcommand rendering,
and the suggester's positive-match / below-threshold-rejection behaviour.

**Known gaps.** None material.

---

## 15. Monitor TUIs — `src/monitor/` *(`monitor-tui`)*

**Responsibility.** Service-specific ratatui dashboards for trusty-search and
trusty-memory, exposed as a CLI subcommand (not a standalone binary).

**Key types/modules.** `search_tui::run` / `memory_tui::run` (the two ratatui
apps), `search_client` / `memory_client` (typed HTTP transports), `dashboard`
(shared wire structs + number formatters), `tui_common`, `utils` (activity log,
status enum, formatters). Deps: ratatui, crossterm, chrono.

**Current state.** ✅ Began as a unified two-panel dashboard (#31), split into two
dedicated TUIs for per-index/per-palace depth (#34), and re-homed as the
`monitor tui` subcommand of each daemon rather than a published binary (#32).
Tests cover the pure state, rendering, and client pieces.

**Known gaps.** None material.

---

## 16. Setup helpers — `src/claude_config.rs`, `src/project_discovery.rs`, `src/launchd.rs` *(default; launchd macOS-only)*

**Responsibility.** Shared "make this tool installable" plumbing: Claude Code
config patching, project discovery, and macOS launchd integration (#1, #2, #3,
#86, #132).

**Key types/modules.**
- `claude_config.rs` — scan `$HOME` for `.claude/settings*.json`
  (`SCAN_SKIP_DIRS`), idempotent MCP-server upsert (`mcp_server_entry`), atomic
  write + backup, `merge_hook_entries`.
- `project_discovery.rs` — `ClaudeProject` + `discover_claude_projects`
  (`.claude/`/`CLAUDE.md` markers, depth-3 default).
- `launchd.rs` — `LaunchdConfig` plist XML rendering, install under
  `~/Library/LaunchAgents`, `launchctl` bootstrap/bootout; `#[cfg(target_os =
  "macos")]`.

**Current state.** ✅ Replaced three divergent copies across the daemons. `#132`
fixed the launchd `serve`-forks-and-exits supervision bug. Filesystem-touching
tests are `#[ignore]`; pure logic (entry shape, hook idempotency, plist string,
skip-dir behaviour) is unit-tested.

**Known gaps.** ⚪ macOS-only — no Windows service integration.
