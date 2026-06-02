# trusty-memory — Component Specifications

> **Status:** Canonical · Living Document
> **Last reviewed:** 2026-06-01
> **Derived from:** code/docs/tickets audit (v0.14.0)

**Status legend:** ✅ Implemented · 🟡 Partial · 🔵 Designed-not-built · ⚪ Aspirational

One section per major subsystem. Each states **Responsibility**, **Key
types/modules** (with `src/` paths), **Current state**, and **Known gaps**, framed
Vision / Current / Gap. For the frontend/core split and transport model see
[ARCHITECTURE.md](./ARCHITECTURE.md); for product framing see [PRD.md](./PRD.md).

Paths under `memory_core/` are in `crates/trusty-common/src/memory_core/`; all
others are in `crates/trusty-memory/src/`.

---

## 1. MCP Server / Tool Surface — `tools.rs`, `mcp_service.rs`, `openrpc.rs`

**Responsibility.** Expose memory + palace + KG operations as a stable MCP tool
contract, kept in sync across the `tools/list` payload, the in-process dispatcher,
and the OpenRPC `rpc.discover` manifest; let in-process hosts (open-mpm) register
the same tools without spawning a child.

**Key types/modules.**
- `tools.rs` — `MemoryMcpServer`, `tool_definitions()` / `tool_definitions_with(has_default)` (the **24-tool** payload), in-process dispatcher wired to the real `PalaceRegistry` + retrieval/KG APIs.
- `mcp_service.rs` — `MemoryMcpService`, a zero-sized `trusty_mcp_core::ServiceDescriptor` impl delegating to `tool_definitions_with` + `scopes_for_tool`.
- `openrpc.rs` — `scopes_for_tool` (read/write/knowledge.write split) + `build_discover_response` (OpenRPC 1.3.2 with `x-scopes`).

**Current state.** ✅ **24 tools**: the five memory ops, six palace ops, four
KG/alias ops, three prompt-facts ops, `memory_note`, `memory_send_message`,
`memory_recall_all`, and the new `upgrade` tool (#537). `--palace <name>` makes
the `palace` argument optional in every tool (`tool_definitions_with(has_default)`).
`MemoryMcpService` lets open-mpm merge these into a unified `rpc.discover` with no
glue code.

**Known gaps.**
- 🟡 **Tool-count doc drift:** the wire surface is **24** (per the
  `tool_definitions_lists_all_tools` test), but the crate `README.md` may still
  show an older curated table. The authoritative source is `tool_definitions()`.
- 🔗 Broken README link to `trusty-memory-mcp` path (#398).

---

## 2. HTTP API & Transport — `web.rs`, `service.rs`, `chat.rs`, `transport/`

**Responsibility.** Serve the full REST/SSE surface and the embedded UI over axum,
the JSON-RPC `POST /rpc` and UDS transports, and a chat/tool-calling loop — all on
top of one `AppState`, with HTTP handlers staying thin.

**Key types/modules.**
- `web.rs` — every `/api/v1/*` handler (status, palaces, drawers, recall, KG
  subjects/graph/triples/gaps/aliases, config, activity, dream, messages, logs),
  `/sse`, `/health` round-trip probe (dedicated probe palace, #185), `/rpc`, and
  the embedded-asset fallback.
- `service.rs` — `MemoryService`, a pure-logic facade over `AppState` returning
  `anyhow::Result<Value>` (extracted from `web.rs`, #151; no behaviour change) so
  HTTP handlers and chat dispatch share the same code paths.
- `chat.rs` — OpenRouter/Ollama SSE chat, the tool dispatcher, chat-session CRUD,
  and `/api/v1/messages*`.
- `transport/rpc.rs` — transport-agnostic JSON-RPC `dispatch` (`initialize`,
  `tools/list`, `tools/call`, `rpc.discover`, direct tool methods).
- `transport/uds.rs` — UDS listener + short-path socket resolution (≤104 bytes),
  `UDS_ADDR_FILE`, orphan cleanup (#153).

**Current state.** ✅ One daemon, three transports converging on `AppState`. axum
is gated behind the `axum-server` feature (default on; off for open-mpm rlib use,
#226). Dynamic port `7070..=7079` with discovery file. Multi-process concurrent
reads via snapshot copy (#59). `/health` now reports `open_fds`, `fd_soft_limit`
(fd-exhaustion observability, #464), and `update_available` (background version
check, #455). Graceful SIGTERM drain via `trusty_common::shutdown_signal()` (#534).
`mcp_bridge` reconnects with exponential backoff on socket-close (#535).
`port` CLI command for safe shell-substitution address lookup (#526).
Single-instance guard prevents launchd zombie herds (#464).

**Known gaps.** None material.

---

## 3. Palace Store & Data Model — `memory_core/palace.rs`, `registry.rs`, `store/`, `project_root.rs`

**Responsibility.** Own the spatial memory data model (Palace → Wing → Room →
Closet → Drawer), the concurrent multi-palace registry, durable per-palace storage,
and the project-anchored naming rules.

**Key types/modules.**
- `memory_core/palace.rs` — `PalaceId`, `Palace`, `Wing`, `RoomType`, `Room`, `Drawer`, `DrawerType`.
- `memory_core/registry.rs` — `PalaceRegistry` (`DashMap<PalaceId, Arc<PalaceHandle>>`).
- `memory_core/store/palace_store.rs` — `palace.json` + `identity.txt` atomic persistence.
- `memory_core/store/l1_cache.rs` — L1 essential-drawer snapshot.
- `memory_core/store/payload_store.rs`, `chat_sessions.rs` — redb sidecars (#46/#52/#56).
- `project_root.rs` — `project_slug()` + `personal` sentinel, plus `.trusty-tools/trusty-memory.yaml` pin-file management (#88, #446). `ProjectPin`, `PIN_FILE_REL`, `write_project_pin`, `read_project_pin`.
- `startup_scan.rs` — `scan_pin_map` builds `AppState::pin_project_map` at startup from a single directory-tree pass (#470).
- `attribution.rs` — `creator:*` tag namespace on every write (#202).

**Current state.** ✅ Full CRUD (`palace_create/list/info/update/delete/compact`);
`palace_delete` backs `DELETE /api/v1/palaces/{id}` (#180). Palace-as-project
enforcement on **new** palace creation; existing palaces grandfathered.
Palace slugs are now durably pinned in `.trusty-tools/trusty-memory.yaml` (#446),
surviving directory renames. The `trusty-memory link` CLI explicitly writes/refreshes
the pin file. The daemon startup scan (`startup_scan.rs`) builds the complete
`palace_id → project_path` map in one pass. `write_project_pin` is guarded against
writing into temp/home/root directories (#492).

**Known gaps.**
- 🔵 **Orphaned-palace remediation is advisory only** — `doctor --fix-palaces` /
  `doctor --fix` print rename suggestions but never mutate the filesystem; actual
  merge of orphans into `personal` is unbuilt (PRD FR-2.2/FR-10.4).
- 🟡 **LRU redb-handle cache unbuilt** — each palace holds ~3 open redb files;
  with many palaces this can exhaust file descriptors (EMFILE). Lazy/LRU caching
  of palace handles is tracked in #463; mitigated by the LaunchAgent fd-limit
  increase but not yet eliminated architecturally.

---

## 4. Retrieval & Ranking — `memory_core/retrieval.rs`, `filter.rs`, `decay.rs`, `analytics.rs`

**Responsibility.** Return the most relevant drawers cheaply via 4-layer
progressive retrieval, keep noise out at ingest, de-weight stale memories, and
close the feedback loop on recall usefulness.

**Key types/modules.**
- `memory_core/retrieval.rs` — L0 (identity), L1 (essential, cached ~900 tokens),
  L2 (on-demand vector + BM25), L3 (deep); the canonical `PalaceHandle`.
- `memory_core/filter.rs` — `FilterConfig` / `classify` / `apply` (#61) + rolling
  Jaro-Winkler dedup window (#220).
- `memory_core/decay.rs` — `effective_importance` (90-day half-life, access boost).
- `memory_core/analytics.rs` — redb-backed `RecallLog` (#57).

**Current state.** ✅ `memory_recall` (L0+L1+L2), `memory_recall_deep` (L3),
`memory_recall_all` (cross-palace). Hybrid lexical (BM25 sidecar) + dense (HNSW)
ranking with temporal decay. Ingest filter + dedup reject noise before storage.

**Known gaps.** None material at this time.

---

## 5. BM25 Sidecar Integration — `bm25_supervisor.rs`, `bin/bm25_daemon.rs`

**Responsibility.** Provide lexical (BM25) recall by bundling and auto-supervising
the external `trusty-bm25-daemon` so it works out of the box, one process per
palace.

**Key types/modules.**
- `bin/bm25_daemon.rs` — bundled `[[bin]]` shim delegating to `trusty_bm25_daemon::run()`.
- `bm25_supervisor.rs` — `Bm25Supervisor` keyed by palace id; `ensure_running` /
  `shutdown`; `Mutex<HashMap<String, ChildHandle>>` anti-double-spawn; SIGTERM→SIGKILL
  via `libc::kill`.

**Current state.** ✅ Single-install (`cargo install trusty-memory` → three
binaries, mirrors #190). On first BM25 use for a palace, the supervisor discovers
the binary, spawns a child with `--palace`/`--data-dir`, polls the socket, and owns
the child. `TRUSTY_BM25_EXTERNAL=1` opts out for operator-managed daemons.

**Known gaps.**
- 🟡 **Unix-only** — the spawn supervisor and daemon protocol are UDS, so there is
  no Windows path.

---

## 6. Knowledge Graph & Facts — `kg_extract.rs`, `bootstrap.rs`, `discovery.rs`, `prompt_facts.rs`, `memory_core/store/kg*.rs`, `community.rs`

**Responsibility.** Maintain a temporal triple store, populate it automatically on
write and at palace creation, surface hot facts into the model's working context,
and detect knowledge gaps.

**Key types/modules.**
- `memory_core/store/kg.rs` + `kg_redb.rs` + `kg_store.rs` + `kg_writer.rs` —
  redb-backed `KnowledgeGraph` / `Triple` with `valid_from`/`valid_to` (#44).
- `memory_core/store/kg_sqlite.rs` — legacy SQLite KG (`sqlite-kg` feature, #45).
- `kg_extract.rs` — deterministic `extract_triples` on write (#97).
- `bootstrap.rs` — `bootstrap_palace` / `scan_project` from project files (#60).
- `discovery.rs` — alias discovery → `is_alias_for` triples.
- `prompt_facts.rs` — `HOT_PREDICATES`, `PromptFactsCache`, `get_prompt_context`.
- `memory_core/community.rs` — Louvain community detection → knowledge gaps (#52).

**Current state.** ✅ `kg_assert`/`kg_query`/`kg_gaps`/`kg_bootstrap`/`add_alias`/
`discover_aliases` and the prompt-facts tools. Auto-extraction on every write keeps
graphs non-empty; bootstrap seeds from manifests; gap detection drives
consolidation. `memory_forget` cascade-deletes derived triples; surgical triple
delete via `DELETE /api/v1/palaces/{id}/kg/triples/{triple_id}` (#278).

**Known gaps.**
- 🟡 **Legacy SQLite KG lingers** behind `sqlite-kg` pending #47 retirement.
- 🔵 **Louvain-only** community detection — Leiden phase-2 refinement deferred.
- Auto-extraction is **heuristic-only** (tag/room/hashtag + a small pattern table);
  semantic extraction is left to the dream LLM pass.

---

## 7. Dream / Consolidation — `memory_core/dream.rs`, `semantic_consolidation.rs`

**Responsibility.** Keep a palace healthy as it grows: prune low-value/stale
drawers, dedup near-duplicates, compact storage, refresh closet indexes, and
(optionally) collapse paraphrases/aliases with an LLM.

**Key types/modules.**
- `memory_core/dream.rs` — `DreamConfig`, `DreamStats`, `Dreamer` (idle clock +
  `dream_cycle`: content-prune #222, dedup, prune, compaction, closet refresh).
- `memory_core/semantic_consolidation.rs` — `Inference` trait;
  `OpenRouterInference` / `OllamaInference` / `MockInference`; `SemanticConsolidator`
  (Alias / Merge / Flag actions, additive `superseded_by` lineage, SHA-256 response
  cache, per-cycle call budget) (#87).

**Current state.** ✅ Idle background dreaming + on-demand `memory_dream` /
`POST /api/v1/dream/run`. Semantic phase enabled when an inference backend is
available (OpenRouter > Ollama > disabled no-op); default model
`anthropic/claude-haiku-4-5`. `MockInference` keeps `cargo test` offline; live tests
are `#[ignore]`-gated.

**Known gaps.** None material.

---

## 8. Inter-Project Messaging — `messaging.rs`, `commands/send_message.rs`, `commands/inbox_check.rs`

**Responsibility.** Let one project deliver a message to another project's palace
inbox and surface unread messages at the recipient's next Claude Code session —
with no new schema and no IPC.

**Key types/modules.**
- `messaging.rs` — `msg:*` tag envelope (`v1`/`from`/`to`/`purpose`/`sent_at`/`read`),
  `slugify_for_palace` addressing (#99).
- `commands/send_message.rs` — `send-message` CLI → `POST /api/v1/messages`.
- `commands/inbox_check.rs` — `inbox-check` (installed as a `SessionStart` hook by
  `setup`): fetch unread → print as Markdown (Claude injects stdout) → atomically
  mark read.

**Current state.** ✅ A message is just a drawer with a tag envelope (no schema
change). Atomic compare-and-swap on the read flag prevents double-delivery under
concurrent sessions. Every `inbox-check` failure path exits 0 with empty stdout so
a missing/slow daemon never blocks session start. Replaces claude-mpm's Python
`/mpm-message` skill.

**Known gaps.**
- No central registry — sender and receiver agree on the repo slug **out of band**
  (a deliberate design choice, not a gap).

---

## 9. Embedded UI & Activity — `web.rs`, `activity.rs`, `prompt_log.rs`, `hook_emit.rs`

**Responsibility.** Give humans a browser dashboard (palace overview, live event
stream, manual dream, memory browsing, KG graph) with no Node at runtime, and a
persistent, source-tagged activity record across all write origins.

**Key types/modules.**
- `web.rs` — `rust-embed` of `ui/dist/`, `/sse` event stream, asset fallback.
- `activity.rs` — `ActivityLog` (redb, FIFO-capped at `MAX_ENTRIES`,
  `ActivitySource` = HTTP/MCP/Hook) (#96); paged drawer list w/ creator info (#184).
- `hook_emit.rs` — best-effort `POST /api/v1/activity/hook` from hook subprocesses.
- `prompt_log.rs` — enriched-prompt JSONL logger (daily + size-cap rotation,
  retention pruning, optional prompt hashing) (#105).

**Current state.** ✅ Svelte UI compiled at build time and embedded; the feed shows
historical entries on mount (not just live SSE) and captures writes from HTTP, MCP,
*and* hook origins — closing the "empty feed in a normal Claude Code session"
complaint, since hooks now emit via `hook_emit.rs`.

**Known gaps.**
- 🟡 The embedded UI requires `pnpm` at **build** time (or `SKIP_UI_BUILD`-style
  bypass for CI publish flows); runtime needs nothing.

---

## 10. `note` CLI & Hooks — `commands/note.rs`, `commands/prompt_context.rs`, `commands/inbox_check.rs`

**Responsibility.** Provide MCP-free write/read entry points for sub-agents and
Claude Code hooks, all best-effort and non-blocking.

**Key types/modules.**
- `commands/note.rs` — `trusty-memory note` → `POST /api/v1/remember` (detached
  `tokio::spawn`); prints `Queued.`, exits 0 even if the daemon is down.
- `commands/prompt_context.rs` — `UserPromptSubmit` hook: inject hot KG context.
- `commands/inbox_check.rs` — `SessionStart` hook: deliver unread messages (see §8).

**Current state.** ✅ Sub-agents (which inherit no MCP connection) get a writable
handle that needs no MCP plumbing; the endpoint queues the write so the CLI never
waits on redb or the content gates. Hook subprocesses populate the activity feed via
`hook_emit.rs`.

**Known gaps.** None material.

---

## 11. Setup, Service & Migration — `commands/setup.rs`, `service.rs` (launchd), `migrate.rs`, `kuzu_migrate.rs`, `doctor.rs`, `kg_rebuild.rs`

**Responsibility.** First-time install, OS service management, migration off the
legacy kuzu-memory server, and operational diagnostics.

**Key types/modules.**
- `commands/setup.rs` — launchd LaunchAgent install (macOS), embedder pre-warm,
  Claude settings patch (MCP entry + `prompt-context`/`inbox-check` hooks) (#86).
- `commands/service.rs` — macOS launchd lifecycle; plist now includes
  `SoftResourceLimits` / `HardResourceLimits` = 8192 (#464).
- `commands/migrate.rs` — `migrate kuzu-memory` (rewrite Claude MCP config, #278).
- `commands/kuzu_migrate.rs` — `migrate kuzu-data` (import `store.redb`; idempotent
  via SHA-256-derived UUIDs, #277).
- `commands/migrations.rs` — startup data migrations.
- `commands/doctor.rs` — palace/orphan audit + `--fix-palaces`; enhanced with
  startup-pin-map integration for faster orphan discovery (#470/#474/#475).
- `commands/kg_rebuild.rs` — rebuild the KG from drawers.
- `commands/{start,stop,monitor}.rs` — unified start/serve/stop (#83), monitor TUI.
- `commands/port.rs` — `trusty-memory port [--addr|--json]` — report daemon's live
  listening address without guessing the dynamic port (#526).
- `commands/upgrade.rs` — `trusty-memory upgrade [--check] [--yes]` — interactive
  version check + `cargo install` + daemon self-restart via launchd (#537/#539).
- `commands/link.rs` — `trusty-memory link [--path] [--slug] [--force]` —
  explicit palace-slug pin in `.trusty-tools/trusty-memory.yaml` (#446).
- `commands/single_instance.rs` — probe existing daemon at startup; exit 0 if
  healthy (prevents launchd zombie respawn loop, #464).

**Current state.** ✅ Idempotent setup with service-owned hooks; unified
start/serve/stop matching trusty-search; kuzu config + data migration; diagnostics;
`port` / `upgrade` / `link` operational CLI additions.

**Known gaps.**
- 🔵 `doctor --fix` is advisory only — no actual orphan remediation (see §3).
- 🟡 launchd path is macOS-specific; Linux relies on systemd/manual supervision.

---

## 12. Storage Backends (shared core) — `memory_core/store/`

**Responsibility.** Pure-Rust embedded persistence for every palace concern, with
no rusqlite/usearch native chain on the default path (the redb sweep, #43–#56).

**Key types/modules.**
- Vector: `hnsw_store.rs` (redb-backed `hnsw_rs`), `vector.rs` (`VectorStore` trait,
  `UsearchStore` name preserved for back-compat) (#50/#51).
- KG: `kg.rs` / `kg_redb.rs` / `kg_store.rs` / `kg_writer.rs` (redb, #44);
  `kg_sqlite.rs` (legacy, `sqlite-kg`).
- Metadata/cache: `palace_store.rs`, `l1_cache.rs`.
- Sidecars: `payload_store.rs` (#46/#52), `chat_sessions.rs` (#56).
- Concurrency: `concurrent_open.rs` (snapshot-on-lock, #59).
- Import: `kuzu.rs` (read-only; `kuzu` feature for real Cypher).
- Embeddings: `embed.rs` (re-export of unified `FastEmbedder`, 384-dim).

**Current state.** ✅ All default-path storage is redb + HNSW + JSON snapshots.
Async ops run on `spawn_blocking` so the reactor isn't stalled.

**Known gaps.**
- 🟡 SQLite KG code retained behind `sqlite-kg` pending #47 removal.

---

## 13. Bug-Capture & Update-Check — `trusty-common` `error_capture/`, `update/`

**Responsibility.** Surface runtime errors to operators for bug reporting, and
notify users when a new release is available — enabling self-service upgrades.

**Key types/modules.**
- `trusty-common/src/error_capture/` — `BugCaptureLayer` (tower middleware),
  `ErrorStore` (in-memory ring + JSONL file, fingerprint-deduped, SHA-256 keyed),
  `types.rs` (`CapturedError`). Enabled via `bug-capture` feature flag in
  `trusty-common`; trusty-memory pulls it in via its Cargo.toml feature list. The
  `AppState::error_store: Option<ErrorStore>` field is populated by
  `AppState::with_error_store(...)` at daemon startup (#478/#490).
- `trusty-common/src/update/mod.rs` — `check_crates_io`, `UpdateInfo`; 24-h
  throttled crates.io poller; wired into daemon startup to populate
  `AppState::update_available` (#455).
- `trusty-common/src/update/upgrade.rs` — `upgrade_and_restart`,
  `perform_upgrade`, `verify_installed_binary`, `is_launchd_supervised`; called by
  both the CLI `upgrade` command and the `upgrade` MCP tool (#537/#539).

**Current state.** ✅ Bug-capture layer active in the trusty-memory daemon.
`/health` exposes `update_available`. `upgrade` CLI and MCP tool drive the full
check → install → launchd-self-restart pipeline. Background version check at
startup (throttled) keeps the health payload fresh without per-request crates.io
calls.

**Known gaps.**
- `BugCaptureLayer` captures 500-level HTTP errors but not panics from
  `tokio::spawn` tasks that are not connected to an HTTP handler.
