# trusty-memory — System Architecture

> **Status:** Canonical · Living Document
> **Last reviewed:** 2026-06-01
> **Derived from:** code/docs/tickets audit (v0.14.0)

**Status legend:** ✅ Implemented · 🟡 Partial · 🔵 Designed-not-built · ⚪ Aspirational

This document describes how trusty-memory fits together: the **frontend/core
split**, the multi-transport model, the bundled BM25 sidecar, the fire-and-forget
remember path, the storage model, the progressive-retrieval layering, and the
source-module map.

Module paths under `memory_core/` refer to
`crates/trusty-common/src/memory_core/`; all other paths refer to
`crates/trusty-memory/src/`.

---

## 1. Frontend / Core Split (the defining structural fact)

trusty-memory is split into a **frontend** (this crate) and a **storage core**
(absorbed into `trusty-common`). This is the single most important thing to
understand about the codebase — see [ADR-0001](../decisions/0001-frontend-core-split.md).

```
┌──────────────────────────────────────────────────────────────────────┐
│  trusty-memory  (this crate — the FRONTEND)                            │
│  ─────────────────────────────────────────────────────────────────    │
│  • MCP tool surface          tools.rs, mcp_service.rs, openrpc.rs      │
│  • HTTP/SSE + REST + /rpc     web.rs, chat.rs, transport/rpc.rs        │
│  • UDS transport              transport/uds.rs                          │
│  • Embedded Svelte UI         web.rs (rust-embed of ui/dist/)          │
│  • CLI commands               main.rs, commands/*                       │
│  • BM25 sidecar supervisor    bm25_supervisor.rs                        │
│  • KG auto-extract / bootstrap kg_extract.rs, bootstrap.rs, discovery.rs│
│  • Messaging / activity / logs messaging.rs, activity.rs, prompt_log.rs │
└───────────────────────────────┬────────────────────────────────────────┘
                                 │  calls (memory-core feature)
                                 ▼
┌──────────────────────────────────────────────────────────────────────┐
│  trusty-common :: memory_core   (the CORE — storage + retrieval)       │
│  ─────────────────────────────────────────────────────────────────    │
│  • Palace data model          palace.rs (Palace→Wing→Room→Closet→Drawer)│
│  • Concurrent registry         registry.rs (DashMap<PalaceId, Arc<…>>)  │
│  • 4-layer retrieval (L0–L3)   retrieval.rs (PalaceHandle)              │
│  • Vector store (HNSW/redb)    store/hnsw_store.rs, store/vector.rs     │
│  • Temporal KG (redb)          store/kg.rs, kg_redb.rs (+ kg_sqlite)    │
│  • Dream + semantic consolid.  dream.rs, semantic_consolidation.rs      │
│  • Filter / decay / community  filter.rs, decay.rs, community.rs        │
│  • Embeddings (re-export)      embed.rs → trusty-common::embedder       │
└──────────────────────────────────────────────────────────────────────┘
```

- **The historical `trusty-memory-core` crate no longer exists.** It was fully
  **absorbed into `trusty-common`** as the `memory_core` module (issue #5, phase
  2d) so the toolchain links one internal library and ships one fewer published
  crate. The module-level doc says so explicitly (`memory_core/mod.rs`). ✅
  > Note: the *workspace-root* `CLAUDE.md` still lists `crates/trusty-memory-core/`
  > as a shim crate; that is stale — the worktree's `CLAUDE.md` correctly says
  > "re-export shim — absorbed into trusty-common's memory-core feature," and the
  > directory is gone from disk.
- **The core is feature-gated.** `memory_core` is compiled only with
  `trusty-common`'s `memory-core` feature, which pulls in the heavy storage deps
  (`redb`, HNSW, `tiktoken-rs`, `git2`); consumers that don't need storage skip it.
- **This crate enables it** via
  `trusty-common = { …, features = ["mcp", "memory-core", "monitor-tui", "bm25-client", "cli-help"] }`.
- **Two consumption modes.** open-mpm links this crate as an **rlib** with
  `default-features = false` to get `MemoryMcpService` *without* the axum/HTTP
  surface (#226); the standalone daemon keeps `axum-server` on (the default). See
  `docs/trusty-memory/research/axum-feature-flag-decision-2026-05-26.md`.

---

## 2. Process & Transport Model

trusty-memory runs as **one long-lived daemon per host** that owns the redb write
locks. Clients reach it over three transports, all converging on the same
`AppState` (shared `PalaceRegistry` + data root + lazily-initialized embedder).

```
Claude Code (stdio MCP)                  curl / browser / open-mpm-HTTP
        │                                          │
        ▼                                          ▼
trusty-memory-mcp-bridge          ┌──────────── HTTP/SSE (axum) ────────────┐
  (src/bin/mcp_bridge.rs)         │  /api/v1/*  •  /sse  •  /rpc  •  /health │
  tokio::io::copy_bidirectional   │  + embedded Svelte UI (rust-embed)       │
        │  (byte pipe, NO redb)   └──────────────────────────────────────────┘
        ▼                                          │
   Unix domain socket  ─────────────────────────►  │
   transport/uds.rs                                ▼
                                          ┌──────────────────────┐
                                          │   AppState            │
                                          │   PalaceRegistry      │
                                          │   data_root           │
                                          │   embedder (lazy)     │
                                          │   Bm25Supervisor      │
                                          │   ActivityLog         │
                                          └──────────┬───────────┘
                                                     ▼
                                       trusty-common :: memory_core
                                       (redb + HNSW + KG + dream)
```

### Key properties

- **One daemon, three transports.** The same process serves: (a) MCP JSON-RPC over
  a Unix domain socket (`transport/uds.rs`), (b) the full HTTP/SSE + REST surface
  plus the embedded UI (`web.rs`), and (c) a `POST /rpc` JSON-RPC dispatch
  (`transport/rpc.rs`). The dispatch layer (`transport::dispatch`) is
  transport-agnostic so future transports (gRPC, named pipes) can be added without
  touching the core. ✅
- **The MCP bridge owns no database.** Claude Code launches MCP servers as stdio
  child processes, but the daemon owns the redb locks and must not be re-spawned
  per session. `trusty-memory-mcp-bridge` (PR #149) is a ~40-line byte pipe
  (`tokio::io::copy_bidirectional`) between Claude Code's stdio and the daemon's
  UDS — it has **zero database knowledge**. ✅
- **`serve --stdio` was removed.** The legacy in-process stdio path deadlocked on
  redb's exclusive write lock whenever a daemon was already running; it was deleted
  in #150. The canonical stdio integration is now the bridge. ✅
- **Logs to stderr, never stdout.** MCP/UDS framing reserves stdout; the daemon
  logs to stderr (workspace-wide rule). ✅
- **Dynamic port discovery.** `serve` self-spawns a detached `serve --foreground`,
  binds HTTP on `7070..=7079` (OS fallback), and writes the resolved address to a
  discovery file read by `trusty_common::read_daemon_addr` (`commands/start.rs`). ✅
- **Port query command.** `trusty-memory port` reads the discovery file and prints
  the daemon's live address as a bare port, `host:port`, or JSON — enabling safe
  shell substitution (`curl http://127.0.0.1:$(trusty-memory port)/...`) without
  hard-coding the dynamic port (#526, `commands/port.rs`). ✅
- **Single-instance guard.** Before binding, the daemon probes the discovery files
  and exits `0` (success) if a healthy daemon is already responding to `/health`.
  This prevents launchd `KeepAlive { SuccessfulExit: false }` from spawning a zombie
  herd when `EADDRINUSE` would otherwise cause a non-zero exit and a cascade of
  respawns (#464, `commands/single_instance.rs`). ✅
- **Graceful shutdown + bridge reconnect.** The daemon uses
  `trusty_common::shutdown_signal()` with axum's `with_graceful_shutdown` to drain
  in-flight requests before exiting on SIGTERM (`trusty-common::shutdown.rs`, #534,
  `trusty-common` ≥ 0.10.0). The `mcp_bridge` detects socket-close after the pipe
  drains and reconnects with exponential backoff (200 ms → 30 s ceiling), so
  `launchctl bootout` + launchd respawn of a new binary is connection-safe from
  Claude Code's perspective. ✅
- **Multi-process concurrent reads.** redb takes an exclusive `flock` per file. To
  let a second process *read* a palace the daemon owns, `concurrent_open.rs` (#59)
  copies the database to a process-local snapshot when an exclusive open hits
  `DatabaseAlreadyOpen`. Writers always go through the daemon. ✅

### 2.1 Socket & data-dir resolution (`transport/uds.rs`)

The UDS path is deliberately kept **short** (≤104 bytes, the macOS `sun_path`
limit) by living under the runtime dir, not under `~/Library/Application Support`:

- `socket_path()` → `<runtime-dir>/trusty-memory.sock`
  (`$XDG_RUNTIME_DIR` when set, else `$TMPDIR` / `/var/folders/...` on macOS).
- `socket_path_for(data_root)` → `<runtime>/trusty-memory-<16-hex-hash>.sock`,
  hashing the data root so multiple data dirs get distinct sockets.
- `UDS_ADDR_FILE` (`uds_addr`) under the data root records the bound address.
- Orphaned `.sock` files are cleaned up on exit (#153).

Palace data persists under the OS-standard data dir
(`trusty_common::resolve_data_dir("trusty-memory")`):
- **macOS:** `~/Library/Application Support/trusty-memory/<palace-id>/`
- **Linux:** `~/.local/share/trusty-memory/<palace-id>/`

`resolve_data_dir` now guards against empty, relative, and root-level path
overrides (#503/#520 — `TRUSTY_DATA_DIR_OVERRIDE` must be an absolute,
non-root path; any other value falls back to the OS default with a warning
rather than scattering files in unexpected locations).

Per-palace files: `drawers.db` / payload sidecar, `vectors` HNSW redb,
`kg.redb` (or legacy `kg.db`), `chat_sessions` redb, `l1_cache.json`,
`palace.json` (metadata + `identity.txt`). `TRUSTY_DATA_DIR_OVERRIDE` redirects
the root (intended for tests; must be an absolute, non-root path — guarded since
#503/#520).

**Palace slug pinning** (`.trusty-tools/` convention, #446): `project_slug_at()`
now implements a two-step resolution order — first read
`.trusty-tools/trusty-memory.yaml` if present (the *pin file*), then fall back to
deriving the slug from the directory basename and lazily writing the pin file.
The pin file travels with the repo via git so a directory rename no longer
orphans the project from its palace. `trusty-memory link` (`commands/link.rs`)
explicitly writes or refreshes the pin file from the CLI. Constants live in
`project_root.rs`: `PIN_FILE_REL` (`".trusty-tools/trusty-memory.yaml"`),
`TRUSTY_TOOLS_DIR`, `PIN_SCHEMA_VERSION`. `write_project_pin` is guarded against
writing into a temp dir, home dir, or root directory (#492). ✅

**Startup pin-map scan** (`startup_scan.rs`): at daemon startup, a single
pass over `~/Projects`, `~/Developer`, `~/Code`, and `~/` builds a
`DashMap<palace_id → PathBuf>` (`AppState::pin_project_map`) so handlers can
look up a project's filesystem location by palace id without further I/O.
This also feeds `doctor` to avoid N-pass scans over the same directories. ✅

---

## 3. Fire-and-Forget Remember Path

Sub-agents spawned via Claude Code's Agent tool inherit **no MCP connection**, so
`mcp__trusty-memory__memory_remember` is unreachable to them — but they can run
shell commands. The `note` path closes that gap with a **detached** write:

```
trusty-memory note "fact" --palace p --tag t
        │
        ├─ read_daemon_addr("trusty-memory")        # discovery file
        ├─ POST /api/v1/remember  { palace, text, tags }   (short timeout)
        │        │
        │        └─ handler: tokio::spawn(write)     # returns 202 immediately
        │                       │
        │                       └─ filter → dedup → store → KG auto-extract
        └─ print "Queued."  •  exit 0  (even if daemon unreachable)
```

- The CLI **never blocks** on the redb write or the content gates; the endpoint
  queues the dispatch on a `tokio::spawn` (`commands/note.rs`, `web.rs`). ✅
- Failure is **swallowed**: a missing/slow daemon yields `exit 0` with no error,
  because a failure that arrives after the agent has already exited is useless. ✅
- The same detached-spawn discipline backs the **hook emit** path
  (`hook_emit.rs`): the `prompt-context` / `inbox-check` hook subprocesses POST to
  `/api/v1/activity/hook` so the activity feed isn't empty in a normal Claude Code
  session, again best-effort with exit 0 on failure. ✅

---

## 4. Storage Model (`memory_core/store/`)

All storage is **pure-Rust embedded** on the default path. The redb migration
sweep (#43–#56) removed the rusqlite/r2d2 and C++ usearch chains.

| Concern | Backend | Module | Notes |
|---|---|---|---|
| Vector index | **redb-backed HNSW** (`hnsw_rs` 0.3) | `store/hnsw_store.rs`, `store/vector.rs` | Type name `UsearchStore` preserved for back-compat (#50/#51); vectors postcard-encoded in a redb table; HNSW graph rebuilt on open; async ops on `spawn_blocking`. |
| Knowledge graph | **redb** (`KgStoreRedb`) | `store/kg.rs`, `kg_redb.rs`, `kg_store.rs`, `kg_writer.rs` | Temporal triples with `valid_from`/`valid_to`; composite-key + postcard codecs (#44). |
| Legacy KG | **SQLite** (feature-gated) | `store/kg_sqlite.rs` | `#[cfg(feature = "sqlite-kg")]`, for #45 migration; #47 will remove. 🟡 |
| Palace metadata | JSON | `store/palace_store.rs` | `palace.json` + `identity.txt` (L0 baseline), atomic writes. |
| L1 cache | JSON snapshot | `store/l1_cache.json` (`l1_cache.rs`) | Top-N essential drawers; hydrated on open, refreshed lazily. |
| External payloads | **redb** | `store/payload_store.rs` | Durable string-id↔uuid↔JSON map for open-mpm's `TrustyBackedMemoryStore` (#46/#52). |
| Chat sessions | **redb** | `store/chat_sessions.rs` | Web-UI chat resume (#56). |
| Recall analytics | **redb** | `memory_core/analytics.rs` | Hit/miss `RecallLog` (#57); one-shot SQLite→redb migration on first open. |
| Multi-process reads | redb snapshot copy | `store/concurrent_open.rs` | Falls back to a tmp snapshot on `DatabaseAlreadyOpen` (#59). |
| kuzu import | KuzuDB reader | `store/kuzu.rs` | Import-only; `kuzu` feature swaps the stub for real Cypher reads (#277). |

### Embeddings

`memory_core/embed.rs` is now a **re-export shim** for the unified `Embedder` /
`FastEmbedder` (`AllMiniLML6V2Q`, **384-dim**, `EMBED_DIM`) that moved to the
shared embedder surface in `trusty-common` so trusty-memory and trusty-search ship
the same code. The model cache (~22 MB) downloads on first run (~100 MB disk).

---

## 5. Progressive Retrieval (L0–L3) — `memory_core/retrieval.rs`

LLM context is precious, so recall is **layered and paid lazily**. The
`PalaceHandle` owns the per-palace storage handles plus the pre-cached L0/L1.

| Layer | Name | Cost | What it returns |
|---|---|---|---|
| **L0** | Identity | Always (cached) | Palace identity / baseline context (`identity.txt`). |
| **L1** | Essential | Always (cached) | Top-N drawers by importance (`l1_cache.json`); ~900 tokens combined with L0. |
| **L2** | On-demand vector | Paid per query | HNSW vector similarity + BM25 (bundled daemon) hybrid hits. |
| **L3** | Deep | `memory_recall_deep` | Slower, higher-recall search for what L0–L2 miss. |

- `memory_recall` → L0 + L1 + L2. `memory_recall_deep` → L3. `memory_recall_all`
  fans out across every palace.
- **Hybrid ranking** combines lexical (BM25 from the sidecar) and dense (HNSW)
  scores; temporal **decay** (`decay.rs`, 90-day half-life) de-weights stale
  drawers; the **filter** (`filter.rs`) keeps noise out of the store in the first
  place; **recall analytics** (`analytics.rs`) close the loop on which drawers are
  actually useful.

---

## 6. BM25 Sidecar — bundling & supervision

trusty-memory does not implement BM25 in-process; it drives the bundled
**`trusty-bm25-daemon`** (also bundled by trusty-search via #156/#190).

```
cargo install trusty-memory  ⇒  trusty-memory
                                 trusty-memory-mcp-bridge
                                 trusty-bm25-daemon     ← bundled [[bin]] shim
```

- **Single-install:** the third `[[bin]]` (`src/bin/bm25_daemon.rs`) is a thin
  shim that calls `trusty_bm25_daemon::run()`; declaring the daemon as a Cargo
  dependency makes `cargo install trusty-memory` build and install all three
  binaries in one command. This closes the "set `TRUSTY_BM25_DAEMON=1` but never
  installed the daemon → silently degraded lexical recall" footgun. ✅
- **Per-palace spawn supervision:** `Bm25Supervisor` (`bm25_supervisor.rs`, #193)
  keyed by palace id, with a `Mutex<HashMap<String, ChildHandle>>` preventing
  double-spawn. `ensure_running` discovers the binary, spawns a child with
  `--palace`/`--data-dir`, polls the socket, and owns the `tokio::process::Child`
  for the daemon's life. `TRUSTY_BM25_EXTERNAL=1` opts out (operator-managed
  daemon); shutdown SIGTERMs (via `libc::kill`) then SIGKILLs each child and
  cleans up its socket. Unix-only (UDS protocol). ✅

---

## 7. Embedded UI Build

The Svelte admin dashboard (`ui/`) is compiled at build time and embedded into the
binary via `rust-embed`; `web.rs` serves `ui/dist/` with an asset fallback. No Node
or separate web server is needed at runtime — deployment is "drop the binary on a
host." The UI consumes the same `/api/v1/*` surface as `curl` plus the `/sse` event
stream, so anything trusty-memory can do via Claude Code is reachable from the
browser. ✅

---

## 8. Source Module Map

### 8.1 Frontend — `crates/trusty-memory/src/`

| Module | Responsibility |
|---|---|
| `lib.rs` | Crate root: `run_http*` (axum HTTP/SSE/REST/UI), `AppState`, re-exports. |
| `main.rs` | `clap` CLI shim; dispatches to `commands::*`. |
| `tools.rs` | MCP tool surface: `MemoryMcpServer`, `tool_definitions[_with]` (the **24-tool** `tools/list` payload), in-process dispatcher. |
| `mcp_service.rs` | `MemoryMcpService` — `trusty_mcp_core::ServiceDescriptor` impl for in-process hosts (open-mpm). |
| `openrpc.rs` | OpenRPC 1.3.2 `rpc.discover` builder + `scopes_for_tool` (read/write/knowledge.write). |
| `web.rs` | All `/api/v1/*` handlers, `/sse`, `/rpc`, `/health`, embedded-UI fallback. |
| `service.rs` | `MemoryService` — pure business-logic facade over `AppState` (extracted from `web.rs`, #151) reused by HTTP + chat dispatch. |
| `chat.rs` | OpenRouter/Ollama SSE chat, tool dispatch loop, chat-session CRUD, `/api/v1/messages*`. |
| `transport/` | `rpc.rs` (transport-agnostic JSON-RPC dispatch), `uds.rs` (Unix-socket listener + path resolution). |
| `bm25_supervisor.rs` | Per-palace `trusty-bm25-daemon` spawn supervisor (#193). |
| `kg_extract.rs` | Deterministic KG triple extraction on write (#97). |
| `bootstrap.rs` | KG bootstrap from project files after `palace_create` (#60). |
| `discovery.rs` | Automatic project-alias discovery → `is_alias_for` triples. |
| `prompt_facts.rs` | Hot-predicate prompt-context surface (`get_prompt_context`). |
| `messaging.rs` | Inter-project messaging via `msg:*`-tagged drawers (#99). |
| `activity.rs` | Persistent redb-backed activity log, FIFO-capped, source-tagged (#96). |
| `prompt_log.rs` | Enriched-prompt JSONL logger with rotation/retention (#105). |
| `attribution.rs` | `creator:*` tag namespace attached to every write (#202). |
| `hook_emit.rs` | Cross-process hook activity emit (`POST /api/v1/activity/hook`). |
| `project_root.rs` | Project-root detection, palace-slug derivation, and `.trusty-tools/trusty-memory.yaml` pin-file management (#88, #446). `ProjectPin`, `PIN_FILE_REL`, `TRUSTY_TOOLS_DIR`, `PIN_SCHEMA_VERSION`, `read_project_pin`, `write_project_pin`, `project_slug_from_basename`. |
| `startup_scan.rs` | Single-pass pin-file scanner at daemon start → `AppState::pin_project_map` (`DashMap<palace_id, PathBuf>`). `default_search_dirs()` returns the four standard roots (#470). |
| `fd_metrics.rs` | Best-effort open-fd count (`count_open_fds`) and soft RLIMIT_NOFILE (`fd_soft_limit`) for the `/health` fd gauge (#464). |
| `commands/` | Subcommand handlers: `serve`/`start`/`stop`, `setup`, `service` (launchd), `migrate`/`kuzu_migrate`/`migrations`, `monitor`, `note`, `send_message`, `inbox_check`, `doctor`, `kg_rebuild`, `prompt_context`, `port`, `upgrade`, `link`, `single_instance`. |
| `bin/mcp_bridge.rs` | `trusty-memory-mcp-bridge` — stdio↔UDS byte pipe with idle-safe exponential-backoff reconnect (PR #149, #535). |
| `bin/bm25_daemon.rs` | Bundled `trusty-bm25-daemon` binary shim. |

### 8.2 Core — `crates/trusty-common/src/memory_core/`

| Module | Responsibility |
|---|---|
| `mod.rs` | Re-exports the palace hierarchy, registry, retrieval handle, consolidation types. Gated behind the `memory-core` feature. |
| `palace.rs` | Data model: `PalaceId`, `Palace` → `Wing` → `Room` → Closet → `Drawer`. |
| `registry.rs` | `PalaceRegistry` — concurrent `DashMap<PalaceId, Arc<PalaceHandle>>`. |
| `retrieval.rs` | 4-layer (L0–L3) progressive retrieval; the canonical `PalaceHandle`. |
| `dream.rs` | `Dreamer` / `dream_cycle` — idle consolidation (prune/dedup/compact/closet) + optional semantic phase. |
| `semantic_consolidation.rs` | `Inference` trait + `OpenRouter`/`Ollama`/`Mock` impls + `SemanticConsolidator` (#87). |
| `filter.rs` | Signal/noise ingest filter (#61). |
| `decay.rs` | Temporal importance/confidence decay (90-day half-life). |
| `community.rs` | Louvain community detection → knowledge gaps (#52). |
| `analytics.rs` | redb-backed `RecallLog` hit/miss tracking (#57). |
| `git.rs` | Git-history fact extractor (rule-based NLP, zero LLM). |
| `embed.rs` | Re-export shim for the unified `Embedder`/`FastEmbedder`. |
| `store/` | All storage backends (see §4): `hnsw_store`, `vector`, `kg`/`kg_redb`/`kg_store`/`kg_writer`/`kg_sqlite`, `palace_store`, `l1_cache`, `payload_store`, `chat_sessions`, `kuzu`, `concurrent_open`. |

---

## 9. RPC / Tool Dispatch Surface

The transport-agnostic `transport::dispatch` (`transport/rpc.rs`) handles JSON-RPC
methods over both UDS and `POST /rpc`:

- **Lifecycle:** `initialize`, `tools/list`, `tools/call`, `rpc.discover`.
- **Direct tool methods** (also callable by name): the memory/palace/KG tools
  enumerated in PRD §4.3 — e.g. `memory_remember`, `memory_recall`,
  `memory_recall_deep`, `memory_recall_all`, `memory_list`, `memory_forget`,
  `palace_create/list/info/compact`, plus helper resolvers `palace_id` /
  `palace_name`, `memory_note` / `memory_send_message`, and `upgrade`.
- **Scopes** are advertised per method via OpenRPC `x-scopes` (`openrpc.rs`):
  `memory.read` for queries, `memory.write` for mutations, `knowledge.write` for
  `kg_bootstrap`. Advertised, not locally enforced (PRD §2 Non-Goals).

---

## 10. Operational Reliability Layer

Several features added since v0.10.0 (current: v0.14.0) harden the daemon for production use.

### 10.1 Bug-Capture Error Layer (`trusty-common` `error_capture/`, #478/#490)

`trusty-common`'s `error_capture` module (enabled via the `bug-capture` feature
flag) provides a `tower` middleware layer (`BugCaptureLayer`) that intercepts
500-level responses, fingerprints them with a SHA-256-keyed dedup scheme, and
appends them to a JSONL ring store (`ErrorStore`). The MCP `upgrade` tool and
the HTTP surface expose these captures so operators can report bugs without
manually tailing logs. trusty-memory enables `bug-capture` in its feature list
(`Cargo.toml`). The `error_store: Option<ErrorStore>` field is attached to
`AppState` via `AppState::with_error_store(...)`.

### 10.2 Update-Check & Upgrade (`trusty-common` `update/`, #455/#537/#539)

`trusty-common`'s `update` module polls crates.io for new versions (throttled;
24-hour cache). The result is stored in `AppState::update_available:
Arc<Mutex<Option<String>>>` so `/health` always reports it lock-free with no
per-request I/O. Three upgrade surfaces:

- **`/health` `update_available` field** — background throttled check at startup.
- **`trusty-memory upgrade [--check] [--yes]`** — interactive CLI (`commands/upgrade.rs`).
- **`upgrade` MCP tool** — `check=true` (report only) / `confirm=true` (install +
  restart). The daemon calls `std::process::exit(1)` after a successful install so
  launchd's `KeepAlive { SuccessfulExit: false }` respawns the new binary; the MCP
  response is flushed first (500 ms delay). Delegates to
  `trusty_common::update::upgrade_and_restart` from `update/upgrade.rs`.

### 10.3 File-Descriptor Observability & LaunchAgent Limits (#464)

The fd-exhaustion bug (EMFILE at 256 fds with ~82 palaces × 3 redb files each)
is now observable and mitigated:

- `fd_metrics.rs` — `count_open_fds()` (reads `/dev/fd` or `/proc/self/fd`)
  and `fd_soft_limit()` (via `libc::getrlimit`). Exposed as `open_fds` and
  `fd_soft_limit` fields in the `/health` JSON response.
- The macOS LaunchAgent plist generated by `service install` now includes
  `SoftResourceLimits` / `HardResourceLimits` = 8192 so the daemon can handle
  large palace collections.
- Open ticket #463 tracks a future lazy/LRU redb-handle cache for bounded fd usage.
