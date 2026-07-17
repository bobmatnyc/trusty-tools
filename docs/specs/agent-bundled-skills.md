# DOC-42 — Agent-Bundled Skills: Declarative Skill Association and Co-Deployment

**Status:** Draft
**Subsystem:** trusty-mpm — agent composition / skill deployment
**Owner:** Engineering (trusty-mpm)
**Last-updated:** 2026-07-17
**Spec ID:** `SPEC-AGENTSKILLS-01~draft` (DOC-42)
**Linked issues:** [#2889](https://github.com/bobmatnyc/trusty-tools/issues/2889) (agent-bundled skills mechanism); [#2890](https://github.com/bobmatnyc/trusty-tools/issues/2890) (code-critic content restoration)
**Builds on:** DOC-31 — SYSTEM vs PROJECT Agents & Skills (`docs/specs/system-project-agents-skills.md`, the 3-tier skill precedence model and agent/skill deployment pipeline); DOC-29 — Primary trusty-mpm Harness Behaviors (`docs/specs/mpm-behavior-conformance.md`, the agent compose chain).
**Cross-ref:** agent asset frontmatter parsing (`crates/trusty-mpm/src/core/agent_manifest.rs`, `crates/trusty-mpm/src/assets/agents/*.md`); agent composer (`crates/trusty-mpm/src/core/agent_deployer.rs`); skill deployer and tier resolver (`crates/trusty-mpm/src/core/skill_deployer.rs`, `crates/trusty-mpm/src/core/skill_tiers.rs`); session launcher (`crates/trusty-mpm/src/core/session_launch/mod.rs`); doctor/validation (`crates/trusty-mpm/src/bin/tm/commands/doctor.rs`); CLI agent list/show (`crates/trusty-mpm/src/bin/tm/commands/agent.rs`).

> **Scope note.** This is a **behavior contract** for first-class association between **agents** and **skills** — a structured `skills:` frontmatter array on agent assets that declares which skills an agent depends on, plus the co-deployment and validation semantics that follow. It is a **declarative metadata + validation + co-deployment** layer on top of DOC-31's 3-tier skill provisioning; it does **not** introduce new skill tiers, inheritance/composition chains, or per-agent sandboxing. It sits **at deploy time** and **at doctor/diagnostic time**, consuming the existing agent composition chain and skill deployer exactly as-is.

---

## Purpose & Scope

Today, agents and skills are deployed independently. Upstream agents (e.g., the open-source `claude-mpm-agents` repo) declare their skill dependencies in a **`skills:` frontmatter array** — e.g., `code-critic` lists `skills: [code-review-standards, code-production-process, systematic-debugging]` — but when trusty-mpm agents are composed from upstream sources, those declarations are manually dropped or transcribed as prose-only references in agent bodies. This creates three problems:

1. **Dangling references**: an agent's body mentions "load skill X", but X is not guaranteed to be deployed alongside the agent. If a user runs the agent without manually deploying X, they get a "skill not found" error.
2. **Lost content**: when porting agents upstream (e.g., code-critic), the upstream `skills:` array is omitted, so bundled skill content (e.g., code-production-process.md) is silently dropped from the agent's portfolio. Issue #2890 tracks one such loss.
3. **No co-deployment guarantee**: there is no mechanism to ensure an agent's declared dependencies are deployed when the agent is deployed.

This spec introduces **agent-bundled skills** — a declarative layer that:
- Adds an optional `skills:` frontmatter array to agent assets (Markdown frontmatter, like `extends:`).
- Ensures declared skills are **co-deployed** whenever the agent is deployed.
- Validates every declared skill resolves to a deployed skill at doctor/diagnostic time.
- Surfaces declared skills and their resolution tier in CLI output (agent list/show).

**In scope:** the `skills:` frontmatter schema and parsing; co-deployment semantics in the session launcher; validation and diagnostics in `tm doctor`; CLI surfacing in agent list/show; observability (logs of co-deployed skills).

**Out of scope** (consumed, not re-specified): the 3-tier skill precedence model (DOC-31 §SPEC-PROVISION-01); the existing agent composition and skill deployment machinery; skill inheritance or per-agent skill sandboxing; changes to the skill manifest or ownership ledger.

---

## Terminology

| Term | Definition |
|---|---|
| **Agent asset** | A Markdown file in `crates/trusty-mpm/src/assets/agents/*.md` (or PROJECT-tier `repo/.claude/agents/*.md`) declaring an agent's identity, metadata, and instructions. |
| **Frontmatter** | YAML metadata block between `---` delimiters at the start of an agent asset (e.g., `name:`, `role:`, `description:`, `model:`, `extends:`). |
| **`skills:` field** | Optional frontmatter array listing skill names the agent depends on (e.g., `skills: [code-review-standards, verification-before-completion]`). |
| **Skill name** | The **stem** or directory name of a skill (e.g., `code-review-standards` for `~/.claude/skills/code-review-standards/SKILL.md`). |
| **Resolution** | Looking up a skill name through the 3-tier precedence (PROJECT > USER > BUNDLED, per DOC-31 §SPEC-PROVISION-01) to determine which deployed instance, if any, the agent will see. |
| **Co-deployment** | When an agent is deployed (via session launcher, managed routes, or standalone driver), all skills declared in its `skills:` array are also deployed, in the same session/target. |
| **Shadow** | When a higher-precedence tier's skill of the same name suppresses a lower-tier skill (e.g., user-tier `code-review-standards` shadows bundled `code-review-standards`). Agent-bundled skills do **not** introduce new shadowing rules; they consume DOC-31's existing precedence. |

---

## Table of Contents

| ID | Section | Implementing module(s) |
|----|---------|--------------------------|
| SPEC-AGENTSKILLS-01~draft | [Schema and parsing](#schema-and-parsing-spec-agentskills-01draft) | `core::agent_manifest`, `core::*_frontmatter` (or inline parser) |
| SPEC-AGENTSKILLS-02~draft | [Co-deployment semantics](#co-deployment-semantics-spec-agentskills-02draft) | `core::session_launch`, agent deployer flow |
| SPEC-AGENTSKILLS-03~draft | [Validation and diagnostics](#validation-and-diagnostics-spec-agentskills-03draft) | `bin::tm::commands::doctor`, possibly `core::validation` |
| SPEC-AGENTSKILLS-04~draft | [CLI surfacing](#cli-surfacing-spec-agentskills-04draft) | `bin::tm::commands::agent` (list/show) |
| SPEC-AGENTSKILLS-05~draft | [Observability and logging](#observability-and-logging-spec-agentskills-05draft) | session launcher, deployers, doctor |

---

## 1. Motivation & Current State (verified 2026-07-17)

### 1.1 Upstream agents declare skill dependencies; trusty-mpm drops them

The open-source [`claude-mpm-agents`](https://github.com/bobmatnyc/claude-mpm-agents/) repo's agents (e.g., `code-critic.md`, `rust-engineer.md`) include a `skills:` frontmatter array. For example:
```yaml
---
name: code-critic
role: qa
skills:
  - code-review-standards
  - code-production-process
  - software-patterns
  - systematic-debugging
  - verification-before-completion
---
```

When trusty-mpm agents are ported from upstream (composed via inheritance, e.g., `extends: base-qa`), the `skills:` field is **manually omitted or transcribed as prose** ("load skill X") rather than declared as structured frontmatter. This is a conscious omission — agents are designed to work without a formal dependency declaration — but it has consequences.

### 1.2 Problem 1: Dangling references

An agent's body may say "your first action is to load the code-review-standards skill", but if a user runs that agent without explicitly deploying code-review-standards, they get:
```
ERROR: skill 'code-review-standards' not found in ~/.claude/skills/
```

The reference is unenforceable.

### 1.3 Problem 2: Lost upstream content

Issue #2890 documents that the code-critic agent, when ported to trusty-mpm, **lost approximately 130 lines of upstream skill content** that should have been restored as bundled skills. The lost content includes structured guidance on code-production-process and other standards. Because there was no `skills:` declaration to signal which skills the agent depends on, the restoration was missed.

### 1.4 Problem 3: No co-deployment guarantee

If an agent mentions a skill, the user must remember to deploy both. There is no mechanism to ensure "when I deploy code-critic, code-review-standards is also deployed."

### 1.5 Existing infrastructure is ready

The skill deployer already understands the 3-tier precedence (DOC-31). Agent manifests already parse frontmatter metadata. Session launcher already calls the skill deployer. The only missing piece is **declaring which skills an agent depends on** and **ensuring they co-deploy**.

---

## 2. Behavior Contract Sections

### Schema and Parsing {#SPEC-AGENTSKILLS-01~draft}

**ID:** SPEC-AGENTSKILLS-01~draft
**Status:** Draft

#### Behavior Contract (WHAT)

- **Input:** an agent asset file (Markdown with YAML frontmatter) in any tier (SYSTEM bundled, SYSTEM catalog, USER, PROJECT).
- **Output:** the agent's metadata struct, including an optional `skills: Vec<String>` field populated from the `skills:` frontmatter array, or `None` if the field is absent.
- **Frontmatter grammar:**
  ```yaml
  ---
  name: agent-name
  role: role-name
  extends: [base-agent-name]
  model: [model-id]
  description: [description]
  skills: [skill-name-1, skill-name-2, ...]  # ← NEW; optional; default empty
  ---
  ```
  - `skills:` is optional and defaults to an empty list if absent.
  - Each element must be a string (skill name / stem).
  - Skill names follow the same constraints as today: alphanumeric, hyphens, no special chars (already enforced by filesystem paths).
  - Order is preserved (YAML list order) but not semantically significant for validation.
  - An empty array (`skills: []`) is equivalent to omission.

- **Parsing:** frontmatter is parsed using the existing agent manifest parser (or extended inline). If `skills:` is present, extract the list; if absent or malformed, log a WARN and treat as empty.
- **Preconditions:** the agent asset file is readable and syntactically valid YAML (frontmatter).
- **Postconditions:** the agent's loaded metadata includes the `skills` list, populated or empty.
- **Error conditions:** malformed YAML (frontmatter), invalid syntax in the skills list (non-strings, nested structures) — log a WARN and skip (treat as empty), never halt the load.

#### Rationale (WHY)

Declarative metadata is clearer than prose references: a machine can enforce it, it survives tool refactoring, and it mirrors upstream agent practices. Placing it in frontmatter (like `extends:`, `model:`, `role:`) keeps agent metadata in one place and co-locates skill declarations with agent identity. Making it optional preserves backward compatibility — existing agents without `skills:` continue to work.

#### Implementing Modules

| Module | Role |
|--------|------|
| `core::agent_manifest` or `core::*_frontmatter` | Parse frontmatter YAML; extract `skills:` list if present. |

---

### Co-deployment Semantics {#SPEC-AGENTSKILLS-02~draft}

**ID:** SPEC-AGENTSKILLS-02~draft
**Status:** Draft

#### Behavior Contract (WHAT)

- **Inputs:** an agent to be deployed (from any tier: bundled, user, project); its loaded metadata including `skills` list; the session's skill deployment target.
- **Outputs:** the agent is deployed to the target; **all skills in the agent's `skills:` list are also deployed** to the same target, subject to the 3-tier precedence (DOC-31 §SPEC-PROVISION-01).
- **Behavior:**
  1. When the deployer writes an agent to the target (e.g., `~/.claude/agents/code-critic.md` in a session), it reads the agent's `skills` list from the loaded metadata.
  2. For each skill name in the list:
     - Look up the skill through the 3-tier resolver (PROJECT > USER > BUNDLED, per DOC-31).
     - If the skill resolves (exists in at least one tier), **add it to the co-deploy set**.
     - If the skill does **not** resolve (absent from all three tiers), **log a WARN** (see §SPEC-AGENTSKILLS-03 Validation) but **do not halt**.
  3. After deploying all requested agents, run the skill deployer once with the co-deploy set merged into the session's skill selection.
  4. The skill deployer's 3-tier precedence and shadowing rules (DOC-31) apply unchanged — agent-bundled skills do not override or bypass those rules.

- **Preconditions:** the agent is loaded with its `skills:` list; the skill deployer is operational; the 3-tier skill resolver is functional (per DOC-31).
- **Postconditions:** all skills in the agent's `skills:` list that resolve to a deployed skill are written to the target; all shadowing relationships (PROJECT > USER > BUNDLED) are recorded per DOC-31 §SPEC-PROVISION-07.
- **Error conditions:** a declared skill does not exist in any tier — log a WARN with the agent name and missing skill name; proceed anyway (skill is not deployed, agent is deployed). This is a validation issue, not a deployment issue (see §03).
- **Non-behavior:** agent-bundled skills **do not** change the skill tier precedence, introduce per-agent sandboxing, or allow an agent to override a skill's deployment target. They are purely a **co-deploy trigger** and **validation signal**.

#### Rationale (WHY)

If an agent declares a skill dependency, that skill should be present in the session alongside the agent. Co-deployment is the mechanism that makes the declaration enforceable. Logging (not halting) on missing skills keeps sessions resilient — a session with an agent but not its declared skill is better than a session that fails to launch.

#### Implementing Modules

| Module | Role |
|--------|------|
| `core::session_launch` | Merge co-deploy sets from agents into the session's skill selection before calling the skill deployer. |
| `core::agent_deployer` | Pass the agent's `skills` list to the session launcher or co-deploy orchestrator. |
| `core::skill_deployer` | Consume the merged co-deploy set; apply 3-tier precedence unchanged. |

---

### Validation and Diagnostics {#SPEC-AGENTSKILLS-03~draft}

**ID:** SPEC-AGENTSKILLS-03~draft
**Status:** Draft

#### Behavior Contract (WHAT)

- **Inputs:** the bundled + user-custom + project-custom agent and skill inventories; their declared dependencies (from `skills:` frontmatter).
- **Outputs:** a set of validation findings, reported by `tm doctor` and related diagnostic commands.
- **Validations:**

  1. **Declared skill existence (mandatory):** For each agent `A` with a `skills:` entry listing skill `S`:
     - Resolve `S` through the 3-tier resolver (PROJECT > USER > BUNDLED).
     - If `S` resolves (exists in at least one tier), emit an INFO-level diagnostic (silent by default).
     - If `S` does **not** resolve, emit a WARN: *"Agent `A` declares skill `S`, but `S` not found in any tier (project/user/bundled)."*

  2. **Prose-only references (informational heuristic):** Scan agent bodies for prose patterns like "load skill X" or "load the X skill" (case-insensitive) where X matches a known skill name. If a skill is mentioned in prose but **not** declared in the `skills:` frontmatter array, emit an INFO-level note: *"Agent `A` body mentions skill `X`, but `X` is not in the `skills:` frontmatter array. Consider adding it to ensure co-deployment."*
     - This is a best-effort heuristic, not a hard rule. Phrase as informational.
     - Regex: `(?i)(load\s+(?:the\s+)?)([\w\-]+)\s+skill` or similar; match against known skill stems.

  3. **Upstream vs local divergence (informational):** If an agent is known to inherit from an upstream agent (via `extends:` or manifest metadata), and the upstream declaration includes a `skills:` array that is **not** present in the local asset, emit an INFO note: *"Agent `A` inherits from upstream agent `B`. The upstream version declares skills `[X, Y, Z]`, but this version does not. Consider syncing the `skills:` field."*
     - This requires curating a mapping of "upstream agents and their declared skills"; it may be a first-phase enhancement, not required for MVP.
     - Mark as "future" or optional.

- **Preconditions:** all agent and skill assets are loaded; metadata including `skills:` frontmatter is parsed.
- **Postconditions:** a report is emitted (inline to `tm doctor` output or to a separate diagnostics stream); no agent or skill deployment is blocked.
- **Error conditions:** asset loading fails, parser errors — already handled by existing validation; no new error conditions introduced.

#### Rationale (WHY)

Validation ensures declared dependencies are real and helps operators debug missing skills. Prose-scanning is a soft check to catch manually-written agents that reference skills but forget to declare them. Making these warnings visible (but non-fatal) keeps sessions running while surfacing guidance.

#### Implementing Modules

| Module | Role |
|--------|------|
| `bin::tm::commands::doctor` | Enumerate agents and skills; run validation checks; emit report. |
| `core::validation` (new, or inline in doctor) | Implement the three validation checks above. |

---

### CLI Surfacing {#SPEC-AGENTSKILLS-04~draft}

**ID:** SPEC-AGENTSKILLS-04~draft
**Status:** Draft

#### Behavior Contract (WHAT)

- **Input:** a request to list or show an agent (e.g., `tm agent list`, `tm agent show code-critic`).
- **Output:** the agent's metadata is displayed, including the `skills:` field and **resolution metadata** for each declared skill (which tier it will resolve to).
- **List output (augmented):** when `tm agent list` displays agents, add an optional `--verbose` or `--with-skills` flag that shows:
  ```
  Name              Role     Skills
  ----              ----     ------
  code-critic       qa       code-review-standards (bundled), code-production-process (user), systematic-debugging (project)
  rust-engineer     engineer toolchains-rust-core (bundled)
  ```
  - Or simpler: just list skill names in a `Skills` column, defaulting to the agent list format. Tier labels are optional in list view.

- **Show output (detailed):** when `tm agent show code-critic` displays a single agent, include:
  ```markdown
  Name: code-critic
  Role: qa
  Model: sonnet
  Extends: base-qa
  Description: ...
  
  Declared Skills:
  - code-review-standards (bundled, deployed)
  - code-production-process (user, deployed)
  - systematic-debugging (project, deployed)
  - missing-skill (NOT FOUND, will warn at deploy time)
  
  [agent body...]
  ```
  - For each declared skill:
    - Show the skill name.
    - Show the resolved tier (or "NOT FOUND").
    - Show whether the skill will actually be deployed (e.g., "deployed" vs "shadowed by X").

- **Preconditions:** the agent assets are loaded and frontmatter is parsed; the 3-tier skill resolver is operational.
- **Postconditions:** the displayed information matches the resolved state at deploy time.
- **Error conditions:** skills or agents missing or unresolvable — show "NOT FOUND" or similar; do not halt.

#### Rationale (WHY)

Surfacing declared skills in CLI output makes it obvious (a) which skills an agent expects, (b) whether they are available, and (c) which tier they come from. This aids debugging when a user runs an agent and gets a missing-skill error — they can check `tm agent show` to see whether the skill should be deployed.

#### Implementing Modules

| Module | Role |
|--------|------|
| `bin::tm::commands::agent` | Enhance list and show subcommands to display `skills:` metadata and resolution. |
| `core::skill_tiers` or resolver | Resolve each declared skill name to determine tier. |

---

### Observability and Logging {#SPEC-AGENTSKILLS-05~draft}

**ID:** SPEC-AGENTSKILLS-05~draft
**Status:** Draft

#### Behavior Contract (WHAT)

- **Inputs:** agent deployment and skill co-deployment (from §02).
- **Outputs:** log records capturing:
  1. **Agent deploy + skill co-deploy summary:** At deployment time, emit a log line:
     ```
     info: deploying agent code-critic with declared skills: code-review-standards, code-production-process, systematic-debugging
     ```
     - One line per agent that has non-empty `skills:` list.
     - Lists skill names, not resolution tier (brevity).

  2. **Unresolved skill warnings:** For each declared skill that does not resolve:
     ```
     warn: agent code-critic declares skill missing-skill, but it is not found in any tier (project/user/bundled)
     ```
     - One line per missing skill.

  3. **Co-deployed skill details (optional, verbose mode):** At debug/trace level, list which tier each co-deployed skill resolved to:
     ```
     debug: co-deployed skill code-review-standards (bundled) for agent code-critic
     debug: co-deployed skill code-production-process (user, shadows bundled) for agent code-critic
     ```

- **Preconditions:** session launcher is operational; skill deployer is emitting logs.
- **Postconditions:** logs are emitted to stderr (per trusty-mpm conventions) and available via standard log filtering (e.g., `RUST_LOG=info`).
- **Error conditions:** none; logging is purely informational.

#### Rationale (WHY)

Observable logs aid debugging when sessions fail or skills are missing. Recording co-deployed skills makes it clear what was deployed alongside each agent and why, especially when shadowing occurs.

#### Implementing Modules

| Module | Role |
|--------|------|
| Session launcher, agent/skill deployers | Emit log records as described. |

---

## 3. Compatibility & Migration

**Backward compatibility:** The `skills:` field is optional. Existing agents **without** a `skills:` declaration continue to work exactly as before — their behavior is unchanged. Users who want to migrate upstream agents (e.g., code-critic) to include their full `skills:` array can do so incrementally; old agents without the field are never broken.

**Upstream synchronization:** Over time, trusty-mpm agents can be enriched with `skills:` declarations by porting them from the upstream `claude-mpm-agents` repo or by collecting community feedback on which skills each agent should declare.

**Future extension:** This spec does not preclude adding more frontmatter fields (e.g., `requires-features:`, `min-version:`) in the future; the `skills:` field is the first of a pattern.

---

## 4. Non-Goals (Explicitly Out of Scope)

- **Skill inheritance/composition:** skills themselves remain flat; no skill can declare dependencies on other skills.
- **Per-agent skill sandboxing:** all deployed skills are global to the session; no agent-specific skill visibility rules.
- **Skill ordering or priority:** the 3-tier precedence (DOC-31) is the only ordering rule; agent-bundled skills do not introduce new tie-breaking rules.
- **Lazy loading:** skills are deployed eagerly (at session launch time), not on-demand when an agent loads them.
- **Agent-skill co-versioning:** no enforcement that agent version X matches skill version Y.

---

## 5. Conformance Matrix

| Requirement | Implementing module | Status |
|---|---|---|
| Parse `skills:` from agent frontmatter | `core::agent_manifest` or frontmatter parser | Design |
| Co-deploy declared skills at session launch | `core::session_launch` orchestrator | Design |
| Validate declared skills resolve (doctor check) | `bin::tm::commands::doctor` | Design |
| Display skills in agent list/show | `bin::tm::commands::agent` | Design |
| Log co-deployed skills | Session launcher, deployers | Design |
| Preserve backward compatibility (no `skills:` → no change) | All modules | Design |

