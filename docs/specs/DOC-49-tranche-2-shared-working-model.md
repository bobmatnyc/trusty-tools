---
spec_refs:
  - id: SPEC-SLD-01~draft
    path: docs/specs/spec-linked-documentation.md
    anchor: SPEC-SLD-01~draft
---

# DOC-49 — Tranche 2: Shared PM Working-Model Layer & Harness-Identity Correction

**Status:** Draft — PROPOSAL, not approved. No Tranche-2 code moves until Bob signs off (see §0).
**Subsystem:** trusty-agents-common (shared working-model home); trusty-code (harness identity, provider layer); trusty-mpm (circuit/trust migration source); trusty-agents (CTO-assistant migration target)
**Owner:** Bob Matsuoka (all §11 open questions are his call)
**Last-updated:** 2026-07-19
**Spec ID:** `SPEC-SWM-01~draft` … `SPEC-SWM-10~draft` (DOC-49)
**Epic:** none yet — §10 milestones each get their own issue only after this spec is approved
**Builds on:** [DOC-21 — Harness Understanding](./harness-understanding.md) (the `harness_doc` precedent this spec extends); [DOC-38 — SLD](./spec-linked-documentation.md); [ADR-0004 / docs/architecture/harnesses.md](../architecture/harnesses.md) (three-harness peer boundary, event-driven mandate); [docs/trusty-code/vision-and-architecture-spec.md](../trusty-code/vision-and-architecture-spec.md); [docs/trusty-code/parity-spec.md](../trusty-code/parity-spec.md)
**Cross-ref:** epic #587 (tcode extraction); #2400/#2409/#2410 (`trusty_common::inference` adapter migration); #3052/#3105/#3135/#3144 (CTO-assistant → trusty-agents migration, in flight); DOC-44 (unmerged, engineering-lead twin orchestration, `spec-twin-lead-architecture` branch)

> **Scope note.** This is a **target-architecture + migration-plan spec**, not an implementation. It records decisions already made by the owner (harness identity, CTO-assistant destination) and proposes a shared working-model architecture for review. The PR carrying this document changes **only**: this file, its catalog row, and wording corrections to `docs/trusty-code/vision-and-architecture-spec.md`, `crates/trusty-code/README.md`, and `docs/architecture/harnesses.md`/ADR-0004 (harness-identity language only — no Rust changes anywhere).

---

## 0. Approval Gate

**Every milestone in §10 requires Bob's explicit approval before any code lands.** This spec is the artifact that approval is granted against. Once approved:
1. Each §10 milestone is filed as its own GitHub issue (this spec is `Builds on:` for each).
2. Migrations proceed in the order §10 specifies — no milestone starts before its dependencies merge.
3. §11's open questions must be resolved (persona-schema pick, at minimum) before the milestone that depends on them starts.

Nothing in this PR moves code. The README/vision-spec/ADR-0004 wording edits in §2.2 are the one exception — they are corrective documentation, approved as part of accepting this spec, not a "Tranche 2 code move."

---

## 1. Motivation and Problem Statement {#SPEC-SWM-01~draft}

**ID:** SPEC-SWM-01~draft · **Status:** Draft

ADR-0004 correctly keeps trusty-code, trusty-mpm, and trusty-agents as Cargo peers — no harness takes a library dependency on a sibling harness, which prevents cargo cycles and keeps each harness independently deployable ([docs/architecture/harnesses.md](../architecture/harnesses.md) §Inter-Harness Delegation, "No harness takes a Cargo dependency on a sibling harness"). That boundary is not in question here.

What the peer-boundary decision did **not** settle is: where does the actual **PM working model** — instruction assembly, delegation routing, workflow phases, circuit-breaking, permission gating — live? Today the answer is "twice, independently, and diverging":

- **trusty-mpm** ships a mature, production-hardened working model as Markdown assets consumed by its instruction pipeline: `BASE_PM.md` (non-overridable floor, appended last), `AGENT_DELEGATION.md` (routing table), `WORKFLOW.md` (phase definitions), plus in-process state machines `core::circuit` (delegation circuit breaker) and `core::project_trust` (MCP trust gating).
- **trusty-code** independently re-encodes overlapping concepts in Rust: `intent::IntentClass` (a classifier, not a routing table), a hardcoded Research→Plan→Code→QA pipeline in `agent_loop`/`intent::mod.rs` (vision-spec §5.10 "Execution Patterns"), and no circuit breaker or trust-gating equivalent at all.

This is architectural debt with a conformance risk attached: the two harnesses' PM behavior can silently diverge (a delegation-routing fix landed in `AGENT_DELEGATION.md` never reaches tcode; a circuit-breaker fix in `trusty-mpm::core::circuit` never protects tcode's own delegation loop). It also means every future PM-behavior improvement must be built and tested twice.

Compounding this, tcode's own documentation overstates its coupling to Anthropic and to the `claude-code` binary specifically (§2), when the actual and intended constraint is narrower: tcode reads the **`.claude/`-shaped configuration format** — agents, skills, MCP descriptors, `CLAUDE.md`, permissions — and is free to run under any model provider tcode's `Provider` trait supports (`crates/trusty-code/src/provider/`, OpenRouter today, Bedrock stubbed).

Finally, the CTO-assistant application cluster (`cto-assistant`, `tc-services`, `trusty-cto-db`) predates trusty-agents' persona system and is already migrating into it under epic #3105 as "a validating production migration for the framework" (issue #3105 body, 2026-07-18). This spec canonizes that target shape and adds the one piece not yet covered by #3105's child issues: `trusty-gworkspace` becoming a managed daemon MCP service instead of a per-client stdio subprocess.

**Owner decisions this spec canonizes (verbatim intent):**
1. tcode shares mpm's PM + instructions + subagents working model.
2. tcode is NOT bound to Anthropic's models or the claude-code harness.
3. The goal of tcode is to build the harness around the mpm working model and workflow.
4. The CTO-assistant cluster migrates into the trusty-agents surface as Bob's personal-assistant agent; `trusty-gworkspace` becomes a running MCP service.

---

## 2. Part 1 — Harness Identity (tcode) {#SPEC-SWM-02~draft}

**ID:** SPEC-SWM-02~draft · **Status:** Draft

### 2.1 What "Claude-Code-compatible" currently means vs. what is true

Grep across the tree turns up two different claims wearing the same words:

| Claim | Where | Actually true? |
|---|---|---|
| "reads `.claude/` config, agents, skills, MCP descriptors, `CLAUDE.md`, permission grants" | `crates/trusty-code/README.md:121-122` | **Yes** — this is a file-format reader, no Anthropic dependency. |
| "Claude-Code-native orchestration entry point" | `crates/trusty-code/README.md:10` | **No** — overstates it; tcode has its own `Provider` trait, `agent_loop`, and PM loop. Nothing about it is "native" to Claude Code. |
| "per-project, Claude-Code-compatible MPM orchestration" | `crates/trusty-code/README.md:3`, `docs/adr/0004-…:15` | **Ambiguous** — reads as "compatible with the claude-code binary/product," when the only real compatibility is the on-disk config format. |
| "A Claude-Code-compatible configuration reader" | `docs/trusty-code/vision-and-architecture-spec.md:25` | **Accurate as written** — this one already says the narrow, correct thing. |

The pattern: wherever the doc says "reads/parses `.claude/` config" it is accurate; wherever it says "Claude-Code-native/-compatible harness" as an identity claim, it overstates coupling that does not exist in the code (`Provider` trait, OpenRouter/Bedrock backends, tcode's own PM loop).

### 2.2 Corrective wording (applied in this PR)

| File | Old | New |
|---|---|---|
| `crates/trusty-code/README.md:3` | "The **Coding Harness** — per-project, Claude-Code-compatible MPM orchestration." | "The **Coding Harness** — per-project MPM orchestration that reads the same `.claude/`-shaped config format as Claude Code." |
| `crates/trusty-code/README.md:10` | "It is the Claude-Code-native orchestration entry point" | "It is an original orchestration entry point, provider-agnostic, that reads the same `.claude/`-shaped config format" |
| `crates/trusty-code/README.md:101` | `tcode` \| Per-project Claude-Code-compatible MPM orchestration harness | `tcode` \| Per-project MPM orchestration harness reading `.claude/`-shaped config |
| `crates/trusty-code/README.md:121` | "**Claude-Code compatible** — reads `.claude/` config…" | "**Reads `.claude/`-shaped config format** — agents, skills, MCP descriptors…" (drop "compatible," keep the accurate description) |
| `docs/adr/0004-…md:15` | "the Claude-Code-compatible harness" | "the harness that reads the same `.claude/`-shaped config format as Claude Code" |
| `docs/architecture/harnesses.md:57` | "Provides the per-project Claude-Code-compatible MPM orchestration entry point." | "Provides the per-project MPM orchestration entry point, reading the same `.claude/`-shaped configuration format as Claude Code." |

**Rule of thumb going forward:** "Claude-Code-compatible" / "-native" describes an *identity/coupling* claim and must not be used for tcode. "Reads/parses the same `.claude/`-shaped config format" describes the *actual, narrow* claim and is always correct. Vision-spec §4.5's "generic Claude-Code-compatible MCP client" (line 350) and §5.3's "(Claude-Code-compatible path)" (lines 500, 1155) are **path/format** references (`.claude/skills/<name>/SKILL.md`), not identity claims — left as-is, since they already mean "same directory shape," not "same product."

### 2.3 What does NOT change

- tcode's `Provider` trait (`crates/trusty-code/src/provider/`) and per-agent model routing are untouched — this spec does not add or remove a provider.
- `docs/trusty-code/parity-spec.md`'s Claude-baseline comparisons remain valid **as a benchmark reference point** (§5.9's Parity mode exists specifically to run byte-identical-schema comparisons against Claude Code and other harnesses) — that is a measurement methodology, not an architectural dependency, and is out of scope for this correction.
- No Cargo dependency changes. tcode has never depended on an Anthropic SDK; this is a documentation-only fix.

---

## 3. Part 2 — Shared Working-Model Layer: Home and Consumption {#SPEC-SWM-03~draft}

**ID:** SPEC-SWM-03~draft · **Status:** Draft

### 3.1 Home: `trusty-agents-common`, extending the DOC-21 pattern

`trusty-agents-common` already hosts two precedents for "shared content both mpm and tcode consume without either harness depending on the other":

1. **`harness_doc` (DOC-21)** — `crates/trusty-agents-common/src/harness_doc.rs` compiles four Markdown assets (`assets/harness_understanding/HARNESS_{AGNOSTIC,MPM_SM,TCODE,OVERSEER}.md`) into `&'static str` accessors via `include_str!`, consumed by trusty-mpm's SM prompt today and reserved for a future tcode overseer.
2. **The agent-compose chain** — `crates/trusty-agents-common/src/agents/{builder,builder_in_memory,frontmatter,metadata}.rs` already resolves mpm's `extends:` frontmatter inheritance chain (`compose_agent_in_memory`) — and tcode's own `agents::md_loader::load_md_agent_with_extends` already calls into this **same** trusty-agents-common code to resolve mpm's bundled `.md` agent catalog before projecting the result into its `AgentConfig` struct (`crates/trusty-code/src/agents/md_loader.rs:105-136`). Agent-catalog sharing is **already partially done** — see §5.

The shared **PM working model** is a third instance of this pattern: a new `crates/trusty-agents-common/src/working_model.rs` (mirroring `harness_doc.rs`'s shape) exposing a harness-neutral `PM_WORKING_MODEL.md` asset plus structured accessors for its sub-sections (identity, delegation matrix, workflow phases — §4). No new crate; no new dependency edge — both mpm and tcode already depend on `trusty-agents-common` (mpm via the compose chain reference above's counterpart on the mpm side; tcode via `agents::md_loader`).

### 3.2 Consumption: two thin harness-binding layers

`PM_WORKING_MODEL.md` is **harness-neutral prose + shared data** (§4). Each harness appends its own thin binding layer **last**, mirroring `BASE_PM.md`'s existing "always appended to PM prompt, cannot be overridden" precedent (`crates/trusty-mpm/src/assets/instructions/BASE_PM.md:1-3`):

| Harness | Binding layer content | Where it plugs in |
|---|---|---|
| **trusty-mpm** | Claude-Code/tmux mechanics: `--dangerously-skip-permissions`/`--setting-sources` flag injection (`core::model_inject`), tmux pane lifecycle, hook relay wiring | Appended after `PM_WORKING_MODEL.md` in the existing instruction-pipeline assembly, in the same slot `BASE_PM.md` occupies today |
| **trusty-code** | Provider dispatch (which `Provider` impl backs the model), and the parity-floor selection (`HarnessMode::{DailyDriver, Parity}`, vision-spec §5.9) | Appended after `PM_WORKING_MODEL.md` in tcode's `prompt::assemble_system_prompt` |

This is additive to mpm's existing assembly (mpm keeps `BASE_PM.md` as its binding layer; `PM_WORKING_MODEL.md` slots in *before* it, replacing the currently-mpm-only identity/delegation/workflow prose that moves out to the shared asset) and net-new for tcode (which has no equivalent layer today — `prompt::assemble_system_prompt` currently only assembles BASE preamble + agent prompt + project context per the parity-spec, vision-spec §4.7 step 2).

### 3.3 What stays harness-specific (non-goal of this layer)

- mpm's `.trusty-mpm/` project-override file mechanism (`INSTRUCTIONS.md`, `AGENT_DELEGATION.md`, `WORKFLOW.md`, `MEMORY.md`, `PM_INSTRUCTIONS_DEPLOYED.md` — `BASE_PM.md:22-38`) is **mpm's own override surface**; this spec does not require tcode to grow an equivalent per-project override file scheme in Tranche 2 (a future tranche may, if the owner wants project-level tcode customization — out of scope here).
- tcode's `HarnessMode` (parity vs. daily-driver, vision-spec §5.9) and Execution Patterns (QUICK OPS / VIBE / FULL LOOP, §5.10) stay tcode-owned decisions about *how much ceremony* a task gets — the shared layer supplies the *routing table* (§4) that decides *who* handles a task, not *how much process* it gets.

---

## 4. Delegation Matrix and Workflow Phases as Shared Data {#SPEC-SWM-04~draft}

**ID:** SPEC-SWM-04~draft · **Status:** Draft

### 4.1 Current state — same concept, two disconnected encodings

| Concern | trusty-mpm today | trusty-code today |
|---|---|---|
| Who handles a task | `AGENT_DELEGATION.md` — a Markdown routing table (agent ↔ trigger keywords ↔ capabilities), consumed as prose injected into the PM prompt | `intent::IntentClass` (`crates/trusty-code/src/intent/mod.rs`) — a heuristic classifier producing `Conversational \| Research \| Implementation`, consumed programmatically |
| What phases a task goes through | `WORKFLOW.md` — a 5-phase Markdown description, consumed as prose | Hardcoded Research→Plan→Code→QA pipeline for every `Implementation`-class task (vision-spec §5.10, `agent_loop`) — no VIBE tier exists yet (tracked separately, issue #2596) |

Both encode "route + phase" decisions, but mpm's is data (an operator can edit `.trusty-mpm/AGENT_DELEGATION.md` and the PM re-reads it) while tcode's is compiled logic (changing routing requires a Rust change and a rebuild).

### 4.2 Target: one structured schema, per-harness rendering

A structured (not prose-only) `DelegationMatrix` and `WorkflowPhases` data type lives in `trusty-agents-common::working_model` alongside `PM_WORKING_MODEL.md` (§3.1). Each harness renders/consumes it differently — this is a rendering split, not a duplication:

- **trusty-mpm** renders the shared data to Markdown for prompt injection, preserving today's behavior and its `.trusty-mpm/AGENT_DELEGATION.md` / `WORKFLOW.md` override files (§3.3) — an override replaces the *rendered* section, same as today.
- **trusty-code** consumes the shared data **programmatically** to drive `intent::mod.rs`'s dispatch and, once built, the VIBE/FULL-LOOP split (issue #2596) — replacing today's hand-rolled `IntentClass` heuristic and hardcoded pipeline with lookups against the shared matrix.

### 4.3 Migration is phased, not rip-and-replace

1. **Phase A** — extract the shared `DelegationMatrix`/`WorkflowPhases` schema into `trusty-agents-common`, seeded from mpm's current `AGENT_DELEGATION.md`/`WORKFLOW.md` content (no behavior change for mpm: its renderer must reproduce today's prose byte-for-byte or the SM prompt regresses).
2. **Phase B** — mpm's instruction pipeline switches to rendering from the shared data instead of reading the raw `.md` assets directly; `.trusty-mpm/` overrides continue to work by replacing the render input.
3. **Phase C** — tcode's `intent::mod.rs` consumes the shared matrix for `IntentClass` dispatch, retiring the bespoke heuristic in favor of matrix lookups plus tcode-specific fast-path rules (QUICK OPS) that stay local to tcode.

Each phase is independently shippable and independently reviewable (§10).

---

## 5. Agent-Persona Schema {#SPEC-SWM-05~draft}

**ID:** SPEC-SWM-05~draft · **Status:** Draft

### 5.1 Correction to the original framing: file format is already unified

The on-disk **format** question is already settled, more recently than this spec's briefing assumed: as of #2897 Slice D, tcode's *only* on-disk agent source format is Markdown+frontmatter (`.claude/agents/<name>.md`) — the original TOML loader (`AgentConfig::from_toml_str`) was retired (`crates/trusty-code/src/agents/config.rs:1-10`). tcode's `agents::md_loader` resolves mpm's `extends:` inheritance chain via the **same** `trusty-agents-common::agents::builder` code mpm's own compose chain uses (§3.1 point 2), and already loads mpm's bundled catalog directly — "the bundled tm agent catalog (5 `BASE-*` templates + 28 coding-relevant roster agents) DOES use `extends:` chains" (`md_loader.rs:110-112`).

### 5.2 What is actually still divergent: the projected schema

The gap is the **typed runtime schema** each harness projects composed frontmatter into:

| | mpm bundled-agent frontmatter (e.g. `crates/trusty-mpm/src/assets/agents/golang-engineer.md`) | tcode `AgentConfig` (`crates/trusty-code/src/agents/config.rs`) |
|---|---|---|
| Shape | Flat: `name`, `role`, `description`, `model`, `extends`, `skills: [...]` | Nested: `agent{name,role,model,description}`, `llm{temperature,max_tokens,model_override}`, `system_prompt{content,append_skills}`, `tools{allowed}` (optional), `runner{kind}` (optional) |
| Purpose | Drives prose injected into Claude Code's own Task-tool subagent dispatch — Claude Code's native tool-permission model applies; no typed LLM params needed | Drives tcode's **own** in-process `AgentLoop`/LLM client — needs explicit `temperature`/`max_tokens`/`runner` because there is no host harness supplying defaults |
| Inheritance | `extends:` chain (already shared, §5.1) | `extends:` chain (already shared, §5.1) |

So the schemas are not competing encodings of the same information — mpm's is a **composition/skill-bundling** schema, tcode's is a **composition + runtime-execution-parameters** schema. They agree on the composition half already.

### 5.3 Recommendation (open — see §11)

Treat tcode's `llm:`/`tools:`/`runner:` sections as **optional extension fields** layered onto mpm's existing flat frontmatter schema, rather than as a competing schema:
- mpm's `name`/`role`/`description`/`model`/`extends`/`skills` stay canonical and unchanged — this is a non-breaking addition for mpm's ~100+ bundled agents.
- A single `.md` agent file gains **optional** `llm:`, `tools:`, `runner:` frontmatter keys, meaningful only to a tcode-driven execution, silently ignored by mpm's renderer (which doesn't read them today and has no reason to start).
- `agent_metadata_from_str` (shared, `trusty-agents-common::agents::metadata`) is extended to parse the optional keys; tcode's `md_loader::project_to_agent_config` reads them, mpm's compose chain leaves them untouched in the rendered prose.

This gives ONE schema (mpm's, extended) with a generation path FOR the other direction not required — tcode already consumes mpm's schema natively; the only new work is parsing three optional keys tcode alone cares about. **This recommendation and the alternative (keep two schemas, add a generator) are both live until the owner picks — §11 Q1.**

---

## 6. Circuit Breaker, Project Trust, and a Harness-Neutral Permission Model {#SPEC-SWM-06~draft}

**ID:** SPEC-SWM-06~draft · **Status:** Draft

### 6.1 Migrate `circuit.rs` and `project_trust.rs` to `trusty-agents-common`

Both are strong migration candidates because both are **pure, already-isolation-tested state machines** with no daemon/tmux/filesystem coupling in their core logic:

- **`crates/trusty-mpm/src/core/circuit.rs`** (205 lines) — `CircuitState`/`CircuitConfig`/`CircuitBreaker`, a plain value type (`Debug, Clone, Serialize, Deserialize`) with `allows_delegation`/`exceeds_depth`/`record_success`/`record_failure`/`attempt_reset` methods and a full `#[cfg(test)]` suite (`breaker_trips_and_recovers`, `depth_limit_is_enforced`) exercising every transition with **no** async, no I/O, no daemon dependency.
- **`crates/trusty-mpm/src/core/project_trust.rs`** (436 lines) — `ProjectTrustStore`, a `BTreeSet<PathBuf>`-backed load/mutate/save store with a fail-closed `is_project_trusted` entry point; its only I/O is JSON read/write against a caller-supplied root path (already parameterized, not hardcoded to mpm's config dir).

**Target:** both move to `crates/trusty-agents-common/src/{circuit,project_trust}.rs` (or a shared `security`/`gating` module); `trusty-mpm` becomes a consumer (re-exports or thin wrappers where call sites need mpm-specific types, e.g. `AgentSummary`). tcode gains delegation circuit-breaking and MCP-trust gating for free — today it has **neither** (§1).

### 6.2 A harness-neutral permission/trust-mode abstraction

`trusty-mpm::core::model_inject::PERMISSION_MODE_FLAG` hardcodes `--dangerously-skip-permissions` as a raw CLI flag string, always appended (`model_inject.rs:72-81,109-113`) — this is Claude-Code-CLI-specific by construction and has no equivalent in tcode, which has its own in-process tool-permission gating (`rbac`, per-agent `tools.allowed`, vision-spec §4.5) but no *concept* shared with mpm's flag-injection approach.

**Target:** a harness-neutral `PermissionMode` (or `TrustMode`) enum in `trusty-agents-common`, expressing the same semantic — "this session runs unattended, under full delegated authority, no human approval gate per tool call" — without committing to *how* each harness enforces it:

```rust
pub enum PermissionMode {
    /// Full delegated authority — every tool call proceeds without a
    /// per-call human approval gate. mpm renders this as
    /// `--dangerously-skip-permissions`; tcode enforces it via its own
    /// RBAC/ToolsConfig gate (no flag to inject — nothing external to invoke).
    Unattended,
    /// Interactive — a human is present and tool calls may be gated.
    Attended,
}
```

mpm's `model_inject::build_claude_command` becomes one *renderer* of `PermissionMode::Unattended` (into the `claude` CLI flag); tcode's `rbac`/`tools.allowed` gate becomes another renderer of the same mode (already-permissive tool gating, no flag needed). Neither harness's current behavior changes — this is a naming/abstraction unification so the *intent* ("this session is unattended") is expressed once and each harness keeps its own enforcement mechanism.

---

## 7. Harness-Conformance Test Tier {#SPEC-SWM-07~draft}

**ID:** SPEC-SWM-07~draft · **Status:** Draft

`trusty-agents-common::connectors::test_kit::ConnectorTestKit` is the direct precedent (DOC-44 Phase 1 deliverable, issue #3007): a normal (non-`#[cfg(test)]`) module of `assert!`/`assert_eq!` helpers taking `&dyn WorkstreamConnector`, called from **both** `crates/trusty-mpm/src/connectors/tm_tests.rs` and `crates/trusty-code/tests/connector_e2e.rs` — proving the same trait contract against two structurally different backends without the two suites drifting apart (`test_kit.rs:1-30`).

**Target:** a `WorkingModelConformanceKit` in the same shape, asserting behavioral parity for the shared surfaces this spec introduces:
- A given `DelegationMatrix` entry routes to the same agent-selection decision in both mpm's renderer and tcode's `intent` consumer (§4.2).
- `CircuitBreaker` transition behavior is identical whether driven from `trusty-mpm`'s or `trusty-code`'s call site (§6.1) — trivial once both consume the same moved type, but worth asserting explicitly since it is the regression a silent divergence would reintroduce.
- `PermissionMode::Unattended` renders to *some* enforcement in both harnesses (not a specific mechanism — the kit asserts "a call gated behind Unattended is not blocked," not "the flag string matches").

Not a new test *framework* — a shared assertion module, called from each harness's own test binary, exactly like `ConnectorTestKit`.

---

## 8. Provider/LLM Layer Folds into the `trusty_common::inference` Migration {#SPEC-SWM-08~draft}

**ID:** SPEC-SWM-08~draft · **Status:** Draft

Issue #2400 already defines a `trusty_common::inference` `InferenceAdapter` trait (merged from tcode's `LlmClientTrait`+`Provider` plus trusty-review's `supports_structured_output`) intended to replace **six** bespoke LLM clients workspace-wide, including tcode's own. #2409 migrates trusty-review onto it; #2410 migrates trusty-agents' three internal routing layers onto it. **tcode's `Provider` trait and `provider_for()` (vision-spec §4.6) is explicitly one of the six clients #2400 targets** — this spec does not duplicate that work, it declares tcode's provider layer an in-scope *consumer* of the #2400 migration once #2409/#2410 land, using the same `InferenceAdapter` trait mpm's SM and trusty-agents' routing layers converge on. No new design here: this section exists so §10's sequencing places tcode's provider-layer migration in its correct dependency position (after #2409/#2410 prove the adapter against two other consumers first).

---

## 9. Part 3 — CTO-Assistant Migration {#SPEC-SWM-09~draft}

**ID:** SPEC-SWM-09~draft · **Status:** Draft

### 9.1 This is already in flight — this section canonizes the destination, not the mechanics

Epic #3105 ("[Goal] Convert the Duetto 'CTO Assistant' bot into a trusty-agent," 2026-07-18) is **already executing** this migration with child issues covering persona re-expression (#3135 `CTO-A-1`: retire the standalone `cto-assistant.toml`, port `prompts.py` into an `extends: assistant` overlay, keep the sensitive `query_headcount`/`query_budget`/`query_risks`/`query_work_classification` tools, one unified memory palace with tagged scopes), inbound Slack routing (#3139 `CTO-C-1`), and cutover (#3144 `CTO-D-3`: shadow-run against the legacy `launchd`+PM2 bot before retiring it). This spec does not re-plan that work — it records the target shape for the record this Tranche-2 doc lives in, and adds one piece not yet covered by #3105's filed children.

**Target shape:** the `cto-assistant`/`tc-services`/`trusty-cto-db` cluster's persona and query tools live on `trusty-agents` as a first-class agent (an `extends: assistant` overlay, per #3135) — not a standalone CLI/daemon cluster. `tc-services`' database/API adapters (`trusty-cto-db`, Granola, Google Workspace wrappers) remain as library dependencies of that agent's tool implementations; they do not need to become agents themselves.

### 9.2 The `trusty-agents-local` → `cto-assistant` edge (Tranche 0, already severed by this spec's writing)

`crates/trusty-agents-local/src/main.rs` directly imports and installs `cto_assistant::agent_plugin()` (`main.rs:16-20`) — a generic local-execution shim coupled at compile time to one specific business persona. This coupling predates #3105 and is the "Tranche 0" edge-severing referenced in this spec's brief: `trusty-agents-local` must stay domain-agnostic (its whole purpose, per its own README, is "executes tasks locally… Implements the agent-api contract for the orchestrator" — nothing CTO-specific). #3135's `extends: assistant` overlay approach makes this edge unnecessary once complete: the persona becomes a `.trusty-agents/agents/*.md` asset loaded through the normal compose chain (§5.1), not a compiled-in plugin registration. **This spec does not re-open that severing** — it is already the direction #3135 takes; this section exists to name it explicitly so Tranche-2 sequencing (§10) does not accidentally re-couple it.

### 9.3 New in this spec: `trusty-gworkspace` becomes a managed daemon MCP service

`trusty-gworkspace` today ships one binary, `trusty-gworkspace-mcp`, that is **both** the onboarding CLI (`setup`/`doctor`/`accounts`) and the MCP stdio server — invoked fresh per client from a `.mcp.json` `"command"` entry (`crates/trusty-gworkspace/README.md`). This is a different lifecycle from trusty-memory/trusty-search/trusty-analyze, which run as **managed daemons** (launchd plist, `GET /health`, graceful SIGTERM drain per the "Connection-safe daemon restart convention," issue #534) that an MCP `serve --stdio` proxy connects to, reconnecting on daemon restart.

**Target:** `trusty-gworkspace` grows the same daemon shape:
- A long-running `trusty-gworkspace serve` (or equivalent) process, managed by a launchd plist like its siblings.
- The existing `trusty-gworkspace-mcp` stdio entry point becomes a thin proxy to the daemon (mirroring trusty-memory's `serve --stdio` pattern) rather than embedding the full OAuth/tool-dispatch logic per client process.
- OAuth token handling (`~/.gworkspace-mcp/tokens.json`) is unchanged — this is a process-lifecycle change, not an auth-model change.

**Rationale:** the CTO-assistant agent (§9.1) needs Google Workspace access as one of its regular tool calls, not a cold-started subprocess per invocation; running it as a managed service matches the reliability bar the other MCP-backed tools already meet, and removes one more per-session subprocess-spawn cost from every trusty-agents session that uses gworkspace tools (not just the CTO-assistant persona — any persona with gworkspace tools benefits).

---

## 10. Part 4 — Sequencing {#SPEC-SWM-10~draft}

**ID:** SPEC-SWM-10~draft · **Status:** Draft

### 10.1 Milestones (each its own issue, filed only after this spec is approved)

| # | Milestone | Depends on | Notes |
|---|---|---|---|
| M1 | Harness-identity wording corrections (§2.2) | — | Ships **with this PR** — docs-only, pre-approved as part of accepting the spec (§0). |
| M2 | Extract `PM_WORKING_MODEL.md` + `working_model.rs` skeleton, seeded from mpm's current assets, zero behavior change (§3.1, §4.3 Phase A) | M1 merged | |
| M3 | mpm's instruction pipeline renders from shared data (§4.3 Phase B) | M2 | Regression-tested against today's exact SM prompt output. |
| M4 | Migrate `circuit.rs` + `project_trust.rs` to `trusty-agents-common`; mpm becomes consumer (§6.1) | M2 (same crate target) | Independent of M3 — can run in parallel. |
| M5 | `PermissionMode` abstraction (§6.2) | M4 | |
| M6 | Agent-persona schema: resolve §11 Q1, implement chosen extension (§5.3) | M2 | Independent of M3–M5. |
| M7 | tcode consumes shared delegation matrix + workflow phases (§4.3 Phase C) | M3, M6 | The largest single milestone — replaces `intent::mod.rs`'s heuristic. |
| M8 | `WorkingModelConformanceKit` (§7) | M3, M7 | Written once both consumers exist — a kit with one consumer can't prove parity. |
| M9 | tcode `Provider` layer folds into `trusty_common::inference` (§8) | #2409 and #2410 land (external dependency, not gated by M1–M8) | Sequenced by the pre-existing #2400 epic, not by this spec. |
| M10 | `trusty-gworkspace` daemon-service conversion (§9.3) | — | Independent of M1–M9; can run any time. |
| M11 | CTO-assistant target-shape completion | Tracked entirely under #3105's own children (#3135/#3139/#3144) | This spec does not own it — listed for completeness only. |

### 10.2 Explicit non-goals

- **No big-bang `trusty-common` split.** Every migration above is one bounded module/type moving into an existing crate (`trusty-agents-common`) that both harnesses already depend on — not a `trusty-common` restructuring.
- **`memory_core` extraction is a separate, later tranche (referenced, not planned here).** `trusty_common::memory_core` (trusty-memory's backend, also used by trusty-agents) is out of scope for Tranche 2 — noted here only so a future "Tranche 4" spec has a named slot to land in; this document makes no claims about its shape or timing.
- **No rename, license, or Cargo dependency-graph changes.** ADR-0004's peer-harness boundary (no harness depends on a sibling harness) is preserved throughout — every migration target in this spec is `trusty-agents-common`, which both mpm and tcode already depend on today.

---

## 11. Open Questions (Bob's call)

1. **Persona-schema pick (§5.3).** Extend mpm's flat frontmatter with optional `llm:`/`tools:`/`runner:` keys (this spec's recommendation), or keep tcode's nested schema separate with a generator from mpm's format? Blocks M6/M7.
2. **`PermissionMode` naming and enum shape (§6.2).** Is `Unattended`/`Attended` the right two-value split, or does tcode's finer-grained `rbac`/`ServiceTier` model need a third state this spec's sketch doesn't capture? Blocks M5.
3. **Does tcode need a `.trusty-mpm/`-equivalent project-override scheme?** §3.3 declares it a non-goal for Tranche 2; confirm that holds, or scope it into M7.
4. **VIBE-tier timing (§4.1, issue #2596).** Should tcode's VIBE/FULL-LOOP split (currently unbuilt) land as part of M7 (consuming the shared matrix from day one) or ship independently first under #2596 and be retrofitted onto the shared matrix later? Affects M7's scope, not its dependency order.
5. **`WorkingModelConformanceKit` location (§7).** `trusty-agents-common::connectors` already hosts `test_kit.rs`; does the new kit belong beside it (`connectors::working_model_test_kit`) or in a fresh top-level module? Naming only — no design ambiguity.

---

## 12. Relationship to Other Specs

| Spec | Relationship |
|---|---|
| [DOC-21 — Harness Understanding](./harness-understanding.md) | This spec's §3.1 shared-asset pattern (`working_model.rs`) directly extends `harness_doc.rs`'s established shape; both live in `trusty-agents-common`. |
| [ADR-0004 / docs/architecture/harnesses.md](../architecture/harnesses.md) | This spec operates entirely within ADR-0004's peer-harness, no-cross-dependency boundary (§10.2) — it is an implementation of "shared commonality lives in trusty-common"-style thinking applied to `trusty-agents-common`, not a revision of the boundary itself. |
| [docs/trusty-code/vision-and-architecture-spec.md](../trusty-code/vision-and-architecture-spec.md) | §2 corrects harness-identity wording in this doc; §4.3/§5.10's Execution Patterns and §4.6's `Provider` trait are consumers of §4 and §8 respectively. |
| [DOC-38 — Spec-Linked Documentation](./spec-linked-documentation.md) | This spec follows DOC-38's header block, DOC-N scan-before-claim assignment (§13), and catalog-row conventions. |
| DOC-44 (unmerged, `spec-twin-lead-architecture` branch — Engineering-Lead Twin Orchestration) | Source of the `ConnectorTestKit` precedent this spec's §7 follows; not a dependency — DOC-44 is independent, unmerged work this spec does not require. |

## 13. DOC-N Assignment Verification

Per DOC-38 §4.1's scan-before-claim rule (the catalog's own hint is non-authoritative), this PR scanned: every `# DOC-N` self-labeled header under `docs/` (`grep -rE '^# DOC-[0-9]+' docs/`), the `docs/specs/README.md` catalog table, and every open `spec-*`/`docs/*-spec` branch's diff against `origin/main` for `docs/specs/*.md` additions. Findings: highest cataloged number is DOC-48; DOC-43 is claimed (uncataloged) by the unmerged `spec-requirements` branch; DOC-44 by `spec-twin-lead-architecture`; DOC-45 by `spec-remote-mcp-credentials`; DOC-46/47/48 are merged and cataloged; DOC-37 is self-labeled (uncataloged) by `trusty-search-managed-repo-awareness.md`; DOC-34 is assigned but uncataloged (`managed-session-config-dir.md`, #1999); DOC-28 has a self-label collision (`mpm-cutover-resume-native-optimization.md`) flagged but not fixed by this PR. **DOC-49 is the next genuinely free integer** — this PR claims it and adds the catalog row (§14 note below is updated accordingly).

---

## References

- Issue #3105 — CTO-Assistant → trusty-agents migration epic (§9)
- Issues #3135, #3139, #3144 — CTO-assistant migration children (§9.1)
- Issues #2400, #2409, #2410 — `trusty_common::inference` adapter migration (§8)
- Issue #3007 — `ConnectorTestKit` precedent (§7)
- Issue #2596 — tcode VIBE-tier tracking (§4.1, §11 Q4)
- `crates/trusty-agents-common/src/harness_doc.rs` — DOC-21 shared-asset pattern
- `crates/trusty-agents-common/src/agents/{builder,frontmatter,metadata}.rs` — shared compose chain
- `crates/trusty-agents-common/src/connectors/test_kit.rs` — `ConnectorTestKit` precedent
- `crates/trusty-mpm/src/core/{circuit,project_trust,model_inject}.rs` — migration sources
- `crates/trusty-code/src/{intent/mod.rs,agent_loop/mod.rs,agents/config.rs,agents/md_loader.rs}` — migration targets
- `crates/trusty-mpm/src/assets/instructions/{BASE_PM,AGENT_DELEGATION,WORKFLOW,PM_INSTRUCTIONS}.md` — working-model source content

