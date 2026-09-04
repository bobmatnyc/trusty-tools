---
spec_refs:
  - id: SPEC-PMINSTR-12~draft
    path: docs/specs/SPEC-PMINSTR-01-p1-p2-instruction-restructure.md
    anchor: SPEC-PMINSTR-12~draft
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
**Last-updated:** 2026-08-08
**DOC-N claim:** `DOC-65`, scan-before-claim per [DOC-38 §4.1](./spec-linked-documentation.md), verified free on `origin/main` (`d6e13326`): no filename or self-label claim under `docs/specs/**` (highest cataloged is `DOC-64`, README's own "next free" hint says `DOC-65`); no open pull requests at all (`gh pr list --state open` → empty); `scripts/check_doc_numbers.sh` clean (96 docs / 90 claims, 0 violations) before this file is added.
**Builds on:** [ADR-0025](../adr/0025-collapse-agent-and-skill-tier-hierarchies.md) and its 2026-08-03 addendum ("Manifest-Based Project Configuration and the Four-Category Agent Model", §B1–B6) — the deployment/sourcing/precedence model this document maps its catalog onto, cited not restated. [DOC-61](./DOC-61-canonical-agent-standard.md) — the compose-chain source format (`extends:` inheritance, frontmatter merge, per-product builders) every agent cataloged here is built from, cited not restated. `crates/trusty-mpm/src/assets/instructions/sections/agent-delegation.md` — the hand-authored routing prose this document formalizes into a governed spec artifact without duplicating its table.
**Related issues:** **#4755** (this spec), **#4760** (the framework manifest this document was updated for), **#5202** (workflow/ticketing/version-control ownership) — scheduled in the 1.3.5 release line

> **Source-ownership amendment (2026-09-03):** The MPM asset paths below are
> current provenance, not permanent canonical ownership. ADR-0059 requires one
> host-neutral authored source and generated per-product adapters. Trusty Code
> and Trusty Agents implementation is #5425; MPM adoption is a separate
> cross-product handoff.

---

## 1. Summary

trusty-mpm ships a bundled agent catalog of 42 sub-agent source files under
`crates/trusty-mpm/src/assets/agents/`: five `BASE-*` inheritance templates
plus 37 dispatchable agents. ADR-0025's 2026-08-03 addendum names the
always-deployed set "**category 1: universal bundled**" in its four-category
model (§B3) but gives only an illustrative subset as example (`research, qa,
version-control, documentation, code-critic, engineer, local-ops, security,
…`). This document is the missing catalog: it enumerates every agent, states
each one's trigger, model tier, and ownership boundary, and records how the
deployment categories settle three findings the original inventory surfaced.

**What "universal" means, precisely, per the code (not per prose).** Since
issue #4760 it is a **declaration**, read from the bundled
`crates/trusty-mpm/src/assets/framework-manifest.toml`. That file is the
FRAMEWORK TIER of the existing `manifest.toml` format — a `HarnessManifest`
document parsed by the same `HarnessManifest::from_toml`, not a second file
format — and its `[agent_categories]` section partitions all 37 dispatchable
agents into five disjoint lists:

| Category | Count | Gate |
|---|---|---|
| `universal` | 19 | Deploys to every project. No detection at all. |
| `language` | 11 | Deploys only when a LANGUAGE marker is detected. |
| `framework` | 5 | Deploys only when a FRAMEWORK marker is detected. |
| `platform` | 2 | Deploys only when a PLATFORM marker is detected. |
| `deprecated` | 0 | Never deploys. Empty today — see §6.1. |

The previous definition — "universal iff its stem is **absent** from
`LANGUAGE_ENGINEERS`" — was a mechanical complement, not a decision, and it
is what let `ops` stay deprecated in prose while reaching every roster. That
definition produced 22 "universal" agents against 15 stack-specific ones. The
catalog is still 37 dispatchable agents: `ops` was deleted (§6.1) and
`elixir-engineer` added (§4.1), a net zero, and the remainder are now sorted
by an explicit rule instead of by omission from one table.
`core::manifest::framework::parse_framework_manifest` enforces that the five
lists exactly partition the bundled catalog, so an agent can no longer be
forgotten into (or out of) deploying — a bundled agent nobody classified is a
hard error, not a silent default.

The roster path is unchanged and remains broader than the deploy path:
`deployed_agent_dirs` / `roster_from_dirs`
(`crates/trusty-mpm/src/core/delegation_authority.rs`) scan every `.md` file
in each tier directory and admit anything with a frontmatter `name:` that
isn't a `BASE-*` foundation file (`is_foundation_file`). A deprecated or
undetected agent stops being DEPLOYED; a copy already on disk is not
retracted (orphan retraction is issue #391 / ADR-0025 clause 9, unshipped).

**Naming caution.** ADR-0025's "four-category agent model" and these four
deployment categories share a number and nothing else. ADR-0025 classifies by
WHO AUTHORED an agent and WHERE it lives (universal bundled / stack-specific
bundled / project custom / user custom); this document's categories classify,
within ADR-0025's bundled categories 1–2 only, by WHAT GATES deployment. The
axes are orthogonal — see §5.

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
| `ticketing` | Issue operations: promote, search/deduplicate, create, update, close/reopen, label, milestone, link, triage, and comment | sonnet | Scope validation and workflow-state intelligence require judgment, not just mechanical repository operations | Owns issue artifacts only, following `tm-ticketing`; never performs git operations or creates/edits/merges PRs (→ `version-control`) | `crates/trusty-mpm/src/assets/agents/ticketing.md`, `extends: base-agent` |
| `version-control` | All git and PR operations: branches, commits, rebases, pushes, tags, PR creation/update/merge, title, and body | **haiku** | Repository operations are procedural once the change and lifecycle are decided by `tm-workflow` | Never does issue bookkeeping (→ `ticketing`) or defines delivery policy (→ `tm-workflow`); checks git identity before main-branch access | `crates/trusty-mpm/src/assets/agents/version-control.md`, `extends: base-ops` |
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
| `mpm-skills-manager` | Skill-catalog lifecycle: discovery, deployment, tech-stack-based recommendations, contribution workflow | sonnet | Skill-side counterpart to `mpm-agent-manager`; per ADR-0025 addendum §"Deliberately Out of Scope," no stack-detection auto-selection exists for skills the way `detected_engineers` + `framework-manifest.toml`'s gated categories do for agents — `[skills] include/exclude` stays manual-only | `crates/trusty-mpm/src/assets/agents/mpm-skills-manager.md`, `extends: base-agent` |

## 4. The Four Deployment Categories {#SPEC-UNIVAGENT-04~draft}

Deployment is DECLARED in one place and EVALUATED in another, and the split is
declaration-vs-mechanism, not authority-vs-authority. The bundled
`framework-manifest.toml` declares which category each agent is in AND the
`markers` that gate it — issue #4765 moved the former `LANGUAGE_ENGINEERS` /
`PLATFORM_AGENTS` Rust tables into it, per the owner ruling of 2026-08-04
("authoritative agent/skill bundling should be in framework-manifest.toml or
project manifest.toml"). `crates/trusty-mpm/src/core/manifest/project_lang.rs`
retains only the marker EVALUATOR: how a marker string is tested against a
directory, bounded and fail-closed.
`core::manifest::framework::agent_scope_from` composes declaration with
evaluation:

```text
exclude = deprecated
        ∪ ((language ∪ framework) \ detected stacks)
        ∪ (platform \ detected platforms)
```

A `universal` stem appears in no term of that expression, so nothing can
exclude it. That is the whole meaning of "always deploy": not that some code
path forgot to filter it, but that the manifest declares it and the
composition has no term that could remove it.

The gate is stated as an EXCLUSION rather than an `include` allowlist on
purpose. The agent source directory also carries the five `BASE-*`
inheritance fragments, and a `ContentSource::Catalog` checkout may carry
agents no bundled manifest names; an allowlist would silently drop all of
them. The exhaustiveness invariant buys what the allowlist would have — a
bundled agent absent from the manifest is a hard error, so it cannot be
forgotten into deploying.

### 4.1 `language` and `framework` — two declarations, two real gates

Both resolve through `MarkerProbe::detect`, which probes the project root (and
every declared workspace member) against each entry's own declared `markers` —
16 gated stems: 11 `language`, 5 `framework`. The two categories gate on
genuinely different evidence.

The original split was a declaration the markers did not honour: four of the
five framework stems fired on a marker that identified a *language*, not a
framework. The owner ruled on 2026-08-04 to tighten them. What changed:

| Stem | Markers before | Markers now |
|---|---|---|
| `tauri-engineer` | `src-tauri/tauri.conf.json`, `tauri.conf.json` | unchanged — already framework-only |
| `nextjs-engineer` | `next.config.*` **or `package.json`** | `next.config.*` or `package.json::"next"` |
| `svelte-engineer` | `svelte.config.*` **or `package.json`** | `svelte.config.*` or `package.json::"svelte"` |
| `react-engineer` | `package.json` | `package.json::"react"` |
| `phoenix-engineer` | `mix.exs` | `mix.exs::{:phoenix,` |
| `elixir-engineer` | *(did not exist)* | `mix.exs` |

**React and Phoenix could not be gated on a fixed path at all.** React ships
no configuration file — it is a library, and Vite, CRA, and Next.js projects
share nothing React-specific on disk. Phoenix ships nothing plain Elixir
lacks: `mix.exs`, `config/*.exs`, and `priv/` all exist in ordinary Mix
projects, and the directory that *would* identify a Phoenix app
(`lib/<app>_web/`) is named after the application, so no fixed path reaches
it. Next.js has the same shape in miniature — `next.config.*` is genuinely
optional, so a config-only gate would miss real Next.js projects.

Rather than infer from a proxy, `marker_present` gained a third marker form:
a **content probe**, written `<path>::<needle>`, true when the file exists, is
at most 1 MiB, and contains `<needle>` as a literal substring. Every needle
carries its surrounding syntax, so a substring match is an exact declaration
match: `"react"` (with both quotes) does not match `"react-dom"`,
`"react-native"`, or `"@types/react"`; `{:phoenix,` does not match
`{:phoenix_live_view,`. This reads a dependency declaration the project wrote
about itself. It is not a heuristic, and it is bounded — any I/O failure, an
oversized file, or a directory yields "marker absent", never an error.

**`elixir-engineer` is new, and it is the other half of the Phoenix change.**
`mix.exs` was `phoenix-engineer`'s marker, which meant a plain Elixir project
received a Phoenix specialist and no general Elixir engineer at all. Narrowing
Phoenix without adding Elixir would have left Elixir with no coverage, so the
two land together. A Phoenix project detects both — Phoenix apps are Elixir
apps — and `phoenix-engineer`'s description narrowed to the web layer
(Router, Controllers, LiveView, Channels, HEEx) so the two roles do not
overlap.

**Monorepos probe their members, not just the root.** Marker detection
evaluates every marker against the project root AND every workspace member the
ROOT MANIFEST ITSELF declares — npm/yarn `workspaces` (array and
`{packages: […]}` forms), `pnpm-workspace.yaml` `packages:`, and an Elixir
umbrella's `apps_path:`. Without this, a turborepo or pnpm-workspaces root —
which declares neither `"react"` nor `"next"` and carries no `next.config.*` —
would lose three engineers it had before the gates tightened, and an Elixir
umbrella would lose `phoenix-engineer`. Narrowing was authorised for
single-package projects that never declared the framework; it was not
authorised for monorepos that declare it one directory down. Declared globs are
honoured rather than `packages/*` and `apps/*` being assumed, so a workspace
using `services/*` or `libs/*` is covered without the code guessing at
conventions (`core::manifest::workspace`).

Every dimension of that walk is bounded, because it runs on the launch path:

| Bound | Value | Rationale |
|---|---|---|
| Glob depth | one `*` segment, no recursive descent | npm/pnpm globs are overwhelmingly one level. Expanding one level makes enumeration O(entries in one directory) and never exponential. A `**` is treated as a single `*`. |
| Patterns read | 64 per manifest | A manifest declaring more member globs than this is malformed, not large. |
| Members probed | 256 | Bounds directory enumeration. Detection is a boolean OR — one matching member decides the answer — so the marginal member past 256 almost never changes the result. |
| Bytes read | 16 MiB per detection call, shared | Per-file caps alone still permit `members × cap` in aggregate (256 MiB). One shared allowance bounds the quantity that actually matters. A real 256-member monorepo with few-KB manifests reads well under 1 MiB. |

Exhausting a bound is fail-closed: probing stops and the unread members read as
carrying no marker, exactly as an unreadable file does. It never errors and
never blocks a launch. A declared pattern containing `..` is rejected so a
manifest cannot walk outside the project.

**This is a deliberate behavior change, not accidental scope loss.** Stated
precisely — the earlier claim of "plain JavaScript projects" was too narrow,
and that imprecision is what let the monorepo case go unnamed until review:

| Project shape | Loses | Keeps |
|---|---|---|
| Single-package JS repo that declares react/next/svelte | nothing | all three, as before |
| Single-package JS repo that declares NONE of them | `react-engineer`, `nextjs-engineer`, `svelte-engineer` | `javascript-engineer`, `typescript-engineer`, every universal agent |
| JS monorepo whose members declare the framework | nothing | all three, via member probing |
| JS monorepo whose members declare none of them | the same three | the language engineers |
| Plain Elixir project (`mix.exs`, no Phoenix dep) | `phoenix-engineer` | **gains** `elixir-engineer` |
| Elixir umbrella with a Phoenix app under `apps_path` | nothing | both, via member probing |
| Project with no GCP/Vercel marker at root or in a member | `gcp-ops`, `vercel-ops` | everything else |

Residual gaps, named rather than left implicit: a framework declared ONLY as a
transitive dependency (absent from every member's own manifest) is not seen; a
member beyond the 256-member or 16-MiB bound is not probed; and a workspace glob
relying on recursion deeper than one level resolves only its first level.

### 4.2 `platform` — a genuinely new gate

`gcp-ops` and `vercel-ops` are the two `platform` entries, each declaring its
own markers in `framework-manifest.toml`. Markers are deliberately narrow —
files the platform's own tooling creates or requires, not inferred heuristics:

| Stem | Markers |
|---|---|
| `gcp-ops` | `app.yaml`, `cloudbuild.yaml`, `cloudbuild.yml`, `.gcloudignore` |
| `vercel-ops` | `vercel.json`, `.vercelignore`, `.vercel/project.json` |

**This is an intended behavior change.** Both agents deployed to every
project before this category existed; they now deploy only where a marker is
present. A project matching no platform deploys zero platform agents — an
ordinary, explicitly tested result, distinct from the loud error an unusable
manifest raises.

### 4.3 The unknown-project fallback

A project with NO recognised stack marker (e.g. the daemon's own framework
root) contributes nothing to the stack term, so every `language` and
`framework` engineer still deploys — unchanged from `language_agent_scope`.
`platform` has no such fallback by design: "I could not tell what stack this
is" is a reason to be generous with engineers and is not evidence that the
project targets GCP or Vercel.

### 4.4 Loud failure

`parse_framework_manifest` returns `Err` — never a permissive default — for a
malformed document, a missing `[agent_categories]` section, an empty
`universal` list, a declared stem with no bundled agent, a bundled agent
nobody declared, a stem declared twice, or a gated stem with no marker row.
The manifest is embedded with `include_str!`, so a MISSING file is a compile
error. `framework_agent_scope` refuses to resolve a selection from an
unusable manifest rather than falling back to deploying everything or
nothing.

## 5. Mapping onto the ADR-0025 Four-Category Model {#SPEC-UNIVAGENT-05~draft}

ADR-0025's 2026-08-03 addendum (§B3) defines four categories; this section
maps the catalog above onto them without restating the addendum's own
column definitions (deploy target, selection mechanism, code status).

**The two axes are orthogonal, and share only a number.** ADR-0025 asks *who
authored this agent and where does it live*; §4 asks *what gates its
deployment*. Every stem `framework-manifest.toml` declares falls inside
ADR-0025 categories 1–2; the manifest says nothing at all about ADR-0025
categories 3–4, which no manifest declares. Do not read a §4 category name as
an ADR-0025 category name — `universal` is the one word both taxonomies use,
and it means different things in each.

### 5.1 Category 1 — universal bundled

Every agent declared `universal` in `framework-manifest.toml` (19 of the 37
dispatchable agents: §3.1 and §3.2 plus the four §6.1 reports as
under-documented). Deploy target: `FrameworkPaths::agent_deploy_dir()`,
unconditionally selected.

### 5.2 Category 2 — stack-specific bundled

The 18 marker-gated agents: 11 declared `language`, 5 declared `framework`,
2 declared `platform`. Same deploy target as category 1 — selection, not
a separate directory, is what distinguishes ADR-0025's categories 1 and 2
(ADR-0025 addendum §B3's own point). ADR-0025 assumed this set was exactly
`LANGUAGE_ENGINEERS`; §4.2 adds the platform gate it did not anticipate.

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

## 6. Findings {#SPEC-UNIVAGENT-06~draft}

This section was written as "reported, not resolved." Issue #4760 resolved
three of its findings; each is kept below with its original evidence and a
**RESOLVED** note describing how the manifest settles it. The findings still
open keep their original wording. Evidence quoted under a RESOLVED note
describes the code as it stood when the finding was made — in particular it
cites `language_agent_scope`, which #4760 removed — and is preserved
deliberately so the reasoning stays auditable.

### 6.1 Undocumented, duplicated, or deprecated-but-still-deployed agents

- **RESOLVED (#4760) — `ops` is deprecated in prose but still a full,
  still-deployed bundled asset.** The owner ruled on 2026-08-04 that it is
  **deleted from the bundle**, not kept-but-not-deployed: the asset, its
  `OPS_AGENT` constant, its `bundle_all.rs` registration, and its test
  fixtures are all gone. `ALL` is unchanged at 178 because
  `elixir-engineer` was added in the same change (§4.1).

  Two consequences, stated so neither is mistaken for done. **An `ops.md`
  ALREADY deployed to a machine is not retracted** — orphan retraction is
  issue #391 / ADR-0025 clause 9 and has not shipped, so existing machines
  keep an orphan copy until it does. And the `deprecated` category is now
  **empty**, which is its correct state rather than a leftover: it remains as
  the mechanism for retiring a future agent that must stay bundled, and its
  behaviour is proven against a synthetic catalog by
  `deprecated_agent_never_deploys`, which does not depend on whichever agent
  happens to be retired today. Original evidence: `agent-delegation.md` states twice — "Generic `ops` is
  DEPRECATED; use `local-ops` for localhost/PM2/docker" and "**NOTE**: Generic `ops` agent is
  DEPRECATED. Use platform-specific agents." — but
  `crates/trusty-mpm/src/assets/agents/ops.md` is a 63-line, fully-formed
  agent definition, largely overlapping `local-ops.md` (151 lines) in
  content. Nothing in the deploy or roster path excludes it: it is absent
  from `LANGUAGE_ENGINEERS` (so `language_agent_scope` never touches it)
  and its stem is not `base-*` (so `is_foundation_file` never excludes it
  from `resolve_roster`). `agent-delegation.md`'s own header states "The
  ONLY agents filtered out are foundation templates... frontmatter is never
  used to hide an agent" — confirming `ops` mechanically reaches every
  project's roster and the PM's delegation prompt, deprecation notice
  notwithstanding.
- **RESOLVED (#4760) — `gcp-ops` and `vercel-ops` are platform-specific but
  not scoped like a language engineer.** They are now the `platform`
  category, gated on the `PLATFORM_AGENTS` marker table (§4.2). A project
  with no GCP or Vercel marker no longer deploys them — an intended behavior
  change, not accidental scope loss. Original evidence: They are, in character, exactly the same shape as a
  `*-engineer` stem — relevant only to projects targeting that one
  platform — but `LANGUAGE_ENGINEERS` only covers language markers, not
  cloud-platform markers, so `language_agent_scope` never excludes them.
  A project with no GCP or Vercel footprint still deploys and advertises
  both agents in its roster, unconditionally — the same class of roster
  noise issue #1941 built `language_agent_scope` to eliminate for
  languages, left unaddressed for ops platforms.
- **PARTIALLY RESOLVED (#4760) — `web-ui-engineer`, `data-engineer`,
  `prompt-engineer`, and `refactoring-engineer` have no routing entry in
  `agent-delegation.md`.** Their CLASSIFICATION is now a decision rather than
  a default: all four are declared `universal` in `framework-manifest.toml`,
  on the ground that none is tied to a single stack, platform, or framework
  (`web-ui-engineer` covers HTML/CSS/accessibility across any front end,
  `data-engineer` covers ETL and migrations across any store,
  `prompt-engineer` and `refactoring-engineer` are language-agnostic by
  construction). Their absence from the ROUTING TABLE is untouched and stays
  open — classification and routing are different documents. Original
  evidence: all four are correctly left ungated (none is tied to one
  specific stack), but none appears in the routing table's "When to
  Delegate to Each Agent" section, so a PM has no documented signal
  distinguishing "delegate to `engineer`" from "delegate to
  `web-ui-engineer`" or `refactoring-engineer`. `web-qa`/`api-qa`, by
  contrast, ARE documented (grouped with `qa` in the same table row) — this
  gap is specific to the four named here.

### 6.2 Conflicts between the deployed roster, ADR-0025, DOC-61, and the delegation prose

- **RESOLVED (#4760) — ADR-0025 addendum §B3's category-1 example list is
  illustrative, not exhaustive, and could be mistaken for the actual
  boundary.** Its own text — "`research`, `qa`, `version-control`,
  `documentation`, `code-critic`, `engineer`, `local-ops`, `security`, …" —
  trails with an ellipsis and gives no pointer to what the "…" contains, and
  the real boundary at the time was the mechanical complement of
  `LANGUAGE_ENGINEERS`, a substantially larger and less curated set. Both
  open questions that finding raised are now answered by
  `framework-manifest.toml`: "universal" is NOT the mechanical complement —
  it is a declared list of 19 — and platform-ops agents DID get their own
  scoping dimension (§4.2). The `[agent_categories]` section is the
  exhaustive list the ellipsis stood in for, and
  `parse_framework_manifest`'s partition invariant keeps it exhaustive.
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
  catalog: 5 `BASE-*` templates plus 37 dispatchable agents (19 `universal`,
  11 `language`, 5 `framework`, 2 `platform`, 0 `deprecated`).
- `crates/trusty-mpm/src/assets/framework-manifest.toml` — the framework tier
  of the `manifest.toml` format; its `[agent_categories]` section is the
  declaration §4 describes.
- `crates/trusty-mpm/src/core/manifest/framework.rs` —
  `parse_framework_manifest`, `framework_agent_categories`,
  `agent_scope_from`, `framework_agent_scope`, `bundled_agent_stems`,
  `detected_stack_engineers`, plus the #4765 skill-roster half
  (`parse_framework_skills`, `framework_skill_categories`,
  `bundled_skill_stems`) — the declaration's parser, its validation invariants,
  and the composition with evaluated markers.
- `crates/trusty-mpm/src/core/manifest/schema.rs` — `AgentCategories`,
  `GatedAgent`, `SkillCategories` — the additive sections this work adds to the
  shared manifest schema.
- `crates/trusty-mpm/src/core/manifest/project_lang.rs` — `MarkerProbe`,
  `marker_present` (exact path, `*.<ext>` glob, and the `<path>::<needle>`
  content probe), `file_contains` — the marker EVALUATOR the declared gates are
  tested with. (`LANGUAGE_ENGINEERS` and `PLATFORM_AGENTS`, the marker tables
  this module used to own, moved into `framework-manifest.toml` in #4765;
  `language_agent_scope`, the exclude-by-complement function this document's
  original text described, was removed by #4760.)
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

- 2026-08-08 — Aligns the ticketing/version-control artifact boundary with
  DOC-59 §12 and #5202. Issues, including milestone/state/comment operations,
  belong to `ticketing`; all PR operations, including title and body, belong to
  `version-control`. `tm-workflow` owns lifecycle policy and `tm-ticketing`
  owns issue policy; neither agent invents policy.
- 2026-08-04 — Initial DRAFT. Claims `DOC-65` (scan-before-claim, DOC-38
  §4.1). Catalogs the universal agent roster verified against
  `crates/trusty-mpm/src/assets/agents/`,
  `crates/trusty-mpm/src/core/manifest/project_lang.rs`, and
  `crates/trusty-mpm/src/core/delegation_authority.rs` on `origin/main`
  (`d6e13326`). Maps onto ADR-0025's 2026-08-03 four-category-model
  addendum. Filed for issue #4755, milestone `1.3.5-2`.
- 2026-08-04 — Updated for issue #4760, landing with the code change rather
  than one PR later. "Universal" is now a DECLARATION read from the bundled
  `framework-manifest.toml`, not the mechanical complement of
  `LANGUAGE_ENGINEERS`. §1 restated around the four deployment categories
  (`universal` / `language` / `framework` / `platform`, plus `deprecated`);
  §4 rewritten from "universal vs. stack-specific" to the four categories,
  their composition, the language-vs-framework gating caveat (§4.1), the new
  platform gate (§4.2), the unknown-project fallback (§4.3), and the
  loud-failure contract (§4.4); §5 gained an explicit statement that
  ADR-0025's four-category model is a different axis that shares only a
  number; §6.1 and §6.2 mark the `ops`, `gcp-ops`/`vercel-ops`, and
  ellipsis-boundary findings RESOLVED, and the four undocumented agents
  PARTIALLY resolved (classified, still unrouted), each keeping its original
  evidence.
- 2026-08-04 — Owner rulings of the same day folded in. §4.1 rewritten: the
  `react`/`nextjs`/`svelte`/`phoenix` gates are TIGHTENED to framework
  evidence via a new bounded content-probe marker form, and `elixir-engineer`
  is added so narrowing Phoenix does not leave Elixir uncovered — a deliberate
  behavior change. §6.1's `ops` finding re-resolved: the agent is DELETED from
  the bundle, not kept-but-not-deployed, leaving `deprecated` empty by design.
  Counts: 37 dispatchable agents = 19 + 11 + 5 + 2 + 0, unchanged in total
  (−`ops`, +`elixir-engineer`); `ALL` unchanged at 178 for the same reason.
- 2026-08-04 — Owner ruling of 2026-08-04 ("authoritative agent/skill bundling
  should be in framework-manifest.toml or project manifest.toml. That's it")
  applied, issue #4765. The manifest is now the SINGLE authority for three
  things, not one: which agents and skills are bundled, which category each is
  in, and the marker condition that gates it. `LANGUAGE_ENGINEERS` and
  `PLATFORM_AGENTS` are deleted; each gated entry declares its own `markers`,
  and a gated entry declaring none is a hard error. A new `[skill_categories]`
  section declares the bundled skill roster under the same exhaustiveness
  invariant. §4 preamble, §4.1, §4.2 and §7 updated for the moved tables. The
  bundled documents that used to restate the roster or its gates
  (`assets/skills/tm.md`, `assets/skills/tm-delegation-patterns.md`,
  `assets/instructions/sections/agent-delegation.md`) now POINT at the manifest
  and at the mechanically generated `tm-capabilities/references/agents.md`,
  which gains a **Deploys When** column rendered from the manifest.
