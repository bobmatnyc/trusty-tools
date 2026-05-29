# trusty-common — Architecture

> **Status:** Canonical · Living Document
> **Last reviewed:** 2026-05-29
> **Derived from:** code/docs/tickets audit

**Status legend:** ✅ Implemented · 🟡 Partial · 🔵 Designed-not-built · ⚪ Aspirational

This document describes *how trusty-common fits together*: the feature-flag model
that is its defining architectural decision, the cross-crate consumption
relationships, the design conventions every subsystem obeys, and the
source-module map. For product framing see [PRD.md](./PRD.md); for per-subsystem
detail see [COMPONENTS.md](./COMPONENTS.md).

---

## 1. The shape of the crate

trusty-common is a **single `rlib`** (`crate-type = ["rlib"]`, `src/lib.rs`)
plus two **feature-gated binary shims**:

- `tickets-mcp` (`src/bin/tickets_mcp.rs`, `required-features = ["tickets"]`)
- `candle_metal_bench` (`src/bin/candle_metal_bench.rs`, `required-features = ["embedder-candle"]`)

The library surface is a set of **independent, feature-gated subsystems**. The
crate root (`src/lib.rs`) carries only the always-on core (port-walk, data-dir,
daemon-addr, tracing, chat re-exports, misc helpers); every heavier subsystem is a
`#[cfg(feature = "…")] pub mod …`. There is **no global state** beyond the
idempotent tracing subscriber and the process-wide shared `FastEmbedder`
`OnceCell` inside `memory-core` (#57).

```
                       trusty-common (rlib, edition 2021, ELv2)
 ┌──────────────────────────────────────────────────────────────────────────┐
 │  ALWAYS-ON CORE (default = [])                                            │
 │   lib.rs        bind_with_auto_port · resolve_data_dir · daemon-addr ·    │
 │                 check_already_running · init_tracing · chat re-exports    │
 │   chat.rs       ChatProvider · OpenRouter · Ollama · tool-use             │
 │   log_buffer.rs · sys_metrics.rs · claude_config.rs · project_discovery   │
 │   launchd.rs (#[cfg(macos)])                                              │
 ├──────────────────────────────────────────────────────────────────────────┤
 │  OPT-IN SUBSYSTEMS (each behind one or more feature flags)                │
 │   axum-server → server.rs                                                 │
 │   mcp         → mcp/        rpc → rpc/        bm25 → bm25.rs               │
 │   embedder    → embedder/   embedder-client → embedder_client/            │
 │   bm25-client → bm25_client.rs        migrations → migrations/            │
 │   symgraph(-parser/-server) → symgraph/                                   │
 │   memory-core(-kuzu) / usearch-migrate / sqlite-kg → memory_core/         │
 │   tickets → tickets/        cli-help → help.rs    monitor-tui → monitor/  │
 └──────────────────────────────────────────────────────────────────────────┘
```

---

## 2. The feature-flag model (the load-bearing decision)

The crate's central architectural choice: **`default = []`**. A bare
`trusty-common` dependency compiles only the always-on core plus its light deps
(`tokio`, `serde`, `serde_json`, `reqwest`, `tracing`, `tracing-subscriber`,
`sysinfo`, `dirs`, `colored`, `futures-util`, `async-trait`, `anyhow`). Every
heavy transitive dependency — ONNX Runtime, tree-sitter grammars, redb, usearch,
git2, axum, ratatui, candle — is reachable **only** through an explicit feature.

This is what made the issue-[#5](https://github.com/bobmatnyc/trusty-tools/issues/5)
consolidation of seven micro-crates safe: absorbing `trusty-embedder` into
trusty-common does not force a chat-only consumer to compile ONNX, because the
ONNX deps sit behind `embedder`. See
[decisions/0001-consolidate-library-micro-crates.md](../decisions/0001-consolidate-library-micro-crates.md).

### Feature → module → dependency matrix

| Feature | Enables module | Pulls in (key deps) | Implies | Former crate |
|---|---|---|---|---|
| *(default)* | core, `chat`, `log_buffer`, `sys_metrics`, `claude_config`, `project_discovery`, `launchd`(macOS) | tokio, serde, reqwest, tracing, sysinfo, dirs, colored | — | base `trusty-common` |
| `axum-server` | `server` | axum, tower-http | — | — |
| `mcp` | `mcp` | *(none beyond serde/tokio)* | — | `trusty-mcp-core` |
| `rpc` | `rpc` | uuid | — | `trusty-rpc` (lib half) |
| `embedder` | `embedder` | fastembed, lru, parking_lot, ort | — | `trusty-embedder` |
| `embedder-bundled-ort` | — | fastembed bundled ORT | `embedder` | — |
| `embedder-cuda` | — | ort/cuda, fastembed/cuda (load-dynamic) | `embedder` | — |
| `embedder-load-dynamic` | — | ort/load-dynamic | `embedder` | — |
| `embedder-coreml` | — | *(deprecated no-op alias)* | `embedder` | — |
| `embedder-test-support` | `MockEmbedder` (outside `cfg(test)`) | — | `embedder` | — |
| `embedder-candle` | `embedder::candle_embedder` | candle-core/-nn/-transformers, hf-hub, tokenizers | `embedder` | — (#54) |
| `embedder-client` | `embedder_client` | thiserror | `embedder` | `trusty-embedder-client` + `embed_client` |
| `bm25` | `bm25` | *(pure std)* | — | (from open-mpm) |
| `bm25-client` | `bm25_client` | *(pure tokio/serde)* | — | — (#156) |
| `migrations` | `migrations` | *(pure serde/anyhow)* | — | (from trusty-search, #179) |
| `symgraph` | `symgraph::contracts` | thiserror, sha2 | — | `trusty-symgraph` (contracts) |
| `symgraph-parser` | `symgraph` parser/emitter/editor | tree-sitter + 8 grammars, syn, petgraph, indexmap, similar, walkdir, genco | `symgraph` | `trusty-symgraph` (full) |
| `symgraph-server` | `symgraph::server` | (axum stack) | `symgraph-parser`, `axum-server` | — |
| `memory-core` | `memory_core` | hnsw_rs, redb, postcard, git2, dashmap, chrono, regex, petgraph, sha2, hex | `embedder`, `embedder-bundled-ort` | `trusty-memory-core` (#5 phase 2d) |
| `memory-core-kuzu` | kuzu read path | kuzu | `memory-core` | — |
| `usearch-migrate` | `.usearch` drain | usearch (FFI) | `memory-core` | — (#51) |
| `sqlite-kg` | legacy SQLite KG read | rusqlite, r2d2, r2d2_sqlite | `memory-core` | — (#47) |
| `monitor-tui` | `monitor` | ratatui, crossterm, chrono | — | `trusty-monitor-tui` (#31/#34) |
| `tickets` | `tickets` | chrono, uuid, base64, thiserror, toml | `mcp` | `trusty-tickets` |
| `cli-help` | `help` | serde_yaml, strsim, indexmap, thiserror | — | — (#216) |

### Mutual-exclusion & single-slot constraints

- **ORT linking variants are mutually exclusive at the `ort-sys` level.**
  `embedder-bundled-ort` (static, macOS/glibc ≥ 2.38) vs `embedder-cuda` /
  `embedder-load-dynamic` (dynamic, operator-supplied `libonnxruntime.so` via
  `ORT_DYLIB_PATH`). Pick one.
- **`symgraph-parser` claims `links = "tree-sitter"`.** A `links` slot is
  unique per build graph, so enable the parser in **at most one crate** per
  build (typically open-mpm). Other crates enable `symgraph` (contracts only) and
  resolve symbol types without the grammar slot.
- **`memory-core` auto-enables `embedder` + `embedder-bundled-ort`** so a palace
  always has a working `FastEmbedder`; a consumer can switch ORT variants by
  additionally enabling `embedder-load-dynamic` / `embedder-cuda` explicitly.

---

## 3. Cross-crate consumption

trusty-common is consumed via the workspace path dependency
(`trusty-common = { workspace = true }`); the workspace manifest declares the
path so every member resolves the in-tree source. The most architecturally
significant relationship is **`memory-core` ⇄ `trusty-memory`**:

```
 trusty-memory (MCP frontend, MIT) ── enables ──▶ trusty-common[memory-core]
   ├─ owns the MCP server, HTTP API, Svelte UI, BM25-daemon supervision
   └─ delegates ALL palace storage/retrieval/dream/decay to
      trusty_common::memory_core::{Palace, PalaceRegistry, PalaceHandle, store::*}

 trusty-memory-core (shim crate) ── re-exports ──▶ trusty-common[memory-core]
   (kept as a thin re-export so old import paths still resolve)
```

The memory-palace engine **lives in trusty-common**; `trusty-memory` is a frontend
over it, and the standalone `trusty-memory-core` crate is now a re-export shim
(issue #5 phase 2d). The same pattern holds for the other absorbed crates:
`trusty-search` consumes `embedder-client` + `symgraph` (contracts) + `mcp` +
`bm25` + `migrations`; `open-mpm` is the single holder of `symgraph-parser`
(the tree-sitter `links` slot); `trusty-embedderd` consumes `embedder` +
`embedder-client`.

---

## 4. Design conventions every subsystem obeys

These are enforced by the root `CLAUDE.md` and visible throughout the source:

1. **No unconditional axum.** The HTTP server stack (`server.rs`,
   `symgraph::server`) is reachable only through `axum-server`. A library
   consumer that does not serve HTTP must not transitively pull in axum + tower
   (cf. #226, #249 where downstream crates had to gate their own axum).

2. **API keys passed in, never read from env in the library.**
   `OpenRouterProvider::new(api_key, model)` takes the key as an argument
   (`src/chat.rs`, `src/lib.rs`). The library defines `OPENROUTER_URL` /
   `HTTP_REFERER` / `X_TITLE` constants and timeouts but reads **no** secret from
   the process environment — that is the binary's responsibility. The only env
   vars the library itself reads are operational, non-secret tunables:
   `RUST_LOG` / `RUST_LOG_BUFFER` (tracing), `NO_COLOR` / `TERM` (colour), and the
   **test-only** `TRUSTY_DATA_DIR_OVERRIDE`.

3. **Logs to stderr.** `init_tracing` / `init_tracing_with_buffer` install a
   stderr `fmt` layer with `try_init` (idempotent). stdout is reserved for MCP
   JSON-RPC framing — a stray `println!` would corrupt the protocol.

4. **`thiserror` for library errors, `anyhow` for binary glue.** Subsystems that
   are imported as a library API define structured error enums
   (`embedder_client::EmbedderError`, the symgraph error types). `anyhow::Result`
   appears in the binary shims (`tickets-mcp`, `candle_metal_bench`) and in the
   handful of daemon-glue free functions in `lib.rs` (`bind_with_auto_port`,
   `resolve_data_dir`, `write_daemon_addr`) where the caller is always a binary.

5. **No global state.** Free functions and small structs. The only permitted
   `OnceCell`/`Lazy` are the idempotent tracing subscriber and the process-wide
   shared `FastEmbedder` in `memory_core::retrieval` (#57), which exists
   specifically to *avoid* forking dozens of ~90 MB ONNX sessions.

6. **500-line file cap.** Subsystems that grow past the cap are split into a thin
   `mod.rs` + sibling files (e.g. `memory_core/store/` is 14 files;
   `embedder_client/` is 7; `tickets/api/backends/` is one file per backend).

---

## 5. Source-module map

| Path | Feature | Responsibility |
|---|---|---|
| `src/lib.rs` | default | Core utilities: port-walk, data-dir, daemon-addr, already-running guard, tracing init, colour, `ChatMessage`, deprecated OpenRouter free fns. |
| `src/chat.rs` | default | `ChatProvider` trait, OpenRouter + Ollama providers, tool-use types, local auto-detect. |
| `src/log_buffer.rs` | default | `LogBuffer` ring + `LogBufferLayer` tracing layer for `/logs/tail`. |
| `src/sys_metrics.rs` | default | `SysMetrics` RSS/CPU sampler + `dir_size_bytes`. |
| `src/claude_config.rs` | default | Claude Code settings discovery + idempotent MCP-server/hook upsert. |
| `src/project_discovery.rs` | default | `ClaudeProject` + `discover_claude_projects` walk. |
| `src/launchd.rs` | default (macOS) | `LaunchdConfig` plist generation + `launchctl` lifecycle. |
| `src/server.rs` | `axum-server` | `with_standard_middleware` (CORS/Trace/gzip) + `daemon_http_client`. |
| `src/mcp/` | `mcp` | JSON-RPC 2.0 envelopes, error codes, `initialize`, stdio loop, OpenRPC discover, `ServiceDescriptor`. |
| `src/rpc/` | `rpc` | `RpcClient`, `Transport` + stdio/HTTP transports, pretty-printers. |
| `src/embedder/` | `embedder`(+variants) | `Embedder` trait, `FastEmbedder`, `MockEmbedder`, Candle backend, RSS helper. |
| `src/embedder_client/` | `embedder-client` | `EmbedderClient` trait + InProcess/HTTP/UDS/stdio clients + `EmbedderSupervisor`. |
| `src/bm25.rs` | `bm25` | Code-aware tokenizer + incremental `BM25Index`. |
| `src/bm25_client.rs` | `bm25-client` | UDS JSON-RPC client for `trusty-bm25-daemon`. |
| `src/migrations/` | `migrations` | `SchemaVersion`, `Migration<S>`, `MigrationRunner`, `file_stamp`, `redb_stamp` (doc-only). |
| `src/symgraph/` | `symgraph`/`-parser`/`-server` | Contracts surface; tree-sitter parser → registry → emit; editor; HTTP server. |
| `src/memory_core/` | `memory-core`(+sub) | Palace hierarchy, registry, retrieval, stores (HNSW/redb-KG/payload/chat/L1), dream/decay/analytics/community/consolidation/git. |
| `src/tickets/` | `tickets` | `api::*` (config/models/Backend/GitHub/JIRA/Linear), MCP `server`, `tools` schema. |
| `src/help.rs` | `cli-help` | `HelpConfig` YAML model, `render_help`, Jaro-Winkler `suggest`. |
| `src/monitor/` | `monitor-tui` | `search_tui`/`memory_tui` ratatui apps + typed HTTP clients + shared dashboard/utils. |
| `src/bin/tickets_mcp.rs` | `tickets` | Binary shim → `tickets::server::run_stdio`. |
| `src/bin/candle_metal_bench.rs` | `embedder-candle` | Candle Metal RSS validation harness. |

---

## 6. Memory-core internal topology (`src/memory_core/`)

The heaviest subsystem warrants its own sub-map. The palace engine is a layered
store stack behind a single `PalaceHandle`:

```
 PalaceRegistry (registry.rs) ──owns──▶ many PalaceHandle (retrieval.rs)
   PalaceHandle = progressive retrieval (L0 identity → L1 essential →
                  L2 on-demand vector → L3 deep search)
     ├─ store/palace_store.rs  durable palace/wing/room/drawer rows (redb)
     ├─ store/vector.rs        VectorStore trait + UsearchStore (HNSW, hnsw_rs)
     ├─ store/hnsw_store.rs    pure-Rust HNSW graph
     ├─ store/kg*.rs           KnowledgeGraph: kg_redb (current) / kg_sqlite
     │                         (legacy read, sqlite-kg) / kg_writer / kg_store
     ├─ store/payload_store.rs large-payload sidecar (postcard)
     ├─ store/chat_sessions.rs chat-session log (redb, #56)
     ├─ store/l1_cache.rs      pre-cached L0/L1 layer
     ├─ store/kuzu.rs          optional kuzu read path (memory-core-kuzu)
     └─ store/concurrent_open.rs  multi-process open coordination
   Analytics surfaces (operate over the stores):
     analytics.rs (RecallLog, redb #57) · decay.rs · dream.rs ·
     community.rs (Louvain on petgraph, #52) · semantic_consolidation.rs
     (inference-backed dedup via Mock/Ollama/OpenRouter, #87) · git.rs
     (git2 history extraction) · embed.rs (palace-local Embedder shim) ·
     filter.rs (content-quality gate, #222)
```

All stores migrated from SQLite/rusqlite to **redb** (#43–#47, #56, #57); the
`sqlite-kg` and `usearch-migrate` features exist only to read legacy stores
during the one-shot upgrade and are slated for removal.

---

## 7. Build & verification

```bash
# Default (dependency-light) build — what a bare consumer gets
cargo check -p trusty-common

# A representative multi-feature build
cargo test -p trusty-common --features axum-server,mcp,rpc,symgraph

# Embedder (ONNX-backed tests are #[ignore] by default)
cargo test -p trusty-common --features embedder -- --include-ignored

# Memory-palace engine
cargo test -p trusty-common --features memory-core
```

Because every consumer build is atomic in the workspace, a change to a
trusty-common subsystem must be validated with `cargo check` (workspace-wide) and
`cargo test -p <consumer>` for each dependent crate before commit (root
`CLAUDE.md` cross-crate workflow).
