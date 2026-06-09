# trusty-memory — Product Requirements Document

> **Status:** Canonical · Living Document
> **Last reviewed:** 2026-06-08
> **Derived from:** code/docs/tickets audit (v0.15.0)

**Status legend:** ✅ Implemented · 🟡 Partial · 🔵 Designed-not-built · ⚪ Aspirational
Each requirement is framed **Vision / Current / Gap**.

## 0.15.0 changes

This revision refreshes the PRD to v0.15.0. Product-visible changes since v0.14.0:

- **redb 4.x store migration (#702)** — embedded activity and memory stores moved
  from redb 2.x to 4.x. On first start after upgrade, incompatible 2.x stores are
  backed up to `*.v2-incompatible` and recreated empty (graceful recovery, no
  crash). Operators upgrading from <0.15.0 should expect a one-time reset of these
  stores.
- **Dashboard auto-start (#687)** — the web UI dashboard auto-starts on first
  daemon launch; operators no longer need a manual command to bring it up.
- **`add_alias` / `discover_aliases` optional `palace` param (#664)** — both MCP
  tools now accept an optional `palace` argument so callers can scope alias
  operations to a named palace.

---

## 1. Vision & Mission

### North-star vision

> **trusty-memory is a persistent memory palace for AI agents and humans** — a
> local, embedded daemon that lets any MCP-aware client *remember* natural-language
> facts and *recall* them later via hybrid BM25 + vector retrieval, organized into
> per-project namespaces ("palaces"), with an optional knowledge-graph layer for
> structured facts — **with no external services, no cloud, and no Python/Node in
> the core**.

Where Claude Code's session memory evaporates at the end of a conversation,
trusty-memory is **durable long-term memory** that survives sessions, agents, and
machines reboots. It is the cross-session, cross-agent, cross-project knowledge
substrate of the trusty-* ecosystem: open-mpm links it as an in-process library,
Claude Code talks to it over a thin stdio→UDS bridge, sub-agents fire notes at it
over HTTP, and a human can browse the same data in an embedded web dashboard.

The product is deliberately **single-install**: `cargo install trusty-memory`
produces three binaries (`trusty-memory`, `trusty-memory-mcp-bridge`, and the
bundled `trusty-bm25-daemon`) so an operator never has to hand-assemble a sidecar
fleet.

### Mission

Give every agent — and every human working alongside one — a memory that is
**always available, always local, and organized by the project it belongs to**,
reachable through whatever surface the caller already speaks (MCP tool, HTTP REST,
SSE, Unix socket, or a one-line shell command), without standing up a database or
a service mesh.

### Why this is novel

trusty-memory unifies four capabilities that are usually separate products into a
single embedded binary: (1) a **vector recall** store, (2) a **temporal
knowledge-graph**, (3) an **inter-project message bus**, and (4) an **idle-time
LLM consolidation** ("dream") loop — all behind one MCP tool surface, all pure
Rust, all licensed MIT. The frontend/core split (this crate ⇄ `trusty-common`'s
`memory_core`) lets the same storage engine be embedded in-process by open-mpm or
served as a standalone daemon, with no code change.

---

## 2. Goals & Non-Goals

### Goals

| # | Goal | Status |
|---|---|---|
| G1 | **Remember / recall over MCP** — store natural-language drawers and recall them by hybrid BM25 + vector retrieval, exposed as MCP tools. | ✅ |
| G2 | **Palace namespacing anchored to projects** — one palace per project (slug-derived), plus a `personal` escape hatch. | ✅ |
| G3 | **Multi-transport single daemon** — one long-lived daemon serving MCP (via the stdio→UDS bridge), HTTP/SSE REST, and a `POST /rpc` JSON-RPC surface. | ✅ |
| G4 | **Fire-and-forget writes for sub-agents** — a `note` CLI + `POST /api/v1/remember` that returns immediately (detached spawn) for agents with no MCP connection. | ✅ |
| G5 | **Bundled BM25 sidecar** — ship and auto-spawn `trusty-bm25-daemon` so lexical recall works out of the box (single-install convention). | ✅ |
| G6 | **Temporal knowledge graph** — assert/query time-bounded triples; auto-extraction on write; visual graph in the UI. | ✅ |
| G7 | **Idle-time consolidation ("dream")** — NLP dedup/prune/compact + optional LLM-backed semantic consolidation. | ✅ |
| G8 | **Embedded admin UI** — Svelte dashboard compiled into the binary (no Node at runtime). | ✅ |
| G9 | **Inter-project messaging** — deliver messages to another palace's inbox; surfaced at Claude Code session start. | ✅ |
| G10 | **Import / migration** — migrate from kuzu-memory (MCP config + data) into palaces. | ✅ |
| G11 | **Pure-Rust, service-free storage** — redb + in-process HNSW + fastembed ONNX; no rusqlite/usearch native chain on the default path. | ✅ |
| G12 | **In-process embeddability** — open-mpm links `MemoryMcpService` as an rlib without the axum/HTTP surface. | ✅ |
| G13 | **Self-upgrade** — `upgrade` CLI and MCP tool let operators and agents check for and install new releases, with launchd self-restart. | ✅ |
| G14 | **Operational observability** — `/health` exposes fd usage, `update_available`, and uptime; bug-capture layer fingerprints errors for operator reporting. | ✅ |

### Non-Goals

| Non-Goal | Rationale |
|---|---|
| Central server / cloud-hosted memory | trusty-memory is a single-binary **local** daemon. |
| Multi-tenant auth / RBAC enforcement | Scopes (`memory.read` / `memory.write`) are *advertised* via OpenRPC for orchestrators to enforce; the daemon itself trusts its local caller. |
| Embedding-model training or fine-tuning | Uses the fixed `AllMiniLML6V2Q` 384-dim model; not a model lab. |
| Cross-machine replication / sync | Each host owns its palaces on local disk; no built-in replication. |
| Node.js at runtime | Node is used only to *build* the embedded Svelte UI; the runtime is the single Rust binary. |
| A general graph database | The KG is a purpose-built temporal triple store, not a Cypher/Gremlin engine (the kuzu reader is import-only). |

---

## 3. Target Users / Personas

| Persona | Who | Primary need | Surface |
|---|---|---|---|
| **MCP agent (Claude Code)** | An interactive coding agent in a session | Remember/recall facts per-turn; pull hot project context (aliases, conventions) without blind searching | MCP tools via `trusty-memory-mcp-bridge` (stdio→UDS) |
| **Embedded host (open-mpm)** | A Rust orchestrator linking memory in-process | Register `MemoryMcpService` into a merged `rpc.discover`; call tools without spawning a child | rlib (`default-features = false`, no axum) |
| **Sub-agent / shell script** | A spawned agent with **no** MCP connection | Save a fact and move on, never blocking on the write | `trusty-memory note` → `POST /api/v1/remember` (detached) |
| **Human operator** | A developer auditing/curating memory | Browse palaces, watch the live activity feed, trigger a dream, inspect the KG | Embedded Svelte dashboard + REST |
| **Cross-project automation** | One project messaging another | Drop a message into another palace's inbox; have it surface at the recipient's next session | `send-message` / `inbox-check` (SessionStart hook) |

**Unifying need across all surfaces:** a durable, project-scoped memory that is
reachable through whatever protocol the caller already speaks, with no service to
provision.

---

## 4. Functional Requirements

Grouped by capability area. Each requirement carries Vision / Current / Gap and
an inline status tag. Source paths are cited where known. Module paths under
`memory_core/` refer to `crates/trusty-common/src/memory_core/`; all others refer
to `crates/trusty-memory/src/`.

### 4.1 Remember / Recall (`tools.rs`, `memory_core/retrieval.rs`)

**FR-1.1 — Store a memory (`memory_remember` / `memory_note`)** ✅
- *Vision:* A caller stores a natural-language fact in a palace and gets back a stable `drawer_id`; tags, room, and creator attribution are attached automatically.
- *Current:* Implemented (`tools.rs`, `attribution.rs`). Writes pass a **signal/noise filter** (`memory_core/filter.rs`, #61) and a **rolling Jaro-Winkler dedup window** (#220) before storage; creator tags (`creator:*`, #202) tag every write path (HTTP/MCP/CLI/hook). Auto-KG extraction (`kg_extract.rs`, #97) populates the graph on write.
- *Gap:* None material.

**FR-1.2 — Hybrid recall (`memory_recall`)** ✅
- *Vision:* A query returns the most relevant drawers using BM25 *and* vector similarity, cheaply.
- *Current:* Implemented over the **4-layer progressive retrieval** (`memory_core/retrieval.rs`): L0 (palace identity) + L1 (top-N essential, persisted as `l1_cache.json`) are always loaded (~900 tokens); L2 (on-demand vector) is paid only when the query needs it. BM25 comes from the bundled `trusty-bm25-daemon`; vectors from the in-process HNSW index.
- *Gap:* None material.

**FR-1.3 — Deep recall (`memory_recall_deep`)** ✅
- *Vision:* A slower, higher-recall path (L3 deep search) for queries the fast layers miss.
- *Current:* Implemented (L3 layer in `memory_core/retrieval.rs`).
- *Gap:* None material.

**FR-1.4 — Cross-palace recall (`memory_recall_all`)** ✅
- *Vision:* Recall across every palace at once, not just the bound one.
- *Current:* Implemented (`tools.rs`, HTTP `GET /api/v1/recall`).
- *Gap:* None material.

**FR-1.5 — List / forget (`memory_list`, `memory_forget`)** ✅
- *Vision:* Enumerate stored drawers (optionally filtered by room/tag) and delete a specific drawer by id, cascading its derived KG triples.
- *Current:* Implemented; `memory_forget` cascade-deletes auto-derived triples (#278).
- *Gap:* None material.

### 4.2 Palace Management (`tools.rs`, `project_root.rs`, `memory_core/palace.rs`, `registry.rs`)

**FR-2.1 — Palace CRUD (`palace_create/list/info/update/delete`)** ✅
- *Vision:* Create, list, inspect, update, and delete palace namespaces; deletion removes on-disk state.
- *Current:* All five implemented (`tools.rs`); `palace_delete` backs `DELETE /api/v1/palaces/{id}` (#180). The `PalaceRegistry` is a concurrent `DashMap<PalaceId, Arc<PalaceHandle>>` (`memory_core/registry.rs`).
- *Gap:* None material.

**FR-2.2 — Palace-as-project enforcement** ✅
- *Vision:* New palace names must match a slug derived from the project root (`.git`/`Cargo.toml`/`pyproject.toml`/`package.json` walk); `personal` is the only allowed name outside any project. Existing palaces are grandfathered.
- *Current:* Implemented (`project_root.rs`, #88). `doctor --fix-palaces` audits orphans (advisory only).
- *Gap:* Actual rename/merge of orphaned palaces into `personal` is **not implemented** — `doctor --fix` only prints suggestions. 🔵

**FR-2.3 — Palace compaction (`palace_compact`)** ✅
- *Vision:* Compact a palace's on-disk storage on demand.
- *Current:* Implemented (`tools.rs` → store compaction).
- *Gap:* None material.

### 4.3 MCP Tool Surface (`tools.rs`, `mcp_service.rs`, `openrpc.rs`)

**FR-3.1 — Full MCP tool set (24 tools)** ✅
- *Vision:* One auditable file defines every tool the server exposes, kept in sync with the MCP `tools/list` payload and the OpenRPC `rpc.discover` manifest.
- *Current:* **24 tools** in `tool_definitions()` (`tools.rs`): `memory_remember`, `memory_note`, `memory_recall`, `memory_recall_deep`, `memory_recall_all`, `memory_list`, `memory_forget`, `palace_create`, `palace_delete`, `palace_update`, `palace_list`, `palace_info`, `palace_compact`, `kg_assert`, `kg_query`, `kg_gaps`, `kg_bootstrap`, `add_alias`, `discover_aliases`, `list_prompt_facts`, `remove_prompt_fact`, `get_prompt_context`, `memory_send_message`, `upgrade`. (The README's "12 tools" table is a curated subset; the wire surface is 24.)
- *Gap:* Doc drift — the crate `README.md` may still cite an older count. The authoritative count is the `tool_definitions_lists_all_tools` test: **24**.

**FR-3.2 — Per-tool scopes via OpenRPC** ✅
- *Vision:* Each tool advertises a logical scope (`memory.read` / `memory.write` / `knowledge.write`) so orchestrators can authorize without bespoke adapters.
- *Current:* Implemented (`openrpc.rs` `scopes_for_tool`); `build_discover_response` emits OpenRPC 1.3.2 with `x-scopes`.
- *Gap:* Scopes are *advertised* only; the daemon does not itself enforce them (see §2 Non-Goals).

**FR-3.3 — `ServiceDescriptor` for in-process hosts** ✅
- *Vision:* open-mpm links the crate and merges its tools into a single `rpc.discover` document with no glue.
- *Current:* `MemoryMcpService` implements `trusty_mcp_core::ServiceDescriptor` (`mcp_service.rs`).
- *Gap:* None material.

**FR-3.4 — Optional default palace** ✅
- *Vision:* When the daemon is started with `--palace <name>`, the `palace` argument becomes optional in every tool call (`tool_definitions_with(has_default)`).
- *Current:* Implemented (`tools.rs`).
- *Gap:* None material.

### 4.4 HTTP Remember Endpoint & `note` CLI (`commands/note.rs`, `web.rs`, `hook_emit.rs`)

**FR-4.1 — Fire-and-forget remember (`POST /api/v1/remember`)** ✅
- *Vision:* An agent with no MCP connection saves a fact with a one-line shell command that returns immediately, never blocking on the redb write or the content gates.
- *Current:* `trusty-memory note "…" --palace <p> [--tag …]` resolves the daemon address via `trusty_common::read_daemon_addr`, POSTs to `/api/v1/remember`, prints `Queued.`, and exits 0 even when the daemon is unreachable (`commands/note.rs`). The endpoint queues the dispatch on a `tokio::spawn` so the caller never waits (`web.rs`).
- *Gap:* None material.

**FR-4.2 — REST mirror of the tool surface** ✅
- *Vision:* Anything trusty-memory can do over MCP can also be done via `curl`.
- *Current:* `/api/v1/*` covers status, palaces, drawers, recall, KG (subjects/graph/triples/gaps/aliases), chat, config, activity, dream, messages, and logs (`web.rs`).
- *Gap:* None material.

### 4.5 BM25 Sidecar (`bm25_supervisor.rs`, `src/bin/bm25_daemon.rs`)

**FR-5.1 — Bundled single-install daemon** ✅
- *Vision:* `cargo install trusty-memory` produces the `trusty-bm25-daemon` binary too, so lexical recall works without a separate install.
- *Current:* The `[[bin]]` shim (`src/bin/bm25_daemon.rs`) delegates to `trusty_bm25_daemon::run()`; the daemon crate is a Cargo dependency so it builds and installs alongside (mirrors #190 for trusty-embedderd).
- *Gap:* None material.

**FR-5.2 — Per-palace spawn supervision** ✅
- *Vision:* On first BM25 use for a palace, auto-spawn a child with the right `--palace`/`--data-dir`, poll its socket, and own the process for the daemon's life — so operators never hand-`launchctl bootstrap` one daemon per palace.
- *Current:* `Bm25Supervisor` (`bm25_supervisor.rs`, #193) keyed by palace id, with a `tokio::sync::Mutex<HashMap<…>>` guarding against double-spawn. Graceful SIGTERM → SIGKILL shutdown via `libc::kill`. `TRUSTY_BM25_EXTERNAL=1` opts out (operator runs their own daemon).
- *Gap:* Unix-only (the daemon protocol is UDS).

### 4.6 Knowledge Graph & Facts (`kg_extract.rs`, `bootstrap.rs`, `discovery.rs`, `prompt_facts.rs`, `memory_core/store/kg*.rs`)

**FR-6.1 — Temporal triple store (`kg_assert`, `kg_query`)** ✅
- *Vision:* Assert and query time-bounded triples (`valid_from`/`valid_to`); back the store with pure-Rust embedded storage.
- *Current:* `KnowledgeGraph` over **redb** (`memory_core/store/kg.rs`, `kg_redb.rs`, #44); the legacy SQLite path is preserved behind the `sqlite-kg` feature for migration (#45) and slated for removal (#47).
- *Gap:* SQLite KG code still present pending #47 cleanup. 🟡

**FR-6.2 — Auto-extraction on write** ✅
- *Vision:* Every `memory_remember` populates the KG so palaces always have a non-empty graph, offline and fast.
- *Current:* Deterministic `extract_triples` (`kg_extract.rs`, #97) — tag/room/hashtag→drawer plus a small is-a/has-a/works-at pattern table; skips `test`/`fixture`/`cross-project-qa` tags.
- *Gap:* Heuristic-only; richer extraction is left to the dream cycle's LLM pass.

**FR-6.3 — KG bootstrap (`kg_bootstrap`)** ✅
- *Vision:* After `palace_create`, seed the KG from project files so it is never empty.
- *Current:* `bootstrap.rs` (#60) scans `Cargo.toml`/`package.json`/`pyproject.toml`/`CLAUDE.md`/`.git/config`/`go.mod` and seeds structured + temporal triples; missing files are non-errors.
- *Gap:* None material.

**FR-6.4 — Alias discovery (`add_alias`, `discover_aliases`)** ✅
- *Vision:* Surface implicit shorthand (`tga` → `trusty-git-analytics`) automatically as `is_alias_for` triples.
- *Current:* `discovery.rs` scans Cargo/git/abbreviation signals; the tool dedupes against active triples and rebuilds the prompt cache.
- *Gap:* None material.

**FR-6.5 — Prompt-facts surface (`get_prompt_context`, `list_prompt_facts`, `remove_prompt_fact`)** ✅
- *Vision:* Hot KG predicates (aliases, conventions, ambient facts) injected into the model's working context per-turn, query-filterable.
- *Current:* `prompt_facts.rs` defines `HOT_PREDICATES`, the formatter, and a cached `PromptFactsCache`. The `get_prompt_context` tool is invoked per-turn (replacing the session-init MCP-prompts approach that hosts read only once).
- *Gap:* None material. (The workspace CLAUDE.md references `get_prompt_context()` auto-resolution; this is the implementing tool.)

**FR-6.6 — Knowledge gaps (`kg_gaps`)** ✅
- *Vision:* Detect sparse "knowledge gap" communities in the KG to drive consolidation.
- *Current:* In-tree Louvain community detection (`memory_core/community.rs`, #52); a community is a gap when internal density < 0.2.
- *Gap:* Leiden phase-2 refinement deferred (Louvain only). 🔵

### 4.7 Dream / Consolidation (`memory_core/dream.rs`, `semantic_consolidation.rs`, `decay.rs`)

**FR-7.1 — Idle-time NLP consolidation** ✅
- *Vision:* A background idle clock periodically dedups, prunes low-value/stale drawers, refreshes closet indexes, and compacts storage.
- *Current:* `Dreamer` + `dream_cycle` (`memory_core/dream.rs`) with content-prune (#222), dedup, prune, compaction, closet refresh. Temporal decay (`memory_core/decay.rs`) de-weights old drawers (90-day half-life). Triggerable via `memory_dream` / `POST /api/v1/dream/run`.
- *Gap:* None material.

**FR-7.2 — LLM-backed semantic consolidation** ✅
- *Vision:* When an inference backend is available, an extra phase canonicalizes paraphrases and aliases the NLP passes miss (Alias / Merge / Flag actions), additively (originals preserved; `superseded_by` triples link lineage), with a per-cycle call budget and SHA-256 response caching.
- *Current:* `SemanticConsolidator` (`memory_core/semantic_consolidation.rs`, #87). Backend priority: OpenRouter (`OPENROUTER_API_KEY`) > Ollama (`local_model.enabled`) > disabled no-op. Default model `anthropic/claude-haiku-4-5`. `MockInference` keeps `cargo test` offline.
- *Gap:* None material.

### 4.8 Inter-Project Messaging (`messaging.rs`, `commands/send_message.rs`, `commands/inbox_check.rs`)

**FR-8.1 — Message delivery (`memory_send_message`, `send-message`)** ✅
- *Vision:* Deliver a message to another palace's inbox with no new schema — encode it as a drawer with a `msg:*` tag envelope.
- *Current:* `messaging.rs` (#99) defines the `msg:v1`/`msg:from`/`msg:to`/`msg:purpose`/`msg:sent_at`/`msg:read` envelope; addressing is by repo slug. CLI `send-message` and `POST /api/v1/messages`.
- *Gap:* No central registry — sender/receiver agree on the slug out of band (by design).

**FR-8.2 — Inbox surfacing at session start (`inbox-check`)** ✅
- *Vision:* Unread messages for the cwd-derived palace are injected into Claude Code session context, then atomically marked read.
- *Current:* `commands/inbox_check.rs`; installed as a `SessionStart` hook by `setup`. Atomic compare-and-swap on the read flag prevents double-delivery under concurrency. Every failure path exits 0 with empty stdout.
- *Gap:* None material.

### 4.9 Embedded UI & Activity (`web.rs`, `activity.rs`, `prompt_log.rs`)

**FR-9.1 — Embedded Svelte dashboard** ✅
- *Vision:* A browser admin panel served from the daemon with no Node at runtime: palace overview, live event stream, manual dream trigger, memory browsing, KG graph view.
- *Current:* Svelte build compiled via `rust-embed` and served by `web.rs`; SSE at `/sse`.
- *Gap:* None material.

**FR-9.2 — Persistent activity feed** ✅
- *Vision:* The activity feed shows historical entries on mount (not just live SSE) and captures writes from *every* origin (HTTP / MCP / Hook).
- *Current:* `ActivityLog` (`activity.rs`, #96) — redb-backed, FIFO-capped at `MAX_ENTRIES`, source-tagged. Hook subprocesses emit via `hook_emit.rs` → `POST /api/v1/activity/hook`.
- *Gap:* None material.

**FR-9.3 — Enriched-prompt logging** ✅
- *Vision:* Record what each `prompt-context` / `inbox-check` hook injected, for effectiveness analysis.
- *Current:* `prompt_log.rs` (#105) — rolling JSONL under `<data_root>/logs/`, daily + size-cap rotation, retention pruning, optional prompt hashing.
- *Gap:* None material.

### 4.10 Setup, Service & Migration (`commands/setup.rs`, `service.rs`, `migrate.rs`, `kuzu_migrate.rs`, `doctor.rs`)

**FR-10.1 — One-shot setup (`setup`)** ✅
- *Vision:* `trusty-memory setup` installs the launchd LaunchAgent (macOS), pre-warms the embedder cache, and patches every Claude settings file with the MCP entry + the `prompt-context`/`inbox-check` hooks.
- *Current:* Implemented (`commands/setup.rs`, #86); idempotent, service-owned hooks.
- *Gap:* None material.

**FR-10.2 — Unified start/serve/stop** ✅
- *Vision:* `start`/`serve`/`stop` match trusty-search semantics — `serve` self-spawns a detached `serve --foreground`; a second `start` is a no-op when healthy.
- *Current:* `commands/{start,stop}.rs` (#83). Dynamic port `7070..=7079` with OS fallback; address written to the discovery file.
- *Gap:* None material.

**FR-10.3 — kuzu-memory migration (`migrate kuzu-memory`, `migrate kuzu-data`)** ✅
- *Vision:* Rewrite Claude MCP config from the legacy kuzu-memory server, and import its `store.redb` data into palaces (idempotent).
- *Current:* `commands/migrate.rs` (#278) + `kuzu_migrate.rs` (#277); each entity → drawer, each relation → triple; SHA-256-derived UUIDs make re-import idempotent.
- *Gap:* None material.

**FR-10.4 — Diagnostics & KG rebuild (`doctor`, `kg_rebuild`)** ✅
- *Vision:* Audit palaces/orphans and rebuild the KG from drawers.
- *Current:* `commands/doctor.rs` and `kg_rebuild.rs`; startup-pin-map (`AppState::pin_project_map`) feeds doctor to avoid redundant directory traversals.
- *Gap:* `doctor --fix` is advisory only (see FR-2.2). 🔵

**FR-10.5 — Port query (`port`)** ✅
- *Vision:* Operators and shell scripts need the daemon's live port without guessing the dynamically-selected value (7070–7079).
- *Current:* `trusty-memory port [--addr|--json]` reads the discovery file and prints bare port, `host:port`, or JSON (#526, `commands/port.rs`). Exits non-zero when no daemon is running so shell substitution fails cleanly.
- *Gap:* None material.

**FR-10.6 — Explicit palace-slug pinning (`link`)** ✅
- *Vision:* Lock in a palace slug before a directory rename so the project never becomes orphaned from its palace.
- *Current:* `trusty-memory link [--path] [--slug] [--force]` writes/refreshes `.trusty-tools/trusty-memory.yaml`. Project slug resolution (`project_slug_at`) reads the pin file first, deriving from basename only as a fallback (#446, `commands/link.rs`, `project_root.rs`).
- *Gap:* None material.

### 4.11 Self-Upgrade (`commands/upgrade.rs`, `tools.rs`, `trusty-common` `update/`)

**FR-11.1 — Version check and self-upgrade** ✅
- *Vision:* Operators and MCP agents can check whether a newer release is available and, with explicit confirmation, install it and restart the daemon from a single command.
- *Current:* Three surfaces: (a) `trusty-memory upgrade [--check] [--yes]` CLI; (b) `upgrade` MCP tool (`check=true` / `confirm=true`); (c) `/health` `update_available` field populated by a background throttled crates.io poll at startup. The MCP tool flushes its response before the 500 ms delayed `exit(1)` so the client sees the result before launchd respawns the new binary (#537/#539). Delegates to `trusty_common::update::upgrade_and_restart`.
- *Gap:* None material.

### 4.12 Operational Reliability (`fd_metrics.rs`, `commands/single_instance.rs`, `trusty-common` `error_capture/`, `shutdown.rs`)

**FR-12.1 — Single-instance guard** ✅
- *Vision:* The daemon must not spawn zombie copies when launchd `KeepAlive { SuccessfulExit: false }` triggers on `EADDRINUSE`.
- *Current:* At startup the daemon probes existing discovery files and exits `0` if a healthy daemon is already responding. launchd treats exit-0 as clean shutdown and does not respawn (#464, `commands/single_instance.rs`).
- *Gap:* None material.

**FR-12.2 — File-descriptor observability** ✅
- *Vision:* Operators must be able to see "N fds open vs. M limit" before EMFILE exhaustion makes the daemon non-functional.
- *Current:* `fd_metrics.rs` provides `count_open_fds()` and `fd_soft_limit()`; both are exposed in the `/health` JSON response as `open_fds` and `fd_soft_limit` (#464). LaunchAgent plist raised to 8192 via `SoftResourceLimits`/`HardResourceLimits`.
- *Gap:* 🟡 Architectural bound — each open palace holds ~3 redb files; LRU caching of palace handles (#463) would permanently bound fd usage regardless of palace count.

**FR-12.3 — Bug-capture error layer** ✅
- *Vision:* 500-level daemon errors should be fingerprintable and reportable without requiring operators to tail logs.
- *Current:* `trusty-common` `error_capture` module (`bug-capture` feature, #478/#490): `BugCaptureLayer` intercepts 500 responses, deduplicates by SHA-256 fingerprint, and stores them in an `ErrorStore` ring. `AppState::error_store` holds the handle. The `upgrade` MCP tool and HTTP surfaces can expose capture counts.
- *Gap:* Does not capture errors from `tokio::spawn` tasks outside HTTP handler context.

**FR-12.4 — Graceful shutdown** ✅
- *Vision:* `launchctl bootout` (SIGTERM) must drain in-flight requests before the process exits, so a connection-safe upgrade never drops an MCP call mid-flight.
- *Current:* `trusty_common::shutdown_signal()` (`shutdown.rs`) watches SIGTERM+SIGINT; passed to `axum::serve(...).with_graceful_shutdown(...)` (#534, `trusty-common` ≥ 0.10.0). The `mcp_bridge` reconnects with exponential backoff after the socket closes.
- *Gap:* None material.

---

## 5. Success Criteria / Differentiators

A release meets the bar when:

1. **Remember/recall is real and frictionless** — an agent stores a fact via MCP
   (or a one-line `note`) and recalls it accurately later via hybrid BM25 +
   vector retrieval, with L0/L1 grounding always present. ✅
2. **Single-install holds** — `cargo install trusty-memory` yields a working
   daemon *and* its bundled BM25 sidecar, auto-spawned per palace, with no manual
   service assembly. ✅
3. **One daemon, many transports** — the same long-lived process serves MCP (via
   the bridge), HTTP/SSE REST, and JSON-RPC, never re-spawning per session and
   never deadlocking on the redb write lock (the reason `serve --stdio` was
   removed, #150). ✅
4. **Project-scoped and predictable** — palace names map deterministically to the
   project on disk; "which palace am I in?" is answerable from cwd alone. ✅
5. **Service-free and embeddable** — no cloud, no Python, no native SQLite/usearch
   chain on the default path; open-mpm can link the core in-process without axum. ✅
6. **Memory stays healthy over time** — the dream cycle dedups, prunes, and
   (optionally) semantically consolidates so recall quality does not degrade as a
   palace grows. ✅
7. **Operationally self-sufficient** — operators can check for updates, upgrade,
   and observe health (fd usage, version) without consulting external docs or
   tailing logs. ✅

**Core differentiator (restated):** trusty-memory is a single embedded MIT-licensed
Rust binary that unifies vector recall, a temporal knowledge graph, an
inter-project message bus, and idle-time LLM consolidation behind one MCP tool
surface — with a frontend/core split that lets the same engine be a standalone
daemon *or* an in-process library.

---

## 6. Open Questions & Roadmap

### Open questions

- **Tool-count doc drift:** the wire surface is now **24 tools** (per
  `tool_definitions_lists_all_tools`). The crate README should be updated to
  reflect this. The authoritative source remains `tool_definitions()`. 🟡
- **Orphaned-palace remediation:** `doctor --fix-palaces` is advisory only.
  Should it actually merge orphans into `personal` (FR-2.2)? 🔵
- **SQLite KG retirement (#47):** the legacy `sqlite-kg` path lingers for
  migration. When can it be removed entirely? 🟡
- **LRU palace-handle cache (#463):** each open palace holds ~3 redb files;
  with many palaces this can exhaust file descriptors even with the 8192 fd
  limit. Should handle lifetime be bounded by an LRU cache? 🔵
- **Leiden refinement:** community detection is Louvain-only; is phase-2
  refinement worth adding to reduce false-positive knowledge gaps (FR-6.6)? 🔵
- **Scope enforcement:** scopes are advertised, not enforced. Should the daemon
  ever enforce `memory.read`/`memory.write` locally, or is that always the
  orchestrator's job (§2 Non-Goals)? ⚪
- **Cross-machine sync:** explicitly a non-goal today — revisit if multi-host
  agent fleets need shared memory. ⚪
- **Broken README link (#398):** `trusty-memory-mcp` path in README.md still
  points to the wrong location. 🟡

### Roadmap (phased, from current gaps)

| Phase | Theme | Highlights |
|---|---|---|
| **Now** | Doc hygiene | Update README tool table to 24; fix the broken README link (#398). |
| **Phase 2** | Storage cleanup | Retire the legacy `sqlite-kg` path (#47) once migration tooling is no longer needed. |
| **Phase 3** | Palace lifecycle | Implement actual orphaned-palace merge into `personal` behind `doctor --fix` (FR-2.2/FR-10.4). |
| **Phase 3** | fd robustness | Implement lazy/LRU redb-handle cache (#463) to bound fd usage regardless of palace count. |
| **Later** | Retrieval/graph depth | Leiden phase-2 refinement for knowledge gaps (FR-6.6); richer KG auto-extraction beyond the heuristic table (FR-6.2). |
