---
spec_refs:
  - id: SPEC-AGENTSTD-03~draft
    path: docs/specs/DOC-61-canonical-agent-standard.md
    anchor: SPEC-AGENTSTD-03~draft
  - id: SPEC-AGENTSTD-05~draft
    path: docs/specs/DOC-61-canonical-agent-standard.md
    anchor: SPEC-AGENTSTD-05~draft
---

# DOC-65 — Universal Framework Agents: Catalog, Boundaries, and the Four-Category Model

**Status:** Draft
**Spec ID:** `SPEC-UNIVAGENT-01~draft` … `SPEC-UNIVAGENT-06~draft` (DOC-65)
**Subsystem:** `trusty-mpm` — bundled agent catalog (`crates/trusty-mpm/src/assets/agents/`), delegation roster (`crates/trusty-mpm/src/core/delegation_authority.rs`), language scoping (`crates/trusty-mpm/src/core/manifest/project_lang.rs`); consumed identically by `trusty-code` (`crates/trusty-code/src/assets/mod.rs`) and referenced by `trusty-agents`' sub-agent tier (ADR-0024)
**Owner:** Engineering (trusty-mpm) / Bob Matsuoka
**Last-updated:** 2026-08-04
**DOC-N claim:** `DOC-65`, scan-before-claim per [DOC-38 §4.1](./spec-linked-documentation.md), verified free on `origin/main` (`d6e13326`): no filename or self-label claim under `docs/specs/**` (highest cataloged is `DOC-64`, README's own "next free" hint says `DOC-65`); no open pull requests at all (`gh pr list --state open` → empty); `scripts/check_doc_numbers.sh` clean (96 docs / 90 claims, 0 violations) before this file is added.
**Builds on:** [ADR-0025](../adr/0025-collapse-agent-and-skill-tier-hierarchies.md) and its 2026-08-03 addendum ("Manifest-Based Project Configuration and the Four-Category Agent Model", §B1–B6) — the deployment/sourcing/precedence model this document maps its catalog onto, cited not restated. [DOC-61](./DOC-61-canonical-agent-standard.md) — the compose-chain source format (`extends:` inheritance, frontmatter merge, per-product builders) every agent cataloged here is built from, cited not restated. `crates/trusty-mpm/src/assets/instructions/sections/agent-delegation.md` — the hand-authored routing prose this document formalizes into a governed spec artifact without duplicating its table.
**Related issues:** **#4755** (this spec, milestone `1.3.5-2`)

---

## 1. Summary

trusty-mpm ships a bundled agent catalog of 42 sub-agent source files under
`crates/trusty-mpm/src/assets/agents/` (five `BASE-*` inheritance templates,
14 language/framework-specific `*-engineer` agents gated by
`language_agent_scope`, and 23 agents that are not gated by it). ADR-0025's
2026-08-03 addendum names the ungated set "**category 1: universal
bundled**" in its four-category model (§B3) but gives only an illustrative
subset as example (`research, qa, version-control, documentation,
code-critic, engineer, local-ops, security, …`). This document is the
missing catalog: it enumerates every agent the code actually treats as
universal, states each one's trigger, model tier, and ownership boundary,
and reconciles three findings the inventory surfaced against ADR-0025,
DOC-61, and the delegation-routing prose — reported to the owner in §6
rather than resolved here, per this document's own dispatch instruction.

**What "universal" means, precisely, per the code (not per prose):** an
agent is universal if and only if its stem is **absent** from
`LANGUAGE_ENGINEERS`
(`crates/trusty-mpm/src/core/manifest/project_lang.rs`) — the single table
`language_agent_scope` consults to decide which `*-engineer` stems to
exclude from a project whose marker files (`Cargo.toml`, `package.json`,
`go.mod`, …) don't match. Nothing else in the deploy or roster path
narrows the "universal" set any further: `deployed_agent_dirs` /
`roster_from_dirs`
(`crates/trusty-mpm/src/core/delegation_authority.rs`) scan every `.md`
file in each tier directory and admit anything with a frontmatter `name:`
that isn't a `BASE-*` foundation file (`is_foundation_file`). This is a
narrower, purely mechanical test than "applies to every project regardless
of stack" — §6 flags where that gap matters.

## 2. Scope and Non-Goals

**In scope:**

- A per-agent catalog (§3) for every bundled agent this document classifies
  as universal by the test above: purpose/trigger, model tier and rationale,
  ownership boundary (what it explicitly does not do, and which agent does),
  and source/deploy path.
- How the universal set relates to the stack-specific `*-engineer` roster
  (§4) and to ADR-0025's four-category model (§5).
- The shared-catalog relationship across trusty-mpm, trusty-code, and
  trusty-agents (§5.4).
- Findings reported to the owner, not resolved here (§6): undocumented,
  duplicated, or deprecated-but-still-deployed agents; conflicts between the
  deployed roster, ADR-0025, DOC-61, and the delegation-routing prose; and a
  recommendation on where this document itself should live.

**Explicitly out of scope:**

- The compose-chain source format itself (frontmatter grammar, `extends:`
  merge rules, the deploy/manifest write path) — DOC-61's subject, not
  restated here.
- Deployment precedence, the manifest schema, and the four-category
  model's own definition — ADR-0025's subject, cited by reference.
- Assistants (tier L0 — `pm`, `izzie`, `cto-assistant`,
  `personal-assistant`, `ctrl`) — a distinct object kind per ADR-0024,
  out of scope by the same boundary DOC-61 §2 draws.
- Resolving the findings in §6. This document reports; the owner decides.

## 3. The Universal Agent Catalog {#SPEC-UNIVAGENT-03~draft}

Ten agents form the core routing table already hand-documented in
`agent-delegation.md`'s "When to Delegate to Each Agent" section (that
table is the routing authority; this section adds model-tier rationale and
explicit boundaries, it does not re-derive the routing itself). Three more
are framework-operational agents — they manage trusty-mpm's own catalog
rather than a user project's code — cataloged separately in §3.2 because
their trigger is "operate on the agent/skill system," not a project task.
§6.1 catalogs the remaining universal-by-the-code's-definition agents that
are neither in this core ten nor framework-operational, because their
routing and boundary are genuinely undocumented.

### 3.1 Core routing agents

| Agent | Purpose / Trigger | Model | Why this tier | Boundary — does NOT own | Source |
|---|---|---|---|---|---|
| `research` | Understanding codebase, investigating approaches, analyzing files before a change | sonnet | Investigation requires synthesizing multiple files/patterns — above haiku's single-pass ceiling, below the adversarial-judgment need that would justify opus | Does not edit code (→ `engineer`); does not render a pass/fail verdict (→ `code-analyzer`/`code-critic`) | `crates/trusty-mpm/src/assets/agents/research.md`, `extends: base-research` |
| `engineer` | Writing/modifying code, implementing features, refactoring | sonnet | Implementation work needs sustained multi-file reasoning | General-purpose only — `agent-delegation.md` directs: "Prefer the language-specific engineer when one exists"; does not review its own output (→ `qa`/`code-critic`); does not manage tickets or PRs (→ `ticketing`/`version-control`) | `crates/trusty-mpm/src/assets/agents/engineer.md`, `extends: base-engineer` |
| `qa` / `web-qa` / `api-qa` | Testing implementations, verifying deployments, regression tests | sonnet (all three) | Test-strategy design and verification judgment | `qa` is generic; `web-qa` owns browser-surface testing exclusively — `agent-delegation.md` forbids calling `chrome-devtools`/`claude-in-chrome`/`playwright` directly, routing must go through `web-qa`; `api-qa` owns REST/GraphQL backend testing. None of the three render an adversarial code-quality verdict (→ `code-critic`) or a pre-implementation static-analysis gate (→ `code-analyzer`) | `crates/trusty-mpm/src/assets/agents/{qa,web-qa,api-qa}.md`, all `extends: base-qa` |
| `code-analyzer` | Reviewing a *proposed* solution before implementation — static analysis, correctness, architectural health | sonnet | Same reasoning-depth need as `research`, whose base (`base-research`) it extends | The **phase-2, pre-implementation** gate: `APPROVED`/`NEEDS_IMPROVEMENT`/`BLOCKED`. `agent-delegation.md` states explicitly: "`code-analyzer` and `code-critic` are separate agents, not interchangeable" | `crates/trusty-mpm/src/assets/agents/code-analyzer.md`, `extends: base-research` |
| `code-critic` | Adversarial, independent code review after implementation — rubric-based `APPROVE`/`WARN`/`BLOCK` | sonnet | Adversarial review needs the same reasoning depth as implementation, deliberately run by a separate dispatch to avoid anchoring bias | The **post-implementation** gate, distinct from `code-analyzer` (above). Does not implement its own fixes — hands findings back to `engineer` | `crates/trusty-mpm/src/assets/agents/code-critic.md`, `extends: base-qa` |
| `documentation` | Creating/updating docs, README, API docs, guides | **haiku** | Formulaic, pattern-following work against established conventions — no adversarial judgment call | Does not assess code correctness (→ `qa`/`code-critic`); does not write tickets (→ `ticketing`) | `crates/trusty-mpm/src/assets/agents/documentation.md`, `extends: base-agent` |
| `ticketing` | Issue/ticket bookkeeping: create, update, close, label, triage, comment | sonnet | Scope validation and workflow-state intelligence require judgment, not just mechanical git operations | `agent-delegation.md`: "ticket bookkeeping never goes to `version-control`" — never does branch/PR mechanics, that is `version-control`'s job | `crates/trusty-mpm/src/assets/agents/ticketing.md`, `extends: base-agent` |
| `version-control` | Creating PRs, managing branches, complex git ops | **haiku** | Git operations are mechanical/procedural once the change is decided — low reasoning need relative to deciding *what* to change | Never does issue/ticket bookkeeping (→ `ticketing`); `agent-delegation.md` requires checking git user identity before main-branch access | `crates/trusty-mpm/src/assets/agents/version-control.md`, `extends: base-ops` |
| `security` | Pre-push credential scan, vulnerability assessment, secret scanning, compliance review | sonnet | Attack-vector reasoning needs sustained judgment | Coordinates with (does not replace) the acting `ops`/engineer agent for actual remediation — `ops.md`'s own text: "Coordinate with the security agent for secrets handling" | `crates/trusty-mpm/src/assets/agents/security.md`, `extends: base-agent` |
| `local-ops` | Deploying apps, managing infra, starting servers, port/process management, `make`/`mise` build & release targets | sonnet | Process supervision and deployment-gate judgment | The **platform-agnostic local** ops agent — explicit successor to deprecated `ops` (§6.1); does not cover platform-specific cloud ops (`gcp-ops`, `vercel-ops`, §6.1) — no documented handoff rule exists between them (flagged §6.2) | `crates/trusty-mpm/src/assets/agents/local-ops.md`, `extends: base-ops` |

Two model-tier facts that generalize across the table: haiku is reserved for
the two agents whose work is genuinely mechanical relative to the rest of
the roster (`documentation`, `version-control`); every other core agent is
sonnet. No core universal agent uses opus. Per DOC-61 §3.3, `model:` is set
directly in each source file's frontmatter (not derived from a
`resource_tier:` value), so the "why" column above is this document's own
rationale read off the roster's actual tier choices, not a value the code
itself states.

### 3.2 Framework-operational agents

These three operate on trusty-mpm's own agent/skill catalog rather than on
a user project's code — a different trigger category from §3.1, not a
different precedence tier.

| Agent | Purpose / Trigger | Model | Boundary | Source |
|---|---|---|---|---|
| `memory-manager` | Store/recall/tag/prune project and session facts, exclusively through the trusty-memory MCP backend | **haiku** | No static memory files — MCP-only, per its own frontmatter description; does not decide *what* is worth remembering strategically (that judgment stays with the delegating assistant) | `crates/trusty-mpm/src/assets/agents/memory-manager.md`, `extends: base-agent` |
| `mpm-agent-manager` | Agent-catalog lifecycle: discovery, validation, bundled-asset deployment, contribution workflow | sonnet | Per ADR-0025 addendum §B2: its differentiator from Claude Code's own `/agents` is that it *composes* a new custom agent from the five `BASE_*` fragments rather than writing a bare file — but that on-request authoring workflow is **not built today**; only the underlying `compose_agent` primitive exists (§6.2) | `crates/trusty-mpm/src/assets/agents/mpm-agent-manager.md`, `extends: base-agent` |
| `mpm-skills-manager` | Skill-catalog lifecycle: discovery, deployment, tech-stack-based recommendations, contribution workflow | sonnet | Skill-side counterpart to `mpm-agent-manager`; per ADR-0025 addendum §"Deliberately Out of Scope," no stack-detection auto-selection exists for skills the way `language_agent_scope` exists for agents — `[skills] include/exclude` stays manual-only | `crates/trusty-mpm/src/assets/agents/mpm-skills-manager.md`, `extends: base-agent` |

## 4. Universal vs. Stack-Specific Agents {#SPEC-UNIVAGENT-04~draft}

The only code-level line between "universal" and "stack-specific" is
`LANGUAGE_ENGINEERS`
(`crates/trusty-mpm/src/core/manifest/project_lang.rs`): 14 `*-engineer`
stems, each paired with marker files (`Cargo.toml` → `rust-engineer`,
`package.json`/`tsconfig.json` → the JS/TS family, `go.mod` →
`golang-engineer`, …). `language_agent_scope` probes the project root for
those markers and, when at least one matches, returns an `AgentSet` whose
`exclude` list drops every *non-matching* language engineer — a Rust
workspace's deploy excludes `python-engineer`, `javascript-engineer`, etc.,
but keeps every agent not in the table at all, unconditionally. An unknown
project type (no marker recognized) returns `None`, so `resolve_manifest`
falls back to deploying the full, unscoped roster — universal agents plus
every stack-specific one — rather than deploying nothing.

This means: a stack-specific agent is scoped **out** by absence of its
marker; a universal agent is never scoped by this mechanism at all,
regardless of the project. That is the entire mechanism — there is no
separate "universal allowlist," only a stack-specific exclusion list whose
complement is, by construction, everything else. §6.1/§6.2 report where
this construction produces agents that are universal by this mechanical
test but not universal in the sense a reader of ADR-0025's illustrative
list would expect.

## 5. Mapping onto the ADR-0025 Four-Category Model {#SPEC-UNIVAGENT-05~draft}

ADR-0025's 2026-08-03 addendum (§B3) defines four categories; this section
maps the catalog above onto them without restating the addendum's own
column definitions (deploy target, selection mechanism, code status).

### 5.1 Category 1 — universal bundled

Every agent in §3.1 and §3.2, plus every agent §6.1 reports as
mechanically universal but under-documented. Deploy target:
`FrameworkPaths::agent_deploy_dir()`, unconditionally selected (absent from
`LANGUAGE_ENGINEERS`).

### 5.2 Category 2 — stack-specific bundled

The 14 `*-engineer` stems in `LANGUAGE_ENGINEERS`, selected via
`[agents] include/exclude` in `manifest.toml`, auto-derived per project by
`language_agent_scope`. Same deploy target as category 1 — selection, not a
separate directory, is what distinguishes the two (ADR-0025 addendum §B3's
own point).

### 5.3 Categories 3 and 4 — user-installed custom agents

Project-level (category 3, `<project>/.claude/agents/`) and user-level
(category 4, the managed `CLAUDE_CONFIG_DIR/agents` or, for a standalone
session, real `~/.claude/agents` — an open question per ADR-0025 addendum
§"Open Questions" item 4) custom agents. Neither category exists in this
document's catalog by definition — they are operator- or
`tm-agent-manager`-authored, not bundled. `resolve_roster`
(`crates/trusty-mpm/src/core/delegation_authority.rs`) already unions all
four categories into one roster today, project tier winning on a name
collision — a category-3 or category-4 agent with the same stem as a
universal bundled agent silently shadows it, the same mechanism ADR-0025's
own #4408 incident narrative describes for the stack-specific case.

### 5.4 Relationship to trusty-code and trusty-agents

Bob's directive that the same agents serve trusty-mpm, trusty-code, and
trusty-agents is implemented today, not merely aspirational, for the
trusty-mpm ↔ trusty-code pair: `crates/trusty-code/src/assets/mod.rs`
embeds a 33-agent dispatchable roster — tcode's own 4 defaults plus **29
of trusty-mpm's own bundled agents, `extends:`-chained through the same
five `BASE-*` templates**, reused verbatim via
`agents::md_loader::project_embedded_md_with_extends` rather than forked.
This is DOC-61 §4's "one shared source model, per-product builders"
pipeline in production: trusty-code's builder projects the identical
frontmatter into its own `AgentConfig` shape rather than a Claude Code
subagent file. trusty-agents' relationship is narrower and governed
entirely by ADR-0024/DOC-61 §5 — sub-agents (L1) there are the same object
kind this document catalogs, but trusty-agents' assistant tier (L0) is
explicitly the different, out-of-scope object kind neither this document
nor DOC-61 touches.

## 6. Findings for the Owner {#SPEC-UNIVAGENT-06~draft}

Per this document's own dispatch instruction, the following are reported,
not resolved.

### 6.1 Undocumented, duplicated, or deprecated-but-still-deployed agents

- **`ops` is deprecated in prose but still a full, still-deployed bundled
  asset.** `agent-delegation.md` states twice — "Generic `ops` agent is
  DEPRECATED; use `local-ops`" and "**NOTE**: Generic `ops` agent is
  DEPRECATED. Use platform-specific agents." — but
  `crates/trusty-mpm/src/assets/agents/ops.md` is a 63-line, fully-formed
  agent definition, largely overlapping `local-ops.md` (151 lines) in
  content. Nothing in the deploy or roster path excludes it: it is absent
  from `LANGUAGE_ENGINEERS` (so `language_agent_scope` never touches it)
  and its stem is not `base-*` (so `is_foundation_file` never excludes it
  from `resolve_roster`). `agent-delegation.md`'s own header states "The
  ONLY agents filtered out are foundation templates... frontmatter is never
  used to hide an agent" — confirming `ops` mechanically reaches every
  project's roster and the PM's delegation prompt today, deprecation notice
  notwithstanding.
- **`gcp-ops` and `vercel-ops` are platform-specific but not scoped like a
  language engineer.** They are, in character, exactly the same shape as a
  `*-engineer` stem — relevant only to projects targeting that one
  platform — but `LANGUAGE_ENGINEERS` only covers language markers, not
  cloud-platform markers, so `language_agent_scope` never excludes them.
  A project with no GCP or Vercel footprint still deploys and advertises
  both agents in its roster, unconditionally — the same class of roster
  noise issue #1941 built `language_agent_scope` to eliminate for
  languages, left unaddressed for ops platforms.
- **`web-ui-engineer`, `data-engineer`, `prompt-engineer`, and
  `refactoring-engineer` have no routing entry in `agent-delegation.md`.**
  All four are correctly left ungated by `LANGUAGE_ENGINEERS` (none is tied
  to one specific stack), but none appears in the routing table's "When to
  Delegate to Each Agent" section, so a PM has no documented signal
  distinguishing "delegate to `engineer`" from "delegate to
  `web-ui-engineer`" or `refactoring-engineer`. `web-qa`/`api-qa`, by
  contrast, ARE documented (grouped with `qa` in the same table row) — this
  gap is specific to the four named here.

### 6.2 Conflicts between the deployed roster, ADR-0025, DOC-61, and the delegation prose

- **ADR-0025 addendum §B3's category-1 example list is illustrative, not
  exhaustive, and could be mistaken for the actual boundary.** Its own text
  — "`research`, `qa`, `version-control`, `documentation`, `code-critic`,
  `engineer`, `local-ops`, `security`, …" — trails with an ellipsis, but
  gives no pointer to what the "…" actually contains. §3–§4 of this
  document supply that: the real boundary is "absent from
  `LANGUAGE_ENGINEERS`," which is a substantially larger and less curated
  set than the ADR's example (it includes `gcp-ops`, `vercel-ops`,
  `web-ui-engineer`, `data-engineer`, `prompt-engineer`,
  `refactoring-engineer`, `memory-manager`, `mpm-agent-manager`,
  `mpm-skills-manager`, and the deprecated `ops`). Whether "universal"
  should be formally redefined as exactly that mechanical complement, or
  whether platform-ops agents deserve their own scoping dimension
  alongside `language_agent_scope`, is not decided here.
- **`mpm-agent-manager`'s documented differentiator over Claude Code's
  `/agents` is not yet backed by code.** ADR-0025 addendum §B2 states its
  value is composing a new custom agent from the `BASE_*` fragments — but
  also states plainly "What is **not** built: a code path that lets
  `tm-agent-manager` invoke this same composer to author a *new* custom
  agent on an operator's request." This document's §3.2 entry for
  `mpm-agent-manager` describes its intended purpose per the ADR while
  flagging the same gap; no code path currently makes that purpose true.
- **No conflict found with DOC-61** — its scope (§2) never claims to
  catalog individual agent semantics, so this document does not overlap or
  contradict it; the compose-chain mechanics DOC-61 documents (§3.3
  frontmatter merge, §4 per-product builders) are exactly what §5.4 of this
  document cites, not re-derives.

### 6.3 Where this document should live

**Recommendation: a new standalone spec (this document, claiming `DOC-65`),
not an extension of an existing one.** Reasoning:

- **Not DOC-61** — its own §2 Non-Goals scope it to "the source authoring
  model... and compose semantics," explicitly product-agnostic and
  explicitly not a per-agent semantic catalog. Folding a roster/boundary
  catalog into it would blur a scope DOC-61 states deliberately.
- **Not ADR-0025** — an ADR records a decision and its consequences; a
  living per-agent catalog with trigger conditions and boundaries is
  reference material that will need routine updates as agents are added,
  renamed, or reclassified (exactly the kind of change an ADR's
  Accepted/Superseded lifecycle is not built for). This document builds on
  ADR-0025 rather than amending it again.
- **Not DOC-31** (`docs/specs/system-project-agents-skills.md`) — already
  marked "Superseded in part" by ADR-0025 itself (ADR-0025's own Related
  Decisions section), and its code citations (`agent_deployer.rs`,
  `agent_manifest.rs`) predate the #2892 move to
  `trusty-agents-common::agents::{deployer,manifest}`. Extending a
  partially-superseded document with stale citations would compound rather
  than resolve that drift.
- `agent-delegation.md` itself stays the routing-table source of truth (it
  is loaded into every PM prompt); this document does not replace it, and
  recommends it eventually declare `spec_refs: [{id: SPEC-UNIVAGENT-03,
  path: docs/specs/DOC-65-universal-framework-agents.md}]` once this
  document's status moves past Draft, so the routing table and this
  catalog are linked rather than free-floating duplicates.

## 7. References

**Code (verified directly in this repository, this worktree):**

- `crates/trusty-mpm/src/assets/agents/*.md` — the 42-file bundled agent
  catalog (5 `BASE-*` templates, 14 stack-specific engineers, 23 universal
  agents by this document's test).
- `crates/trusty-mpm/src/core/manifest/project_lang.rs` —
  `LANGUAGE_ENGINEERS`, `language_agent_scope`, `detected_engineers`,
  `marker_present` — the sole code-level universal/stack-specific
  boundary.
- `crates/trusty-mpm/src/core/delegation_authority.rs` —
  `deployed_agent_dirs`, `roster_from_dirs`, `resolve_roster`,
  `deployed_roster_section`, `is_foundation_file` — the roster resolver
  every consumer (PM prompt, `tm session start`, `tm doctor`) shares.
- `crates/trusty-mpm/src/assets/instructions/sections/agent-delegation.md`
  — the hand-authored routing table this document formalizes without
  duplicating.
- `crates/trusty-code/src/assets/mod.rs` — `DEFAULT_AGENTS`,
  `EMBEDDED_TM_AGENT_SOURCES` — the trusty-code shared-catalog reuse
  evidence for §5.4.

**Specs and ADRs:**

- `docs/adr/0025-collapse-agent-and-skill-tier-hierarchies.md` — the
  deployment/sourcing model and, in its 2026-08-03 addendum, the
  four-category agent model this document maps its catalog onto (§5).
- `docs/specs/DOC-61-canonical-agent-standard.md` — the compose-chain
  source format and per-product builder pipeline (§5.4, §6.2).
- `docs/specs/system-project-agents-skills.md` (DOC-31) — superseded in
  part by ADR-0025; not extended here (§6.3).

## 8. Change Log

- 2026-08-04 — Initial DRAFT. Claims `DOC-65` (scan-before-claim, DOC-38
  §4.1). Catalogs the universal agent roster verified against
  `crates/trusty-mpm/src/assets/agents/`,
  `crates/trusty-mpm/src/core/manifest/project_lang.rs`, and
  `crates/trusty-mpm/src/core/delegation_authority.rs` on `origin/main`
  (`d6e13326`). Maps onto ADR-0025's 2026-08-03 four-category-model
  addendum. Filed for issue #4755, milestone `1.3.5-2`.
