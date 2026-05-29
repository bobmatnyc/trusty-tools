# trusty-common — Product Requirements Document

> **Status:** Canonical · Living Document
> **Last reviewed:** 2026-05-29
> **Derived from:** code/docs/tickets audit

**Status legend:** ✅ Implemented · 🟡 Partial · 🔵 Designed-not-built · ⚪ Aspirational
Each requirement is framed **Vision / Current / Gap**.

---

## 1. Vision & Mission

### North-star vision

> **trusty-common is the single foundational library of the trusty-\* workspace.**
> Every other trusty-* tool links exactly one internal crate for its shared
> primitives — protocol envelopes, embeddings, the memory-palace engine, the
> symbol graph, chat, CLI help, monitoring — and pays **only** for the
> subsystems it opts into via feature flags. The default build is
> dependency-light; the heavy subsystems are invisible until enabled.

Where the trusty-* ecosystem once shipped eight separate published micro-crates
(`trusty-mcp-core`, `trusty-rpc`, `trusty-embedder`, `trusty-symgraph`,
`trusty-memory-core`, `trusty-tickets`, `trusty-monitor-tui`, plus the base
utilities), each with its own version, its own `[patch.crates-io]` dance, and its
own slow drift, trusty-common collapses them into **one heavily feature-gated
crate** (issue [#5](https://github.com/bobmatnyc/trusty-tools/issues/5)). The
defining properties are: **a single internal link target** (one `cargo` graph,
one version to bump for shared code), **pay-for-what-you-use feature gating** (a
`lexical_only` consumer never compiles ONNX; a chat-only consumer never compiles
tree-sitter), and **strict library discipline** — pure functions, no global
state, `thiserror` errors, logs to stderr.

### Mission

Give every trusty-* binary one import surface for the primitives they share, with
no behavioural drift between consumers, no duplicated bug fixes, and no
dependency a consumer did not ask for. Keep the crate publishable to crates.io
(Elastic-2.0) so external consumers can adopt individual subsystems.

### Why this matters

The pre-consolidation world had the *same* `Request`/`Response` MCP envelope, the
*same* `Embedder` trait, and the *same* launchd plist generator copy-pasted
across trusty-memory and trusty-search with subtle divergence (cache vs no-cache
embedders, drifting JSON-RPC error-code types, three different ways to read the
user's UID). Centralising fixes one bug in one place. Feature-gating keeps the
centralisation from forcing every consumer to carry every dependency — the design
tension that originally justified separate crates.

---

## 2. Goals & Non-Goals

### Goals

| # | Goal | Status |
|---|---|---|
| G1 | **Single internal shared library** — one crate every trusty-* tool links, replacing seven standalone micro-crates (#5). | ✅ |
| G2 | **Pay-for-what-you-use feature gating** — every heavy subsystem behind an opt-in feature; default build is `tokio`/`serde`/`reqwest`/`tracing`/`sysinfo`/`dirs` only. | ✅ |
| G3 | **Shared daemon-lifecycle primitives** — port auto-walk, data-dir resolution, daemon addr file, already-running guard, tracing init. | ✅ |
| G4 | **Provider-agnostic streaming chat** — `ChatProvider` trait + OpenRouter + Ollama + local auto-detect + tool-use. | ✅ |
| G5 | **Shared MCP / JSON-RPC primitives** — `Request`/`Response`/error codes/`initialize`/stdio loop/OpenRPC discover (`mcp` feature). | ✅ |
| G6 | **Shared embedding abstraction** — `Embedder` trait + `FastEmbedder` (all-MiniLM-L6-v2, 384-d) + `MockEmbedder` + Candle backend (`embedder` features). | ✅ |
| G7 | **Memory-palace storage engine** — palace hierarchy + HNSW vector store + redb KG + retrieval + dream/decay/analytics, consumed by `trusty-memory` (`memory-core`). | ✅ |
| G8 | **Symbol-graph engine** — tree-sitter parse → registry → emit, with a pure-data contracts surface separable from the parser (`symgraph` / `symgraph-parser`). | ✅ |
| G9 | **Reusable schema-migration kernel** — `SchemaVersion`/`Migration`/`MigrationRunner` + file-stamp helpers (#179, `migrations`). | ✅ |
| G10 | **Unified ticketing MCP server** — GitHub/JIRA/Linear backends behind one MCP surface (`tickets`). | ✅ |
| G11 | **Declarative CLI help system** — YAML-driven `HelpConfig` + canonical renderer + Jaro-Winkler "did you mean?" (#216, `cli-help`). | ✅ |
| G12 | **Service-specific monitor TUIs** — ratatui dashboards for trusty-search and trusty-memory (#31/#34, `monitor-tui`). | ✅ |
| G13 | **Unified embedder-client surface** — one `EmbedderClient` trait over in-process / HTTP / UDS / stdio transports for `trusty-embedderd` (#110/#164, `embedder-client`). | ✅ |
| G14 | **Shared setup helpers** — Claude-config discovery/patching, project discovery, macOS launchd plist generation (#1/#2/#3/#86). | ✅ |
| G15 | **Publishable to crates.io** — external consumers can adopt individual subsystems via features. | ✅ |

### Non-Goals

| Non-Goal | Rationale |
|---|---|
| **Unconditional `axum` / `tower-http` dependency** | The HTTP server stack is gated behind the `axum-server` feature. A library consumer that does not serve HTTP must not pull in the full axum + tower stack (root `CLAUDE.md` hard rule; cf. #226, #249 where downstream crates had to gate axum). |
| **Reading secrets from the environment inside the library** | Chat helpers (`OpenRouterProvider`) take the API key as an argument; the library **never** reads `OPENROUTER_API_KEY` itself. Env-reading is the binary's job. |
| **Global mutable state** | All helpers are free functions or small structs. The only `Lazy`/`OnceCell` permitted is the idempotent tracing subscriber (`try_init`) and the process-wide shared `FastEmbedder` `OnceCell` inside `memory-core` (#57). |
| **`anyhow` in library-facing error surfaces** | Library subsystems define `thiserror` enums (`EmbedderError`, symgraph errors, etc.). `anyhow::Result` is used only in the binary shims (`tickets-mcp`, `candle_metal_bench`) and the few daemon-glue free functions. |
| **A binary deliverable** | trusty-common is a library (`crate-type = ["rlib"]`). The two `[[bin]]` targets (`tickets-mcp`, `candle_metal_bench`) are feature-gated thin shims, not the product. |
| **Multiple tree-sitter `links` slots** | `symgraph-parser` claims the `links = "tree-sitter"` native slot; enable it in at most **one** crate per build graph. |
| **Windows-specific daemon integration** | `launchd` is macOS-only (`#[cfg(target_os = "macos")]`); Windows service integration is not provided. |

---

## 3. Target Users / Personas

trusty-common's users are **other crates**, not end users. Its API surface is
designed for the consuming binaries below.

| Persona (consumer crate) | What it links | Features it enables |
|---|---|---|
| **trusty-search** | embedder-client, symgraph contracts, MCP, RPC, BM25, migrations, server, monitor, setup helpers | `mcp`, `rpc`, `embedder-client`, `bm25`, `symgraph`, `migrations`, `axum-server`, `monitor-tui`, `cli-help` |
| **trusty-memory** | the entire memory-palace engine, BM25-client, embedder, monitor, setup helpers | `memory-core`, `bm25-client`, `mcp`, `axum-server`, `monitor-tui`, `cli-help` |
| **open-mpm** | the full symbol-graph parser, chat, MCP | `symgraph-parser`, `mcp` (the single tree-sitter `links` holder) |
| **trusty-analyze** | symgraph contracts, MCP, setup helpers | `symgraph`, `mcp`, `cli-help`, `axum-server` |
| **trusty-embedderd** | the embedder + embedder-client wire types | `embedder`, `embedder-client` + an ORT variant |
| **trusty-mpm / tga / cto-assistant** | chat, MCP, CLI help, utilities | `mcp`, `cli-help`, chat (default) |
| **External crates.io consumer** | any single subsystem | whatever subset they opt into |

**Unifying need across all consumers:** import the shared primitive, get the
same behaviour every other consumer gets, and compile **only** the dependencies
that primitive actually needs.

---

## 4. Functional Requirements

Grouped by subsystem. Each requirement carries Vision / Current / Gap and an
inline status tag. Source paths are cited where known. Feature flags are noted in
parentheses.

### 4.1 Core utilities (default features — `src/lib.rs`)

**FR-CORE-1 — Port auto-walk** ✅
- *Vision:* daemon restarts and concurrent instances should not fail noisily on a
  busy port.
- *Current:* `bind_with_auto_port(addr, max_attempts)` walks forward through
  ports on `AddrInUse`, returning the first listener that binds
  (`src/lib.rs:283`). `max_attempts == 0` tries the requested port exactly once.
- *Gap:* none material.

**FR-CORE-2 — OS-standard data directory** ✅
- *Vision:* one per-app, per-machine data dir resolved identically across tools
  and platforms.
- *Current:* `resolve_data_dir(app_name)` resolves `~/Library/Application
  Support/<app>` (macOS), `~/.local/share/<app>` (Linux), with a `~/.<app>`
  fallback, creating the directory (`src/lib.rs:352`). The
  `TRUSTY_DATA_DIR_OVERRIDE` env var is a documented **test-only** escape hatch,
  needed because macOS's `dirs::data_dir()` bypasses `HOME`/`XDG_DATA_HOME` via
  `NSFileManager`.
- *Gap:* Windows paths are unverified in CI.

**FR-CORE-3 — Daemon address file + already-running guard** ✅
- *Vision:* MCP clients and follow-up CLI calls must discover where an
  auto-port-walked daemon landed, and a second `start` must refuse to spawn a
  duplicate.
- *Current:* `write_daemon_addr` / `read_daemon_addr` persist `host:port` to
  `<data_dir>/http_addr`; `probe_health` + `check_already_running` probe the
  recorded address for a 2xx `/health` within ~1.5 s and best-effort delete a
  stale file (`src/lib.rs:390`–`490`).
- *Gap:* none material.

**FR-CORE-4 — Colour control + dir shim** ✅
- *Current:* `maybe_disable_color` honours `NO_COLOR`/`TERM=dumb`/`--no-color`;
  `is_dir` is a clarity shim (`src/lib.rs:589`, `:809`).

### 4.2 Tracing & logging (default — `src/lib.rs`, `src/log_buffer.rs`)

**FR-LOG-1 — Verbosity-laddered tracing to stderr** ✅
- *Vision:* every binary shares one verbosity ladder and `RUST_LOG` override, and
  logs **never** corrupt MCP stdout framing.
- *Current:* `init_tracing(verbose_count)` maps `0→warn,1→info,2→debug,3+→trace`,
  honours `RUST_LOG`, writes to stderr, and uses `try_init` so it is idempotent
  across test binaries (`src/lib.rs:504`).
- *Gap:* none.

**FR-LOG-2 — In-memory log ring for `/logs/tail`** ✅
- *Current:* `init_tracing_with_buffer` adds a `LogBufferLayer` feeding a capped
  `LogBuffer` (`VecDeque<String>`), filtered independently via `RUST_LOG_BUFFER`
  (default `info`) so lifecycle events reach the buffer even at stderr `warn`
  (`src/lib.rs:538`, `src/log_buffer.rs`).

**FR-LOG-3 — Process RSS/CPU sampling** ✅
- *Current:* `SysMetrics` (sysinfo, scoped to current PID) returns `(rss_mb,
  cpu_pct)` for `/health`; first sample reports `0.0` CPU (delta-based)
  (`src/sys_metrics.rs`).

### 4.3 Chat / OpenRouter (default — `src/chat.rs`, `src/lib.rs`)

**FR-CHAT-1 — Provider-agnostic streaming chat with tool-use** ✅
- *Vision:* support cloud (OpenRouter) and local (Ollama/LM Studio) LLMs behind
  one trait, including OpenAI-style function calling.
- *Current:* `ChatProvider` trait + `OpenRouterProvider` + `OllamaProvider` (both
  speak OpenAI-compatible `/v1/chat/completions` SSE), `ChatEvent` /
  `ToolDef` / `ToolCall` tool-use types, and `auto_detect_local_provider`
  (probes `/v1/models` with a 1 s timeout) (`src/chat.rs`).
- *API-key contract:* the key is passed to `OpenRouterProvider::new`; the library
  never reads it from the environment.
- *Gap:* the legacy free functions `openrouter_chat` / `openrouter_chat_stream`
  are **deprecated** (since 0.3.1) in favour of `OpenRouterProvider::chat_stream`
  (`src/lib.rs:664`, `:720`) — 🟡 retained for back-compat.

### 4.4 MCP / JSON-RPC primitives (`mcp` — `src/mcp/`)

**FR-MCP-1 — Shared JSON-RPC 2.0 / MCP envelopes** ✅
- *Current:* `Request` / `Response` / `JsonRpcError` envelopes, `error_codes`
  (spec-canonical i32 constants), `initialize_response`, an async
  `run_stdio_loop`, and OpenRPC `rpc.discover` helpers (`src/mcp/mod.rs`,
  `openrpc.rs`, `service.rs`). Pulls in no deps beyond `serde`/`tokio`.

### 4.5 RPC client (`rpc` — `src/rpc/`)

**FR-RPC-1 — General-purpose JSON-RPC client + transports** ✅
- *Current:* `RpcClient` + `Transport` trait + `StdioTransport` (subprocess) +
  `HttpTransport`, `new_id` (UUID-v4), `extract_result`, and pretty-printers
  (`print_json`, `print_tools_list`, …) (`src/rpc/`). Requires `uuid`.

### 4.6 Embedder (`embedder` + variants — `src/embedder/`)

**FR-EMB-1 — Shared embedding abstraction** ✅
- *Vision:* one `Embedder` trait + production `FastEmbedder` so trusty-memory and
  trusty-search stop shipping divergent copies.
- *Current:* async `Embedder` trait (`embed_batch` primitive), `FastEmbedder`
  (fastembed-rs, all-MiniLM-L6-v2 INT8, 384-d) with LRU cache + ORT warmup,
  `MockEmbedder` (`embedder-test-support`), and a Candle Metal/CPU backend
  (`embedder-candle`, #54) (`src/embedder/mod.rs`, `candle_embedder.rs`,
  `rss.rs`).
- *ORT variants:* `embedder-bundled-ort` (macOS/modern Linux),
  `embedder-cuda`, `embedder-load-dynamic` (AL2023/glibc < 2.38);
  `embedder-coreml` is a deprecated no-op (CoreML is auto-detected).
- *Gap:* ONNX-backed tests are `#[ignore]` (CI-fast); CoreML safety required a
  double opt-in hardening (#85).

**FR-EMB-2 — Unified embedder-client transports** ✅
- *Vision:* one client trait over every `trusty-embedderd` deployment mode.
- *Current:* `embedder-client` exposes `EmbedderClient` + `InProcessEmbedderClient`,
  `RemoteEmbedderClient` (HTTP), `UdsEmbedderClient` (UDS), `StdioEmbedderClient`,
  and `EmbedderSupervisor` (auto-spawn lifecycle) (`src/embedder_client/`).
  Consolidated the former `trusty-embedder-client` + `embed_client` (#110, #164).

### 4.7 BM25 lexical (`bm25` / `bm25-client` — `src/bm25.rs`, `src/bm25_client.rs`)

**FR-BM25-1 — Zero-dependency BM25 index + code-aware tokenizer** ✅
- *Current:* `tokenize` (camelCase/PascalCase/alpha↔digit splits) + incremental
  `BM25Index` keyed by opaque string ids, pure `std`+`tracing`, ported from
  open-mpm (#156) (`src/bm25.rs`).

**FR-BM25-2 — UDS client for per-palace BM25 daemon** ✅
- *Current:* `Bm25Client` opens a fresh `UnixStream` per call, newline-framed
  JSON-RPC (`index`/`search`/`delete`); end-to-end coverage in
  `trusty-bm25-daemon` (`src/bm25_client.rs`).

### 4.8 Memory-palace engine (`memory-core` — `src/memory_core/`)

**FR-MEM-1 — Palace data model + storage + retrieval** ✅
- *Vision:* the entire Memory-Palace storage engine, consumed by `trusty-memory`,
  living behind one feature so chat/MCP-only consumers pay nothing (#5 phase 2d).
- *Current:* the 5-level hierarchy (`Palace`/`Wing`/`Room`/`Drawer`,
  `src/memory_core/palace.rs`), `PalaceRegistry`, `PalaceHandle` 4-layer
  progressive retrieval (L0 identity → L1 essential → L2 vector → L3 deep,
  `retrieval.rs`), the storage backends (HNSW vector store, redb KG, payload
  store, chat-session store, L1 cache — `store/`), and dream/decay/analytics/
  community/semantic-consolidation/git-history surfaces. Storage migrated off
  SQLite to redb (#43–#47, #56, #57); a process-wide shared `FastEmbedder`
  `OnceCell` (#57) avoids forking model instances.
- *Sub-features:* `memory-core-kuzu` (read-only kuzu graph), `usearch-migrate`
  (one-shot `.usearch` drain, #51), `sqlite-kg` (legacy SQLite read path for
  migration, #47).
- *Gap:* `sqlite-kg` / `usearch-migrate` are 🟡 transitional and slated for
  removal once all production palaces are upgraded.

### 4.9 Symbol graph (`symgraph` / `symgraph-parser` / `symgraph-server` — `src/symgraph/`)

**FR-SYM-1 — Pure-data contracts surface** ✅
- *Current:* `symgraph` exposes only `EntityType`/`RawEntity`/`EdgeKind`/
  `fact_hash_str`/tables — no tree-sitter, no `links` conflict
  (`src/symgraph/contracts.rs`).

**FR-SYM-2 — Full tree-sitter parser + emitter + editor** ✅
- *Current:* `symgraph-parser` adds `SymbolGraph`/`SymbolRegistry`/parse → registry
  → emit + editor primitives, tree-sitter grammars for Rust/Python/JS/TS/Go/
  Java/C/C++; `symgraph-server` adds the HTTP frontend (implies `axum-server`).
- *Constraint:* claims `links = "tree-sitter"` — enable in at most one crate per
  build graph (typically open-mpm).

### 4.10 Migrations (`migrations` — `src/migrations/`)

**FR-MIG-1 — Reusable schema-migration kernel** ✅
- *Vision:* replace the ad-hoc "if schema_version < N" branches across
  trusty-search/trusty-memory with one ordered runner (#179).
- *Current:* `SchemaVersion` (u32, `UNVERSIONED = 0`), `Migration<S>` trait,
  `MigrationRunner` (applies pending steps in order, stamps after each),
  `file_stamp` (atomic JSON sidecar), and `redb_stamp` (documentation-only recipe
  so the feature adds zero deps) (`src/migrations/`).

### 4.11 Tickets (`tickets` — `src/tickets/`)

**FR-TKT-1 — Unified ticketing MCP server** ✅
- *Current:* `tickets::api::*` (config, models, `Backend` trait + GitHub/JIRA/
  Linear backends), `tickets::server` (MCP dispatch + `run_stdio`),
  `tickets::tools` (tool-list schema); the `tickets-mcp` binary shim drives it
  (requires `mcp`) (`src/tickets/`, `src/bin/tickets_mcp.rs`).
- *Gap:* live-backend tests require env-var credentials.

### 4.12 CLI help (`cli-help` — `src/help.rs`)

**FR-HELP-1 — Declarative help + "did you mean?"** ✅
- *Current:* `HelpConfig`/`CommandDef`/`FlagDef`/`Example` parsed from a per-binary
  `help.yaml` (typically `include_str!`-bundled), `render_help`, and a
  Jaro-Winkler `suggest` for mistyped subcommands; all pure functions (#216)
  (`src/help.rs`).

### 4.13 Monitor TUI (`monitor-tui` — `src/monitor/`)

**FR-MON-1 — Service-specific TUIs** ✅
- *Current:* `search_tui` + `memory_tui` (ratatui apps), `search_client` +
  `memory_client` (typed HTTP transports), shared `dashboard`/`utils`. Began as a
  unified dashboard (#31), split into two dedicated TUIs (#34), exposed as a CLI
  subcommand rather than a standalone binary (#32) (`src/monitor/`).

### 4.14 Setup helpers (default + macOS — `src/claude_config.rs`, `src/project_discovery.rs`, `src/launchd.rs`)

**FR-SETUP-1 — Claude-config discovery + idempotent patching** ✅
- *Current:* scan `$HOME` for `.claude/settings*.json`, idempotent MCP-server
  upsert, atomic write + backup, hook merging (#1, #2, #3, #86)
  (`src/claude_config.rs`).

**FR-SETUP-2 — Claude project discovery** ✅
- *Current:* `ClaudeProject` + `discover_claude_projects` walk
  (`.claude/`/`CLAUDE.md` markers, depth-3 default) (`src/project_discovery.rs`).

**FR-SETUP-3 — macOS launchd LaunchAgent** ✅
- *Current:* `LaunchdConfig` renders plist XML, installs under
  `~/Library/LaunchAgents`, bootstraps/bootouts via `launchctl`; `#[cfg(macos)]`
  only (#132 fixed the `serve`-forks-and-exits supervision bug)
  (`src/launchd.rs`).

---

## 5. Success Criteria & Differentiators

| Criterion | Target | Status |
|---|---|---|
| **Default build is dependency-light** | Only `tokio`/`serde`/`reqwest`/`tracing`/`sysinfo`/`dirs`/`colored`/`futures-util` on `default = []`. | ✅ |
| **No unconditional axum** | `cargo tree -p trusty-common` (default) shows no `axum`. | ✅ |
| **One version to bump for shared code** | Single `version` field (0.8.0); consumers update one pin. | ✅ |
| **Behavioural parity across consumers** | Both daemons share one `Embedder`, one MCP envelope, one launchd generator. | ✅ |
| **Feature isolation compiles** | `cargo check -p trusty-common` (default) passes; each feature builds independently. | ✅ |
| **Publishable** | Released to crates.io under Elastic-2.0; external single-subsystem adoption supported. | ✅ |

**Differentiators vs. a generic "utils" crate:** trusty-common is not a grab-bag —
it is a *consolidation* of formerly-published crates behind a disciplined
feature-flag matrix, so the consolidation never forces a consumer to carry an
unwanted dependency. The same crate ships a chat client, an ONNX embedder, a
tree-sitter symbol graph, a redb memory engine, and a ratatui TUI without any one
consumer paying for all of them.

---

## 6. Open Questions / Roadmap

| # | Question | Notes |
|---|---|---|
| OQ-1 | When can `sqlite-kg` / `usearch-migrate` be removed? | Once all production palaces have drained legacy SQLite/`.usearch` files (#47, #51). 🟡 |
| OQ-2 | Reconcile `embed_client` vs `embedder_client` naming fully? | #164 retired `embed-client`; residual naming reconciliation tracked there. 🟡 |
| OQ-3 | Stale crate README / version pins | `crates/trusty-common/README.md` still says v0.3 and omits five features; #430 tracks the broader inventory reconciliation. 🟡 |
| OQ-4 | Windows daemon support | `launchd` is macOS-only; no Windows service path. ⚪ |
| OQ-5 | Remove deprecated `openrouter_chat*` free functions | Deprecated since 0.3.1 in favour of `OpenRouterProvider`; a future major can drop them. 🔵 |
