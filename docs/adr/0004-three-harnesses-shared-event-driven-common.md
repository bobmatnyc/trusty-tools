# 0004. Three distinct harnesses (coding / meta / agentic) on a shared event-driven trusty-common foundation

- **Status:** Proposed
- **Date:** 2026-06-05
- **Scope:** Workspace-wide (all harness crates + `trusty-common`)
- **Supersedes / Superseded by:** —

## Context

The trusty-tools workspace has grown to include three families of AI
orchestration crates that serve fundamentally different principals and
have different responsibilities:

1. **`trusty-code`** — per-project coding orchestration (`tcode` binary), the
   Claude-Code-compatible harness. Currently a Phase 0 scaffold
   (`crates/trusty-code/`); full extraction from `open-mpm` is tracked in
   epic #587.

2. **`trusty-mpm`** — the operator's control plane for multi-project, multi-
   session workflow management (`tm` / `trusty-mpmd` / TUI / Telegram). It
   oversees coding sessions, relays hooks, and exposes an MCP server to Claude
   Code. It delegates coding work downward; it does not execute code itself.

3. **`open-mpm`** (proposed rename: `trusty-agents`) — a general-purpose
   agentic harness for non-coding knowledge-worker workflows (CRM, HR,
   scheduling, memory, communications). It uses the same PM-orchestrator +
   sub-agent subprocess pattern as trusty-code but is not a coding harness.

Two problems motivate this ADR:

**Problem A — unclear boundaries.** Both `open-mpm` and `trusty-mpm` carry
orchestration/PM-style machinery. `open-mpm/src/workflow/` holds a workflow
engine that is generic enough to belong in `trusty-mpm`; `open-mpm/src/ctrl/`
holds both coding-agent orchestration (destined for `trusty-code`) and
knowledge-worker persona orchestration (staying in `trusty-agents`). Without
an explicit decision, each harness grows in undefined directions and the
workspace accumulates duplicate machinery.

**Problem B — event-driven gap.** `open-mpm` has a process-global event bus
(`src/events.rs`, `tokio::sync::broadcast`) and a UDS inter-project message bus
(`src/bus/`). `trusty-mpm` has a hook relay but no typed broadcast bus in the
daemon. `trusty-code` has nothing (Phase 0 stub). The three harnesses cannot
coordinate, fan out, or stream progress to UIs unless they share an event
foundation.

Additionally, cross-harness infrastructure (the `ToolExecutor` trait, RBAC
`ServiceTier` enum, intent classifier, event types) currently lives inside
`open-mpm` or the thin `open-mpm-agent-api` crate, making it unavailable to
`trusty-code` and `trusty-mpm` without a peer-crate dependency, which would
couple harnesses that should be independent.

## Decision

### 1. Three distinct harnesses

We will maintain three distinct harnesses, each with a clear identity and
scope:

| Harness | Crate | Analogy | Principal |
|---------|-------|---------|-----------|
| **trusty-code** | `crates/trusty-code/` | Claude Code | Developer / CI pipeline |
| **trusty-mpm** | `crates/trusty-mpm/` | Claude MPM | Operator / PM role |
| **trusty-agents** | `crates/open-mpm/` (rename) | OpenClaw / Hermes | Knowledge worker |

**Boundaries:**
- PM/workflow/multi-agent orchestration belongs to **trusty-mpm** (the meta-harness).
- General non-coding assistants belong to **trusty-agents**.
- Per-project coding execution belongs to **trusty-code**.
- All three are peers; none is a dependency of another at the library level.

### 2. Rename open-mpm to trusty-agents

We will rename the `open-mpm` crate to `trusty-agents` to reflect its
actual role. The package `name` in `Cargo.toml` changes from `open-mpm` to
`trusty-agents`; binaries are renamed accordingly. This rename is tracked
separately from the #587 extraction work.

### 3. Shared commonality lives in trusty-common

All infrastructure that is needed by more than one harness belongs in
`trusty-common` behind a feature flag. We will add the following features to
`trusty-common`:

| Feature | Contents | Migrated from |
|---------|----------|---------------|
| `events` | `HarnessEvent` enum, `HarnessKind` enum, `EventBus` type alias | `open-mpm/src/events.rs` |
| `tool-registry` | `ToolExecutor` trait, `ToolResult`, `ServiceTier` RBAC, `UserIdentity` | `open-mpm-agent-api`, `open-mpm/src/rbac/` |
| `intent` | `IntentClass` enum, heuristic classifier | `open-mpm/src/intent/` |

The `open-mpm-agent-api` crate may be retired once `tool-registry` is in
`trusty-common`.

### 4. All three harnesses are event-driven

Every harness will publish a `HarnessEvent` stream via a
`tokio::sync::broadcast::Sender<HarnessEvent>` (from `trusty-common::events`)
held in its process-global state. This is a binding architectural principle.

Each harness is responsible for:
- Publishing lifecycle events (`SessionStarted`, `AgentStarted`, `ToolCall`, etc.)
- Exposing a subscriber interface (SSE endpoint for HTTP daemons; stderr relay
  for subprocess flows)
- Consuming events from delegated harnesses to track delegation progress

The `open-mpm` UDS bus (`src/bus/`) remains for cross-project open-mpm
coordination; it is distinct from the `HarnessEvent` intra-process bus.

### 5. Inter-harness delegation graph

The delegation graph is:

```
trusty-agents  →  trusty-code   (coding task: HTTP POST / CLI subprocess)
trusty-agents  →  trusty-mpm    (managed multi-agent project: MCP agent_delegate)
trusty-mpm     →  trusty-code   (session launch: SessionLaunchConfig → tcode subprocess)
```

No reverse edges. No harness takes a Cargo dependency on a sibling harness.
Delegation is over network/IPC boundaries only.

### 6. Boundary resolution for open-mpm/trusty-mpm overlap

We will apply the following migrations as part of #587 and the rename:

| Module | Current location | Migration target |
|--------|-----------------|-----------------|
| `workflow/` (WorkflowEngine, phase-based pipelines) | `open-mpm/src/workflow/` | Migrate to `trusty-mpm` or `trusty-common` (owner decision required — see Open Questions) |
| Coding-agent ctrl loop | `open-mpm/src/ctrl/` (coding agent subset) | Extract to `trusty-code` via #587 |
| Knowledge-worker ctrl loop | `open-mpm/src/ctrl/` (persona subset) | Stays in `trusty-agents` |
| Cross-project session registry | `open-mpm/src/session_registry.rs` | Migrate to `trusty-mpm` |
| Event bus types | `open-mpm/src/events.rs` | Migrate to `trusty-common::events` |
| Tool registry + RBAC | `open-mpm/src/tools/traits.rs`, `src/rbac/` | Migrate to `trusty-common::tool-registry` |
| Intent classifier | `open-mpm/src/intent/` | Migrate to `trusty-common::intent` |
| Harness adapter detection | `open-mpm/src/adapters/` | Migrate to `trusty-mpm` |

## Consequences

### Positive

- **Clarity for contributors:** a new contributor can identify the right crate
  for a feature by asking "is this coding? meta-orchestration? knowledge-worker
  assistance?" with a clear answer.
- **No peer-crate coupling:** harnesses remain independent; trusty-agents can
  be deployed without trusty-mpm, and vice versa.
- **Shared event model:** all three harnesses speak the same `HarnessEvent`
  wire format, enabling cross-harness monitoring and future unified dashboards.
- **trusty-common grows the right way:** adding `events`, `tool-registry`, and
  `intent` features follows the established pattern (previously: `mcp`, `rpc`,
  `memory-core`, `tickets`, `symgraph`, `embedder`).
- **`trusty-agents` name signals non-coding:** the rename removes the
  `mpm` namespace collision and communicates the harness's actual purpose.

### Negative / Risks

- **Migration cost:** moving `WorkflowEngine`, event bus, tool registry, and
  intent classifier requires coordinated changes across multiple crates and a
  version bump for each affected crate.
- **#587 scope risk:** the boundary between "coding ctrl" and "knowledge-worker
  ctrl" inside `open-mpm/src/ctrl/` is not always obvious from the code alone.
  The split may surface hidden coupling.
- **Rename churn:** renaming `open-mpm` to `trusty-agents` requires updating
  all call sites, `[patch.crates-io]` entries, documentation, and deployment
  configurations.
- **Event-driven gap for trusty-code and trusty-mpm:** neither has a broadcast
  bus today; implementing one requires daemon-level changes to both crates.

### Open Questions

1. **WorkflowEngine destination:** should `open-mpm/src/workflow/` move to
   `trusty-mpm` (making the meta-harness the sole owner of workflow logic) or
   to `trusty-common` (making workflow pipelines available to all three
   harnesses)? Recommendation: `trusty-mpm` first; promote to `trusty-common`
   only if trusty-agents needs it.

2. **Timing of open-mpm rename:** should the rename happen in the same PR as
   #587 Phase 1, or in a dedicated rename PR before #587 starts? Recommendation:
   dedicated rename PR before #587 to reduce scope.

3. **open-mpm-agent-api retirement:** once `tool-registry` lands in
   `trusty-common`, `open-mpm-agent-api` can be retired. Should it be
   deprecated immediately or kept as a re-export shim? Recommendation: shim
   for one release, then remove.

See [docs/architecture/harnesses.md](../architecture/harnesses.md) for the
full architecture spec, delegation contracts, and event model details.
