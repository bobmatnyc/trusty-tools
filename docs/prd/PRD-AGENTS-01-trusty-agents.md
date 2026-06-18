# trusty-agents — Rust-powered durable agent framework {#PRD-AGENTS-01}

**ID:** PRD-AGENTS-01  
**Status:** Draft — Future reference. NOT prioritized or scheduled.  
**Owner:** Product  
**Version:** v1  
**Last-updated:** 2026-06-18  
**Related specs/ADRs:** None yet (design phase)

---

## Problem & Context

Existing agent frameworks (OpenClaw, Hermes, et al.) excel at orchestration but fall short on durability,
operational flexibility, and principled agent composition. The gap:

- **No durability story:** Agents crash or restart; session state is lost. Developers must implement
  their own recovery logic.
- **Single-deployment model:** Most are "local OR cloud," not both. Switching between local iteration
  and remote execution requires architecture changes.
- **Ad-hoc agent composition:** Agent definition and capability inheritance are informal. Teams
  duplicate capability definitions or manually manage agent roles.
- **Proprietary frameworks:** Agent specifications are framework-specific. No standards-based
  foundation for reuse or interop.
- **Productivity agents underserved:** Most frameworks optimize for single-turn or workflow tasks;
  productivity agents (multi-session, multi-project context) are afterthoughts.

**trusty-agents** fills this gap: a Rust-powered framework that combines durable session state
(inspired by trusty-mpm's session harness), local+remote execution flexibility, principled agent
inheritance, and standards-based agent definitions.

---

## Target Users / Personas

| Persona | Need | Context |
|---------|------|---------|
| **Developer (local iteration)** | Build and test agents locally without cloud setup | Early-stage agent work, rapid iteration |
| **Developer (deployment)** | Deploy agents remotely to serve multiple users | Production multi-tenant scenarios |
| **Productivity user** | Run multiple specialized agents across projects | Multi-project workflows, session continuity |
| **Platform builder** | Compose reusable agent templates and capabilities | Scaling agent definitions across teams |

---

## Goals & Non-Goals

### Goals

- **Durability:** Agent and session state persists across crashes and restarts. Multi-turn conversations
  resume with full context.
- **Flexible deployment:** Same agent code runs locally or remote (daemon / HTTP / MCP) with no
  architecture changes.
- **Principled composition:** Agents inherit capabilities via a class hierarchy (`BASE_AGENT` → role-specific
  → concrete agent); capability definitions are reusable and composable.
- **Standards-based:** Agent definitions are JSON (or YAML); wherever a standard exists (LLM APIs, tool
  calling, protocol conventions), use it.
- **Productivity-first UX:** Multi-session harness, persistent context, project scoping, and an agent
  classifier that routes tasks to the right agent variant.

### Non-Goals

- Single-turn chat. (Focus on multi-turn, persistent agents.)
- Replacing LLM providers. (trusty-agents integrates with LLM APIs; it is not a model.)
- UI/UX layers. (Agents expose APIs; frontends are separate.)
- Handling non-agent workflows. (Agents are the unit; orchestration is separate.)

---

## User Stories / Jobs-to-be-Done

1. **As a developer**, I want to define an agent once in JSON and run it locally or deployed to a
   daemon, so I can iterate fast without duplicating code.

2. **As a platform builder**, I want to inherit agent capabilities via a hierarchy (e.g.,
   `BASE_AGENT` → `ENGINEER_AGENT` → `RUST_ENGINEER_AGENT`), so I can compose specialized agents
   without duplication.

3. **As a productivity user**, I want agents to remember multi-session context and route new requests
   to the right agent, so I can work across projects without re-explaining my setup.

4. **As an operator**, I want agents to survive crashes and maintain session state in durable storage,
   so I don't lose context and can upgrade without user-facing interruption.

5. **As an integrator**, I want agent definitions to include their dependent skills and capabilities,
   so I can deploy a self-describing agent package.

---

## Requirements

### Functional Requirements

#### Core: Durable Session Model {#PRD-AGENTS-02}
**ID:** PRD-AGENTS-02  
**Status:** Draft  
**Priority:** Must  

Agents maintain persistent session state across restarts. Session data includes conversation history,
tool outputs, intermediate state, and agent-specific metadata. Sessions survive process crashes and
can be resumed or migrated.

**Why:** Durability is the defining feature that separates trusty-agents from stateless frameworks.
Inspired by trusty-mpm's proven session model.

---

#### Core: Local OR Remote Execution {#PRD-AGENTS-03}
**ID:** PRD-AGENTS-03  
**Status:** Draft  
**Priority:** Must  

An agent runs unmodified in three modes: (1) as a standalone local binary, (2) as an HTTP daemon,
or (3) via stdio MCP bridge. Configuration selects the mode; agent code is identical.

**Why:** Developers iterate locally; production deploys remotely. Forcing a choice is operational friction.

---

#### Core: Agent Inheritance Framework {#PRD-AGENTS-04}
**ID:** PRD-AGENTS-04  
**Status:** Draft  
**Priority:** Must  

Agents inherit from a typed hierarchy: `BASE_AGENT` (traits, session lifecycle, durability hooks)
→ role-specific base (`ENGINEER_AGENT`, `ANALYST_AGENT`, `COORDINATOR_AGENT`, etc.) → concrete
agent (project-specific, fine-tuned).

Each layer defines and exposes capabilities; child agents inherit, override, or extend. The hierarchy
is discoverable and composable.

**Why:** Principled composition prevents capability drift and enables reuse. Mirrors the proven
claude-mpm BASE_AGENT pattern.

---

#### Core: JSON Agent Definition with Skill Dependencies {#PRD-AGENTS-05}
**ID:** PRD-AGENTS-05  
**Status:** Draft  
**Priority:** Must  

Agent definitions are declarative JSON (or YAML). The schema includes:

- Agent name, role, and hierarchy (`inherits_from`).
- Declared capabilities and traits.
- Dependent skills (list of skill names and versions the agent requires).
- Model routing (which LLM model(s) for this agent; can override global default).
- Session storage backend (file, database, MCP-backed).
- Execution mode (local, daemon, stdio).

**Why:** Self-describing agents enable reproducible deployments, skill dependency resolution, and
automated packaging.

---

#### Agent Classifier (Open Design Question) {#PRD-AGENTS-06}
**ID:** PRD-AGENTS-06  
**Status:** Draft  
**Priority:** Should  

An agent classifier routes incoming requests to the most appropriate agent variant based on task
type, context, and agent capability matrix. The classifier is itself an agent or a policy engine.

**Open question:** Is there an existing standard (OpenAI, anthropic-sdk, LLM routing pattern) for
agent classification and inheritance? If not, design and implement trusty-agents' own. This is a
material design decision; record outcome in an ADR.

**Why:** Multi-project, multi-agent systems need smart routing. Avoids user-facing agent selection.

---

### Non-Functional Requirements

#### Reliability & Durability {#PRD-AGENTS-07}
**ID:** PRD-AGENTS-07  
**Status:** Draft  
**Priority:** Must  

- Sessions persist to durable storage (filesystem, database, or MCP-backed service).
- In-flight requests complete or are rolled back on crash.
- No session data loss due to process restart or machine failure.
- Target: RPO (Recovery Point Objective) ≤ 1 second; RTO (Recovery Time Objective) ≤ 5 seconds
  for local deployment; ≤ 30 seconds for remote.

**Why:** Durability is the north-star differentiator.

---

#### Performance {#PRD-AGENTS-08}
**ID:** PRD-AGENTS-08  
**Status:** Draft  
**Priority:** Should  

- Agent initialization: ≤ 500ms (local), ≤ 2s (remote via HTTP).
- Session resume: ≤ 200ms for cold start (fetch from disk), ≤ 10ms (warm cache).
- Request latency: ≤ 50ms overhead from session durability (async log, not synchronous).

**Why:** Local iteration and remote deployments both need responsive feel.

---

#### Standards Compliance {#PRD-AGENTS-09}
**ID:** PRD-AGENTS-09  
**Status:** Draft  
**Priority:** Must  

- Agent LLM calls use anthropic-sdk or OpenRouter (no proprietary wrapping).
- Tool calling and responses follow standard JSON-RPC or MCP conventions.
- Session storage APIs are pluggable (file, database, MCP service).
- Agent definitions are JSON Schema–compliant; schema is published and versioned.

**Why:** Interop, portability, and ecosystem fit.

---

#### Rust Implementation {#PRD-AGENTS-10}
**ID:** PRD-AGENTS-10  
**Status:** Draft  
**Priority:** Must  

Framework and runtime are implemented in Rust. Agents can invoke Rust, Python, or JavaScript
tools. MSRV ≥ 1.91 (trusty-tools baseline).

**Why:** Durability, performance, and ecosystem fit (already in trusty-tools workspace).

---

## Acceptance Criteria

**PRD-AGENTS-02 (Durable Session Model)**
- [ ] Session data persists to disk or database without explicit user code.
- [ ] Sessions resume with full conversation history and tool output.
- [ ] Crash simulation (kill -9) does not lose uncommitted state.
- [ ] Benchmark: session resume from disk ≤ 200ms.

**PRD-AGENTS-03 (Local OR Remote Execution)**
- [ ] Single agent runs as (1) standalone binary, (2) HTTP daemon, (3) stdio MCP without code change.
- [ ] Configuration file selects mode; no conditional compilation.
- [ ] Session semantics are identical across all three modes.

**PRD-AGENTS-04 (Agent Inheritance Framework)**
- [ ] BASE_AGENT trait defines session lifecycle, durability hooks, and required capabilities.
- [ ] At least two role-specific base agents (e.g., ENGINEER_AGENT, ANALYST_AGENT) exist and inherit from BASE_AGENT.
- [ ] Concrete agents inherit and override; capability list is discoverable.
- [ ] No code duplication across agent hierarchy.

**PRD-AGENTS-05 (JSON Agent Definition)**
- [ ] JSON schema defines agent, capabilities, skills, model routing, storage backend, execution mode.
- [ ] Example agent definitions exist and are deployable.
- [ ] Skill dependencies are resolved and validated at boot.

**PRD-AGENTS-06 (Agent Classifier)**
- [ ] Design document or ADR exists stating the classification strategy.
- [ ] If no standard found: trusty-agents classifier is implemented.
- [ ] Classifier correctly routes 90%+ of test queries to expected agent.

**PRD-AGENTS-07 (Reliability & Durability)**
- [ ] Session state in-memory mirror is synchronized to durable storage ≤ 1s.
- [ ] In-flight tool calls are tracked; incomplete calls are rolled back or restarted on recovery.
- [ ] RPO ≤ 1s verified in local and remote deployments.

**PRD-AGENTS-08 (Performance)**
- [ ] Agent init time ≤ 500ms (local), ≤ 2s (remote).
- [ ] Session resume ≤ 200ms (cold), ≤ 10ms (warm).
- [ ] Request latency overhead ≤ 50ms.

**PRD-AGENTS-09 (Standards Compliance)**
- [ ] No proprietary LLM wrapping; anthropic-sdk or OpenRouter used directly.
- [ ] Tool calling and JSON-RPC responses are spec-compliant.
- [ ] Session storage is pluggable (filesystem, database, MCP).
- [ ] Agent JSON schema is published and versioned.

**PRD-AGENTS-10 (Rust Implementation)**
- [ ] Core framework compiles and runs on Rust ≥ 1.91.
- [ ] No unsafe code except where justified (security review required).
- [ ] Cross-language tool invocation (Rust, Python, JavaScript) is supported.

---

## Success Metrics / KPIs

| Metric | Baseline | Target | How measured |
|--------|----------|--------|--------------|
| Session persistence (data loss incidents) | TBD | 0 per month | Production deployment logs |
| Agent initialization latency (p95) | N/A (new) | ≤ 500ms (local), ≤ 2s (remote) | Benchmark suite |
| Session resume latency (p95, cold) | N/A (new) | ≤ 200ms | Benchmark suite |
| Framework adoption (teams using trusty-agents) | 0 | ≥ 3 internal teams | Internal survey |
| Agent definition reuse (hierarchy depth, concrete agents) | 0 | ≥ 5 concrete agents, depth ≥ 3 | Code inventory |
| Crash recovery success rate | N/A (new) | ≥ 99.5% | Failure injection tests |

---

## Scope & Out-of-Scope

### In Scope

- Durable session management and durability guarantees.
- Local and remote execution modes.
- Agent inheritance hierarchy and capability composition.
- JSON-based agent definitions with skill dependencies.
- Integration with anthropic-sdk and standard LLM APIs.
- MCP and HTTP daemon transport.
- Initial (local) storage backend; database backend is deferred.

### Out of Scope

- UI or frontend for agent management. (Agents expose APIs; frontends are separate projects.)
- Agent marketplace or registry. (Defined as future phase.)
- Fine-tuning or model training. (trusty-agents calls existing models.)
- Non-agent workflow orchestration. (Focus is agents; workflows are separate.)
- Multi-tenant auth and isolation. (Assumed to be handled by deployment layer.)
- Horizontal scaling or load balancing. (Single-instance focus; clustering is future phase.)

---

## Risks & Assumptions

| Risk | Mitigation | Status |
|------|-----------|--------|
| Agent classifier design space is novel; no standard exists | Record decision in ADR; design trade-offs explicitly | Open — validate via research |
| Durable session storage adds latency | Async logging + in-memory cache; benchmark to validate ≤50ms overhead | Validate in prototype |
| Cross-language tool invocation is fragile | Standardize tool calling via JSON-RPC; add retry/fallback | Prototype in parallel |
| Rust adoption by productivity users (non-engineers) | Focus on JSON definitions and HTTP API; hide Rust complexity | Defer to UX phase |

| Assumption | Validation plan |
|-----------|-----------------|
| trusty-mpm session model is transferable to agents | Code review with mpm team; adapt where needed |
| JSON agent definitions are sufficient for most use cases | Gather use cases from 3+ pilot teams |
| Productivity agents (multi-session) are a first-class use case | User research with 5+ productivity users |
| Existing LLM APIs (anthropic-sdk, OpenRouter) are adequate | Prototype tool calling and model routing |

---

## Open Questions

1. **Agent classification standard:** Does an existing standard (OpenAI, Anthropic, LLM framework
   convention) govern agent classification and inheritance? If so, adopt it. If not, design our own
   and document in an ADR.

2. **Storage backend priority:** Should the initial release support file-based (durable)
   storage only, or include database and/or MCP-backed options? Recommend: file-based initially;
   defer database to v1.1.

3. **Skill versioning and resolution:** How should conflicting skill versions be resolved? Should
   the framework enforce version pinning or allow compatible ranges? Recommend: pinning initially.

4. **Multi-project session scoping:** Should sessions be scoped to projects, or should agents
   manage cross-project context? Recommend: session ↔ project 1:1 initially; cross-project context
   is future.

5. **Agent state isolation:** Can multiple agents share session state, or should each agent have
   its own isolated session? Recommend: 1:1 initially; shared state is future.

---

## Linked Specs

When engineering specs are authored, they will implement these requirements. No specs are linked yet
(design phase).

| PRD Requirement | Spec ID | Status | Notes |
|-----------------|---------|--------|-------|
| PRD-AGENTS-02 (Durable Session Model) | TBD | TBD | Spec will define session storage API, recovery protocol |
| PRD-AGENTS-03 (Local OR Remote Execution) | TBD | TBD | Spec will define transport layer, configuration schema |
| PRD-AGENTS-04 (Agent Inheritance Framework) | TBD | TBD | Spec will define trait hierarchy, capability traits |
| PRD-AGENTS-05 (JSON Agent Definition) | TBD | TBD | Spec will define JSON schema, loader, validation |
| PRD-AGENTS-06 (Agent Classifier) | TBD | TBD | Spec deferred pending design research (ADR) |
| PRD-AGENTS-07 (Reliability & Durability) | TBD | TBD | Spec will define SLOs, failure modes, recovery |
| PRD-AGENTS-08 (Performance) | TBD | TBD | Spec will define benchmarks, profiling methodology |
| PRD-AGENTS-09 (Standards Compliance) | TBD | TBD | Spec will cite standard versions, compliance tests |
| PRD-AGENTS-10 (Rust Implementation) | TBD | TBD | Spec will define MSRV, unsafe code policy |

---

## References

### Inspiration & Related Work

- **trusty-mpm session harness:** Claude MPM's durable session model and multi-turn agent orchestration
  (see `crates/trusty-mpm/`). A blueprint for session durability.
- **Existing agent frameworks:** OpenClaw (agent orchestration), Hermes (multi-agent workflows),
  Claude agents (API-native). Each excels in different areas; trusty-agents synthesizes durability +
  flexibility + standards.

### Standards & Conventions

- **anthropic-sdk:** LLM API integration (messages, tool_use, model routing).
- **JSON-RPC 2.0:** Tool calling and inter-agent communication.
- **MCP (Model Context Protocol):** Stdio transport and tool invocation.
- **Rust MSRV 1.91:** trusty-tools baseline.

### Future Research

- Agent classification and inheritance standards: survey OpenAI, Anthropic, and community frameworks.
- Multi-session context management patterns: interview 5+ productivity users.
- Cross-language tool invocation robustness: prototype with Rust ↔ Python ↔ JavaScript.

---

## Context & Related Docs

- **Existing trusty-agents crate:** `crates/trusty-agents/` in the trusty-tools workspace.
  This PRD is the product north-star for that crate.
- **trusty-mpm session model:** `crates/trusty-mpm/` implements durable multi-turn agent loops.
  Investigate reuse and adaptation patterns.
- **claude-mpm agent hierarchy:** BASE_AGENT pattern and agent classification (see claude-code
  documentation). Inspirational reference.

---

**Document history:**
- **v1 (2026-06-18):** Initial draft. Captures product vision, core requirements, and open design
  questions. NOT prioritized; future reference for roadmap planning.
