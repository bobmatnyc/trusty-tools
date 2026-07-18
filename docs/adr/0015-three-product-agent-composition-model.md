# 0015. Unified Agent Composition: Shared `.md`+YAML+`extends` Format Across Three Products

- **Status:** Proposed
- **Date:** 2026-07-18
- **Scope:** Workspace-wide (trusty-agents, trusty-mpm, trusty-code)
- **Supersedes / Superseded by:** — (complements DOC-37, issues #2791/#2792)

## Context

The trusty-tools workspace hosts three distinct agent runtime products:

1. **trusty-agents** — a standalone personal-productivity agent framework competing with OpenClaw-class systems. Agents are 100% declarative (Markdown + YAML manifest), with no coded components per DOC-37's foundational principle (§2.0).

2. **trusty-mpm** — a Claude Code harness orchestrator managing PM sessions, specialist-agent catalogs (37+ agents), and session-lifecycle instrumentation. Uses `.md` + YAML-frontmatter agent definitions with a shared template layer (`BASE-*.md` hierarchy).

3. **trusty-code** — a code-focused agent platform (issue #2897 migration) with similar spec structure to trusty-mpm: `.md`/YAML agents sharing `extends`-compose templates.

**Key insight:** All three products benefit from a **unified config format** (`.md` + YAML frontmatter + `extends:` inheritance), shared via `trusty-agents-common`. The differentiator is not format, but **the purpose agents serve and composition patterns used** — and trusty-agents uniquely layers additional composition modes (runtime delegation, PM-as-agent) on top of the shared format to support orchestration patterns.

## Decision

Adopt a unified 3-product agent composition model with shared format and purpose-based differentiation:

### Dimension 1: Shared Format & `extends` Mechanism

**All three products** use the same `.md`+YAML+`extends:` format, implemented via `trusty-agents-common`:

- **`.md` + YAML frontmatter** — single-file agent definitions (Markdown prose + YAML manifest)
- **`extends:` inheritance** — single-parent scalar, resolved at agent-load time via `trusty-agents-common`'s shared implementation aligned to `compose_agent`
- **Merge semantics** — identical across all three products:
  - `MAX_DEPTH = 8` (cycle/depth guard)
  - Base-first concatenation for prose (`instructions.md`)
  - Scalar child-overrides-when-present
  - List union for `tools`, `subagents.allowed`, etc.
  - Same error enum (ExtendsNotFound, ExtendsCycle, ExtendsTooDeep)

### Dimension 2: `extends` Purpose — Use Case Differentiation

While format is shared, **why agents use `extends` differs by product:**

| Product | `extends` Purpose | Example |
|---|---|---|
| **trusty-agents** | **Personalization** — a user agent extends a stock/base agent to layer personal customization without forking | A user creates `my-researcher.md` that `extends: researcher`, adding personal prompts/tools to the base |
| **trusty-mpm** | **Specialist template hierarchy** — a coded-task specialist agent extends `BASE-code-reviewer` to inherit common review logic | `deep-reviewer.md` extends `BASE-code-reviewer` to layer extra scrutiny rules |
| **trusty-code** | **Specialist template hierarchy** — same as mpm; shared large catalog of BASE-* templates | Similar specialist patterns as mpm |

### Dimension 3: trusty-agents Additional Composition Mode — Runtime Delegation + PM-as-Agent

**trusty-agents ONLY** layers an additional, runtime-based composition mode on top of the shared format:

- **Runtime delegation** — when one agent needs to invoke another at execution time (not at definition time), it uses `DelegateToAgentTool` + `subagents.allowed` allowlist (DOC-37, §5).
- **PM/orchestrator is declarative** — the PM agent itself is a flat `.md`+YAML agent (not special-cased code) with a `delegate` tool in its `tools` section. It does NOT inherit from a base agent; it is a standalone orchestrator agent.
- **Depth-guarded recursion** — delegation chain depth is guarded (related to issue #2894) to prevent runaway loops.
- **Coexists with `extends`** — an agent can both inherit via `extends:` (personalization) AND delegate to other agents at runtime (orchestration). These are orthogonal composition modes.

### Dimension 4: Cross-Product Agent Proxies

When trusty-agents needs to invoke trusty-mpm or trusty-code agents:

- A **delegation proxy** agent (a thin `.md`+YAML agent in trusty-agents) wraps the delegation:

```yaml
# trusty-agents/.trusty-agents/agents/mpm-proxy.md
---
name: mpm-proxy
role: subagent
description: Delegates orchestration tasks to trusty-mpm
tools:
  allowed: [delegate_to_agent]
---

This agent forwards orchestration and session-management tasks to trusty-mpm's PM orchestrator.
```

- The proxy is invoked via `delegate_to_agent` by trusty-agents' PM; it then invokes the real trusty-mpm orchestrator, which may itself use all 37 of mpm's inherited specialist agents.
- Proxies are **thin delegation handles** — they never parse or import mpm's agent catalog or `extends` chains; the actual inheritance stays inside trusty-mpm/trusty-code.

### Why This Works

1. **Format unification eliminates duplication.** All three products share the `.md`+YAML parser, the `extends` resolver, and the merge rules (housed in `trusty-agents-common`). No reimplementation across three binaries.

2. **Purpose-based differentiation is clean.** Format stays identical; the *reason* agents use `extends` (personalization vs. specialist templates) is a product-design choice, not a technical fork.

3. **"Declarative-only" is universal.** All agents in all three products are 100% declaration: `.md` prose + YAML manifest, no code. Even trusty-agents' PM/orchestrator agent is a declarative `.md`+YAML agent with a `delegate` tool.

4. **Composition modes coexist.** trusty-agents' PM agent can both extend a base agent (for persona/customization) and delegate to sub-agents (for orchestration). These are independent concerns, both provided by the platform.

5. **trusty-agents-common is genuinely shared.** The crate houses the runtime traits (ToolExecutor, AgentRunner, RunContext, delegate primitive) **and** the `.md`+YAML+`extends` config machinery. All three products use it, confirming the name is accurate.

## Consequences

### Positive

- **Unified format reduces friction.** Operators, docs, and tooling all target one `.md`+YAML+`extends` shape, not product-specific variants.
- **Shared machinery = shared maintenance.** Parser, resolver, and merge rules live in one place; bug fixes and enhancements benefit all three products.
- **Personalization is built in for trusty-agents.** Users can naturally extend agents without forking or editing the base.
- **Runtime delegation enables trusty-agents orchestration.** The PM-as-agent pattern, layered on top of `extends`, gives trusty-agents the composition power it needs for personal-productivity workflows.
- **Proxies keep product boundaries clean.** Cross-product invocation is explicit and intentional (via delegation, not shared inheritance chains).

### Trade-offs

- **trusty-agents users see more format power than they typically need.** A personal-productivity user focuses on personalization via `extends`; runtime delegation is available but optional (primarily used by the PM agent itself).
- **Large shared-template catalogs (mpm/tcode) and personalization agents (trusty-agents) coexist in the same format.** This is acceptable: both use the same `.md`+YAML+`extends` format; the operational concern is managing separate agent directories or namespacing, not technical architecture.

## Verification

- [ ] All three products load agents via the same `.md`+YAML parser (trusty-agents-common).
- [ ] `extends:` resolves identically across all three products (same MAX_DEPTH, merge rules, error handling).
- [ ] trusty-agents agents can `extends:` a base agent for personalization (e.g., `my-researcher extends researcher`).
- [ ] trusty-mpm agents with `extends:` load and compose correctly (BASE-* hierarchy, verified by existing tests).
- [ ] trusty-code agents follow the same format and compose as trusty-mpm (verified by issue #2897's test coverage).
- [ ] A PM-as-agent (flat `.md`+YAML with `delegate` tool) invokes sub-agents via DelegateToAgentTool (delegation tests).
- [ ] Agent proxies (mpm-proxy in trusty-agents) successfully delegate to mpm agents that themselves use `extends:`.
- [ ] Documentation (DOC-37, tm-agent-architecture skill) reflects unified format and per-product uses (verified by the spec delta).

## Related Issues

- **#2791, #2792** — Eve spec (DOC-37) design reviews that informed this decision.
- **#2897** — trusty-code `.md` unification onto the shared format.
- **#2894** — Hierarchical delegation depth/cycle guards for trusty-agents' runtime orchestration.
- **#2892** — Composition and reuse epic.
