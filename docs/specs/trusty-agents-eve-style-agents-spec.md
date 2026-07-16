# DOC-37 — Eve-Style Agent Framework for trusty-agents

**Status:** Draft
**Subsystem:** trusty-agents — agent definition / runtime / tool-calling / memory
**Owner:** Engineering (trusty-agents)
**Last-updated:** 2026-07-15
**Spec ID:** `SPEC-AGENTFW-01~draft` … `SPEC-AGENTFW-06~draft` (DOC-37)
**Builds on:** the existing Markdown+YAML-frontmatter "compose" agent-definition
model (`crates/trusty-agents/src/agents/registry/md_agent.rs`), the workflow
engine split (#171/#172), the ctrl-plane split (#170), the trusty-memory Palace
integration (issue #379, `crates/trusty-agents/src/memory/trusty_backed.rs`),
and the planned unified inference-provider adapter layer (fireworks.ai + more,
landing in `trusty-common`).
**Cross-ref:** `crates/trusty-agents/src/runtime/`, `crates/trusty-agents/src/workflow/engine/`,
`crates/trusty-agents/src/ctrl/`, `crates/trusty-agents/src/mcp/mod.rs`,
`crates/trusty-agents/src/rpc/mod.rs`, `crates/trusty-agents/src/tools/delegate.rs`,
`crates/trusty-agents/src/tools/mcp_tools/`, `crates/trusty-agents/src/api/server/events_sse.rs`,
`crates/trusty-agents-common/src/lib.rs`, `crates/trusty-agents-local/src/main.rs`.
**Not to be confused with:** trusty-mpm's PM → sub-agent delegation model,
which is a different product (a Claude Code harness orchestrator, not a
standalone agent runtime). This spec is scoped entirely to **trusty-agents**
(bin `tagent`) as a standalone, separately-installable, non-coding
personal-productivity agent product competing with OpenClaw-class frameworks.

> **Scope note.** This is a **design spec**, not an implementation PR. It
> reviews Vercel's Eve agent framework, maps it against trusty-agents' current
> capabilities, and proposes a Rust-based, local-first equivalent. No Rust
> code changes ship with this document. Implementation is tracked as a
> phased roadmap (§5) under the epic issue this spec closes out.

---

## 1. Review of Vercel Eve

Vercel launched **Eve** on 2026-06-17 at Vercel Ship London: Apache-2.0,
public beta. Primary sources: [vercel.com/blog/introducing-eve][eve-blog],
[vercel.com/docs/eve][eve-docs], [github.com/vercel/eve][eve-repo],
[vercel.com/eve][eve-product]. Secondary confirmation:
[InfoQ][eve-infoq], [The New Stack][eve-tns].

### 1.1 Core abstractions

Eve's central idea is **"a file's name and place in the tree is its
definition"** — an agent is a directory, not a manifest:

```
agent/
  agent.ts              # defineAgent({ model: "anthropic/claude-opus-4.8" })
  instructions.md        # plain-markdown system prompt
  tools/
    search_orders.ts      # defineTool({ description, inputSchema: zod, needsApproval?, execute })
  skills/
    refund-policy.md      # YAML-frontmatter knowledge, loaded on demand — NOT executable
  subagents/
    billing/               # a full nested agent directory, same shape + a `description`
  connections/
    crm.ts                 # defineMcpClientConnection({ url, auth: { getToken } })
                            # or an OpenAPI-described REST API
  channels/
    slack.ts               # defineChannel — per-surface adapter
```

Minimum viable agent is two files: `agent.ts` + `instructions.md`. Tools are
one file each; the filename becomes the tool name — zero registration
boilerplate.

### 1.2 Handoffs / delegation

Subagents are full nested `defineAgent` directories with an added
`description`. A parent "calls a subagent just like it calls a tool" — each
subagent invocation gets a **clean context window and scoped permissions**.
This is a structural (filesystem-shaped), not merely a runtime, delegation
primitive: the parent-child relationship is declared by directory nesting.

### 1.3 Lifecycle model

A **session** is created via `POST /eve/v1/session`, executes in **turns**,
and streams NDJSON lifecycle events via `GET /eve/v1/session/:id/stream`.
Continuation across requests uses a returned `continuationToken` — the
lifecycle is explicitly request/response-shaped, matching a serverless
platform's stateless-invocation model.

### 1.4 Execution / runtime model

Built on **Vercel Workflows**: durable, checkpointed execution where a
session "pauses, survives a crash or deploy, and resumes exactly where it
stopped," including **indefinite waits on human approval at zero compute
cost while paused**. Deployed as ordinary Vercel Functions — there is no
separate always-on process; durability is a platform primitive Eve inherits
for free, not something Eve itself implements.

### 1.5 State persistence

Not a separate Eve concern at all — delegated entirely to Vercel Workflows'
checkpoint/resume mechanism. There is no described database story because
Vercel's platform owns it.

### 1.6 Streaming / event model

Two layers: raw NDJSON event stream over HTTP for consumers, and
OpenTelemetry spans per turn (`ai.eve.turn` → `ai.streamText` → `ai.toolCall`)
exportable to Braintrust/Datadog/Honeycomb/Jaeger for observability.

### 1.7 Tool-calling protocol — MCP

First-class: `defineMcpClientConnection({ url, auth: { getToken } })` under
`connections/`. Eve discovers the remote tools, hands them to the model, and
**brokers the auth so credentials never reach the model**. Arbitrary
OpenAPI-described REST APIs are supported the same way, as a second
`connections/` shape.

### 1.8 Deployment story

`npx eve@latest init` → `eve dev` (local TUI dev server) → `eve eval` →
`vercel deploy`. In-flight sessions are versioned: a code deploy does not
interrupt a running session — it finishes on the version it started on.
Multi-channel by adapter file: the same agent gets a Slack/Discord/
Teams/Telegram/Twilio/GitHub/Linear surface via one file each in `channels/`,
plus `eve channels add slack` as a scaffolding shortcut.

### 1.9 Developer experience

Code-first TypeScript + Markdown. No YAML/JSON agent manifest at all — the
directory tree *is* the manifest. Evals are also code-first
(`defineEval`, `t.send`, `t.calledTool`, `t.check`).

### 1.10 What Eve gets right (portable ideas)

- **Convention over configuration for agent definition.** One file per tool,
  one directory per (sub)agent, markdown for prose instructions. This
  eliminates an entire class of "did I register this tool" bugs and reads
  naturally in a repo browser.
- **MCP as the tool-calling backbone, with credential brokering.** The model
  never sees a secret; the framework mediates auth to the MCP/REST
  connection. This is a real security property worth keeping.
- **Structural delegation.** Subagent-as-directory makes handoffs
  discoverable by `ls`, not just by reading orchestration code.
- **Clean context windows per subagent call.** Prevents context bleed
  between a parent's conversation and a delegated task.
- **Deploy-safe versioning of in-flight sessions.** A durable-execution
  primitive that decouples "ship new code" from "kill running work."

### 1.11 What is specific to Vercel's serverless platform (not portable as-is)

- **Durability via Vercel Workflows** is a hosted-platform feature — Eve
  itself does not implement checkpoint/resume; it inherits it. A
  self-hosted, local-first daemon (trusty-agents) has no equivalent platform
  substrate and must build (or explicitly not build) this capability itself.
- **The serverless request/response session model** (`POST /session`,
  `continuationToken`) assumes stateless compute between turns. A tokio
  daemon that stays resident does not need this shape to get durability —
  in-process state can outlive a single HTTP request. But it *does* still
  need it if the daemon itself restarts (deploy, crash, host reboot).
- **Zero-cost indefinite pause** is a serverless billing property (no compute
  charged while parked). A local daemon's "pause" is just an idle in-memory
  or on-disk state — cheap already, so this Eve selling point doesn't
  translate into a design requirement, just removes an objection.
- **`vercel deploy` and multi-channel adapters as Vercel-hosted glue** — the
  channel adapters (Slack/Discord/etc.) are a real, portable idea, but
  Eve's specific deploy mechanics are platform-specific plumbing.

---

## 2. Comparative Gap Analysis

| Eve abstraction | trusty-agents today | Gap |
|---|---|---|
| Agent = directory convention (`agent.ts`+`instructions.md`+`tools/`+`subagents/`) | `.toml` **and** `.md`+YAML-frontmatter agent definitions (`agents/registry/md_agent.rs`), both loaded by `AgentRegistry::load`; sample agents at `.trusty-agents/agents/{cto-assistant,ctrl,izzie}/persona.md` + a skills library under `.trusty-agents/skills/**` | **Partial.** The compose model (markdown body = prompt, frontmatter = metadata) is directly analogous to Eve's `instructions.md`+`agent.ts` split. Missing: a declared **tools/ manifest** and **subagents/ directory** convention inside an agent's own folder — frontmatter (`MdAgentFrontmatter{name, role, model, description, runner, capabilities}`) has no `tools` or `subagents` fields today. |
| One-file-per-tool, filename = tool name | Tools registered via `src/tools/mcp_tools/{dispatch,executor,schema}.rs` + `src/tools/mcp_service_tools.rs` + `src/tools/delegate.rs`; declarative external-MCP registry in `src/mcp/mod.rs` (`GlobalConfig`, `~/.trusty-agents/config.toml`) | **Partial.** Tool declaration exists but is centralized/config-driven rather than one-file-per-tool inside the agent's own directory. No per-agent tool scoping — `GlobalConfig` is process-global. |
| Subagents = nested agent directories, called like a tool, clean context per call | `DelegateToAgentTool` (`src/tools/delegate.rs`) — PM calls sub-agents by name via a `delegate_to_agent` tool call, pre-flight-validated against on-disk agent configs (issue #204), synchronous/subprocess via `AgentRunner` | **Partial→Gap.** Delegation exists and is tool-call-shaped like Eve's, which is good. Missing: a **structured handoff contract** (typed context/state transfer, not just a prompt string) and **directory-declared** subagent relationships (today a flat registry lookup, not nesting). |
| MCP-native tool-calling with credential brokering | MCP used in two split ways: (a) client-side consumption of external MCP servers declared in `~/.trusty-agents/config.toml` and injected into prompts (not a live per-call MCP loop); (b) trusty-agents hosts its **own** MCP-like services (memory, search) merged into `rpc.discover`/`POST /rpc` (issue #460, `trusty_common::mcp::ServiceDescriptor`) | **Gap.** Not unified — no single MCP-native invocation path for all tools, and proxying actual `tools/call` in-process is a noted TODO. No explicit credential-brokering layer keeping secrets out of model context (needs verification / is likely absent). |
| Durable, checkpointed execution (pause/resume across crash/deploy) | `workflow/engine/` runs an in-process, in-memory phase loop (tokio async) per run; `state.rs` tracks `phases_to_skip` per persona; no persisted step/checkpoint state | **Gap — the largest one.** No durable execution primitive exists. A crash or restart mid-workflow loses run state entirely. This is the single biggest capability delta vs. Eve. |
| Session/turn lifecycle via HTTP, NDJSON streaming | `GET /api/events` SSE endpoint (`api/server/events_sse.rs`, issue #192 Phase B) subscribing to a process-global event bus, optional `session_id` filter | **Small gap.** Streaming infra exists (SSE, not NDJSON, but functionally equivalent); no explicit "turn" abstraction layered over it yet. |
| State persistence (delegated to platform) | Deep, custom integration: `TrustyBackedMemoryStore` (`memory/trusty_backed.rs`) adapts a flat `MemoryStore` trait onto trusty-common's Palace/Wing/Room/Drawer hierarchy, one Palace per `Segment` (issue #379) | **Not a gap** — arguably more mature than Eve's story, since Eve has no bespoke memory model at all (it just inherits workflow-level durability). trusty-agents' memory is *deeper* but not yet *declared* in the agent definition file. |
| OTel spans + eval framework (`defineEval`) | No equivalent found in the researched surface | **Gap.** No structured per-turn tracing/eval harness comparable to Eve's `ai.eve.turn` span hierarchy or `defineEval`/`t.calledTool` test DSL. |
| Multi-channel deployment (Slack/Discord/Teams/Telegram/Twilio/GitHub/Linear via adapter files) | trusty-mpm has a Telegram/TELUI surface (a different product); trusty-agents itself has no channel-adapter convention surveyed | **Gap** (or out of scope — needs a product decision, see open questions). |
| Deploy-safe versioning of in-flight sessions | Not applicable in the same way (no serverless redeploy model) but the *durable-execution* gap above means a `tagent` binary upgrade mid-run has no graceful story either | **Gap**, same root cause as durable execution. |

---

## 3. Proposed Design — a Rust, Local-First Equivalent

The guiding constraint: **trusty-agents is a local-first, tokio-resident
daemon, not a serverless platform tenant.** Eve gets durability, deploy-safe
versioning, and zero-cost pause for free from Vercel Workflows; trusty-agents
must either build a lightweight equivalent itself or explicitly scope those
properties out. The design below borrows Eve's *conventions* (directory
shape, MCP-native tools, structural handoffs) while replacing its
*infrastructure* (serverless workflows) with tokio + on-disk checkpointing +
trusty-memory.

Priority order follows the project's stated layering: **API → CLI → TUI**.
Every primitive below is designed API-first (a Rust trait/struct + an
HTTP/RPC surface), with `tagent` CLI commands and any future TUI as thin
consumers of that same API — never the reverse.

### 3.1 Agent definition format — extend the existing compose model

Keep the current dual `.toml` / `.md`+frontmatter model (`AgentRegistry::load`,
`md_agent.rs`) as the base — it is already directly analogous to Eve's
`agent.ts` + `instructions.md` split and should not be replaced. Extend
`MdAgentFrontmatter` with two new optional fields:

```yaml
---
name: billing-assistant
role: subagent
model: anthropic/claude-opus-4.8   # or a provider-adapter alias, see §3.6
description: Handles billing and refund queries
tools:
  - search_orders
  - issue_refund
subagents:
  - escalation-agent
---
```

And introduce a directory convention alongside the existing single-file
`persona.md`, for agents that want Eve-style per-tool/per-subagent
decomposition:

```
.trusty-agents/agents/billing-assistant/
  persona.md          # existing convention: frontmatter + body = system prompt
  tools/               # NEW, optional: one .toml or .rs-registered-name per tool
    search_orders.toml
  subagents/            # NEW, optional: symlinks or refs to sibling agent dirs
    escalation-agent -> ../escalation-agent
```

This is additive — a single-file `persona.md` agent (today's model) keeps
working unchanged; the `tools/`/`subagents/` subdirectories are opt-in sugar
for agents that want Eve-style decomposition. `AgentConfig` (`agents/config.rs`)
gains `tool_manifest: Vec<ToolRef>` and `subagent_refs: Vec<AgentRef>` fields,
populated by scanning the optional subdirectories if present, falling back to
the existing flat frontmatter fields otherwise.

### 3.2 Runtime primitives — tokio-based durable execution, not serverless

Do not attempt to replicate Vercel Workflows wholesale. Instead, add a
**checkpoint boundary** to the existing `workflow/engine/executor/` phase
loop (`run.rs`, `state.rs`):

- After each phase completes (research/plan/implement/qa/docs — the same
  `phases_to_skip` boundaries already tracked in `state.rs:24-40`), serialize
  the run's `WorkflowState` to a small on-disk journal (SQLite or a flat
  append-only file under `~/.trusty-agents/runs/<run-id>/journal.jsonl` — reuse
  whatever trusty-memory already uses for structured local storage rather
  than inventing a second embedded-DB dependency).
- On daemon start, scan for incomplete journals and offer `tagent resume
  <run-id>` — a crash or restart loses at most the in-flight phase, not the
  whole run. This is intentionally coarser-grained than Eve's step-level
  checkpointing (Vercel Workflows checkpoints *within* a turn); phase-level
  checkpointing is the pragmatic MVP given no platform substrate to lean on.
- **Explicit non-goal for MVP:** true mid-phase resume (resuming inside a
  single LLM call/tool-call sequence). Treat a phase as the atomic unit of
  durability until there's evidence the coarser granularity is insufficient.

This keeps the runtime tokio-native (no new async runtime, no external
workflow engine dependency) while closing the single largest gap identified
in §2 — durability across process restarts — at a cost proportional to what
trusty-agents actually needs as a local daemon, not a hosted platform.

### 3.3 Tool-calling — unify around MCP as the one invocation path

Today MCP is split: client-side consumption (`src/mcp/mod.rs`, injected into
prompts) vs. self-hosted service exposure (`src/rpc/mod.rs`, `rpc.discover`).
Proposed unification:

- Every tool — whether backed by an external MCP server, an in-process Rust
  function, or trusty-agents' own hosted RPC service — is invoked through a
  single `ToolInvoker` trait that speaks the MCP `tools/call` wire shape
  internally, even for in-process tools. This closes the "proxying actual
  `tools/call` in-process is a noted TODO" gap directly.
  Reuse `trusty_common::mcp::ServiceDescriptor` as the schema source of
  truth so external and internal tools describe themselves identically.
- Add credential brokering: an agent's tool/connection declaration names a
  credential reference (e.g. `secret_ref: "crm_api_key"`), resolved by the
  daemon from local secret storage and injected at call time — never placed
  in the prompt or model-visible context. This is the one Eve security
  property (§1.10) worth adopting explicitly rather than leaving implicit.

### 3.4 State / memory — declare it, don't just adapt it

`TrustyBackedMemoryStore` (issue #379) already gives trusty-agents a deeper
memory story than Eve has. The gap is declarative surfacing: an agent
definition should be able to state its memory scope in frontmatter —

```yaml
memory:
  palace: billing-assistant     # maps to a Segment/Palace via PalaceRegistry
  scope: session                # session | agent | shared
```

— rather than memory wiring living entirely in Rust adapter code the agent
author never sees. This is a documentation/config surface change, not a new
storage layer.

### 3.5 Multi-agent handoffs — formalize the existing delegate tool

`DelegateToAgentTool` (`src/tools/delegate.rs`) is already the right shape
(a tool call, matching Eve's "call a subagent like a tool"). Two additions:

1. **Structured handoff payload** — instead of a flat prompt string, define
   a `HandoffContext { summary, relevant_state, constraints }` struct passed
   to the subagent, so context is explicit and typed rather than
   string-concatenated.
2. **Clean-context guarantee** — ensure `AgentRunner`'s subprocess/session
   invocation for a delegated subagent starts a genuinely fresh context
   window (verify this is already true; call it out as a conformance
   requirement if not, since Eve treats it as a first-class guarantee).

### 3.6 Layering on the inference-provider adapter

The planned unified inference-provider adapter (fireworks.ai + more,
`trusty-common`) is where `model: anthropic/claude-opus-4.8`-style
provider-qualified model strings in agent frontmatter get resolved. The
agent definition format should treat `model` as an opaque adapter-routed
string (as Eve does), not a hardcoded Anthropic-specific field — this is
already directionally true given `MdAgentFrontmatter.model` is a plain
string today; the requirement is that the adapter layer, once it lands,
becomes the single resolution point every runtime path (workflow engine,
delegate tool, direct mode) calls through, rather than each call site
special-casing providers.

### 3.7 Streaming / events

The existing SSE endpoint (`api/server/events_sse.rs`) already covers Eve's
NDJSON-stream use case functionally. Proposed addition: introduce an
explicit **turn** boundary in the event schema (`TurnStarted`, `ToolCalled`,
`TurnCompleted`) layered over the existing event bus, matching Eve's
`ai.eve.turn` span hierarchy conceptually — without adopting OpenTelemetry
wholesale unless there's separate appetite for it (flagged as an open
question, §6).

---

## 4. Explicit Non-Goals

- **No serverless/Vercel-Workflows-equivalent hosted durability engine.**
  trusty-agents is and remains a local-first tokio daemon; durability is
  solved with phase-level on-disk checkpointing (§3.2), not by importing or
  reimplementing a general-purpose durable-workflow platform.
- **No wholesale replacement of the `.toml`/`.md`+frontmatter agent format.**
  Eve's directory convention informs *additive* extensions (§3.1), not a
  rewrite of the existing compose model.
- **No commitment to multi-channel adapters (Slack/Discord/etc.) in this
  spec.** That is a product-scope question for trusty-agents as a
  standalone tool, separate from trusty-mpm's existing Telegram/TELUI
  surface — deferred to open questions (§6), not designed here.
- **No OpenTelemetry adoption decision.** Turn-level event structure (§3.7)
  is proposed; OTel export is explicitly out of scope pending a separate
  observability-strategy call.
- **No true mid-phase (sub-turn) checkpointing in the MVP.** Phase-level
  granularity only, per §3.2.

---

## 5. Phased Roadmap

**Phase 0 — MVP (closes the durability gap, the largest identified delta):**
- Add phase-level checkpoint journal to `workflow/engine/executor/state.rs`.
- `tagent resume <run-id>` CLI command (API-first: an RPC method the CLI
  calls, per the API→CLI→TUI layering).
- No agent-definition-format changes yet.

**Phase 1 — Agent definition extensions:**
- `tool_manifest`/`subagent_refs` fields on `MdAgentFrontmatter`/`AgentConfig`.
- Optional `tools/`/`subagents/` subdirectory scanning, additive to the
  existing single-file `persona.md` convention.
- `memory:` frontmatter block wired to `PalaceRegistry` (§3.4).

**Phase 2 — MCP unification:**
- Single `ToolInvoker` trait spanning external MCP, in-process, and
  self-hosted RPC tools (§3.3).
- Credential-brokering layer for connection/tool secrets.

**Phase 3 — Structured handoffs:**
- `HandoffContext` struct replacing flat prompt-string delegation payloads.
- Conformance check/test for clean-context-per-subagent-call guarantee.

**Phase 4 — Inference-provider adapter integration:**
- Route all `model:` string resolution through the shared trusty-common
  adapter layer once it lands, removing any per-call-site provider
  special-casing.

**Phase 5 — Turn-level streaming + eval harness (parity stretch):**
- `TurnStarted`/`ToolCalled`/`TurnCompleted` event schema over the existing
  SSE bus.
- A `tagent eval` command/test DSL, evaluated against the observability
  strategy decision from §6.

Each phase is intended to be its own issue/PR chain off the epic issue this
spec closes out, not a single mega-PR.

---

## 6. Open Questions for Bob

1. **Durability granularity.** Is phase-level checkpointing (§3.2) — losing
   at most one in-flight phase on crash/restart — sufficient for the
   personal-productivity use case, or is finer-grained (sub-turn) durability
   a hard requirement from day one?
2. **Multi-channel scope.** Should trusty-agents grow Eve-style channel
   adapters (Slack/Discord/Telegram/etc.) as its own product surface, or is
   that explicitly trusty-mpm's TELUI territory and out of scope for
   trusty-agents entirely?
3. **Credential storage backend.** For the credential-brokering proposal
   (§3.3), is there an existing/preferred local secret store in the
   trusty-tools ecosystem to build on, or does this need a fresh design?
4. **OpenTelemetry appetite.** Is OTel export (matching Eve's
   Braintrust/Datadog/Honeycomb/Jaeger story) a real near-term want, or
   should the turn-level event schema (§3.7) stay bus/SSE-only for now?
5. **Directory-decomposition adoption pressure.** Should the optional
   `tools/`/`subagents/` subdirectory convention (§3.1) be pushed as the
   *recommended* pattern going forward, or purely opt-in sugar that most
   agents (like the existing `cto-assistant`/`ctrl`/`izzie` samples) will
   never need?
6. **Priority vs. the inference-provider adapter landing.** Should Phase 4
   (adapter integration) be pulled forward ahead of Phases 1–3 if the
   fireworks.ai adapter work lands sooner than expected, to avoid two
   separate migrations of `model:` resolution call sites?

---

[eve-blog]: https://vercel.com/blog/introducing-eve
[eve-docs]: https://vercel.com/docs/eve
[eve-repo]: https://github.com/vercel/eve
[eve-product]: https://vercel.com/eve
[eve-infoq]: https://www.infoq.com/news/2026/06/vercel-eve-agents/
[eve-tns]: https://thenewstack.io/vercel-launches-eve-an-open-source-framework-that-treats-agents-as-directories/
