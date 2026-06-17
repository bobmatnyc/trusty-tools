# DOC-17 — trusty-mpm as an Autonomous Multi-Session Managed Harness Runner

**Status:** Draft
**Subsystem:** trusty-mpm — harness runner / provisioning / instruction & agent assembly
**Owner:** Engineering (trusty-mpm)
**Last-updated:** 2026-06-17
**Spec ID:** `SPEC-HARNESS-01~draft` (DOC-17)
**Builds on:** DOC-16 — Interactive Sessions TUI (`docs/specs/sessions-tui-interactive.md`,
the operator surface), DOC-14 — Session Manager (SM) Agent
(`docs/specs/session-manager-agent.md`)
**Cross-ref:** instruction assembly (`crates/trusty-mpm/src/core/session_launch/mod.rs`),
agent composition (`crates/trusty-mpm/src/core/agent_builder.rs`,
`crates/trusty-mpm/src/core/agent_deployer.rs`), catalog sync
(`crates/trusty-mpm/src/content/catalog_sync.rs`), bundled assets
(`crates/trusty-mpm/src/assets/agents/`,
`crates/trusty-mpm/src/assets/output-styles/`), the claude-mpm upstream
(`https://github.com/bobmatnyc/claude-mpm`), and issues **#1045** (metaharness
epic — the runner core) and **#1272** (interactive-TUI epic — the operator
surface, DOC-16).

> **Scope note.** This is a **vision + architecture** spec. It states the
> north-star for trusty-mpm as an autonomous harness runner, audits the current
> implementation against real code, defines the near-term normative requirements
> (**HR-1…HR-4**) the next epic implements against, and carries the full
> claude-mpm ↔ t-mpm parity master checklist. It does **not** implement anything.
> The PR that carries this doc opens **no** Rust changes. It sits **above** both
> the metaharness runner-core epic (#1045) and the interactive-TUI epic (#1272 /
> DOC-16): #1045 builds the runner, #1272 builds the operator surface, and this
> spec frames why the runner must be *autonomous*.

---

## 1. North-star (the guiding principle)

trusty-mpm (t-mpm) is a **TRUE MULTI-SESSION MANAGED HARNESS RUNNER**. It does
not just *launch* Claude Code sessions — it **owns the full lifecycle** of every
harness it runs: provisioning the workspace, assembling the instructions and
agent hierarchy, keeping that content current against an upstream catalog, and
tearing the harness down cleanly.

> ### G0 — The user has to do NOTHING to manage their harness.
>
> Provisioning, instruction/agent assembly, content updates, and lifecycle are
> **autonomous**. The operator declares *intent* ("run a session on repo X for
> ticket #412") and the runner does everything else: it resolves what a harness
> for that project should contain, materializes it, keeps it fresh, and reclaims
> it when done. The operator never hand-edits a `CLAUDE.md`, never copies an
> agent file, never runs a "sync" command, and never reasons about which
> instruction layer wins. **If the user has to manage it, the runner has failed.**

Everything in this spec is a means to G0. The near-term work (§3) closes the gap
between today's partially-autonomous assembly and a fully-autonomous,
manifest-driven, self-updating runner.

---

## 2. Current state (audited against real code)

A content-assembly audit confirmed the following is **already implemented and
correct**, plus the gaps that remain.

### 2.1 Instruction assembly — implemented (PR #1389)

`assemble_system_prompt()` concatenates the PM instruction floor in a fixed
order: `PM_INSTRUCTIONS → WORKFLOW → AGENT_DELEGATION → BASE_PM`. Runtime
overrides are resolved by `resolve_pm_prompt()` from `<project>/.trusty-mpm/`,
and `build_instructions()` merges the framework instructions + the delegation
block + the project `CLAUDE.md`. The entry point is `prepare_session()` in
`crates/trusty-mpm/src/core/session_launch/mod.rs`.

| Concern | Status | Where |
|---|---|---|
| PM-prompt floor (4-layer concat) | **Done** | `assemble_system_prompt()` |
| Project-level override resolution | **Done** | `resolve_pm_prompt()` ← `<project>/.trusty-mpm/` |
| Framework + delegation + `CLAUDE.md` merge | **Done** | `build_instructions()` |
| Session entry point | **Done** | `prepare_session()` (`core/session_launch/mod.rs`) |

### 2.2 Agent BASE hierarchy — implemented and correct

`compose_agent()` (`crates/trusty-mpm/src/core/agent_builder.rs`) walks the
frontmatter `extends:` chain **base-first** (`engineer` → `base-engineer` →
`base-agent`), strips intermediate frontmatter, concatenates bodies base-first,
and emits **one** merged frontmatter — with cycle detection and a depth cap of 8.
`deploy_agents()` (`agent_deployer.rs`) composes every agent into
`~/.claude/agents/` behind a checksum manifest. The base files live in
`src/assets/agents/`: `BASE-AGENT.md`, `BASE-ENGINEER.md`, `BASE-QA.md`,
`BASE-OPS.md`, `BASE-RESEARCH.md`.

| Concern | Status | Where |
|---|---|---|
| `extends:` chain walk (base-first, frontmatter strip, single merged frontmatter) | **Done** | `compose_agent()` |
| Cycle detection + depth cap (8) | **Done** | `compose_agent()` |
| Compose-all → `~/.claude/agents/` + checksum manifest | **Done** | `deploy_agents()` |
| Base files present | **Done** | `src/assets/agents/BASE-*.md` |

### 2.3 Catalog sync — implemented, NOT wired

`CatalogSync` (`crates/trusty-mpm/src/content/catalog_sync.rs`) pulls the
claude-mpm repo (`https://github.com/bobmatnyc/claude-mpm`,
env-configurable via `TRUSTY_MPM_CATALOG_REPO` / `_REF` / `_TTL_HOURS`) into
`~/.trusty-mpm/catalog/repo/` and exposes `list_agents()` / `list_skills()`.

**Gap:** it is **not wired into session launch**, and there is **no
update-detection** — nothing compares the deployed checksum manifest against the
catalog to discover stale content.

### 2.4 Output style — single bundled style

A single bundled style ships at `src/assets/output-styles/trusty-mpm.md` and is
written as `"outputStyle":"trusty-mpm"` into the project's
`.claude/settings.json`.

**Gap:** no multi-style support, and no version-fallback injection for Claude
Code versions that lack native `outputStyle`.

---

## 3. Normative requirements (the near-term work)

These IDs are **authoritative** and are shared verbatim with the implementing
tickets. Each is a behavior contract the runner-core epic (#1045) implements.

### HR-1 — BASE_AGENT / BASE_ENGINEER content parity {#SPEC-HARNESS-01~draft}

**The hierarchy machinery (§2.2) is confirmed working** — this item is about the
*content* the machinery composes, not the composer. Port the **substantive**
content from claude-mpm's base files into t-mpm's lean base files, and add the
two deploy-time enrichments below.

- **Port `BASE_AGENT.md` (~160 lines):** git workflow, memory routing, handoff
  protocol, the self-action imperative, verification-before-completion, and the
  empty-output protocol.
- **Port `BASE_ENGINEER.md` (~400 lines):** code contracts, ship-working-code
  discipline, dependency verification, duplicate elimination, and the
  test-generation strategy.
- **Add `initialPrompt` injection** at deploy time.
- **Add resource-tier → model defaults** at deploy time:
  `intensive → opus`, `high / standard → sonnet`, `lightweight → haiku`.

- **Behavior contract.**
  - **Inputs:** the upstream claude-mpm base files; each agent's frontmatter
    `resource_tier`; the agent set being deployed.
  - **Outputs:** t-mpm base files carrying the ported content; each deployed
    agent gets an injected `initialPrompt` and a `model` default derived from its
    tier when none is explicitly set.
  - **Preconditions:** the `extends:` chain composes (already true, §2.2).
  - **Postconditions:** composed agents in `~/.claude/agents/` carry the
    substantive base content + tier-appropriate model defaults.
  - **Error conditions:** a missing tier → `standard` default (→ sonnet).
- **Effort: S–M.**

### HR-2 — Manifest-driven harness provisioning

t-mpm provisions a harness from a claude-mpm **manifest** (configurable), not
from compile-time-bundled assets alone. Define a **manifest schema** describing
everything a harness gets, source the content via `CatalogSync`, and have
`prepare_session()` consume the resolved manifest.

- **Manifest schema** (what a harness gets): agent set, skills, instruction
  layers, output style, MCP servers, model tiers.
- **Content source:** `CatalogSync` (configurable repo / ref / TTL, §2.3).
- **Consumption:** `prepare_session()` consumes the resolved manifest instead of
  only the compile-time bundled assets.
- **Precedence (NORMATIVE):**
  **project override > user config > manifest > compiled-in default.**

- **Behavior contract.**
  - **Inputs:** the resolved manifest (from the catalog); user config; project
    overrides under `<project>/.trusty-mpm/`; the compiled-in defaults.
  - **Outputs:** a fully-resolved harness definition (agents, skills,
    instruction layers, output style, MCP servers, model tiers) materialized for
    the session.
  - **Preconditions:** a manifest is resolvable (or the compiled-in default
    stands in as the lowest-precedence layer).
  - **Postconditions:** `prepare_session()` provisions from the resolved
    definition; precedence is applied layer-by-layer.
  - **Error conditions:** unreachable catalog → fall back to compiled-in default
    (never block a launch).
- **Effort: M.**

### HR-3 — Update-check + rebuild offer

Each launched session checks the claude-mpm catalog for changed
instructions/agents and **offers** to rebuild/redeploy with the updated content.

- **Detection:** SHA/hash compare of catalog content vs the deployed checksum
  manifest (the one `deploy_agents()` already writes, §2.2).
- **Surfacing:** `GET /health` returns `catalog_stale: true` and the TUI shows a
  staleness indicator (DOC-16 operator surface).
- **Action:** the runner **offers** to rebuild/redeploy sessions with the updated
  content (it does not silently force a rebuild — but the *check* is autonomous).
- **Wiring:** wire `CatalogSync` into daemon start and/or session prep, gated by
  a TTL.

- **Behavior contract.**
  - **Inputs:** the catalog content hashes; the deployed checksum manifest; the
    TTL.
  - **Outputs:** a `catalog_stale` flag on `/health`; a TUI indicator; a rebuild
    offer.
  - **Preconditions:** `CatalogSync` is wired into daemon start and/or prep.
  - **Postconditions:** staleness is surfaced within one TTL window; accepting
    the offer redeploys with current content.
  - **Error conditions:** catalog unreachable → treat as not-stale (never block),
    log the probe failure.
- **Effort: M.**

### HR-4 — MPM output-style support (multi-style + fallback)

Bundle the mpm output styles, make the active style configurable, and inject the
style into the PM system prompt as a **fallback** when the installed Claude Code
version lacks native `outputStyle`.

- **Bundle:** the mpm output styles `professional` / `teaching` / `research`.
- **Configurable:** active style via a config key or `--style`.
- **Fallback injection:** when the installed Claude Code version lacks native
  `outputStyle`, inject the active style into the PM system prompt instead.

- **Behavior contract.**
  - **Inputs:** the bundled styles; the configured/CLI-selected active style; the
    detected Claude Code `outputStyle` capability.
  - **Outputs:** native `"outputStyle":"<style>"` in `.claude/settings.json` when
    supported; otherwise the style text injected into the PM system prompt.
  - **Preconditions:** the selected style exists in the bundle.
  - **Postconditions:** the active style is in effect regardless of Claude Code
    version.
  - **Error conditions:** unknown style name → fall back to `trusty-mpm` default
    + inline notice.
- **Effort: S.**

---

## 4. Post-MVP parity master checklist (claude-mpm ↔ t-mpm)

Full feature parity matrix. **Status** is `done` / `partial` / `none`; **Effort**
is `S` / `M` / `L` (or `SM` for the multi-provider item). Grouped by status. The
**largest gaps** — the web dashboard and Slack integration — are marked **L** and
are explicitly post-MVP (§5).

### 4.1 Done

| Feature | claude-mpm has it | t-mpm status | Effort |
|---|---|---|---|
| BASE-layer hierarchy machinery (`extends:` compose) | yes | **done** | — |
| Agent catalog (bundled agent set) | yes | **done** | — |
| 5-phase workflow | yes | **done** | — |
| Memory routing | yes | **done** | — |
| Coordinator TUI | yes | **done** | — |

### 4.2 Partial

| Feature | claude-mpm has it | t-mpm status | Effort |
|---|---|---|---|
| BASE-layer **content** parity (substantive base text) | yes | **partial** (HR-1) | S–M |
| Skills system (+ recommendation / selective deploy) | yes | **partial** | M |
| Remote agent/skill registry (CatalogSync wiring) | yes | **partial** (HR-2/HR-3) | M |
| Output styles (multi) | yes | **partial** (HR-4) | S |
| Config commands (`tm config`) | yes | **partial** | S |
| PR workflow automation | yes | **partial** | M |
| Multi-provider LLM | yes | **partial** | SM |

### 4.3 None

| Feature | claude-mpm has it | t-mpm status | Effort |
|---|---|---|---|
| `initialPrompt` injection | yes | **none** (HR-1) | S |
| resource_tier / model defaults | yes | **none** (HR-1) | S |
| PM circuit breakers | yes | **none** | M |
| Session pause/resume (daemon-side state serialization) | yes | **none** | M |
| Hooks | yes | **none** | M |
| Doctor / diagnostics | yes | **none** | S |
| Ticketing integration | yes | **none** | M |
| Session analysis / LLM overseer | yes | **none** | M |
| MCP gateway / aggregation | yes | **none** | M |
| Context compaction | yes | **none** | M |
| Agent recommendation engine | yes | **none** | M |
| init / mpm-configure wizard | yes | **none** | M |
| SLD blocks (spec-linked-doc enforcement) | yes | **none** | S |
| GitHub system-prompt injection | yes | **none** | S |
| Budget tracking | yes | **none** | M |
| Workspace provisioner | yes | **none** | M |
| **Web dashboard / monitoring** | yes | **none** | **L** |
| **Slack integration** | yes | **none** | **L** |

---

## 5. Roadmap & relationships

### 5.1 Near-term (this epic)

**HR-1 → HR-2 → HR-3 → HR-4** (§3). Together they make provisioning,
instruction/agent assembly, and content-currency autonomous — the core of G0.
HR-1 lands the substantive base content + deploy-time enrichments; HR-2 makes
provisioning manifest-driven; HR-3 keeps it fresh; HR-4 completes output-style
parity.

### 5.2 Cross-links

- **Metaharness epic #1045** — the **runner core**. This spec's HR-1…HR-4 are the
  near-term content/provisioning work that epic carries.
- **Interactive-TUI epic #1272 / DOC-16** — the **operator surface**. The
  staleness indicator (HR-3) and rebuild offer render here.
- **This vision sits ABOVE both.** #1045 builds the runner, #1272 surfaces it to
  the operator, and DOC-17 frames *why the runner must be autonomous* (G0). Where
  the two epics need a shared decision (e.g. the rebuild-offer UX), this spec is
  the tie-breaker.

### 5.3 Non-goals / MVP-deferred

The **L-effort** parity items are explicitly **post-MVP**:

- **Web dashboard / monitoring** (`trusty-console` / `trusty-mpm-gui` territory).
- **Slack integration.**

These do not gate G0 — an autonomous runner is fully usable through the CLI + the
DOC-16 TUI without them.

---

## 6. Behavior contract (summary)

- **Inputs:** operator intent (repo/ref/task); the resolved harness manifest (via
  `CatalogSync`); user config; project overrides under `<project>/.trusty-mpm/`;
  the compiled-in defaults; catalog content hashes vs the deployed checksum
  manifest; the detected Claude Code `outputStyle` capability.
- **Outputs:** a fully-provisioned harness (agents with substantive base content +
  tier-derived model defaults + `initialPrompt`; instruction layers; output
  style; MCP servers) materialized per session; a `catalog_stale` signal on
  `/health` + TUI; a rebuild/redeploy offer.
- **Preconditions:** none are operator-facing — an unreachable catalog degrades to
  the compiled-in default and never blocks a launch.
- **Postconditions:** the operator manages **nothing** (G0); precedence
  (project > user > manifest > compiled-in) is applied; staleness surfaces within
  one TTL window.
- **Error conditions:** unreachable catalog → compiled-in default + not-stale;
  missing tier → `standard`/sonnet; unknown style → `trusty-mpm` default.

---

## 7. References

- North-star epic: **#1045** (metaharness — runner core).
- Operator-surface epic: **#1272** / **DOC-16**
  (`docs/specs/sessions-tui-interactive.md`).
- Session Manager agent: **DOC-14** (`docs/specs/session-manager-agent.md`).
- Instruction assembly (PR #1389): `crates/trusty-mpm/src/core/session_launch/mod.rs`
  (`assemble_system_prompt`, `resolve_pm_prompt`, `build_instructions`,
  `prepare_session`).
- Agent composition: `crates/trusty-mpm/src/core/agent_builder.rs`
  (`compose_agent`), `crates/trusty-mpm/src/core/agent_deployer.rs`
  (`deploy_agents`).
- Bundled assets: `crates/trusty-mpm/src/assets/agents/BASE-*.md`,
  `crates/trusty-mpm/src/assets/output-styles/trusty-mpm.md`.
- Catalog sync: `crates/trusty-mpm/src/content/catalog_sync.rs`
  (`list_agents`, `list_skills`; env `TRUSTY_MPM_CATALOG_REPO` / `_REF` /
  `_TTL_HOURS`; cache `~/.trusty-mpm/catalog/repo/`).
- Upstream: `https://github.com/bobmatnyc/claude-mpm`
  (`BASE_AGENT.md`, `BASE_ENGINEER.md`, output styles).

---

## 8. Change log

- **2026-06-17** — Initial draft (DOC-17, `SPEC-HARNESS-01~draft`). Vision +
  architecture spec framing trusty-mpm as an autonomous multi-session managed
  harness runner. States guiding principle **G0** (the user manages nothing);
  audits the implemented content-assembly layer against real code (instruction
  assembly PR #1389, the working agent BASE `extends:` machinery, the
  not-yet-wired `CatalogSync`, the single bundled output style); defines the
  near-term normative requirements **HR-1** (BASE content parity + `initialPrompt`
  + tier→model defaults), **HR-2** (manifest-driven provisioning with
  project > user > manifest > compiled-in precedence), **HR-3** (update-check +
  rebuild offer via checksum/hash compare, `catalog_stale` on `/health`), and
  **HR-4** (multi output-style + version fallback); and carries the full
  claude-mpm ↔ t-mpm parity master checklist with the web dashboard and Slack
  integration marked as the L-effort, post-MVP gaps. Sits above the metaharness
  runner-core epic (#1045) and the interactive-TUI epic (#1272 / DOC-16).
