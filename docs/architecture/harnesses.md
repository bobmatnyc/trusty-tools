# Three-Harness Architecture — trusty-tools

**Status:** Accepted
**Version:** v1
**Subsystem:** HARNESSES
**Owner:** Engineering / Architecture
**Last-updated:** 2026-06-05
**Related:** [ADR-0004](../adr/0004-three-harnesses-shared-event-driven-common.md),
[trusty-code](../../crates/trusty-code/README.md),
[trusty-mpm](../../crates/trusty-mpm/README.md),
[open-mpm](../../crates/open-mpm/README.md)

---

## Purpose & Scope

The trusty-tools workspace organises its AI orchestration crates into **three
distinct harnesses**, each serving a different principal and carrying different
responsibilities. This document defines those boundaries, their shared
foundation in `trusty-common`, the event-driven mandate that applies to all
three, and the inter-harness delegation graph.

**In scope:** harness identity, responsibilities, shared surface, event model,
and delegation edges. **Out of scope:** internal implementation details of any
single harness, tool lists, model routing specifics, and per-crate release
procedures (those live in the crate-level README and `docs/<crate>/`).

---

## Table of Contents

| Section | Topic |
|---------|-------|
| [Harness Definitions](#harness-definitions) | The three harnesses: purpose, analogy, scope |
| [Comparison Table](#comparison-table) | Side-by-side harness summary |
| [Shared Foundation — trusty-common](#shared-foundation--trusty-common) | What belongs to all harnesses |
| [Event-Driven Mandate](#event-driven-mandate) | Binding architectural principle + event model |
| [Inter-Harness Delegation](#inter-harness-delegation) | Call graph, boundaries, transport |
| [Accepted Decision — Boundary Between trusty-mpm and trusty-agents](#accepted-decision--boundary-between-trusty-mpm-and-trusty-agents) | Owner-accepted boundary split + Elastic-2.0 license |
| [Current Status vs. Target State](#current-status-vs-target-state) | What exists today and what is planned |

---

## Harness Definitions

### 1. trusty-code — the Coding Harness

**Crate:** `crates/trusty-code/` — package `-p trusty-code`, binary `tcode`
**Analogy:** Claude Code — a per-project coding orchestration harness.

**Purpose:** Provides the per-project Claude-Code-compatible MPM orchestration
entry point. One `tcode serve` process runs per project root (identified by a
`.claude/` directory). It runs the PM main loop, enforces the mandatory
workflow (research → plan → implement → verify), and delegates authority to
typed coding sub-agents (engineer, QA, ticketing, security) via MCP and/or
subprocess IPC.

**What it owns:**
- The `tcode` binary and its IPC socket / HTTP endpoint
- Per-project agent configuration (`.claude/agents/<name>.toml`)
- Per-project skill injection (`.claude/skills/`)
- Per-project workflow definitions (`.claude/workflows/<name>.toml`)
- The PM main-loop for code-generation, edit, run, test cycles
- Integration with Claude Code hooks (pre-tool-use, post-tool-use, stop)

**What it does not own:**
- Multi-project session management (that is trusty-mpm)
- Non-coding assistant workflows (HR, CRM, scheduling — trusty-agents)
- The daemon / TUI / Telegram transports (trusty-mpm)
- Search, memory, or analysis infrastructure (trusty-search / trusty-memory / trusty-analyze / trusty-common)

**Current state:** Phase 0 scaffold (`crates/trusty-code/src/main.rs` lines 1-107,
`src/lib.rs` lines 1-62). The `tcode` binary parses its CLI surface (`serve`,
`run-task`, `run-workflow`) but every subcommand stubs out with "not yet
implemented (#587 Phase N)". Full extraction from `open-mpm` is tracked in
epic #587.

---

### 2. trusty-mpm — the Meta-Harness

**Crate:** `crates/trusty-mpm/` — package `-p trusty-mpm`,
binaries: `tm` / `trusty-mpm` (CLI), `trusty-mpmd` (daemon), `trusty-mpm-tui`,
`trusty-mpm-telegram`
**Analogy:** Claude MPM — a PM-style multi-agent orchestrator *over* coding work.

**Purpose:** Manages multi-project, multi-session AI workflows. It is the
"operator's control plane": it runs the background daemon, relays hooks, tracks
circuit-breaker state, exposes an MCP server to Claude Code sessions, and
provides the TUI dashboard and Telegram bot for human oversight. It delegates
coding tasks downward to `tcode` (trusty-code) instances; it does not execute
code itself.

**What it owns:**
- `trusty-mpmd`: the always-on background daemon (HTTP API, hook relay, session
  registry, watcher — `crates/trusty-mpm/src/daemon/`)
- `tm` / `trusty-mpm` CLI: session control, service discovery, agent deploy
- TUI: `ratatui`-based coordinator dashboard
- Telegram bot: async operator notifications
- MCP server: nine orchestration tools exposed to Claude Code sessions
  (`session_list`, `session_status`, `agent_delegate`, `memory_protect`,
  `circuit_breaker_status`, `list_recent_errors`, `preview_bug_report`,
  `report_bug`, `hook_event` — `crates/trusty-mpm/src/mcp/`)
- Session overseer + circuit breaker logic (`crates/trusty-mpm/src/core/overseer.rs`,
  `src/core/circuit.rs`)
- Cross-project session registry (`crates/trusty-mpm/src/core/session_store.rs`)
- The `OrchestratorBackend` trait that binds MCP ↔ daemon
  (`crates/trusty-mpm/src/mcp/mod.rs`)

**What it does not own:**
- Per-project coding execution (delegated to trusty-code)
- General knowledge-worker assistant workflows (trusty-agents)
- Search / memory / analysis infrastructure

---

### 3. trusty-agents — the Agentic Harness

**Crate:** `crates/open-mpm/` (current name; planned rename to `trusty-agents`)
**Package:** `-p open-mpm`, binaries: `open-mpm` / `ompm` (REPL + API server)
**Analogy:** OpenClaw / Hermes — a general agentic harness for knowledge-worker tasks.

**Purpose:** Provides a general-purpose agentic harness for non-coding workflows:
CRM interaction, HR queries, scheduling, knowledge retrieval, memory management,
communications (Slack, Telegram), and any domain-specific assistant persona. It
uses the same PM-orchestrator-plus-sub-agent subprocess pattern as trusty-code
but is NOT a coding harness. It is the integration point for external MCP
services (`mcp_service_tools.rs`) and exposes configurable personas (Izzie,
custom) via TOML agent definitions.

**What it owns:**
- PM orchestrator main loop with in-process `delegate_to_agent` tool
  (`crates/open-mpm/src/ctrl/`)
- Intent classifier for fast-pathing (conversational / research / implementation)
  (`crates/open-mpm/src/intent/`)
- MCP service bridge: wraps any MCP server's tools as `ToolExecutor` instances
  (`crates/open-mpm/src/tools/mcp_service_tools.rs`)
- Tool registry + RBAC + identity primitives (`crates/open-mpm/src/tools/`,
  `src/rbac/`, `src/identity/`)
- Persona system (agent TOML + skill injection)
- Transports: REPL (`src/repl/`), HTTP API (`src/api/`), Slack (`src/slack/`),
  Telegram (`src/telegram/`)
- Harness adapter detection (recognises Claude Code, open-mpm, claude-mpm, Codex,
  Gemini panes — `src/adapters/`)
- Process-global event bus + SSE relay (`crates/open-mpm/src/events.rs`,
  `src/bus/`)
- Workflow engine for complex declarative pipelines (`crates/open-mpm/src/workflow/`)
- Agent plugin API (`crates/open-mpm-agent-api/`) for external agent injection

**What it does not own:**
- Per-project coding workflow enforcement (trusty-code)
- Multi-project session management / circuit breaker / hook relay (trusty-mpm)
- Search infrastructure (trusty-search)
- Memory storage engine (trusty-common `memory-core` feature + trusty-memory)

**Planned rename:** `open-mpm` → `trusty-agents`. See
[ADR-0004](../adr/0004-three-harnesses-shared-event-driven-common.md) §Decision
for the rationale.

---

## Comparison Table

| Dimension | trusty-code | trusty-mpm | trusty-agents |
|-----------|-------------|------------|---------------|
| **Analogy** | Claude Code | Claude MPM | OpenClaw / Hermes |
| **Primary user** | Developer / CI pipeline | Operator / PM role | Knowledge worker |
| **Scope** | One project, coding tasks | Many projects, orchestration control | Any domain, non-coding workflows |
| **Main binary** | `tcode` | `tm` / `trusty-mpmd` | `open-mpm` / `ompm` |
| **Core loop** | PM main loop (per project) | Session daemon + hook relay | PM main loop (per persona) |
| **Agent model** | Coding sub-agents (engineer, QA, ticketing) | Overseer / delegation authority | Domain personas + MCP bridge |
| **Tool source** | Claude Code tools + project skill files | Orchestration tools (MCP) | Any MCP service + native tools |
| **Transports** | IPC socket / HTTP | HTTP API / MCP / TUI / Telegram | REPL / HTTP API / Slack / Telegram |
| **What it delegates** | Sub-agents within project | Coding work to trusty-code | Coding tasks to trusty-code; managed projects to trusty-mpm |
| **What it does not do** | Multi-project management | Execute code directly | Manage multi-project sessions |
| **Crate status** | Phase 0 scaffold | Production | Production |
| **Current crate name** | `trusty-code` | `trusty-mpm` | `open-mpm` (rename planned) |

---

## Shared Foundation — trusty-common

**Binding principle:** Shared commonality lives in `trusty-common`. All three
harnesses build their specialisation on top of it.

`trusty-common` (`crates/trusty-common/`) is the cross-harness foundation. Its
module surface is organised behind feature flags so each harness pulls in only
what it needs. The following are relevant to all three harnesses:

### Always-on (no feature flag required)

| Module | Role for harnesses |
|--------|--------------------|
| `chat` | OpenRouter / Anthropic chat-completions client — the LLM call abstraction all three harnesses use |
| `claude_config` | Reads `.claude/` configuration (agents, CLAUDE.md, permissions) — shared between trusty-code and trusty-mpm |
| `project_discovery` | Locates project roots from a working directory |
| `shutdown` | SIGTERM + SIGINT graceful-shutdown signal — every daemon in all three harnesses uses this |
| `log_buffer` | Bounded in-memory log ring buffer for `/logs/tail` endpoints |
| `sys_metrics` | RSS / CPU sampling for `/health` endpoints |

### Feature-gated modules relevant to all three harnesses

| Feature | Module | Purpose |
|---------|--------|---------|
| `mcp` | `trusty_common::mcp` | JSON-RPC 2.0 / MCP primitives (envelope types, stdio dispatch loop, OpenRPC discovery). Every MCP server in the workspace imports from here. |
| `rpc` | `trusty_common::rpc` | General-purpose JSON-RPC client + stdio/HTTP transports |
| `memory-core` | `trusty_common::memory_core` | Memory palace storage engine (palace, note, retrieval) — trusty-memory's backend; also used by trusty-agents |
| `tickets` | `trusty_common::tickets` | Issue-tracker integration primitives |
| `symgraph` | `trusty_common::symgraph` | Knowledge-graph data types (EntityType, RawEntity, EdgeKind) — used by trusty-search |
| `embedder` | `trusty_common::embedder` | Text-embedding abstraction (Embedder trait, FastEmbedder) — used by trusty-search and trusty-analyze |
| `bm25` | `trusty_common::bm25` | BM25 lexical index — used by trusty-search |
| `axum-server` | `trusty_common::server` | Shared axum middleware (CORS, trace, gzip) — every HTTP daemon uses this |
| `migrations` | `trusty_common::migrations` | Schema migration kernel shared by data-layer crates |

### What SHOULD migrate to trusty-common (target state)

The following currently live in `open-mpm` but belong in `trusty-common` as
shared infrastructure so all three harnesses can use them without taking a
dependency on a peer harness crate:

1. **Tool registry and RBAC primitives** (`open-mpm/src/tools/traits.rs`,
   `src/rbac/`, `src/identity/`) — the `ToolExecutor` trait, `ServiceTier` RBAC
   enum, and `UserIdentity` are already partially extracted into
   `open-mpm-agent-api` to break the cargo cycle. The next step is migrating
   them into `trusty-common` behind a `tool-registry` feature flag so
   trusty-code can use them without depending on `open-mpm`.
2. **Shared event types** — see the [Event-Driven Mandate](#event-driven-mandate)
   section below.
3. **Intent classifier** (`open-mpm/src/intent/`) — the heuristic
   `IntentClass` (Conversational / Research / Implementation) is a
   cross-harness primitive.

---

## Event-Driven Mandate

**Binding architectural principle: all three harnesses are event-driven.**

An agent harness that blocks synchronously at each step is fragile: it cannot
fan out to multiple sub-agents, cannot stream progress to a UI, and cannot relay
events across process boundaries. The trusty-tools architecture requires that
every harness publishes and subscribes to a well-defined event stream rather than
operating in pure request-response mode.

### Shared Event Model

The shared event model lives (or will live) in `trusty-common` behind an `events`
feature flag. It consists of:

**Event envelope (wire format):**
```rust
// Target: trusty_common::events::HarnessEvent (to be extracted from open-mpm)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HarnessEvent {
    // Session lifecycle
    SessionStarted   { session_id: String, project: String, harness: HarnessKind },
    SessionFinished  { session_id: String, exit_code: i32 },
    SessionError     { session_id: String, message: String },

    // Agent lifecycle  
    AgentStarted     { session_id: String, agent: String },
    AgentFinished    { session_id: String, agent: String, elapsed_ms: u64 },
    AgentMessage     { session_id: String, agent: String, content: String },

    // Tool execution
    ToolCall         { session_id: String, tool: String, args: serde_json::Value },
    ToolResult       { session_id: String, tool: String, success: bool },

    // PM / workflow
    WorkflowPhase    { session_id: String, phase: String },
    DelegationStarted{ session_id: String, target_harness: HarnessKind, task: String },
    DelegationResult { session_id: String, target_harness: HarnessKind, success: bool },

    // Infrastructure
    Ping             { session_id: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HarnessKind { TrustyCode, TrustyMpm, TrustyAgents }
```

**Transport mechanisms (per harness boundary):**

| Boundary | Transport | Implementation |
|----------|-----------|----------------|
| Within a harness process | `tokio::sync::broadcast` | `open-mpm/src/events.rs` (EVENT_BUS OnceLock); to be replicated in trusty-mpm daemon |
| Harness → UI (browser/TUI) | Server-Sent Events (SSE) over HTTP | open-mpm API `/events` endpoint; trusty-mpmd SSE stream |
| trusty-mpm → trusty-code | HTTP POST to `tcode serve` endpoint | Planned in trusty-code Phase 1 (#587) |
| Subprocess → parent | Stderr relay (`__OMPM_EVENT__ <json>`) | `open-mpm/src/events.rs` EVENT_LINE_PREFIX |
| Cross-project (open-mpm) | Unix Domain Socket NDJSON bus | `open-mpm/src/bus/mod.rs` MessageBus |

### Current Event-Driven Status

| Harness | Event bus today | Gap |
|---------|-----------------|-----|
| **trusty-agents (open-mpm)** | Full: `EVENT_BUS` broadcast channel + SSE relay + UDS MessageBus; `Event` enum covers full PM lifecycle (`src/events.rs`, `src/bus/mod.rs`) | Events not yet typed in shared `trusty-common`; bus types are crate-private |
| **trusty-mpm** | Partial: hook relay and daemon state changes fire events; no process-global broadcast bus exposed to MCP layer | Missing: typed event bus in daemon; MCP notifications not streamed |
| **trusty-code** | None: Phase 0 stub | Requires full implementation in Phase 1+ (#587) |

**Required work to close the gap:**
1. Extract `open-mpm`'s `Event` enum (with `HarnessKind` tag) into `trusty-common::events`.
2. Add a `broadcast::Sender<HarnessEvent>` to `trusty-mpmd`'s daemon state.
3. Implement event emission in trusty-code Phase 1.

---

## Inter-Harness Delegation

### Delegation Graph

```
                              ┌────────────────────┐
                              │   Human / Operator │
                              └─────────┬──────────┘
                                        │ (chat, Telegram, TUI, Slack)
                                        ▼
                   ┌────────────────────────────────────────┐
                   │        trusty-agents (open-mpm)        │
                   │  General agentic harness               │
                   │  - Intent classifier                   │
                   │  - MCP service bridge                  │
                   │  - Domain personas (Izzie, custom)     │
                   └──────────────┬─────────────────────────┘
                                  │ (1) coding task
                                  │ (2) managed multi-agent project
                          ┌───────┴────────────┐
                          │                    │
                          ▼                    ▼
        ┌─────────────────────────┐   ┌──────────────────────────────┐
        │  trusty-code (tcode)    │   │  trusty-mpm (tm / mpmd)      │
        │  Coding harness         │   │  Meta-harness (PM control)   │
        │  - PM main loop         │◄──│  - Multi-project sessions    │
        │  - coding sub-agents    │   │  - Circuit breaker / overseer│
        │  - per-project config   │   │  - Hook relay                │
        │  - workflows            │   │  - MCP server to Claude Code │
        └─────────────────────────┘   └──────────────────────────────┘
                   ▲
                   │ (3) session delegation
                   │
        ┌──────────┴───────────────────────────────────────────────┐
        │                    trusty-mpm                            │
        │  delegates coding work to tcode; monitors its sessions   │
        └──────────────────────────────────────────────────────────┘
```

### Delegation Edges (Defined)

| From | To | Trigger | Transport | What is passed | What is returned |
|------|----|---------|-----------|----------------|-----------------|
| **trusty-agents** | **trusty-code** | Intent classified as `Implementation` — a coding task | HTTP POST to `tcode serve` IPC endpoint (Phase 1) OR CLI subprocess `tcode run-task <agent> <task>` | Task description, agent name, optional context | Result text, exit code |
| **trusty-agents** | **trusty-mpm** | Multi-agent project requiring session management, QA enforcement, or hook relay | HTTP POST to `trusty-mpmd` API OR MCP tool `agent_delegate` | Task description, workflow name, session constraints | Session ID, delegation ID, result |
| **trusty-mpm** | **trusty-code** | New coding session created or existing session overseen | Session launch via `session_launch::SessionLaunchConfig` (`crates/trusty-mpm/src/core/session_launch.rs`) → spawns `tcode serve` subprocess | Project path, agent config, model tier, overseer policy | Session ID; events relayed via hook relay |

### Delegation Contracts

**trusty-agents → trusty-code (coding delegation):**
- Inputs: `{ agent: String, task: String, project_path: PathBuf, context: Option<String> }`
- Outputs: `{ result: String, exit_code: i32, session_id: String }`
- Precondition: `tcode serve` must be running for `project_path`; or trusty-agents
  spawns a transient `tcode` instance
- Error: timeout, agent not found, or compilation failure — returned as
  `DelegationResult { success: false }`

**trusty-agents → trusty-mpm (managed project delegation):**
- Inputs: MCP tool call `agent_delegate(session_id, agent, task, tier?)`
- Outputs: `{ delegation_id: String }` (async — caller subscribes to events
  for completion)
- Precondition: `trusty-mpmd` running; session established
- Error: circuit-breaker open, session not found — MCP error response

**trusty-mpm → trusty-code (session management):**
- Inputs: `SessionLaunchConfig { project_path, agent_names, overseer_policy, model_tier }`
- Outputs: `SessionId` registered in session store; events relayed via hook relay
- Precondition: `tcode` binary on PATH; project has `.claude/` config
- Error: binary not found, project not initialised — `SessionStatus::Error`

### What is NOT a cross-harness delegation

- trusty-search, trusty-memory, trusty-analyze are **tool-layer services**, not
  harnesses. All three harnesses call them over HTTP/MCP as tools; this is not
  delegation in the harness sense.
- trusty-code calling its own sub-agents (engineer, QA, ticketing) is
  **intra-harness** delegation, not cross-harness.

---

## Accepted Decision — Boundary Between trusty-mpm and trusty-agents

**Status: ACCEPTED — boundary and license recorded; implementation proceeds
under epic #587 and the P0 rename.**

### The Problem

Both `open-mpm` (trusty-agents) and `trusty-mpm` carry orchestration/PM-style
machinery that overlaps:

| Capability | open-mpm location | trusty-mpm location |
|------------|-------------------|---------------------|
| PM main loop | `src/ctrl/` (agent orchestrator REPL) | Not present (delegates to tcode) |
| Workflow engine | `src/workflow/` (phase-based pipelines) | Not present |
| Multi-agent session management | `src/session*.rs`, `src/session/` | `src/core/session_store.rs`, `src/daemon/` |
| Intent classification | `src/intent/` (heuristic classifier) | Not present |
| Tool registry + RBAC | `src/tools/`, `src/rbac/` | Not present (uses Claude Code's tool layer) |
| Harness adapter detection | `src/adapters/` | Implicit (via hook relay) |
| MCP service bridge | `src/tools/mcp_service_tools.rs` | Implicit (Claude Code sessions invoke MCP servers directly) |
| Event bus | `src/events.rs`, `src/bus/` | Partial (hook relay only) |

### The Accepted Line

**PM / workflow / multi-agent orchestration belongs to trusty-mpm (the meta-harness).
General non-coding assistants belong to trusty-agents.**

More precisely (all items below are accepted; Cargo.toml updates happen during
the P0 rename, not in this PR):

1. **`open-mpm/src/workflow/`** — the `WorkflowEngine` and declarative phase
   pipelines (`WorkflowDef`, `PhaseDef`, parallel phases, worktree management,
   autopush) are **generic orchestration logic**.
   **Decision: migrate to trusty-mpm.**

2. **`open-mpm/src/ctrl/`** (the PM orchestrator loop) — the subset that drives
   coding agents belongs in `trusty-code` (epic #587); the subset that drives
   knowledge-worker personas stays in trusty-agents. The split follows intent:
   coding agent → trusty-code; domain persona (Izzie, CRM assistant) → trusty-agents.
   **Decision: split by agent type as part of #587 extraction.**

3. **`open-mpm/src/session_registry.rs`** (cross-project session registry) —
   session management at the multi-project level belongs to `trusty-mpm`.
   Per-conversation session state intrinsic to a single knowledge-worker
   conversation stays in trusty-agents.
   **Decision: trusty-mpm owns cross-project sessions; trusty-agents owns
   per-conversation session state for its own loops.**

4. **`open-mpm/src/tools/`, `src/rbac/`, `src/identity/`** — the
   `ToolExecutor` trait + `ServiceTier` RBAC + `UserIdentity` are shared primitives.
   **Decision: migrate to `trusty-common` behind a `tool-registry` feature flag.**

5. **`open-mpm/src/events.rs`, `src/bus/`** — the event bus belongs to
   `trusty-common` so all harnesses can share it.
   **Decision: migrate to `trusty-common::events`.**

6. **`open-mpm/src/intent/`** — the intent classifier is a shared primitive.
   **Decision: migrate to `trusty-common::intent`.**

7. **`open-mpm/src/adapters/`** (harness adapter detection) — trusty-mpm-level
   functionality (recognising which harness is in a tmux pane).
   **Decision: migrate to trusty-mpm.**

### What Stays Exclusively in trusty-agents After the Split

- MCP service bridge (`mcp_service_tools.rs`) — wrapping external MCP servers as
  tool executors for knowledge-worker personas
- Domain persona definitions (Izzie and custom TOML personas)
- Knowledge-worker tool set: `granola_*`, `gmail_*`, calendar, web search, memory
  read/write, ticketing
- REPL, HTTP API, Slack, Telegram transports for knowledge-worker interaction
- Per-conversation session state and interaction log
- Intent classifier (until extracted to trusty-common)

### License Decision — ACCEPTED

**trusty-agents (crates/open-mpm/, planned rename) and its companion crates
(`trusty-agents-api`, `trusty-agents-local`) are licensed under Elastic
License 2.0, matching trusty-search. This is NOT MIT.**

This decision is recorded in
[ADR-0004](../adr/0004-three-harnesses-shared-event-driven-common.md) under
"Accepted Decisions". The `license` field in the three Cargo.toml files will be
set to `"Elastic-2.0"` during the P0 rename PR — it is not changed in this
documentation-only PR.

### Remaining Open Questions

The boundary and license decisions are now settled. The following questions
remain open and will be addressed in subsequent PRs or the #587 implementation:

1. **Model strategy:** default model routing policy for trusty-agents personas.
2. **Relationship to duetto-intelligence gateway:** tool vs. embedded integration.
3. **open-mpm-agent-api retirement timing:** shim or immediate deprecation once
   `trusty-common::tool-registry` is ready.
4. **v1 assistant scope:** minimal useful v1 trusty-agents without tcode extracted.
5. **Rename timing vs. #587:** dedicated rename PR before vs. within #587 Phase 1.

See [ADR-0004](../adr/0004-three-harnesses-shared-event-driven-common.md) for
the full decision record.

---

## Current Status vs. Target State

| Harness | Today | Target |
|---------|-------|--------|
| **trusty-code** | Phase 0: CLI stub only | Full coding PM loop, extracted from open-mpm (#587) |
| **trusty-mpm** | Production: daemon, MCP, TUI, Telegram | + Typed event bus; + session delegation to tcode; + workflow engine (migrated from open-mpm) |
| **trusty-agents (open-mpm)** | Production: PM loop, intent router, MCP bridge, personas | Renamed `trusty-agents`; coding-specific ctrl extracted to tcode; shared primitives migrated to trusty-common |
| **trusty-common** | Foundation: chat, mcp, rpc, memory-core, embedder, symgraph, bm25 | + `events` feature (shared event types); + `tool-registry` feature (ToolExecutor, RBAC); + `intent` module |

---

## References

- [ADR-0004](../adr/0004-three-harnesses-shared-event-driven-common.md) — records
  the three-harness + event-driven + shared-trusty-common decisions
- `crates/trusty-code/src/main.rs` — Phase 0 CLI surface (lines 1-107)
- `crates/trusty-code/src/lib.rs` — Phase 0 library skeleton (lines 1-62)
- `crates/open-mpm/src/events.rs` — current event bus + Event enum
- `crates/open-mpm/src/bus/mod.rs` — UDS inter-project message bus
- `crates/open-mpm/src/intent/mod.rs` — intent classifier
- `crates/open-mpm/src/tools/mcp_service_tools.rs` — MCP service bridge
- `crates/open-mpm/src/workflow/mod.rs` — workflow engine
- `crates/trusty-mpm/src/mcp/mod.rs` — OrchestratorBackend trait + MCP tools
- `crates/trusty-mpm/src/core/overseer.rs` — OverseerDecision + Overseer trait
- `crates/trusty-mpm/src/core/session_store.rs` — cross-project session registry
- `crates/trusty-common/src/lib.rs` — feature-gated shared modules
- Epic #587 — trusty-code extraction phases
