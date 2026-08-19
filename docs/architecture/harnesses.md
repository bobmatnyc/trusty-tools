# Three-Harness Architecture — trusty-tools

**Status:** Accepted
**Version:** v2
**Subsystem:** HARNESSES
**Owner:** Engineering / Architecture
**Last-updated:** 2026-08-07

---

## Purpose & Scope

The trusty-tools workspace organises its AI orchestration crates into **three
harnesses**, each serving a different principal. This page defines what each
one is, the two shared crates all three build on, how events flow, and how a
harness hands work to another harness.

**In scope:** harness identity, responsibilities, shared surface, event
streams, and the delegation edges that exist in source today.

**Out of scope:** per-harness internals, tool inventories, model routing, and
release procedures. Those live in each harness's own documentation
([trusty-mpm](../trusty-mpm/README.md), [trusty-code](../trusty-code/README.md),
[trusty-agents](../trusty-agents/README.md)).

Every claim below is stated against the crate source in `crates/`. Where the
source does not settle a question, the page says so rather than guessing —
see [Known gaps](#known-gaps).

---

## Table of Contents

| Section | Topic |
|---------|-------|
| [Harness Definitions](#harness-definitions) | The three harnesses: purpose, binary, scope |
| [Comparison Table](#comparison-table) | Side-by-side summary |
| [Shared Foundations](#shared-foundations) | The two crates all three build on |
| [Event Streams](#event-streams) | The event-driven principle and where each harness stands |
| [Inter-Harness Delegation](#inter-harness-delegation) | Which harness invokes which, and how |
| [Known gaps](#known-gaps) | What this page does not establish |

---

## Harness Definitions

### 1. trusty-code — the coding harness

**Crate:** `crates/trusty-code/` — package `trusty-code`, binary `tcode`
**Analogy:** Claude Code — a per-project coding orchestration harness.

A `tcode serve` process runs the PM main loop for one project, reading that
project's `.claude/` configuration the way Claude Code does: agents, skills,
MCP descriptors, `CLAUDE.md`, and permission grants. It is single-instance per
project root — multiple CLI and TUI clients attach to the one daemon. Serving
without a project is also a first-class state, used for chat and planning
before a project is chosen.

Clients speak JSON-RPC 2.0 to it, over either NDJSON on stdio (`serve --stdio`)
or loopback HTTP (`serve --http`, `POST /rpc` plus `GET /health`). Exactly one
transport is selected per process. The method surface covers session lifecycle
(`session.create`, `session.status`, `session.send`, `session.cancel`,
`session.get_transcript`), task execution (`task.run`), workstreams, and
agent/skill/filesystem introspection. Session events reach an attached client
as a `session.event` notification on the stdio transport, or as an SSE stream at
`GET /sessions/{id}/events` — which replays a session's backlog before going
live — on the HTTP transport.

**What it owns:**

- The `tcode` binary, its JSON-RPC surface, and the sessions it holds
- Per-project agent configuration, read from `.claude/agents/<name>.md`
  (markdown with YAML frontmatter — the older TOML loader was removed)
- Per-project skill loading from `.claude/skills/`
- Workstreams: durable named groupings of sessions bound to the daemon's project
- Per-agent model routing across AWS Bedrock and OpenRouter
- The PM main loop for code-generation, edit, run, and test cycles

**What it does not own:**

- Multi-project session management (trusty-mpm)
- Non-coding assistant workflows (trusty-agents)
- Claude Code hooks. `tcode` is API/CLI/TUI-driven and has no hooks support;
  hooks are a Claude Code shell-level feature, and `tcode` operates above that
  layer via its event bus.
- Search, memory, or analysis infrastructure (trusty-search / trusty-memory /
  trusty-analyze / trusty-common)

`tcode run-workflow` is declared in the CLI but not implemented — it exits
non-zero with an explanatory message. Treat declarative workflow execution as
absent from this harness today.

---

### 2. trusty-mpm — the meta-harness

**Crate:** `crates/trusty-mpm/` — package `trusty-mpm`, binaries `tm` and
`trusty-mpm` (the same program under two names)
**Analogy:** Claude MPM — a PM-style multi-agent orchestrator *over* coding work.

trusty-mpm is the operator's control plane for multi-project, multi-session
work. The daemon, TUI, Telegram bot, Slack integration, and MCP bridge are all
subcommands of the one `tm` binary, not separate executables:

| Surface | Invocation |
|---|---|
| Background daemon (loopback HTTP API) | `tm daemon`, or `tm start` / `tm serve` |
| MCP stdio bridge for Claude Code | `tm serve --stdio` |
| Terminal dashboard | `tm tui` |
| Telegram bot | `tm telegram` |
| Slack integration | `tm slack` |
| Unattended fleet supervisor | `tm supervisor` |

`tm serve --stdio` is a thin proxy: it forwards JSON-RPC from an MCP client to
the daemon's loopback `POST /rpc`, auto-starting the daemon if needed. This is
the form wired into `.mcp.json`. The daemon is the durable process; the bridge
is stateless.

**What it owns:**

- The always-on daemon: HTTP API, hook relay, session registry, watchers
  (`crates/trusty-mpm/src/daemon/`)
- Session control, service discovery, and agent/skill deployment via the `tm` CLI
- Managed sessions: provisioned in isolated git worktrees, running a runtime
  inside a tmux pane (`crates/trusty-mpm/src/session_manager/`)
- The MCP tool catalog — 33 tools spanning orchestration, session lifecycle,
  console metrics, the project registry, and session-manager proxying
  (`crates/trusty-mpm/src/mcp/tools/`)
- Session overseer and circuit breaker (`src/core/overseer.rs`, `src/core/circuit.rs`)
- Cross-project session registry (`src/core/session_store.rs`)
- The `OrchestratorBackend` trait binding the MCP layer to the daemon
  (`src/mcp/mod.rs`)

**What it does not own:**

- Executing code itself. It provisions and oversees a *runtime* that does.
- General knowledge-worker assistant workflows (trusty-agents)
- Search, memory, or analysis infrastructure

The runtime a managed session spawns is selected by `RuntimeKind`
(`crates/trusty-mpm/src/runtime/`): `claude-code` (the default — the Claude
Code CLI over OAuth) or `tcode` (trusty-code over the direct Anthropic API).
Both are launched into a tmux pane. So trusty-mpm delegates to trusty-code only
when an operator selects that runtime; it is an alternative backend, not the
sole path.

---

### 3. trusty-agents — the agentic harness

**Crate:** `crates/trusty-agents/` — package `trusty-agents`, binary `tagent`

A general-purpose agentic harness for non-coding workflows: knowledge
retrieval, memory management, scheduling, communications, and domain-specific
assistant personas. It uses a PM-orchestrator-plus-sub-agent pattern, but it is
not a coding harness.

Sub-agents run one of two ways, chosen per agent: as a subprocess exchanging
NDJSON over stdin/stdout, or as a tokio task in the PM process. The in-process
path avoids a per-delegation startup cost and is used for read-heavy agents;
agents that shell out or execute user code stay on the subprocess path, where
process isolation is part of the contract.

**What it owns:**

- The PM orchestrator loop and the in-process `delegate_to_agent` tool
  (`src/ctrl/`, `src/tools/delegate.rs`)
- A heuristic intent classifier and the deterministic backend router
  (`src/intent/`)
- MCP service bridge: wraps any MCP server's tools as `ToolExecutor` instances
  (`src/tools/mcp_service_tools.rs`)
- Tool registry, RBAC tiers, and identity primitives (`src/tools/`, `src/rbac/`,
  `src/identity/`)
- Assistant personas defined as TOML agent configs, with markdown skill injection
- Transports: interactive REPL and CTRL multi-project dispatcher (`src/repl/`,
  `src/ctrl/`), HTTP API with an SSE event stream (`src/api/`), Slack
  (`src/slack/`), Telegram (`src/telegram/`), and a stdio MCP server
  (`tagent mcp-serve`)
- A declarative workflow engine for phase-based pipelines (`src/workflow/`)
- A tmux session manager of its own (`src/tm/`), distinct from trusty-mpm

**What it does not own:**

- Per-project coding workflow enforcement (trusty-code)
- Multi-project session management, circuit breaker, or hook relay (trusty-mpm)
- Search infrastructure (trusty-search)
- Memory storage engine (trusty-common's `memory-core` feature, plus trusty-memory)

The harness-adapter framework — which recognises whether a Claude Code,
claude-mpm, Codex, Gemini, or shell process occupies a given pane — and the
JSON-backed session ledger both live in `trusty-agents-common` and are
re-exported by this crate.

---

## Comparison Table

| Dimension | trusty-code | trusty-mpm | trusty-agents |
|-----------|-------------|------------|---------------|
| **Analogy** | Claude Code | Claude MPM | A general agentic assistant |
| **Primary user** | Developer / CI pipeline | Operator / PM role | Knowledge worker |
| **Scope** | One project, coding tasks | Many projects, orchestration control | Any domain, non-coding workflows |
| **Binary** | `tcode` | `tm` (alias `trusty-mpm`) | `tagent` |
| **Core loop** | PM main loop per project | Daemon + session manager + hook relay | PM main loop per persona |
| **Agent model** | Coding sub-agents from `.claude/agents/*.md` | Deploys agents; oversees the runtime that runs them | Domain personas plus an MCP tool bridge |
| **Transports** | JSON-RPC over stdio **or** loopback HTTP; TUI client | Loopback HTTP daemon; MCP stdio bridge; TUI; Telegram; Slack | REPL/CTRL; HTTP API with SSE; Slack; Telegram; MCP stdio |
| **Delegates to** | Its own sub-agents | A runtime per managed session (`claude-code` or `tcode`) | `tcode` or `tm`, via `dispatch_task` |
| **License** | MIT | MIT | MIT |

Every crate in the workspace inherits `license = "MIT"` from
`[workspace.package]` in the root `Cargo.toml`.

---

## Shared Foundations

Two crates carry the commonality. No harness's behaviour is built on another
harness's library surface: where one harness needs another, it invokes its
binary, as described under
[Inter-Harness Delegation](#inter-harness-delegation).

### trusty-common

`crates/trusty-common/` is the workspace-wide foundation, used well beyond the
three harnesses. Its module surface sits behind feature flags so each consumer
pulls in only what it needs.

Always on, no feature flag:

| Module | Role for harnesses |
|--------|--------------------|
| `chat` | Chat-completions client — the LLM call abstraction all three use |
| `claude_config` | Reads `.claude/` configuration (agents, `CLAUDE.md`, permissions) |
| `project_discovery` | Locates project roots from a working directory |
| `shutdown` | SIGTERM + SIGINT graceful-shutdown signal for every daemon |
| `log_buffer` | Bounded in-memory log ring buffer for log-tail endpoints |
| `sys_metrics` | RSS / CPU sampling for health endpoints |

Feature-gated modules the harnesses draw on:

| Feature | Module | Purpose |
|---------|--------|---------|
| (its own crate) | `trusty_mcp` | JSON-RPC 2.0 / MCP primitives — envelope types, the stdio dispatch loop, OpenRPC discovery, the daemon-bridge startup guard. Every MCP server in the workspace imports from here. ADR-0040 (#5803) extracted it from `trusty_common::mcp`. |
| `memory-rpc` | `trusty_common::memory_rpc` | Discovery-based JSON-RPC client for the trusty-memory daemon. What `trusty_common::mcp` left behind. |
| `rpc` | `trusty_common::rpc` | General-purpose JSON-RPC client plus stdio and HTTP transports |
| `stdio-mcp-client` | `trusty_common::stdio_mcp_client` | Client that spawns and talks to a stdio MCP server |
| `memory-core` | `trusty_common::memory_core` | Memory palace storage engine — trusty-memory's backend, also used by trusty-agents |
| `session-naming` | `trusty_common::session_naming` | The one canonical tmux session-naming rule, shared so trusty-mpm's and trusty-agents' session managers never orphan each other's sessions |
| `search-index` | `trusty_common::search_index` | Best-effort "ensure this project is indexed" entry point, used by trusty-mpm at session launch and trusty-code at task start |
| `axum-server` | `trusty_common::server` | Shared axum middleware and the same-origin write guard used by every HTTP daemon |
| `inference-client` | `trusty_common::inference` | Shared OpenAI-compatible adapter layer and provider key configuration |
| `catchup` | `trusty_common::catchup` | Incremental catch-up context generation across git, memory, and session sources |

### trusty-agents-common

`crates/trusty-agents-common/` is the harness-layer shared crate. All three
harness crates depend on it. It holds what the harnesses share with each other
but not with the rest of the workspace:

- `ToolExecutor`, `ToolResult`, `ToolExecutionTier`, `ServiceTier` (RBAC tiers),
  and `AgentPlugin` — the plugin surface an external agent crate implements
- `adapters` — the harness-adapter framework and its registry
- `session_registry` — the JSON-backed session ledger
- `runner` — the `AgentRunner` dependency-injection seam and its value types
- `events` — the unified `HarnessEvent` envelope and process-global bus (see below)
- `harness_doc` — the shared harness-understanding instructions consumed by both
  trusty-mpm's session-manager prompt and trusty-code
- `compress` — portable tool-output compression
- `perf` — portable token/cost value types

---

## Event Streams

**Architectural principle: the harnesses are event-driven.** A harness that
blocks synchronously at each step cannot fan out to sub-agents, stream progress
to a UI, or relay across a process boundary. Each harness therefore publishes to
a broadcast channel that subscribers fan out from, rather than operating in pure
request-response mode.

That principle holds in all three. The *unification* does not yet.

### Where each harness stands

| Harness | Bus | Payload type | External stream |
|---|---|---|---|
| **trusty-agents** | Process-global `tokio::sync::broadcast` (`src/events.rs`) | Its own `Event` enum | SSE over the HTTP API; UDS NDJSON `MessageBus` between projects (`src/bus/`) |
| **trusty-code** | Process-global `tokio::sync::broadcast` (`src/events.rs`) | `SessionEventEnvelope` wrapping a tagged `Event`, with a per-session monotonic sequence number | `session.event` JSON-RPC notifications on stdio; SSE at `/sessions/{id}/events` on HTTP |
| **trusty-mpm** | Daemon-held `tokio::sync::broadcast` (`src/daemon/state/`) | Untyped `serde_json::Value` | SSE at `/events` and `/sessions/{id}/events`; a poll endpoint at `/events/poll` |

trusty-agents and trusty-code each also relay events from a child process to
their parent by prefixing NDJSON lines on stderr with `__OMPM_EVENT__ `. Both
now re-export that prefix from the one declaration in
`trusty_agents_common::events::EVENT_LINE_PREFIX` (#5129); each previously kept
its own crate-local copy.

### The unified envelope, and why it is not yet the wire format

`trusty_agents_common::events` defines `HarnessEvent` — one envelope carrying a
`HarnessSource`, a tagged `HarnessPayload`, and a lag-aware subscription API —
together with the process-global bus and a subscription `Filter`. It was landed
as a foundation with no emit sites migrated onto it.

That is still the state for the *payload*: the three buses above each carry
their own type, so an event crossing a harness boundary is re-encoded rather
than forwarded. Consuming the unified envelope is the outstanding work.

The *framing* is now shared. `EVENT_LINE_PREFIX` used to read
`__HARNESS_EVENT__ ` here while both subprocess relays wrote `__OMPM_EVENT__ `,
and the session manager was told to watch for the former — a marker nothing
emitted (#5129). The constant now carries the emitted value and trusty-code and
trusty-agents re-export it, so the producers, the parent-side parser, and the
harness-understanding doc cannot drift apart again.

---

## Inter-Harness Delegation

### Delegation graph

All three are independently operator-facing — each has its own human entry
points, and none is a front door to the others. Two delegation edges leave
trusty-agents, both through the same tool; one leaves trusty-mpm.

```
     operator             operator              operator
  REPL / Slack /       tm CLI / TUI /         tcode CLI /
  Telegram / HTTP        Telegram                 TUI
         │                    │                    │
         ▼                    ▼                    ▼
 ┌───────────────┐   ┌───────────────┐   ┌───────────────┐
 │ trusty-agents │   │  trusty-mpm   │   │  trusty-code  │
 │    agentic    │   │     meta      │   │    coding     │
 └───────┬───────┘   └───────┬───────┘   └───────▲───────┘
         │                   │                   │
         │  (2) dispatch_task, route = Tm        │
         ├──────────────────►│                   │
         │                                       │
         │  (1) dispatch_task, route = Tcode     │
         └──────────────────────────────────────►│
                             │                   │
                             │  (3) --runtime tcode
                             └──────────────────►│
```

### Delegation edges

| # | From | To | Trigger | Transport | Returned |
|---|------|----|---------|-----------|----------|
| 1 | trusty-agents | trusty-code | `dispatch_task` with `route_task` returning `Tcode` — a repo-file token or a coding verb in the task text | One-shot subprocess `tcode run-task pm <task> --project <dir> --json`, which blocks until the daemon-owned session reaches a terminal state | The full transcript, with backend identity scrubbed |
| 2 | trusty-agents | trusty-mpm | `dispatch_task` with `route_task` returning `Tm` — orchestration, project, session, issue, or multi-agent vocabulary, and the default when no signal is present | Spawns `tm serve --stdio` as an MCP client, calls `session_new`, polls `session_activity`, then `session_decommission` | Observed pane transcript, identity scrubbed |
| 3 | trusty-mpm | trusty-code | A managed session created with `--runtime tcode` instead of the default `claude-code` | `TcodeAdapter` verifies `tcode` is on PATH and sends `tcode run-task <agent> <task> --project <cwd>` into the session's tmux pane | Session events via the hook relay and pane capture |

Two properties of edge 2 are worth stating plainly. trusty-mpm has no
synchronous run-this-and-block RPC, so the bridge polls with a bounded attempt
count and returns whatever transcript it has observed when the budget runs out.
And the bridge scrubs backend identity from both success and error paths, so a
persona calling `dispatch_task` never learns that `tm` or `tcode` exists.

Which backend a task routes to is decided by `intent::route::route_task`, a
pure function with no I/O: a repo-file token or a hard coding verb wins
outright, then any orchestration vocabulary, then a generic code verb, and
`Tm` otherwise.

### What is not cross-harness delegation

- trusty-search, trusty-memory, and trusty-analyze are **tool-layer services**,
  not harnesses. All three harnesses call them over HTTP or MCP as tools.
- A harness invoking its own sub-agents is intra-harness delegation.
- trusty-mpm spawning the Claude Code CLI — its default runtime — is not a
  trusty-tools harness boundary at all.

---

## Known gaps

Stated explicitly so nothing here reads as more settled than it is.

- **The boundary between trusty-mpm and trusty-agents is not fully resolved in
  source.** Both carry a session manager, both carry tmux integration, and
  trusty-agents carries a workflow engine that trusty-code does not. The shared
  tmux session-naming rule in `trusty_common::session_naming` exists precisely
  so the two managers do not orphan each other's sessions, which is a mitigation
  of the overlap rather than a resolution of it. This page describes what each
  crate contains; it does not assert where the line should finally fall.
- **Event unification has a landed foundation and no consumers.** The section
  above states which bus each harness actually runs. Anything beyond that —
  a migration order, a target date — is not established by source.
- **trusty-mpm's hook relay carries Claude Code hook events**, which is why
  edge 3 (`--runtime tcode`) has a different observability shape from the
  default runtime: trusty-code emits no hooks. How completely the pane-capture
  path substitutes for that is not something this page establishes.

---

## References

- [trusty-mpm](../trusty-mpm/README.md), [trusty-code](../trusty-code/README.md),
  [trusty-agents](../trusty-agents/README.md) — per-harness documentation
- [trusty-common](../trusty-common/README.md) — the workspace-wide shared crate

Source of truth for everything above, in the repository:

- `crates/trusty-code/src/lib.rs`, `src/main.rs`, `src/serve/` — the coding harness
- `crates/trusty-mpm/src/bin/tm/cli/mod.rs` — the `tm` subcommand surface
- `crates/trusty-mpm/src/mcp/tools/mod.rs` — the MCP tool catalog
- `crates/trusty-mpm/src/runtime/` — `RuntimeKind` and the two runtime adapters
- `crates/trusty-agents/src/tools/pm_bridge.rs`, `src/tools/pm_bridge_backend.rs`,
  `src/intent/route.rs` — the `dispatch_task` bridge and its router
- `crates/trusty-agents-common/src/lib.rs` — the harness-layer shared surface
- `crates/trusty-common/Cargo.toml` — the feature list behind the table above
- `docs/adr/0004-three-harnesses-shared-event-driven-common.md` and
  `docs/adr/0019-unified-ipc-messaging-on-event-bus.md` — the decision records
  behind the three-harness split and the event bus
