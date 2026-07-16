# DOC-37 — Eve-Style Agent Framework for trusty-agents

**Status:** Draft
**Subsystem:** trusty-agents — agent definition / runtime / tool-calling / memory
**Owner:** Engineering (trusty-agents)
**Last-updated:** 2026-07-16
**Spec ID:** `SPEC-AGENTFW-01~draft` … `SPEC-AGENTFW-06~draft` (DOC-37)
**Builds on:** the existing `.toml` and `.md`+YAML-frontmatter agent-definition
loaders (`crates/trusty-agents/src/agents/registry/mod.rs`,
`crates/trusty-agents/src/agents/registry/md_agent.rs`,
`crates/trusty-agents/src/agents/loader.rs`), the workflow engine split
(#171/#172), the ctrl-plane split (#170), the trusty-memory Palace integration
(issue #379, `crates/trusty-agents/src/memory/trusty_backed.rs`), the
concurrency-safe state writer (issue #198,
`crates/trusty-agents/src/state_writer.rs`), and the planned unified
inference-provider adapter layer (fireworks.ai + more, landing in
`trusty-common`).
**Cross-ref:** `crates/trusty-agents/src/workflow/engine/executor/run.rs`,
`crates/trusty-agents/src/workflow/engine/state.rs`,
`crates/trusty-agents/src/workflow/config/mod.rs`,
`crates/trusty-agents/src/perf/mod.rs`,
`crates/trusty-agents-common/src/perf.rs`,
`crates/trusty-agents/src/tools/delegate.rs`,
`crates/trusty-agents-common/src/runner.rs`,
`crates/trusty-agents-common/src/lib.rs`,
`crates/trusty-agents/src/mcp/mod.rs`, `crates/trusty-agents/src/mcp/config/`,
`crates/trusty-agents/src/rpc/mod.rs`,
`crates/trusty-agents/src/tools/mcp_service_tools.rs`,
`crates/trusty-common/src/mcp/service.rs`,
`crates/trusty-agents/src/memory/store.rs`,
`crates/trusty-agents/src/memory/trusty_backed.rs`,
`crates/trusty-agents/src/events.rs`,
`crates/trusty-agents/src/api/server/events_sse.rs`,
`crates/trusty-agents/src/agents/config.rs`, `crates/trusty-agents/src/agents/model.rs`,
`crates/trusty-agents/src/env_compat.rs`, `crates/trusty-agents/src/events.rs`
(`events::bus/subscribe/publish`), `crates/trusty-agents/src/telegram/`,
`crates/trusty-agents/src/slack/`, `crates/trusty-agents/src/tm/monitor.rs`
(existing ticker pattern), `crates/trusty-agents/src/bus/mod.rs` (existing
cross-project `MessageBus`).
**Not to be confused with:** trusty-mpm's PM → sub-agent delegation model,
which is a different product (a Claude Code harness orchestrator, not a
standalone agent runtime). This spec is scoped entirely to **trusty-agents**
(bin `tagent`) as a standalone, separately-installable, non-coding
personal-productivity agent product competing with OpenClaw-class frameworks.

> **Scope note (v2 rewrite).** The v1 revision of this document (merged as
> PR #2792) was a comparative review of Vercel's Eve agent framework plus
> design *directions* for a Rust equivalent — not a spec. Bob rejected it:
> "the eve research is NOT a spec, it refers to one but doesn't actually
> contain one." This revision makes every `SPEC-AGENTFW-NN` section
> **normative and implementable without further design work**: exact
> frontmatter schemas, an explicit run-state machine, an exact on-disk
> checkpoint format, exact trait/struct signatures, an exact config-key
> table, and testable **Conformance** bullets per section, matching the
> fidelity bar set by [DOC-29](./mpm-behavior-conformance.md) and
> [DOC-36](./tm-manager-vision.md). The Eve review and the comparative gap
> analysis are retained as background (§7–§8) but are no longer the body of
> the document. Every module/type/file cited below was re-read against
> `origin/main` @ `78868a62` (2026-07-15) for this revision; anything that
> does not exist in the current tree is explicitly marked **NEW**.

---

## 1. How to read this document

Sections §2–§7 are the normative spec, one `SPEC-AGENTFW-NN` per section.
Each follows the same shape:

- **Current state** — what exists today, with `file:line` citations, verified
  against the tree at authoring time.
- **Normative requirement** — the exact schema/struct/algorithm to implement.
  Anything not already in the tree is marked **NEW**.
- **Conformance** — testable bullets an implementer/QA can check off, in the
  spirit of [DOC-29](./mpm-behavior-conformance.md)'s per-behavior evidence
  rows.

§8 is the phased roadmap, §9 explicit non-goals, §10 the owner-decision
checklist (which SPEC sections stay `~draft` pending Bob's call), §11–§12 the
demoted Eve review and gap-analysis background, §13 the change log.

---

## 2. SPEC-AGENTFW-01 — Agent Definition Format & Primitive-Binding Manifest

### 2.0 Foundational principle (Bob decision, 2026-07-16): agents are 100% declarative

> "Do we need a coded agent? Seems like instructions should be enough with a
> rich set of primitives (events, channels, tools, etc). Rather than allow
> that, I would define agents entirely as instructions, and use yaml to
> define the relationship of an agent with its primitives." — Bob

This supersedes §2's v2 framing outright. **An agent is exactly two kinds of
content, never a third:**

1. **Instructions** — Markdown prose defining behavior, persona, and policy.
   No executable semantics; the LLM reads it as a system prompt.
2. **A manifest** — YAML declaring which platform-hosted **primitives** this
   agent is bound to (tools, subagents, memory, model, checkpoints, events,
   channels — §2.2). The manifest is data: names, references, and
   scalars/lists — never a code path, a shell command, or a local script
   reference.

**All executable capability lives in the Rust platform layer
(`trusty-agents`/`trusty-agents-common`), never in an agent's own package.**
An agent cannot ship a tool implementation, a hook script, or any file the
runtime would execute — it can only *reference*, by name, a primitive the
platform already hosts. This is stricter than v2's design (which left the
door open to per-agent `tools/*.rs`-style implementations by analogy to
Eve); it is now a foundational, mechanically-enforced invariant, not a style
preference — §2.6 specifies the loader-level rejection rule.

This is also the **core positioning differentiator from Eve**: Eve is
code-first (`agent.ts` + `tools/*.ts` — TypeScript the model's own
capabilities are implemented in, shipped alongside the agent). trusty-agents
is **declaration-first** — an agent's file tree can never contain logic, only
prose and bindings. See §11 for the full comparison.

### 2.1 Current state

trusty-agents has **two independent agent-loading code paths** today, not
one:

1. **`AgentRegistry::load(search_paths: &[PathBuf])`**
   (`crates/trusty-agents/src/agents/registry/mod.rs:70-114`) — scans each
   directory in priority order with a **flat, single-level** `read_dir`
   (no recursion into subdirectories), parsing `*.toml` via `AgentConfig::load`
   and `*.md` via `parse_md_agent`. First occurrence of a given agent name
   wins (`agents.contains_key(&name)` guard, line 96); later directories are
   shadowed. Used for capability-scored `best_match` lookup and roster
   rendering — it does **not** see directory-package agents (below), because
   a directory has no `.toml`/`.extension()` match and falls through `_ =>
   continue` (line 91).
2. **`AgentConfig::by_name(name: &str)`**
   (`crates/trusty-agents/src/agents/loader.rs:48-56`) — resolves a single
   agent by short name for direct dispatch (subprocess re-invocation,
   `delegate_to_agent`). Per issue #482, it **prefers the directory-package
   format** `<agents_dir>/<name>/agent.toml` + `<name>/persona.md` (+ optional
   `<name>/skills.md`, appended with a `\n\n---\n\n` separator) when
   `<agents_dir>/<name>/` is a directory (`load_agent_package`,
   `loader.rs:321-341`), falling back to the flat `<agents_dir>/<name>.toml`
   otherwise. `agents_dir()` resolves via `TAGENT_CONFIG_DIR` when set, else
   CWD-relative `.trusty-agents/agents/` (with a warn log on fallback).

Both directory-package and flat-`.toml` agents are real and in use today —
e.g. `crates/trusty-agents/.trusty-agents/agents/{ctrl,izzie,cto-assistant}/`
(package format: `agent.toml` + `persona.md`) alongside 15+ flat `<name>.toml`
files in the same `agents/` directory.

**`.md`+YAML-frontmatter format** (`parse_md_agent`,
`crates/trusty-agents/src/agents/registry/md_agent.rs:58-153`) parses this
frontmatter shape (`MdAgentFrontmatter`, lines 34-44):

```rust
#[derive(Debug, Deserialize, Default)]
struct MdAgentFrontmatter {
    name: Option<String>,
    role: Option<String>,
    model: Option<String>,
    description: Option<String>,
    #[serde(default)]
    runner: Option<String>,           // "claude-code" | "inline" | "subprocess" (default) | "in-process"
    #[serde(default)]
    capabilities: Option<AgentCapabilities>,
}
```

Unknown YAML keys are silently ignored (no `deny_unknown_fields`) so richer
claude-mpm frontmatter doesn't fail parsing. Critically, **`parse_md_agent`
unconditionally sets `tools: ToolsConfig::default()`** (line 145, i.e.
`allowed: None`, `allow: None`) regardless of frontmatter content — `.md`
agents cannot declare a tool allowlist today even though **TOML agents
already can**, via the existing `[tools]` section on `AgentConfig`
(`crates/trusty-agents/src/agents/config.rs:118-172`):

```rust
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ToolsConfig {
    pub allowed: Option<Vec<String>>,       // exact tool-name allowlist
    pub allow: Option<Vec<String>>,          // glob patterns (#255), e.g. "mcp_*"
    pub native: NativeToolsConfig,           // opt-in native-tool flags (#133)
    pub ast_native: Option<bool>,            // shorthand for native.ast_native (#347)
    // + OpenRPC scope patterns (#455): memory.read, search.read, google.gmail.*
}
```

There is **no `extends`, `subagents`, or `memory` field anywhere** in either
loader today — confirmed by exhaustive grep across
`crates/trusty-agents/`, `crates/trusty-agents-common/`, and
`crates/trusty-agents-local/` (zero hits for `extends` as an
agent-inheritance concept; the only `extends`-substring hits are unrelated
doc-comment prose in `state_writer.rs` and `runner.rs`). Composition/handoff
declaration in an agent's own file is a **genuine gap**, not a
misunderstanding of existing code — this section defines it as **NEW**.

Two further pieces of current-state grounding, needed for the primitives
below and corrected from the v2 draft:

- **Channels — two distinct existing systems, neither agent-scoped today.**
  (a) `crates/trusty-agents/src/telegram/` (#264) and `src/slack/` (#418) are
  **inbound bot gateways**: a long-poll/Socket-Mode listener that routes
  every message to `ctrl::run_pm_task_with_history` — the top-level PM loop,
  one bot per **project**, never a specific named agent
  (`runtime/mode_dispatch.rs:309,326,424`). (b) **`crates/trusty-channels`**
  (epic #2636, ADR-0014) is a *separate* crate — native MCP servers
  (`slack-mcp`/`telegram-mcp` binaries, `crates/trusty-channels/src/lib.rs:1-18`)
  exposing send/read/list/react as **callable tools** over stdio JSON-RPC.
  `trusty-agents` does **not** currently depend on `trusty-channels` (confirmed
  by grep — no such line in `crates/trusty-agents/Cargo.toml`). §2.3.7 binds
  the manifest's `channels:` primitive to both, precisely, without conflating
  them.
- **Events — the bus is emit-only today.** `crate::events::{bus, subscribe,
  publish}` (`events.rs:282-315`) is a process-global `broadcast::Sender<Event>`
  already carrying rich telemetry (session/PM/agent/tool/AST/phase/persona/
  LLM-lifecycle variants, `events.rs:56-`). Nothing today **consumes** the bus
  to invoke an agent — every existing subscriber (the SSE endpoint,
  `api/server/events_sse.rs`) is a read-only telemetry sink. A scheduling
  primitive has no existing equivalent either: `grep -rniE "\bcron\b"
  crates/trusty-agents/src` is empty; the closest analog is
  `crate::tm::monitor::TmMonitor` (`tm/monitor.rs:1-40`), a `tokio::time::interval`
  ticker wrapping a `JoinHandle`, used today only for session-idle polling
  (#318) — a real, citable pattern for a NEW scheduler to mirror, not an
  existing scheduler itself.

### 2.2 The primitive set

Per Bob's decision, an agent's manifest binds it to exactly seven platform
primitives. Each is grounded against what exists today; anything absent is
marked **NEW** rather than invented as green-field:

| Primitive | Binds to | Grounding |
|---|---|---|
| `model` | Provider/model resolution | **Exists** — `resolve_model`/`adapter_for_model` (§2.3.4, §7) |
| `tools` | Native + MCP-external + MCP-management tool dispatch | **Exists** — `ToolExecutor`, `ToolsConfig` (§2.3.1, §4) |
| `subagents` | Delegation targets | **Partial** — `DelegateToAgentTool` exists; declared allowlist is NEW (§2.3.2, §5) |
| `memory` | Palace/Segment scope | **Partial** — `TrustyBackedMemoryStore` exists; declarative binding is NEW (§2.3.3, §7) |
| `checkpoints` | Phase-boundary durability | **NEW** end to end (§2.3.5, §3) |
| `channels` | Chat-platform tools + inbound routing | **Partial** — `trusty-channels` (tools) and the inbound gateways exist; per-agent binding is NEW (§2.3.7) |
| `events` | Trigger-driven wake (subscribe/schedule) | **NEW** end to end — the bus is emit-only today (§2.3.6) |

### 2.3 Manifest schema (NEW)

One schema, two surfaces (§2.4): embedded as `MdAgentFrontmatter` fields for
single-file agents, or as the top-level keys of a sibling `agent.yaml` for
directory-package agents. Both parse via the crate's **existing** YAML
dependency — `serde_yml` (`crates/trusty-agents/Cargo.toml:94`,
`serde_yml::from_str`, already used by `parse_md_agent`, `md_agent.rs:84`) —
no new YAML crate is introduced.

```yaml
# The full key set. Every key is optional except `name`/`role`/`description`
# (already required today via AgentInfo). Unknown top-level keys are a load
# error (§2.6) — this is the enforcement mechanism, not just documentation.

name: billing-assistant
role: subagent
description: Handles billing and refund queries
extends: engineer                    # NEW — §2.5

model: anthropic/claude-opus-4-6      # existing AgentInfo.model — opaque, adapter-resolved (§7.2)

tools:                                # binds ToolsConfig (existing struct, §4)
  allowed: [search_orders, issue_refund]
  deny: []                            # NEW field, §2.5 merge rules

subagents:                            # NEW — binds DelegateToAgentTool pre-flight (§5)
  allowed: [escalation-agent]

memory:                                # binds §7 (SPEC-AGENTFW-06)
  segment: brief
  top_k: 5

checkpoints:                           # binds §3 (SPEC-AGENTFW-02)
  enabled: true                        # default; per-agent opt-out

events:                                 # NEW — §2.3.6
  subscribe: [PhaseDone, ToolResult]     # must name a real `Event` enum variant
  schedule: "15m"                        # NEW minimal interval syntax, §2.3.6

channels:                               # NEW — §2.3.7
  tools: [slack, telegram]               # binds trusty-channels MCP tools (zero new platform code)
  inbound: [slack]                       # NEW — this agent wakes on inbound channel messages
```

Every key above maps onto a field of the **existing** `AgentConfig`/`AgentInfo`
(`config.rs`) except the ones marked NEW, which extend those same structs
additively — no parallel config type is introduced.

#### 2.3.1 `tools` — unchanged from v2, now manifest-sourced

Identical semantics to v2's SPEC-AGENTFW-01 §2.2 field 2/field on
`ToolsConfig` (`config.rs:118-172`: `allowed`, `allow`, **NEW** `deny`,
`native`, `ast_native`, OpenRPC scopes) — the only change is the *source*:
both the single-file frontmatter and the directory-package `agent.yaml` now
populate the same `ToolsConfig` the TOML `[tools]` table always has. See §4
(SPEC-AGENTFW-03) for tool dispatch/credential-brokering, which is unchanged
by the declarative-only decision — credential resolution is a platform-config
concern (`~/.trusty-agents/config.toml`), never an agent-manifest concern, so
it stays out of this schema entirely.

#### 2.3.2 `subagents` — unchanged from v2

Identical to v2's field 3: `subagents.allowed: Option<Vec<String>>`, wired
into `DelegateToAgentTool::execute` (`tools/delegate.rs:141-157`) as a second,
narrower pre-flight check. See §5 (SPEC-AGENTFW-04) for `HandoffContext`.

#### 2.3.3 `memory` — unchanged from v2

See §7 (SPEC-AGENTFW-06) — a declarative default over the five existing
`Segment` variants; no new storage layer.

#### 2.3.4 `model` — resolution mechanism already exists; manifest key is a thin binding

`model:` is **not** a new resolution mechanism — it is the existing
`AgentInfo.model: String`, already resolved through `resolve_model`
(`agents/model.rs:172-192`, 5-tier precedence) and `adapter_for_model`
(`llm/adapter` module) into a concrete `trusty-common`-style adapter. Per
Bob's decision, this is explicitly the **existing** unified
inference-provider layer, not a planned one — `trusty_common::inference::
InferenceAdapter` (`crates/trusty-common/src/inference/adapter.rs:39`) already
ships `OpenAiCompatAdapter`, `BedrockAdapter`, `AnthropicAdapter`, and a
`providers/fireworks.rs` adapter. trusty-agents' own `llm::adapter::
ModelAdapter` (`llm/adapter/mod.rs:117-130`) is a **separate, narrower**
trait (OpenRouter-compatible, driven by `async_openai`) that predates the
commons layer — unifying the two is out of scope for this spec (§9) but the
manifest key is written now in the shape that unification will not have to
change: an opaque `provider/model-id` string.

**NEW**: a system-wide default provider/model pair, `[model] default` in
`~/.trusty-agents/config.toml` (`GlobalConfig`, §6), sitting between the
existing `TAGENT_DEFAULT_MODEL` env var and the hardcoded `FALLBACK_MODEL`
constant in the precedence order — formalizing today's env-var-only default
as an explicit, visible config value:

`TAGENT_MODEL_<NAME>` (env, per-agent) → agent's own `model:` (manifest) →
`TAGENT_DEFAULT_MODEL` (env, global) → **NEW** `[model].default` (config
file, global) → hardcoded `FALLBACK_MODEL = "anthropic/claude-sonnet-4-6"`.

#### 2.3.5 `checkpoints` — thin opt-out over §3's existing design

`checkpoints.enabled: bool` (default `true`). When `false`, `RunState`
transitions still occur in memory (§3.2) but `CheckpointRecord` writes are
skipped — for an agent whose runs are so short-lived that phase-boundary
durability is pure overhead. Everything else in §3 (SPEC-AGENTFW-02) is
unchanged by the declarative-only decision.

#### 2.3.6 `events` — NEW platform infrastructure, explicitly scoped

Two independent triggers, both **NEW**:

- **`subscribe: Vec<String>`** — each entry MUST name a real `Event` enum
  variant (validated at load time against the variant list in `events.rs:56-`,
  e.g. `PhaseDone`, `ToolResult`, `AgentFailed` — an unrecognized name is a
  load error listing the valid variants, the same "helpful list" pattern used
  elsewhere in this spec). Requires a **NEW** `EventTriggerDispatcher`
  (daemon-side): calls `events::subscribe()` (the existing
  `broadcast::Receiver<Event>` constructor, `events.rs:305`), filters for
  variants named in any loaded agent's `events.subscribe`, and invokes that
  agent via `AgentRunner::run_with_context` (§5) when a match fires. This is
  the one genuinely new consumer of the event bus — today it has zero
  consumers beyond telemetry sinks.
- **`schedule: Option<String>`** — a minimal interval syntax (`"<N><unit>"`,
  unit ∈ `{m,h,d}`, e.g. `"15m"`, `"1h"`) — **not** cron-expression syntax; no
  cron-parsing crate exists in this workspace today (confirmed —
  `Cargo.toml` has no `cron`/`tokio-cron-scheduler` entry) and adding one is
  out of scope for the MVP (flagged in §10 if full cron syntax is wanted
  later). Requires a **NEW** `AgentScheduler`, structurally mirroring
  `crate::tm::monitor::TmMonitor` (`tm/monitor.rs:24-40`: owns a
  `tokio::time::interval`-driven `JoinHandle`, `start`/`stop`/`Drop`
  lifecycle) but firing `AgentRunner::run_with_context` on tick instead of
  `TmManager::poll_sessions`.

Both dispatchers are genuinely new platform infrastructure — the largest net
new investment in this spec — which is why they are sequenced as their own
roadmap phase (§8 Phase 4), not bundled into the core manifest phase.

#### 2.3.7 `channels` — extend two real existing systems, invent no third

Per Bob's decision: do not design a new adapter system. Ground the manifest
in exactly what exists:

- **`channels.tools: Vec<String>`** — binds this agent's `tools.allowed` set
  (§2.3.1) to **`trusty-channels`'** MCP tools. Concretely: an operator adds
  an `McpService` entry (existing mechanism, §4) pointing `command` at the
  `slack-mcp`/`telegram-mcp` binary (`crates/trusty-channels/src/bin/
  {slack,telegram}-mcp.rs`); an agent listing `channels.tools: [slack]`
  simply means `slack_*`-prefixed tools are in its effective `tools.allowed`
  set. **Zero new platform code** — this is a documentation/config
  convention over the existing tools primitive (§2.3.1), not a new binding
  mechanism. `trusty-agents` gains a **NEW** `Cargo.toml` dependency on
  `trusty-channels` only if a call site needs its types directly (unlikely —
  MCP tool-calling is process-boundary, per §4); wiring by `McpService`
  config requires no new dependency at all.
- **`channels.inbound: Vec<String>`** — **NEW.** Declares that this
  *specific* agent (not just the project's top-level PM) should receive
  inbound messages from the named channel. Requires extending the existing
  gateway modules (`crates/trusty-agents/src/telegram/handlers.rs`,
  `src/slack/handlers.rs`) with a routing step: today `handle_message`
  dispatches unconditionally to `ctrl::run_pm_task_with_history`; a
  channel-bound agent needs a **NEW** routing rule (e.g., a chat/channel-id
  → agent-name mapping, or a slash-command prefix) that dispatches to
  `AgentRunner::run_with_context(agent_name, ...)` instead. This is
  structurally the same problem trusty-mpm's L2/L3 layering already solved
  with the `ManagedBackend`/`SessionProxy` seam (DOC-36 §3.5: "a thin backend
  trait implementation talking to daemon-local state... exercise this entire
  state machine with `curl`... before ever wiring up a Telegram bot token")
  — trusty-agents should mirror that architectural pattern (a thin routing
  trait over the existing gateway, not a parallel gateway), not trusty-mpm's
  code itself (different crate, different daemon). Scoped precisely as a
  gated roadmap item (§8 Phase 4, §10).

### 2.4 Form factor

Two surfaces, matching the **existing** dual-loader split (§2.1) — no new
tier is invented, and both remain additive to what's on disk today:

1. **Single-file** (`AgentRegistry::load`'s flat `.md` scan) — small agents
   keep Markdown+YAML-frontmatter exactly as today: the manifest keys (§2.3)
   live in the frontmatter block, instructions are the file body.
2. **Directory package** (#482's format, `AgentConfig::by_name`) — richer
   agents get a directory of:
   - `agent.yaml` — **NEW manifest filename/format**, replacing `agent.toml`
     as the *preferred* manifest for directory packages. Carries exactly the
     schema in §2.3, nothing else (no `[llm]`/`system_prompt` TOML tables to
     hand-author — those are derived from the manifest + `instructions.md`).
   - `instructions.md` — **NEW preferred name**, replacing `persona.md`.
     Entirely prose; the entirety of the agent's behavior/persona/policy.
   - Optional static assets: additional `.md`/`.yaml`/`.yml`/`.json`/`.txt`
     files (prompt fragments, templates) — **data, never code** (§2.6).

**Back-compat, not a hard cutover:** `load_agent_package` tries
`agent.yaml`/`instructions.md` first, falling back to the existing
`agent.toml`/`persona.md` pair when the new names are absent — the same
"prefer new, fall back to legacy" shape already established by this
codebase's `TAGENT_*`/`OPEN_MPM_*` env convention (`env_compat.rs`) and by
`.toml`/`.md` dual parsing in `AgentRegistry::load`. Existing packages
(`ctrl`, `izzie`, `cto-assistant`) keep working unmodified; nothing is
deleted. The exact deprecation timeline for `agent.toml`/`persona.md` (dual
support indefinitely vs. a sunset date) is a product-lifecycle call gated in
§10, not invented here.

### 2.5 `extends` — aligned to trusty-mpm's proven `compose_agent` pattern

Per Bob's decision: do not invent a bespoke merge algorithm — mirror the
**existing, proven** reference implementation,
`crates/trusty-mpm/src/core/agent_builder.rs::compose_agent`. That module
already solves exactly this problem for trusty-mpm's `BASE-*.md` agent
hierarchy: single-parent `extends:`, resolved **at instantiation** (agent-load
time — trusty-mpm's `compose_agent` runs ahead of a Claude Code session
starting; trusty-agents has no separate build/deploy pass, so its natural
instantiation point is `AgentRegistry::load`/`AgentConfig::by_name` itself,
which is already where §2.5 below resolves `extends` — no new pipeline
stage is added), which Bob calls out as the more efficient shape (no runtime
re-resolution once loaded, versus re-walking the chain on every dispatch).

trusty-agents gets its **own** analogous implementation (trusty-agents does
not depend on trusty-mpm as a library — `agent_builder.rs` is an internal
module of a different binary crate) but mirrors it precisely:

- Same constant: **`MAX_DEPTH = 8`** (`agent_builder.rs:31`) — coincidentally
  the same ceiling this spec's v2 draft already proposed independently.
- Same case-insensitive resolution discipline: agent names are matched via a
  lowercased lookup key (mirroring `build_source_map`'s `SourceMap`,
  `agent_builder.rs:16-22`), so `extends: engineer` resolves consistently on
  case-sensitive (Linux) and case-insensitive (macOS) filesystems.
- Same default merge semantics for prose: **`instructions.md`/persona content
  concatenates base-first** — the parent's instructions, then the child's,
  joined the same way `compose_agent`'s `bodies.join("\n\n")` does
  (`agent_builder.rs:519`) — **not** "child replaces parent," which v2 of
  this spec had proposed independently before this decision. This is a
  correction: adopt base-first concatenation unconditionally, matching the
  proven mechanism, with no opt-in token.
- Own error enum (structurally analogous to `AgentBuildError`, not a
  cross-crate reuse of it):
  - **`ExtendsNotFound { agent, base }`** — mirrors `AgentBuildError::NotFound`.
  - **`ExtendsCycle { chain: Vec<String> }`** — mirrors `AgentBuildError::Cycle`,
    same "walk a visited-list, push/pop around recursion" shape as
    `agent_builder.rs`'s `resolve()` (`visiting: &mut Vec<String>`).
  - **`ExtendsTooDeep { agent, depth: 8 }`** — mirrors `AgentBuildError::DepthExceeded`.

Other merge rules (scalars child-overrides-when-present; `tools`/`capabilities`
union; `subagents.allowed` union) are unchanged from v2's §2.2 and are
additive to `compose_agent`'s prose-only scope — trusty-mpm's agents don't
carry a `tools`/`subagents`/`memory` binding schema, so there is no
precedent to diverge from there.

### 2.6 No-code enforcement (NEW)

The mechanical guarantee behind §2.0's principle, in two layers:

1. **Closed schema, not a lenient one.** Unlike today's `MdAgentFrontmatter`
   (which silently ignores unknown YAML keys, `md_agent.rs`, no
   `deny_unknown_fields`), the manifest parser for **both** surfaces (§2.4)
   uses a closed key set — any top-level key not in §2.3's schema is a load
   error, `ManifestUnknownKey { key }`. This is the primary enforcement: a
   coded-agent design would need a key like `hook:`/`script:`/`exec:` to
   reference a local file, and no such key exists in the schema to add one
   to without editing this spec.
2. **Directory-package file allowlist.** `load_agent_package` additionally
   validates every file in the package directory against an **allowlist** of
   data extensions — `.md`, `.yaml`, `.yml`, `.json`, `.txt` — and the two
   known filenames (`agent.yaml`/`agent.toml`, `instructions.md`/`persona.md`)
   plus `skills.md`. Any other file (any other extension, or any file with
   the Unix executable permission bit set regardless of extension —
   `fs::metadata(path)?.permissions().mode() & 0o111 != 0`) is a load error,
   `PackageContainsForeignFile { path }`. An allowlist rather than a denylist
   deliberately: a denylist of script extensions (`.sh`/`.py`/`.js`/…) is
   leaky (new interpreters, no-extension scripts); an allowlist of known-safe
   data formats is not.

**Explicit boundary, not ambiguous:** this rule governs the **agent's own
package only**. `~/.trusty-agents/config.toml`'s `McpService.command`
(§4) legitimately references executables (`slack-mcp`, a stdio MCP server
binary) — that is operator-controlled platform infrastructure, a different
trust boundary from agent-authored content, and is explicitly out of scope
for this rule.

### 2.7 Directory-convention layout (worked example)

```
.trusty-agents/agents/
  engineer.md                        # single-file format (§2.4.1) — frontmatter + instructions in one file
  billing-assistant/                 # directory package (§2.4.2)
    agent.yaml                       # NEW manifest — extends/tools/subagents/memory/model/checkpoints/events/channels
    instructions.md                  # NEW name — the entirety of behavior (persona/policy prose)
  escalation-agent/
    agent.yaml
    instructions.md
```

`billing-assistant/agent.yaml`:

```yaml
name: billing-assistant
role: subagent
description: Handles billing and refund queries
extends: engineer                    # base-first concatenation of instructions.md (§2.5)

tools:
  allowed: [search_orders, issue_refund]   # unioned with engineer's own tools.allowed

subagents:
  allowed: [escalation-agent]

memory:
  segment: brief
  top_k: 5

channels:
  tools: [slack]                       # this agent may call slack_* MCP tools
```

### 2.8 Conformance

- A manifest with an unrecognized top-level key (e.g. `run: ./hook.sh`)
  fails to load with `ManifestUnknownKey`, never silently ignored.
- A directory package containing any file outside the §2.6 allowlist (e.g.
  `billing-assistant/notify.py`, with or without the executable bit set)
  fails to load with `PackageContainsForeignFile`.
- `extends` resolves scalar override + list-union merge rules for a 2-level
  chain, with **base-first concatenation** of `instructions.md` content
  (§2.5) — not child-replaces; a 9-level chain is rejected with
  `ExtendsTooDeep { depth: 8 }`.
- A 2-cycle (`a extends b`, `b extends a`) is rejected with `ExtendsCycle`
  naming both agent names in `chain`.
- `agent.yaml`/`instructions.md` is preferred when both the new and legacy
  (`agent.toml`/`persona.md`) filenames are present in the same package
  directory; the legacy pair alone still loads unmodified (regression guard
  for `ctrl`/`izzie`/`cto-assistant`).
- `delegate_to_agent` from an agent whose `subagents.allowed = ["x"]` rejects
  a call targeting agent `"y"` even when `y`'s package exists on disk; an
  agent with no `subagents` key is unaffected (regression guard against the
  three existing tests in `tools/delegate.rs`'s own test module).
- An agent declaring `events.subscribe: ["PhaseDone"]` is invoked by the
  **NEW** `EventTriggerDispatcher` when a `PhaseDone` event is published on
  the bus (`events::publish`); an unrecognized variant name fails to load.
- An agent declaring `channels.tools: [slack]` has `slack_*`-prefixed tools
  in its effective `ToolsConfig.allowed` when the operator has configured
  the corresponding `McpService` — no trusty-agents code change required
  for this half of the binding.

---

## 3. SPEC-AGENTFW-02 — Runtime State Machine & Checkpoint Journal

### 3.1 Current state

`WorkflowDef.phases: Vec<PhaseDef>`
(`crates/trusty-agents/src/workflow/config/mod.rs:110-193`), loaded from
`{config_dir}/{name}.json`. Each `PhaseDef` carries: `name`, `agent`,
optional `model`, `context_template`, `produces_files: Option<bool>`,
`parallel_subtasks: Option<Vec<ParallelSubtask>>`,
`worktree_protection: Option<bool>`, `skip: Option<bool>`,
`skills: Option<Vec<String>>`, `ast_native: Option<bool>`.

**There is no explicit state-machine enum.** Execution is a single imperative
`'phase_loop: for phase in &def.phases` loop inside
`WorkflowEngine::run_with_perf_and_dirs`
(`crates/trusty-agents/src/workflow/engine/executor/run.rs:149-465`). Per
phase: skip-check (`phase_should_skip`) → AST-native guard → progress-start
event (`emit_progress_event`) → `assemble_phase_prompt` → `dispatch_phase` →
on `Err`: `record_phase_failure` + `break 'phase_loop` (lines 290-301); on
`Ok`: perf record, ticket comment, `handle_qa_envelope` (may set
`qa_retry_count`/`qa_failure_feedback`, gating a **same-phase** one-shot QA
retry — the retry re-enters the code phase, not a new phase; important for
checkpoint granularity below), history-indexer record, `ctx.record_phase`,
goal-block parse, plan-output relocation, code-phase reconciliation, file
extraction.

The loop's exit (success or `break`) hands off to `finalize_run`
(`executor/finalize.rs`). **`perf.flush(perf_dir)`
(`finalize.rs:111-112`) is the only on-disk persistence of run state, and it
runs exactly once, at the very end**, regardless of success or failure.
**There is zero on-disk state during an in-flight run today** — a process
crash mid-loop loses the entire in-memory `WorkflowContext`, perf
accumulator, and session-manager state, with nothing recoverable.

Two existing on-disk conventions must not be conflated:

- `docs/performance/runs/<stamp>.json` + `runs.log`
  (`crates/trusty-agents/src/perf/mod.rs:254-263`, schema in
  `crates/trusty-agents-common/src/perf.rs:111-171`, `PerfRecord`/
  `PhaseRecord`/`PerfTotals`/`TokenUsage`) — a **developer-facing analytics
  artifact**, `out_dir`-relative (typically `<cwd>/docs/performance`),
  written via plain `tokio::fs::write` (not the atomic-write primitive
  below). This stays as-is; the new checkpoint journal is **not** colocated
  here.
- `.trusty-agents/state/` — the **project-relative runtime-state root**
  already holding `build.json`, `sessions.json`,
  `mistakes/<session_id>.jsonl`, `interactions/<session_id>.jsonl`, and
  `runs.jsonl` (confirmed via `build_info.rs`, `mistake_log.rs`,
  `interaction_log.rs`, `process_tracker.rs`, `state_writer.rs`). This is
  where the new checkpoint journal belongs.

**Existing atomic-write primitive to reuse verbatim** —
`crate::state_writer::atomic_write(path: &Path, contents: &[u8]) -> Result<()>`
(`crates/trusty-agents/src/state_writer.rs:80-117`): acquires an advisory
exclusive lock on a sibling `<path>.lock` file (via `fs4`), writes to
`<path>.tmp`, `sync_all()` (best-effort fsync), then `fs::rename`s into
place. Already battle-tested by `atomic_write_is_crash_safe` and
`concurrent_writes_no_corruption` (10 concurrent writer threads, zero
corruption). **No new I/O primitive is introduced by this spec.**

### 3.2 Normative requirement — `RunState` (NEW)

```rust
// NEW — crates/trusty-agents/src/workflow/engine/checkpoint.rs
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunState {
    Pending,
    PhaseRunning { phase_index: usize },
    PhaseComplete { phase_index: usize },
    Retrying { phase_index: usize, attempt: u32 },
    Failed { phase_index: usize },
    Done,
}
```

Transitions:

- `Pending -> PhaseRunning{0}` on entering the first non-skipped phase.
- `PhaseRunning{i} -> PhaseComplete{i}` immediately after `ctx.record_phase`
  for phase `i` (the last thing that happens for phase `i` before the loop
  advances) — the checkpoint write for `PhaseComplete{i}` is the very last
  step of iteration `i`, so a resumed run never re-executes a phase already
  marked complete.
- `PhaseComplete{i} -> PhaseRunning{i+1}` on loop advance (honoring
  `phase_should_skip`).
- `PhaseRunning{i} -> Retrying{i,1}` when `handle_qa_envelope` sets
  `qa_retry_count = 1` (the existing "Fix 2" one-shot code-phase retry
  triggered by a `qa`-phase failure signal, `run.rs:129-133`/`384-395`) —
  this is the one existing case where the loop re-enters an
  already-dispatched phase; `attempt` makes it observable/resumable.
- `Retrying{i,1} -> PhaseComplete{i}` or `-> Failed{i}` — same as non-retry.
- `PhaseRunning{i} -> Failed{i}` on `dispatch_phase` `Err`
  (`record_phase_failure`, `run.rs:292-301`).
- `PhaseComplete{last} -> Done` at `finalize_run` entry.
- `Failed{i}` and `Done` are terminal for the journal.

### 3.3 Normative requirement — checkpoint journal

**Location:** `.trusty-agents/state/runs/<run_id>/checkpoint.json`
(project-relative, matching the existing `.trusty-agents/state/` root).
`run_id` reuses the **existing** `TAGENT_RUN_ID`/`OPEN_MPM_RUN_ID` value
already threaded through `emit_progress_event` and `HistoryIndexer`
(`run.rs:399-400`, `state.rs:82`) — no new ID scheme.

```rust
// NEW — crates/trusty-agents/src/workflow/engine/checkpoint.rs
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckpointRecord {
    pub run_id: String,
    pub workflow: String,                       // WorkflowDef.name
    pub state: RunState,
    pub phase_names: Vec<String>,                // snapshot of def.phases[..].name at Pending — diagnostic only, see §3.4
    pub out_dir: PathBuf,
    pub code_dir: PathBuf,
    pub task: String,                             // ORIGINAL (uncleaned) task text
    pub phase_outputs: std::collections::BTreeMap<String, String>,
    pub goal_block: Option<crate::context::goals::GoalBlock>,
    pub qa_retry_count: u32,
    pub qa_failure_feedback: Option<String>,
    pub started_at: String,                        // ISO8601, same convention as PerfCollector::started_at_iso
    pub updated_at: String,                         // ISO8601, refreshed every write
}
```

**Write timing:** exactly at the `PhaseComplete{i}` / `Failed{i}` /
`Retrying{i,_}` transitions — once per phase boundary, **not** per LLM
turn/tool-call within a phase. Phase-level granularity is the deliberate MVP
boundary (see §9 non-goals) and is **APPROVED as spec'd** — Bob's decision,
2026-07-16: sub-turn/mid-phase durability is a non-goal, not a deferred
question (§10 item 1 closed). Write call:
`state_writer::atomic_write(&checkpoint_path, &serde_json::to_vec_pretty(&record)?)`
— the existing primitive, unmodified.

**Deletion:** on `RunState::Done`, `finalize_run` removes
`.trusty-agents/state/runs/<run_id>/` entirely
(`std::fs::remove_dir_all`, best-effort — logged on failure, non-fatal,
matching the existing "log and continue" pattern already used throughout
`finalize.rs` for ticket-manager/auto-push hooks). A `Failed` checkpoint is
**intentionally** left on disk — it is the resumable artifact.

### 3.4 Normative requirement — `tagent resume <run_id>`

- Resolves `.trusty-agents/state/runs/<run_id>/checkpoint.json`; missing file
  → error listing the run IDs that do have one (`ls
  .trusty-agents/state/runs/`).
- Reloads `WorkflowDef` **fresh** from `{config_dir}/{workflow}.json` — not
  from the snapshotted `phase_names`, which is diagnostic/display only.
  **If the on-disk workflow JSON's phase list no longer matches
  `phase_names` at the recorded index, resume MUST fail closed** with
  `WorkflowError::ResumeDefinitionChanged` rather than silently running a
  different pipeline than the one that was checkpointed.
- Resumes the `'phase_loop` immediately **after** `phase_index` for
  `PhaseComplete`/`Done`, or **at** `phase_index` for `Failed`/`Retrying`
  (the failed/in-flight phase re-runs from scratch — no sub-phase resume).
- **Replayed:** every phase from the resume point onward re-executes in full
  (fresh LLM call, fresh `dispatch_phase`), including a phase that failed.
  **Not replayed:** any already-`PhaseComplete` phase's LLM/tool calls — its
  `phase_outputs` entry is reused verbatim. No duplicate `PhaseDone` events
  fire for already-complete phases; a single **NEW** `Event::RunResumed
  { session_id, resumed_at_phase }` fires once at resume start (see §5.3).

### 3.5 Failure matrix

| Failure | Behavior |
|---|---|
| Process crash mid-phase (before the phase-boundary checkpoint write) | Checkpoint reflects the last `PhaseComplete` (or `Pending` if the crash was during phase 0). Resume re-runs the in-flight phase from scratch. At most one phase's work is lost, never more. |
| Checkpoint file present but corrupt (unparseable JSON / schema mismatch) | `tagent resume` fails closed with `WorkflowError::CheckpointCorrupt { path, source }` — does **not** silently start a new run under the same `run_id` (risk of clobbering partial `out_dir`/`code_dir` state). Operator fixes the file or starts a fresh run (new `run_id`). |
| Workspace (`out_dir`/`code_dir`) moved or deleted between checkpoint and resume | `resolve_dirs` re-validates both paths exactly as for a fresh run; a missing `out_dir` is recreated (existing #126/#153/#222 behavior). A missing `code_dir` with partial generated files is **not** treated as data loss by the framework — a known MVP limitation (see §10 owner-decision checklist). |
| Two `tagent resume` invocations racing on the same `run_id` | The checkpoint file is protected by the same `state_writer` advisory lock as every other `.trusty-agents/state/` file — a second resume attempting a phase-boundary write while the first holds the lock **blocks**, does not corrupt. The framework does **not** add a separate "run already resumed" mutex; concurrent resume of the same `run_id` beyond "the journal won't corrupt" is flagged as a real gap, not solved here. |

### 3.6 Conformance

- A synthetic 3-phase workflow killed (`SIGKILL`) mid-phase-2 leaves a
  checkpoint at `PhaseComplete{0}` or `PhaseRunning{1}`/`Failed{1}` (never
  claims phase 2 complete); `tagent resume` re-runs phase 1 (0-indexed) or 2
  and does not re-invoke the phase-0 agent.
- A hand-corrupted `checkpoint.json` (truncated JSON) causes `tagent resume`
  to exit non-zero with `CheckpointCorrupt`, not a panic, and not a silent
  fresh-run fallback.
- A completed (`Done`) run leaves no `.trusty-agents/state/runs/<run_id>/`
  directory on disk afterward.
- `atomic_write` is the only write path exercised — no direct
  `tokio::fs::write`/`OpenOptions` call is added for the checkpoint file.

---

## 4. SPEC-AGENTFW-03 — Unified Tool-Calling, MCP Mapping & Credential Brokering

### 4.1 Current state — correcting the v1 draft's framing

The v1 draft claimed trusty-agents needed a new unifying `ToolInvoker` trait
because MCP tool-calling was "split" between client consumption and
self-hosted exposure. Re-reading the actual code shows this framing was
**wrong on the client side**: a single unifying trait already exists and
external MCP tools already flow through it.

**`ToolExecutor`** (`crates/trusty-agents-common/src/lib.rs:276-313`) is
already the one trait every callable tool implements — native tools, the
five `mcp_*` management tools, and (critically) every tool advertised by an
enabled external MCP service:

```rust
#[async_trait]
pub trait ToolExecutor: Send + Sync {
    fn name(&self) -> &str;
    fn schema(&self) -> Value;
    async fn execute(&self, args: Value) -> ToolResult;
    fn restricted_tiers(&self) -> &[ServiceTier] { &[] }
    fn execution_tier(&self) -> ToolExecutionTier { ToolExecutionTier::OnDemand }
}
```

```rust
#[derive(Debug)]
pub enum ToolResult {
    Success(String),
    Error { message: String, recoverable: bool },
}
```

**External MCP tool-calling is already live**, not a declarative-only
registry as the v1 draft asserted.
`crates/trusty-agents/src/tools/mcp_service_tools.rs:194-197`
(`mcp_service_tool_executors`) reads `GlobalConfig::load()`
(`crates/trusty-agents/src/mcp/mod.rs`), walks each **enabled** service
(`GlobalConfig.mcp.services: Vec<McpService>`,
`crates/trusty-agents/src/mcp/config/types.rs:148-163` — `name`,
`description`, `command`, `args`, optional `url`, `transport: "stdio"|"http"`,
`enabled`), and builds one `McpServiceTool` (lines 95-145) — a real
`ToolExecutor` impl — per advertised tool. `ServiceClient::get_or_spawn`
(lines 36-81) lazily spawns a `StdioMcpClient` (cached per-service via
`OnceCell<Arc<Mutex<StdioMcpClient>>>`), and `McpServiceTool::execute`
(lines 111-144) forwards to `client.call_tool(&self.name, args)`, rendering
MCP's `{content:[{type:"text",text:...}]}` response into a flat string
(`format_mcp_call_result`, lines 155-181). Failures (server not on `PATH`,
handshake failure, JSON-RPC error) surface as `ToolResult::err(...)` — the
existing graceful-degradation pattern, not a panic.

Two real, narrow gaps remain, **not** "unify two parallel paths":

1. **`build_executors_from_services` is stdio-only.** Non-`"stdio"`
   `transport` values are explicitly skipped (not errored) — "skipping
   non-stdio MCP service (only stdio transport supported)". HTTP-transport
   MCP servers are declarable in config (`McpService.url`) but never
   actually callable.
2. **`rpc/mod.rs`'s self-hosted `POST /rpc` endpoint only implements
   `rpc.discover`.** `rpc_handler` (`crates/trusty-agents/src/rpc/mod.rs:57-75`)
   returns `-32601 Method not found` for anything but `rpc.discover`.
   `build_unified_discovery` (lines 41-46) merges `ServiceDescriptor` impls
   for `MemoryMcpService`/`SearchMcpService` (from `trusty-memory`/
   `trusty-search`) into one OpenRPC document via
   `trusty_common::mcp::openrpc::OpenRpcBuilder`, tagging every method with
   `x-service`. Actually **executing** a discovered method (`tools/call`) is
   an explicit, named TODO in the file (lines 77-81, issue #460) — this
   endpoint serves *external* callers looking to invoke trusty-agents' own
   hosted services, and today it can only tell them what's callable, not
   call it.

**Important constraint discovered this pass:** `ServiceDescriptor`
(`crates/trusty-common/src/mcp/service.rs:26-44`) has **no execute/call
method at all** — only `name()`, `version()`, `tools()`, `scopes_for()`. It
is a pure discovery/metadata trait. Closing gap 2 is therefore **not** "add a
match arm" — it requires a genuinely new dispatch layer that calls into
`trusty-memory`'s and `trusty-search`'s own tool-execution entry points
directly (not through `ServiceDescriptor`), which this pass could not fully
verify the shape of. Flagged explicitly in §10 rather than asserting an
unverified signature.

No credential-brokering exists in `mcp/`/`rpc/` today: `McpServiceTool::execute`
(lines 131-132) forwards `args` verbatim to `client.call_tool()`; there is no
secret-reference indirection on `McpService` or `GlobalConfig` today. This
**is** a genuine gap in the MCP-tool-calling path specifically — though, per
Bob's decision (2026-07-16), the *storage backend* for it is not a gap: it
already exists elsewhere in the workspace (§4.2 item 3, below).

### 4.2 Normative requirement

**No new `ToolInvoker` trait.** `ToolExecutor` is retained as the single
unifying interface; every requirement below produces a `ToolExecutor` impl,
not a parallel abstraction.

1. **`tools/call` proxying for self-hosted services (closes issue #460
   TODO).** `POST /rpc` gains a `tools/call` method: request
   `{"jsonrpc":"2.0","id":<id>,"method":"tools/call","params":{"name":<tool
   name>,"arguments":<object>}}`. Dispatch routes on the tool's `x-service`
   annotation (already present on every merged method,
   `build_unified_discovery`) to a **NEW** per-service execution adapter —
   the exact call surface into `trusty-memory`'s/`trusty-search`'s own tool
   runners is an implementation-time verification item (§10), not asserted
   here. Error shape: standard JSON-RPC 2.0; unknown tool name →
   `{"code":-32602,"message":"Invalid params"}`; unknown method still
   `-32601` (existing behavior, `rpc/mod.rs:66-73`, unchanged).
2. **HTTP-transport MCP client support (closes the stdio-only gap).** **NEW**
   `HttpMcpClient` with the same `call_tool(name: &str, args: Value) ->
   Result<Value>` shape `StdioMcpClient` already exposes, so
   `McpServiceTool::execute` needs **no changes** — only
   `ServiceClient::get_or_spawn`'s match on `transport` gains an `"http"` arm
   constructing the client from `McpService.url`.
3. **Credential brokering — backend DECIDED (Bob, 2026-07-16): OS keyring,
   already implemented.** `McpService` gains **NEW** `credential_ref:
   Option<String>` (`crates/trusty-agents/src/mcp/config/types.rs`, alongside
   the existing `command`/`args`/`url`/`transport`/`enabled` fields). The
   value is a `provider` name resolved through the **existing**
   `trusty_common::inference::credentials` resolver — not a new secret
   store:
   - `resolve_key(provider: &str) -> Option<String>`
     (`crates/trusty-common/src/inference/credentials/resolver.rs:73-76`) —
     tier 1 `env_tier(provider)` (process env / `.env.local`, loaded once via
     `dotenv::load_env_local_once()`), tier 2 `default_store()` (line 123):
     `KeyringStore` (`keyring_store.rs:44-`, wraps the **already-a-workspace-
     dependency** `keyring = "3"` crate, `Cargo.toml:129`, features
     `apple-native`/`windows-native`/`sync-secret-service` — i.e. macOS
     Keychain, Windows Credential Manager, Linux Secret Service, exactly the
     "OS keyring everywhere" shape Bob specified) when `probe_available()`
     succeeds, else `FileKeyStore` (`file_store.rs:53-`, `0600`-permission
     hardened, **not** the OS keychain — see the flagged tension below).
   - **Naming convention (already established, reused verbatim):**
     `KeyringStore`'s service name is the literal constant `"trusty-tools"`
     (`keyring_store.rs:23`), account = the `provider` string
     (`credential_ref`'s value) — the exact same convention every inference
     provider credential already uses. `env_var_for(provider)`
     (`resolver.rs:44-52`) already special-cases non-inference providers too
     (`"slack" => "SLACK_BOT_TOKEN"`) — MCP/channel credentials are not a new
     category to this resolver, just a new caller.
   - **Fallback tension, flagged not silently resolved (§10):** Bob's
     directive is "never plaintext files," but `default_store()`'s existing,
     already-shipped fallback when the OS keychain is unavailable
     (headless/CI/locked session) **is** `FileKeyStore` — a real file, just
     `0600`-permission-hardened, not encrypted. This spec does not invent a
     stricter policy unilaterally: either (a) MCP `credential_ref` resolution
     accepts the same fallback every other credential in this codebase
     already accepts, or (b) it hard-fails when the keychain probe is
     negative rather than falling back — a real behavioral fork gated in
     §10, not resolved here.
   - **Injection point**, unchanged from v2's design: **stdio transport** —
     an env var on the spawned subprocess (`StdioMcpClient::spawn`'s existing
     `command`/`args` call site gains an `envs: HashMap<String, String>`
     parameter, populated once at `get_or_spawn` time, not per-call);
     **http transport** — an `Authorization` header per request. The secret
     is **never** interpolated into the `args: Value` the LLM constructed —
     the model never sees the literal value. This is the concrete,
     code-grounded implementation of the "MCP with credential brokering"
     property Eve markets (§11).
4. **RBAC on new tools.** Every `ToolExecutor` produced by (1)-(3) MUST
   implement `restricted_tiers()` consistently with how
   `mcp_tool_executors()` (`tools/mcp_tools/executor.rs`) already gates the
   five `mcp_*` management tools — not silently inherit the trait default
   `&[]` (unrestricted).

**Timeout/retry contract (NEW — no existing documented contract found for
`StdioMcpClient::call_tool` in this pass):** default **30s** per-call
timeout (consistent with the existing `max_turns`-bounded LLM dispatch
budgets elsewhere in the crate); **no automatic retry** — a timeout surfaces
as `ToolResult::err(...)` exactly like today's handshake-failure path
(`mcp_service_tools.rs:112-128`), leaving the retry decision to the LLM,
consistent with `ToolResult`'s recoverable/fatal design intent
(`trusty-agents-common/src/lib.rs:151-175`).

### 4.3 Conformance

- `POST /rpc {"method":"tools/call","params":{"name":"memory_recall",...}}`
  against a running `tagent --api` instance returns a JSON-RPC result (not
  `-32601`), and an unknown tool name returns `-32602`.
- An `McpService` with `transport = "http"` and a reachable `url` produces a
  callable `ToolExecutor` (currently silently skipped).
- An `McpService` with `credential_ref = "fireworks"` (or any provider
  `env_var_for` already recognizes) spawns/calls with the value
  `resolve_key("fireworks")` returns present in the subprocess env (stdio) or
  request header (http), and a `tracing`/debug capture of the LLM-visible
  `ToolResult` content never contains the literal secret value.
- On a host where `KeyringStore::probe_available()` is `false` (headless/CI),
  resolution falls back to `FileKeyStore` exactly as `default_store()`
  already behaves for every other credential — or hard-fails, per whichever
  the §10 fallback-tension item resolves to.
- A tool call exceeding 30s surfaces `ToolResult::Error { recoverable: true,
  .. }`, not a hang or panic.

---

## 5. SPEC-AGENTFW-04 — HandoffContext & Delegation Protocol

### 5.1 Current state

`DelegateToAgentTool` (`crates/trusty-agents/src/tools/delegate.rs:28-171`)
is the PM's tool for dispatching to sub-agents. Its schema
(`schema()`, lines 104-127) is flat:
`{"agent_name": string, "task": string}`, both required. Pre-flight
validation (lines 141-157) checks only that
`<config_dir>/<agent_name>.toml` **exists** — it has no concept of a
*declared* subagent set (that's the new `subagents.allowed` field from §2.3.2,
which this section wires in as a second check). Dispatch (lines 164-169):

```rust
let final_task = crate::agents::persona::assemble_task_with_context(agent_name, task);
match self.runner.run(agent_name, &final_task).await {
    Ok(out) => ToolResult::ok(out.content),
    Err(e) => ToolResult::err(format!("sub-agent '{agent_name}' failed: {e:#}")),
}
```

Today's "context" handed to a subagent is a **single flattened string** —
persona/skill prose prepended to the raw task by
`assemble_task_with_context`, then passed to the plain `run()` method of
`AgentRunner` (`crates/trusty-agents-common/src/runner.rs:201-249`):

```rust
#[async_trait]
pub trait AgentRunner: Send + Sync {
    async fn run(&self, agent_name: &str, task: &str) -> Result<AgentOutput>;
    async fn run_with_history(&self, agent_name: &str, task: &str,
        _history: &[HistoryMessage], ctx: &RunContext) -> Result<AgentOutput> { /* default -> run_with_context */ }
    async fn run_with_context(&self, agent_name: &str, task: &str,
        _ctx: &RunContext) -> Result<AgentOutput> { /* default -> run() */ }
}
```

`RunContext` (`runner.rs:100-115`) already carries `assigned_file:
Option<PathBuf>`, `max_turns_override: Option<u32>`, `working_dir:
Option<PathBuf>`, and a model override — but `DelegateToAgentTool::execute`
calls the plain `run()`, **not** `run_with_context`, so none of this richer
plumbing is exercised on the delegation path today. `AgentOutput`
(`runner.rs:146-158`) is `{content: String, summary: Option<String>, usage:
TokenUsage}` — already sufficient for a normative return protocol; no
changes needed there.

### 5.2 Normative requirement — `HandoffContext` (NEW)

```rust
// NEW — crates/trusty-agents/src/tools/delegate.rs (or a sibling handoff.rs)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HandoffContext {
    pub summary: String,
    pub relevant_state: std::collections::BTreeMap<String, String>,
    pub constraints: Vec<String>,
}
```

**Size limit:** total serialized `HandoffContext` capped at **4 KiB**
(`serde_json::to_vec(&handoff)?.len() <= 4096`) — bounds prompt-tax the same
way perf/cost accounting already treats prompt bloat as a first-class
concern. Exceeding the cap returns `ToolResult::err(...)` (recoverable — the
LLM can shrink `relevant_state` and retry), never a panic.

**Schema change:** `delegate_to_agent`'s `schema()` gains an optional
`handoff: object` parameter alongside the existing required `agent_name` and
`task` (still `"required": ["agent_name", "task"]` — `handoff` is NOT
required). Absent `handoff` defaults to `HandoffContext{ summary: task.clone(),
relevant_state: {}, constraints: [] }`, so all three existing tests in
`tools/delegate.rs`'s own test module
(`unknown_agent_returns_helpful_error_with_available_list`,
`known_agent_reaches_runner`, `no_config_dir_skips_validation`) continue to
pass unmodified.

**Dispatch change:** `DelegateToAgentTool::execute` switches from
`self.runner.run(agent_name, &final_task)` to
`self.runner.run_with_context(agent_name, &final_task, &ctx)`, where `ctx:
RunContext` gains **NEW** field `handoff: Option<HandoffContext>` (additive —
every existing `RunContext` field is unchanged).

**Clean-context guarantee (normative, testable):** every `AgentRunner` impl
that spawns a subprocess (`RunnerKind::Subprocess`, the default) MUST start
that subprocess with **no** inherited conversation history unless
`persistent_session = true` on the **target** agent's own `AgentInfo`
(`config.rs` — existing field, already read at `run.rs:228-230`). Stated
precisely: the only way a subagent sees prior turns is (a) its own
`persistent_session` opt-in (existing mechanism, unchanged) or (b) the
explicit `HandoffContext.relevant_state`/`summary` the parent hands over
(new mechanism) — **never** ambient process/env state leaking from the
parent's own conversation.

**Return protocol:** unchanged — `AgentOutput{content, summary, usage}` flows
back through `ToolResult::ok(out.content)` exactly as today (`delegate.rs:167`).

### 5.3 Conformance

- A `delegate_to_agent` call omitting `handoff` behaves byte-for-byte
  identically to today (all three existing `delegate.rs` tests pass
  unmodified).
- A `HandoffContext` whose serialized size exceeds 4096 bytes returns a
  recoverable `ToolResult::err`, and the runner is **not** invoked (mirrors
  the existing "runner not invoked on validation failure" assertion pattern
  already used by `unknown_agent_returns_helpful_error_with_available_list`).
- A non-`persistent_session` subagent invoked via `delegate_to_agent` cannot
  observe any string from the parent's conversation history that was not
  explicitly present in `HandoffContext` or the `task` string — a
  regression test constructs a parent with private state (e.g. an
  unrelated prior tool result) and asserts the subagent's rendered prompt
  does not contain it.
- A **NEW** `Event::RunResumed { session_id, resumed_at_phase }` variant is
  added to the existing `Event` enum
  (`crates/trusty-agents/src/events.rs:56-`, alongside `PhaseStarted`/
  `PhaseDone`/`PhaseSkipped`) and fires exactly once per `tagent resume`
  invocation, over the existing SSE stream
  (`crates/trusty-agents/src/api/server/events_sse.rs`) — no new streaming
  transport is introduced; the current `GET /api/events?session_id=`
  contract (Server-Sent Events, 15s keepalive, `Lagged` handling) already
  covers this.

---

## 6. SPEC-AGENTFW-05 — Config Surface

Every new/extended config key introduced by §2–§5, following the existing
`TAGENT_*`-with-legacy-fallback convention
(`crates/trusty-agents/src/env_compat.rs`, `env_var(new, legacy)`;
`crates/trusty-agents/src/agents/model.rs:150-190`, `TAGENT_MODEL_<UPPER_SNAKE>`,
`TAGENT_DEFAULT_MODEL`, `TAGENT_CONFIG_DIR`):

| Section / file | Key | Type | Default | Env override |
|---|---|---|---|---|
| Agent manifest (`agent.yaml` or frontmatter, §2.3) | `extends` | `Option<String>` | `None` | n/a (per-file) |
| Agent manifest, `subagents` (**NEW**, §2.3.2) | `allowed` | `Option<Vec<String>>` | `None` (unrestricted — current behavior) | n/a |
| Agent manifest, `tools` (existing key, extended, §2.3.1) | `deny` | `Option<Vec<String>>` | `None` | n/a |
| Agent manifest, `checkpoints` (**NEW**, §2.3.5) | `enabled` | `bool` | `true` | n/a (per-file) |
| Agent manifest, `events` (**NEW**, §2.3.6) | `subscribe` | `Option<Vec<String>>` | `None` | n/a (per-file) |
| Agent manifest, `events` (**NEW**, §2.3.6) | `schedule` | `Option<String>` (`"<N><m\|h\|d>"`) | `None` | n/a (per-file) |
| Agent manifest, `channels` (**NEW**, §2.3.7) | `tools` | `Option<Vec<String>>` | `None` | n/a (per-file) |
| Agent manifest, `channels` (**NEW**, §2.3.7) | `inbound` | `Option<Vec<String>>` | `None` | n/a (per-file) |
| `~/.trusty-agents/config.toml` `[[mcp.services]]` (existing `McpService`, extended, §4.2) | `credential_ref` | `Option<String>` (a `provider` name resolved via `resolve_key`, §4.2 item 3) | `None` | n/a |
| `~/.trusty-agents/config.toml` `[[mcp.services]]` (existing, extended, §4.2) | `timeout_secs` | `u64` | `30` | n/a |
| `~/.trusty-agents/config.toml` (**NEW** `[model]` table, §2.3.4) | `default` | `Option<String>` (`"provider/model-id"`) | `None` | n/a — sits between `TAGENT_DEFAULT_MODEL` and `FALLBACK_MODEL` in precedence (§2.3.4) |
| Workflow engine (process-level, **NEW**, §3.3) | checkpoint journal enabled | `bool` | `true` | `TAGENT_CHECKPOINT_DISABLE=1` — naming/shape gated in §10 (not grounded against a verified existing analogous flag). |
| Workflow engine (**NEW**, §3.3) | checkpoint state root | `PathBuf` | `.trusty-agents/state/runs/` (resolved the same way `agents_dir()`/`TAGENT_CONFIG_DIR` resolves, `loader.rs`) | `TAGENT_STATE_DIR` (**NEW**, mirrors the existing `TAGENT_CONFIG_DIR` pattern exactly) |
| `HandoffContext` (process-level constant, **NEW**, §5.2) | max size (bytes) | `usize` | `4096` | `TAGENT_HANDOFF_MAX_BYTES` (**NEW**) |
| `extends` resolution (**NEW**, §2.5) | max chain depth | `usize` | `8` (mirrors `agent_builder.rs::MAX_DEPTH`) | n/a — compile-time constant (safety ceiling, not an operator preference) |
| `AgentScheduler` (process-level, **NEW**, §2.3.6) | tick interval | `Duration` | `60s` | `TAGENT_SCHEDULER_TICK_SECS` (**NEW**) |
| No-code enforcement (§2.6) | (not configurable) | — | always on | **none** — a hard invariant, deliberately not a toggle |

### 6.1 Conformance

- Each new manifest key round-trips through parse/serialize exactly like the
  existing `tools_config_parses_allow_globs`-style tests in `config.rs`.
- `TAGENT_STATE_DIR` overrides the checkpoint root exactly as
  `TAGENT_CONFIG_DIR` overrides `agents_dir()` — same resolution order, same
  fallback-with-warn-log behavior.
- `[model].default` in `~/.trusty-agents/config.toml` is consulted only when
  both `TAGENT_MODEL_<NAME>` and `TAGENT_DEFAULT_MODEL` are absent, and only
  before `FALLBACK_MODEL` — the precedence order in §2.3.4, exactly.
- Every env var in this table is read through `env_compat::env_var(new,
  legacy)` if a legacy `OPEN_MPM_*` name is later requested for
  back-compat — none of the **NEW** keys need a legacy alias at
  introduction (they don't exist yet under any name), but the helper is the
  established pattern should one be needed.
- No config key or env var disables the §2.6 no-code enforcement — attempting
  to configure around it (e.g. an env var to skip manifest validation) is
  explicitly rejected as a design goal, not merely undocumented.

---

## 7. SPEC-AGENTFW-06 — Memory Scope Declaration & Model/Provider Resolution

### 7.1 Current state

`MemoryStore` trait (`crates/trusty-agents/src/memory/store.rs:151-171+`):

```rust
#[async_trait]
pub trait MemoryStore: Send + Sync {
    async fn insert(&self, segment: Segment, id: &str, vector: &[f32], payload: serde_json::Value) -> Result<()>;
    async fn search(&self, segment: Segment, query_vec: &[f32], top_k: usize) -> Result<Vec<MemoryResult>>;
    async fn get(&self, segment: Segment, id: &str) -> Result<Option<serde_json::Value>>;
    // (+ delete, not re-quoted here)
}
```

`Segment` (`store.rs:22-37`) is a fixed, crate-defined enum: `AgentMemory |
CodeIndex | Context | Brief | History` — chosen entirely by whichever Rust
call site constructs the tool, **not** selectable from an agent's own
definition file today. `TrustyBackedMemoryStore`
(`crates/trusty-agents/src/memory/trusty_backed.rs:73-139`) backs this with
one `PalaceRegistry` + a SQLite `PayloadStore` sidecar (`payloads.db`) per
`data_root`, hydrating a per-`Segment` in-memory map on construction; each
segment maps to its own `PalaceHandle`, opened lazily.

`AgentInfo.model: String` (`config.rs`) is already an opaque, provider-
qualified string, resolved via `crate::agents::resolve_model` (called at
`md_agent.rs:94`: `resolve_model(&name, &agent_model_raw, None)`), which
layers — per `agents/model.rs` — `TAGENT_MODEL_<UPPER_SNAKE>` (agent-specific
env override) → the agent's own TOML/frontmatter `model` → `TAGENT_DEFAULT_MODEL`
(global env fallback), each tracked by a `ModelSource` variant
(`AgentEnv`/`DefaultEnv`, `model.rs:33/36`) for diagnostics. Once resolved,
`adapter_for_model(&resolved_model)` (`llm/adapter` module) selects the
provider adapter.

### 7.2 Normative requirement

**NEW `memory:` frontmatter/TOML block** — does **not** add a new storage
layer or a new `Segment` variant; it is a purely declarative default so an
agent's own file states which of the **five existing** segments it reads/
writes by default, instead of that choice living only in Rust call-site
code:

```yaml
memory:
  segment: brief        # one of: agent_memory | code_index | context | brief | history (serde snake_case)
  top_k: 5                # default search top_k for this agent's memory-querying tools
```

Validation: an unrecognized `segment` value is a load-time error listing the
five valid variants — the same "helpful list" pattern as the `extends`
errors in §2.5 and the `delegate_to_agent` errors in §5.

**Model/provider resolution — DECIDED (Bob, 2026-07-16): the adapter layer
already exists; use it, don't invent one.** `trusty_common::inference::
InferenceAdapter` (`crates/trusty-common/src/inference/adapter.rs:39`) is
**not** a planned future layer — it already ships `OpenAiCompatAdapter`,
`BedrockAdapter`, `AnthropicAdapter`, and `providers/fireworks.rs`
(epic #2400). trusty-agents' own `resolve_model`/`adapter_for_model` (§2.3.4)
is the correct manifest-facing choke point today; unifying it with
`InferenceAdapter` directly is a separate, already-tracked migration (not
re-scoped by this spec) — the **normative requirement** here is narrower:
**no new call site** introduced by §2–§6 (the resume CLI re-resolving a
phase's model, the credential-brokering HTTP client selecting a provider,
etc.) may bypass `resolve_model`/`adapter_for_model` to hand-roll its own
provider special-case, so that whenever the two adapter layers are unified,
it is a one-file change, not a multi-call-site migration.

Per Bob's decision, the manifest's `model:` key (§2.3.4) is the per-agent
override; the **NEW** `[model].default` key in `~/.trusty-agents/
config.toml` (§6) is the system-wide default provider/model pair, both
expressed in the adapter layer's existing routing-prefix shape
(`"provider/model-id"`, e.g. `"anthropic/claude-opus-4-6"`,
`"bedrock/..."`, `"ollama/..."` — already-established prefixes, `ctrl/
config.rs:82-83,100-101`) — not a new key shape invented for this spec.

### 7.3 Conformance

- An agent declaring `memory: { segment: brief, top_k: 5 }` causes its
  memory-querying tools to default to `Segment::Brief` with `top_k=5`
  without the tool's own Rust code hard-coding either value.
- An agent declaring `memory: { segment: nonexistent }` fails to load with
  an error listing the five valid segment names.
- `grep -rn "adapter_for_model\|resolve_model"` across every file touched by
  this spec's implementation shows no parallel/duplicate provider-detection
  logic introduced outside the existing `llm::adapter`/`agents::model`
  modules.

---

## 8. Phased Roadmap

**Phase 0 — Runtime durability (SPEC-AGENTFW-02).** The single largest gap:
`RunState`, `CheckpointRecord`, phase-boundary `atomic_write` calls,
`tagent resume`. No agent-definition-format changes yet. Ships as its own
issue/PR, API-first (an RPC/CLI method the `tagent resume` subcommand calls,
per the API→CLI→TUI layering). **APPROVED as spec'd** (§10 item 1 closed).

**Phase 1 — Core manifest & no-code enforcement (SPEC-AGENTFW-01 core).**
The `agent.yaml`/`instructions.md` form factor (§2.4), the closed-schema
parser + directory-package file allowlist (§2.6), `extends` resolution
aligned to `compose_agent` (§2.5), and the `tools`/`subagents`/`memory`/
`checkpoints`/`model` primitive bindings (§2.3.1–.3.5) — everything that is
either already-existing-struct-extension or a load-time validation change,
no new daemon-side dispatcher. Ships independent of Phase 0.

**Phase 2 — Tool-calling gaps (SPEC-AGENTFW-03).** In priority order: (a)
`rpc/mod.rs` `tools/call` proxying — blocked on the implementation-time
verification of `trusty-memory`/`trusty-search`'s own execution entry
points (§10 item 3); (b) HTTP-transport MCP client; (c) credential
brokering via the **already-implemented** `trusty_common::inference::
credentials` keyring/file-store resolver (§4.2 item 3) — the fallback-tension
nuance is the only open piece (§10).

**Phase 3 — Structured handoffs (SPEC-AGENTFW-04).** `HandoffContext`,
`RunContext.handoff`, `run_with_context` wiring, clean-context conformance
test. Depends on nothing else in this roadmap; can land in parallel with
Phase 2.

**Phase 4 — Events & channels primitives (SPEC-AGENTFW-01 §2.3.6–.3.7).**
The largest **genuinely new** platform-infrastructure investment in this
spec, deliberately sequenced last: the `EventTriggerDispatcher` (a new
consumer of the existing, currently emit-only `events::bus`), the
`AgentScheduler` (mirroring `TmMonitor`'s ticker pattern), and per-agent
`channels.inbound` routing extending the existing `telegram`/`slack` gateway
handlers (with `channels.tools` needing **zero** new platform code, since it
is just an `McpService` binding to the already-existing `trusty-channels`
crate — that half can land any time after Phase 2, not gated on this phase).

**Phase 5 — Config surface (SPEC-AGENTFW-05).** Not a standalone phase —
each key ships alongside the phase that introduces it; this entry exists
only so the config table (§6) has a single place tracking "landed vs.
not yet."

Each phase is its own issue/PR chain off the epic issue this spec closes
out (#2791) — not a single mega-PR.

---

## 9. Explicit Non-Goals

- **No coded agents, ever.** §2.0/§2.6 are not a soft preference — an agent
  package containing anything beyond instructions + manifest + static data
  assets is a load-time rejection, mechanically enforced, not a lint.
- **No serverless/Vercel-Workflows-equivalent hosted durability engine.**
  trusty-agents remains a local-first tokio daemon; durability is
  phase-level on-disk checkpointing (§3), not a general-purpose
  durable-workflow platform.
- **No true mid-phase (sub-turn) checkpointing, ever — not just "in the
  MVP."** **DECIDED (Bob, 2026-07-16):** phase-level granularity (§3.3) is
  approved as the permanent design, not a placeholder pending a harder
  requirement — §10 item 1 is closed, not merely deferred.
- **No wholesale replacement of the `.toml`/`.md`+frontmatter or
  directory-package (#482) agent formats.** §2.4's additions
  (`agent.yaml`/`instructions.md`) are additive, back-compat-preserving
  extensions of both existing loaders, not a replacement.
- **No new `ToolInvoker` abstraction.** `ToolExecutor` (already unifying
  native, MCP-management, and MCP-external tools today) is retained; §4
  closes concrete gaps within it.
- **No new channel-adapter system.** **DECIDED (Bob, 2026-07-16):** channels
  are explicitly in scope as a primitive (§2.3.7) — but the non-goal is
  building a *new* adapter layer. `channels:` extends the two real existing
  systems (`trusty-channels`' MCP tools, trusty-agents' own inbound gateway
  modules) rather than inventing a third.
- **No OpenTelemetry, full stop.** **DECIDED (Bob, 2026-07-16):**
  trusty-agents is a personal-productivity agent framework, not a
  hosted-platform product — local telemetry only (`tracing` to stderr, the
  existing in-process `Event` bus/SSE stream, `events.rs`/`events_sse.rs`)
  is the permanent design, not a placeholder pending an OTel decision. This
  is a stated **design principle**: no external trace/span exporter is ever
  wired in, matching the framework's local-first, no-hosted-telemetry-vendor
  positioning (see §11).
- **No redesign of `resolve_model`/`adapter_for_model` internals.**
  **DECIDED (Bob, 2026-07-16):** use the existing
  `trusty_common::inference::InferenceAdapter` layer's routing-prefix shape
  (§2.3.4, §7.2) rather than inventing a new one; unifying trusty-agents'
  own narrower adapter trait with it is a separate, already-tracked
  migration, not re-scoped by this spec.
- **No cron-expression scheduling in the MVP.** `events.schedule` (§2.3.6)
  is a minimal interval string, not a cron grammar — no cron-parsing crate
  exists in this workspace today and adding one is deferred (§10).

---

## 10. Owner-Decision Checklist

Each item names the `SPEC-AGENTFW-NN` section(s) it gates. Six items from
the previous revision were resolved by Bob on 2026-07-16 and are removed
from this list entirely (not just marked resolved) per his instruction to
shrink this to genuinely-open items; their resolutions are recorded inline
in the sections they gated (§2.3.4/§7.2 model resolution, §2.5 `extends`
composition, §2.3.7/§9 channels scope, §9 OTel/telemetry, §3.3/§9 checkpoint
granularity) and in §13's change log. What remains:

1. **Credential-backend fallback tension (narrows the old "storage backend"
   item — gates SPEC-AGENTFW-03 §4.2 item 3).** Bob's directive is "never
   plaintext files"; the **existing, already-shipped** `default_store()`
   (`trusty-common/src/inference/credentials/resolver.rs:123-`) falls back to
   `FileKeyStore` (`0600`-permission-hardened, but literally plaintext
   content) when the OS keychain probe fails. Confirm: (a) MCP
   `credential_ref` resolution accepts the same fallback every other
   credential in this codebase already accepts, or (b) it hard-fails when
   the keychain is unavailable rather than falling back, diverging from
   `resolve_key`'s existing behavior for this one caller.
2. **`tools/call` proxy dispatch surface (gates SPEC-AGENTFW-03 item 1).**
   `ServiceDescriptor` has no execute method — confirmed this pass. Needs a
   research pass into `trusty-memory`'s/`trusty-search`'s actual
   tool-execution entry points before implementation; flagged here rather
   than asserting an unverified signature. (Research task, not strictly an
   owner decision, but tracked here so it isn't lost.)
3. **`HandoffContext` size cap (gates SPEC-AGENTFW-04 §5.2).** Confirm 4 KiB,
   or specify a different number.
4. **Checkpoint-disable env var naming (gates SPEC-AGENTFW-05 §6).**
   `TAGENT_CHECKPOINT_DISABLE` is proposed but not grounded against a
   verified existing analogous flag in this pass — confirm naming/shape.
5. **`code_dir` integrity on resume (gates SPEC-AGENTFW-02 §3.5).** Confirm
   no integrity check is needed for partial generated files in the MVP, or
   specify one.
6. **`agent.yaml`/`agent.toml` and `instructions.md`/`persona.md` deprecation
   timeline (gates SPEC-AGENTFW-01 §2.4).** The spec establishes "prefer new,
   fall back to legacy, indefinitely" — confirm indefinite dual support, or
   specify a sunset date/version after which `agent.toml`/`persona.md`
   packages must be migrated.
7. **`channels.inbound` routing design (gates SPEC-AGENTFW-01 §2.3.7).**
   Confirmed in scope and grounded in the existing `telegram`/`slack` gateway
   modules (§2.3.7) — the specific routing mechanism is not yet chosen:
   a dedicated bot/token per channel-bound agent, a slash-command prefix
   within the existing single bot, or a chat/channel-id → agent-name mapping
   table. All three are consistent with the spec as written; pick one before
   implementation.
8. **`events.schedule` syntax ceiling (gates SPEC-AGENTFW-01 §2.3.6).** The
   MVP's minimal interval string (`"15m"`) is a firm non-goal boundary for
   full cron syntax (§9) — confirm this is acceptable long-term, or flag
   that cron-expression support (and its new crate dependency) should be
   pulled forward.
9. **Static-asset allowlist exact extension set (gates SPEC-AGENTFW-01
   §2.6).** `.md`/`.yaml`/`.yml`/`.json`/`.txt` is proposed — confirm or
   amend before the no-code enforcement ships (this list, once shipped, is a
   breaking change to loosen but a safe one to tighten).

---

## 11. Background — Eve Review (Appendix)

Vercel's **Eve** launched 2026-06-17 (Apache-2.0, public beta). Primary
sources: [vercel.com/blog/introducing-eve][eve-blog],
[vercel.com/docs/eve][eve-docs], [github.com/vercel/eve][eve-repo].

Core idea: **"a file's name and place in the tree is its definition."** An
agent is a directory: `agent.ts` (`defineAgent({model:...})`) +
`instructions.md` (system prompt) + `tools/*.ts` (one file per tool,
filename = tool name) + `subagents/*/` (full nested agent directories,
called like a tool, each with a clean context window) + `connections/*`
(`defineMcpClientConnection` — MCP-native, with credential brokering so the
model never sees a secret). Execution runs on **Vercel Workflows**: durable,
checkpointed, survives crash/deploy, zero-cost indefinite pauses for human
approval. Streaming: NDJSON + OpenTelemetry spans per turn. Deployment:
`eve dev` → `vercel deploy`, deploy-safe versioning of in-flight sessions,
multi-channel adapters (Slack/Discord/Teams/Telegram/Twilio/GitHub/Linear).

**Portable ideas (informed §2–§7 above):** convention-over-configuration
agent definition; MCP as the tool-calling backbone with credential
brokering; structural, clean-context subagent delegation.

**Platform-specific, not ported as-is:** durability-via-Vercel-Workflows (no
platform substrate exists for a self-hosted tokio daemon — §3 builds a
narrower, phase-level equivalent instead); the stateless request/session
HTTP shape (a resident daemon doesn't need it for intra-process durability,
only for surviving its own restarts, which §3 addresses directly);
zero-cost indefinite pause (a serverless billing property with no local
analog — removes an objection, not a design requirement); OpenTelemetry
export (Eve's Braintrust/Datadog/Honeycomb/Jaeger story is a hosted-platform
observability integration trusty-agents deliberately does not adopt, per
§9 — a personal-productivity, local-first tool has no fleet to observe
centrally).

**The core positioning differentiator (§2.0), stated explicitly rather than
left implicit:** Eve is **code-first** — `agent.ts` + `tools/*.ts` are
TypeScript, and an agent ships its own tool *implementations* alongside its
definition. trusty-agents, per Bob's 2026-07-16 decision, is
**declaration-first, mechanically enforced**: an agent's own package can
never contain logic — only Markdown instructions and a YAML manifest
referencing platform-hosted primitives by name (§2.6's load-time rejection
of any foreign/executable content is the concrete guarantee, not a style
convention). Where Eve's model is "bring your own tool code, we'll run it,"
trusty-agents' model is "reference a tool the platform already hosts, or
don't." This is a narrower agent-authoring surface by design — trading
Eve's code-level flexibility for an auditable, injection-resistant agent
package that a non-engineer persona can safely author or share.

---

## 12. Background — Gap Analysis (Appendix)

| Eve abstraction | trusty-agents today | Disposition |
|---|---|---|
| Directory-as-definition, one file per tool/subagent | Two loaders (flat `.toml`/`.md` + #482 directory-package); no `tools:`/`subagents:`/`extends:` declaration | Closed by SPEC-AGENTFW-01 |
| MCP-native tools, credential brokering | `ToolExecutor` already unifies native + MCP-external + MCP-management tools; external MCP calling is live (stdio-only); no credential brokering | Closed by SPEC-AGENTFW-03 |
| Subagent-as-directory, clean context per call | `DelegateToAgentTool` + `AgentRunner` exist; flat string context, no declared subagent set, no clean-context conformance test | Closed by SPEC-AGENTFW-01 (declaration) + SPEC-AGENTFW-04 (protocol) |
| Durable, checkpointed execution | Zero on-disk state during an in-flight run; `perf.flush` only at the very end | Closed by SPEC-AGENTFW-02 |
| NDJSON/OTel streaming | Full `Event` enum + SSE stream already in place, arguably richer (AST-operation, persona-detection, LLM-lifecycle events) | Not a gap — already present |
| State persistence delegated to platform | `TrustyBackedMemoryStore`/Palace integration (#379) is deeper than Eve's story, but not declaratively surfaced in the agent's own file | Closed by SPEC-AGENTFW-06 |
| Deploy-safe versioning of in-flight sessions | No equivalent — same root cause as the durability gap | Addressed indirectly by SPEC-AGENTFW-02's checkpoint/resume |
| Multi-channel deployment | Two real, separate systems: `trusty-channels` (MCP tool servers) and trusty-agents' own inbound `telegram`/`slack` gateways (project-scoped, not agent-scoped) | Closed by SPEC-AGENTFW-01 §2.3.7 — extends both, invents neither |
| Code-first agent authoring (`agent.ts` + `tools/*.ts`) | N/A — never had this, and per Bob's 2026-07-16 decision never will | **Deliberate non-parity** — declaration-first is the differentiator, not a gap (§2.0, §11) |
| OpenTelemetry / hosted observability integrations | Full `Event` enum + SSE stream, local-only | **Deliberate non-parity** — DECIDED no OTel, ever (§9) |

---

## 13. Change Log

- **2026-07-15 (v1)** — Initial draft merged as PR #2792. Comparative review
  of Vercel's Eve spec plus design *directions* for a Rust equivalent — no
  normative content. **Rejected by Bob:** "the eve research is NOT a spec,
  it refers to one but doesn't actually contain one."
- **2026-07-15 (v2)** — Full rewrite. Every `SPEC-AGENTFW-01..06` section
  made normative: exact frontmatter schema + `extends` resolution algorithm
  and error cases (§2), an explicit `RunState` machine + `CheckpointRecord`
  schema + failure matrix (§3), a corrected (not invented) unified
  tool-calling picture plus the three real remaining gaps (§4), an exact
  `HandoffContext` struct + clean-context conformance test (§5), a full
  config-key table (§6), and a declarative `memory:`/model-resolution
  constraint (§7) — each re-verified against `origin/main` @ `78868a62`
  with `file:line` citations, with anything not already in the tree marked
  **NEW**. Eve review and gap analysis demoted to background appendices
  (§11–§12). Owner-decision checklist (§10) added, gating specific SPEC
  sections rather than floating as generic open questions.
- **2026-07-16 (v3)** — Two rounds of owner input, both folded in:
  (1) **Bob's foundational decision: agents are declarative-only, ever.**
  §2 reframed as a primitive-binding manifest (tools, subagents, memory,
  model, checkpoints, **NEW** events, **NEW** channels — §2.2), with a
  mechanically-enforced no-code invariant (§2.6: closed-schema parsing +
  directory-package file allowlist) and a form factor decision
  (`agent.yaml`/`instructions.md`, back-compat with `agent.toml`/`persona.md`,
  §2.4). (2) **Bob resolved all six originally-open questions**, each
  grounded against real code discovered this pass rather than accepted at
  face value: checkpoint granularity APPROVED as permanent (not MVP-only,
  §9); channels grounded in the **real** `trusty-channels` crate (epic #2636)
  plus trusty-agents' own existing (but project-scoped) `telegram`/`slack`
  gateways — no new adapter system (§2.3.7); credential brokering resolved
  to the **already-implemented** OS-keyring resolver
  (`trusty_common::inference::credentials`, `keyring = "3"` already a
  workspace dependency) with one narrowed fallback-tension question
  surfaced, not invented (§4.2, §10); OpenTelemetry explicitly and
  permanently rejected — local-only telemetry is a design principle, not an
  open question (§9); `extends` composition realigned from an invented
  "child-replaces + opt-in token" rule to **base-first concatenation**,
  mirroring the real, proven `crates/trusty-mpm/src/core/agent_builder.rs::
  compose_agent` (`MAX_DEPTH=8`, cycle/depth error shapes) rather than a
  bespoke algorithm (§2.5); model/provider resolution confirmed to route
  through the **already-existing** `trusty_common::inference::
  InferenceAdapter` layer, with a new `[model].default` config key
  formalizing today's env-var-only global default (§2.3.4, §7.2, §6).
  §10 shrank from 8 items to 9 narrower ones — six fully resolved items
  removed outright per Bob's instruction; three new items surfaced by the
  declarative-only rework (form-factor deprecation timeline, channel-inbound
  routing mechanism, static-asset allowlist confirmation) took their place
  alongside the untouched originals (`HandoffContext` size cap, checkpoint
  env-var naming, `code_dir` resume integrity, the `tools/call` proxy
  research task).

[eve-blog]: https://vercel.com/blog/introducing-eve
[eve-docs]: https://vercel.com/docs/eve
[eve-repo]: https://github.com/vercel/eve
