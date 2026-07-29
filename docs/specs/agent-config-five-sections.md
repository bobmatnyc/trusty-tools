---
spec_refs:
  - id: SPEC-AGENTS-04~draft
    path: docs/specs/trusty-agents-product-spec.md
    anchor: SPEC-AGENTS-04~draft
  - id: SPEC-AGENTS-07~draft
    path: docs/specs/trusty-agents-product-spec.md
    anchor: SPEC-AGENTS-07~draft
  - id: SPEC-AGENTSKILLS-01~draft
    path: docs/specs/agent-bundled-skills.md
    anchor: SPEC-AGENTSKILLS-01~draft
---

# DOC-57 — Five-Section Agent Configuration: Personality / Knowledge / Skills / Listeners / Permissions

**Status:** Draft
**Subsystem:** trusty-agents — agent configuration model, capability declaration, permissions surface, GUI config pane
**Owner:** Engineering (trusty-agents) / Bob Matsuoka
**Last-updated:** 2026-07-26
**Spec ID:** `SPEC-AGENTCFG-01~draft` … `SPEC-AGENTCFG-09~draft` (DOC-57)
**Epic:** #3052 (Assistant M1)
**Builds on:** DOC-54 [Trusty Agents Product Specification](./trusty-agents-product-spec.md) §5 (the config triple this spec supersedes) and §8.4 (the in-pane config sections); DOC-41 [Eve-Style Agent Framework](./trusty-agents-eve-style-agents-spec.md) §2.3/§2.6/§5.5 (manifest schema, no-code enforcement, user-authority singleton); DOC-42 [Agent-Bundled Skills](./agent-bundled-skills.md) (the `skills:` declaration + co-deployment model on the trusty-mpm side); DOC-23 [Learned-Autonomy Auto-Answer](./learned-autonomy-auto-answer.md) (the only designed approval/undo/audit model in the repo)
**Cross-ref:** `crates/trusty-agents/src/agents/config.rs` (`AgentConfig`, `ToolsConfig`, `SystemPrompt`); `crates/trusty-agents/src/stores/config.rs` (`[[stores]]`); `crates/trusty-agents/src/listeners/config.rs` (`[[listeners]]`); `crates/trusty-agents/src/agents/extends/mod.rs` (`merge_extends`); `crates/trusty-agents/src/ctrl/pm_task/dispatch/persona.rs` (`filter_persona_tool_names` — the enforcement point); `crates/trusty-agents/src/tools/registry/scope.rs` (`agent_can_use`); `crates/trusty-agents/src/skills/` (the existing skill subsystem); `crates/trusty-agents/ui/src/components/AgentConfigPanel.svelte` (the GUI canvas as reshaped by PR #3895)
**Related issues:** #3878 (stores binding, merged), #3890 (stores PATCH, open), #3891 (listeners pane, open), #3895 (config-pane takeover, merged), #3857 (Slack RBAC parity), #3816 (declarative templates), #3818 (GUI reshape), #3074/#3075 (`user_authority` field, reserved)

---

## 1. Executive Summary

The owner redefined the agent-configuration model on 2026-07-25:

> *"Instead of tools, let's show skills. Should be Personality, Knowledge (list knowledge tools including and MCP connections to knowledge stores), Skills (each tool should be wrapped in a skill), Listeners, the Permissions."*

This supersedes DOC-54 §5's **config triple** (Stores / Tools / Listeners) with a
**five-section model**: **Personality**, **Knowledge**, **Skills**, **Listeners**,
**Permissions**. The change is not additive UI work — it is a semantic
redefinition of what an agent's configuration *is*:

| Old (DOC-54 §5) | New (this spec) | Nature of the change |
|---|---|---|
| — | **Personality** | Promoted from "the persona file" to a named configuration section. |
| **Stores** (`[[stores]]`) | **Knowledge** | *Widened.* Stores are now one of three sub-surfaces; Knowledge = store bindings **+** knowledge-classified tools **+** MCP connections to knowledge services. |
| **Tools** (`[tools].allow`) | **Skills** | *Replaced.* The skill becomes the visible, configurable unit of capability; raw tool names become an implementation detail beneath it. |
| **Listeners** (`[[listeners]]`) | **Listeners** | Unchanged in substance; the pane is still scaffolded (#3891). |
| — | **Permissions** | *Promoted.* Scopes, tiers, authority flags and autonomy posture become a first-class declared section instead of four unrelated mechanisms scattered across two config files and Rust code. |

**Two honest corrections to the framing, established by reading the tree at
`385aeff7`, that shape this spec:**

1. **The GUI already renders five tabs.** `AgentConfigPanel.svelte:284-285` ships
   `personality` / `okg` / `tools` / `permissions` / `listeners` today. The work
   is therefore *not* "add sections" — it is relabelling and re-backing two of
   them (Tools→Skills, OKG Stores→Knowledge), reordering, and turning
   Permissions from a read-only chip list into a real model. See §8.
2. **trusty-agents already has a substantial skill subsystem** — it is simply not
   a capability wrapper. `crates/trusty-agents/src/skills/` provides
   `SkillEntry`, `SkillRegistry`, priority-ordered sources, and the
   `load_skill`/`list_skills`/`search_skills` tools; agents declare
   `[system_prompt].skills`. But a skill today is **prompt text only**: it has no
   binding to the tools it describes. `.trusty-agents/skills/web-search.md`
   documents the `web_search` tool while `web_search` is granted *separately* in
   `[tools].allow` — two declarations, no link, either one silently wrong. §5
   closes exactly that gap. See §5.1 for the honest gap statement.

The central design commitment that makes this shippable is **§5.4: skills compile
down to the existing tool allow-list.** No enforcement path changes. Every
existing `agent.toml` keeps working byte-for-byte (§9).

---

## 2. SPEC-AGENTCFG-01 — The Five Sections {#SPEC-AGENTCFG-01~draft}

### 2.1 Definition (NORMATIVE)

An agent's configuration consists of exactly five sections. Each answers one
question about the agent, and each maps to exactly one GUI pane (§8) and one
config surface (§2.2).

| # | Section | Answers | Primary config surface |
|---|---|---|---|
| 1 | **Personality** | Who the agent *is* | `persona.md` + `[agent]` identity keys |
| 2 | **Knowledge** | What the agent *knows* | `[[stores]]` + knowledge-classified skills + MCP knowledge endpoints |
| 3 | **Skills** | What the agent *can do* | `[skills]` (NEW) — resolving to tools |
| 4 | **Listeners** | What the agent *reacts to* | `[[listeners]]` |
| 5 | **Permissions** | What the agent is *allowed* to do, and on whose authority | `[permissions]` (NEW) |

**Ordering is normative.** The five sections appear in the order above in every
surface that enumerates them — GUI tabs, API responses, CLI output, and
documentation. This is the owner's stated order and it is a meaningful
progression: identity → knowledge → capability → reactivity → constraint. It is
*not* today's GUI order (which places Permissions before Listeners); §8.2
specifies the reorder.

### 2.2 Section-to-surface map (NORMATIVE)

| Section | `agent.toml` | Other files | Harness config (`~/.trusty-agents/config.toml`) |
|---|---|---|---|
| Personality | `[agent]`, `[llm]`, `[system_prompt]` (aux keys) | `persona.md`, `events/<connector>.md` | — |
| Knowledge | `[[stores]]` | — | `[[tool_registry.endpoints]]` for knowledge services |
| Skills | `[skills]` (NEW), `[tools]` (legacy) | `.trusty-agents/skills/<name>.md` | `[[mcp.services]]`, `[[tool_registry.endpoints]]` |
| Listeners | `[[listeners]]` | `events/<connector>.md` | `[[listeners]]` |
| Permissions | `[permissions]` (NEW), `[tools].scopes` + `[rbac]` (legacy) | — | `[tool_registry] scope_enforcement` |

A section is a **view over** these surfaces, not a new storage location. Nothing
in this spec moves data between files; §9 guarantees every listed legacy key
keeps parsing.

### 2.3 Per-section `extends` merge rule (NORMATIVE)

`merge_extends` (`crates/trusty-agents/src/agents/extends/mod.rs:183`) already
applies a *different* merge rule per key, and the difference is load-bearing —
`[[stores]]` REPLACES specifically so a child does not inherit the base's default
`vector_search` target (`extends/mod.rs:303-312`). Making the five sections
first-class requires stating the rule per section, because "which rule applies"
is currently discoverable only by reading the merge function.

| Section | Merge rule | Status |
|---|---|---|
| Personality | prose **concatenated** base-first (`"\n\n"`); scalars child-wins-when-set; `[llm]` per-key for `temperature`/`max_tokens` only | existing (`concat_prose`, `extends/mod.rs:449`) |
| Knowledge | `[[stores]]` — child **REPLACES** when non-empty | existing (`extends/mod.rs:312`) |
| Skills | **UNION**, deduped, base-first | existing for `tools.allow`/`tools.allowed`/`system_prompt.skills` (`union_opt_vec`, `extends/mod.rs:355`); `[skills].allow` adopts the same rule |
| Listeners | **UNION keyed by `name`**; child replaces a same-named binding | existing (`union_listener_bindings`, `extends/mod.rs:385`) |
| Permissions | `scopes` **UNION** (existing); `grants` union-by-`skill`; `user_authority` **NEVER inherited** | union existing; non-inheritance reserved at `extends/mod.rs:252-262` per DOC-41 §5.5 |

> **Flagged for the owner (OQ-4, §12).** Permissions merging by UNION means a
> child agent can only ever *gain* scopes — it can never narrow what it inherits.
> That is the opposite polarity from Knowledge (REPLACE) and is a
> privilege-escalation-shaped default. Changing it is a behavior change to
> existing agents, so this spec records it rather than silently flipping it.

### 2.4 Conformance

- **C-01.1** Every surface that enumerates sections emits them in §2.1 order.
- **C-01.2** For every agent resolvable by `AgentConfig::by_name_in`, each of the
  five sections is computable — a section with no declared data resolves to a
  well-formed empty section, never an error and never an omitted key.
- **C-01.3** A per-section merge rule change is a spec change: a test asserts the
  §2.3 table against `merge_extends` behavior for each section.

---

## 3. SPEC-AGENTCFG-02 — Personality {#SPEC-AGENTCFG-02~draft}

### 3.1 Composition

Personality is the agent's identity and voice. It is the one section that
already exists end-to-end (config, API, editable GUI pane) and this spec changes
nothing about its substance.

| Element | Source | Today's status |
|---|---|---|
| Main instructions | `persona.md` (directory package) or `[system_prompt].content` (flat) | LIVE, editable — `GET/PATCH /api/agents/:name/persona`, `PATCH /api/agents/:name {personality}` |
| Identity | `[agent].name` / `display_name` / `description` / `prompt_label` / `role` / `kind` / `hidden` | LIVE, read-only in GUI |
| Model + inference | `[agent].model`, `[agent].runner`, `[llm]` | LIVE; `model_id`/`provider_id` writable via PATCH |
| Per-connector instructions | `agents/<name>/events/<connector>.md` (DOC-54 §6, #3817) | Loaded at wake (`listeners/wake.rs:317`); **GUI shows a hardcoded placeholder** (`AgentConfigPanel.svelte:354-363`) |

### 3.2 Normative statements

- **P-1** A directory-package agent MUST carry `persona.md`; `agent.toml` MUST NOT
  also define `[system_prompt].content` (hard-rejected at
  `agents/loader.rs:578-583`). This spec does not relax that.
- **P-2** The Personality pane's per-connector-instructions block MUST be backed
  by the real `events/*.md` file set, not by the `DEFINED_LISTENERS` literal it
  renders today. Absent files render as "not configured"; a fabricated list is
  forbidden (§8.4).
- **P-3** Personality is the only section with an existing write path. The other
  four sections' write contracts are specified in §4.5, §5.7, §6.3 and §7.5, and
  each ships read-only first (§10).

### 3.3 Conformance

- **C-02.1** Editing persona text in the GUI round-trips through
  `PATCH /api/agents/:name` and is observable in `GET /api/agents/:name/persona`.
- **C-02.2** The per-connector block lists exactly the `events/*.md` files present
  on disk for that agent — no more, no fewer.

---

## 4. SPEC-AGENTCFG-03 — Knowledge {#SPEC-AGENTCFG-03~draft}

### 4.1 Composition (NORMATIVE)

Knowledge is **one section listing everything the agent knows**. It subsumes
DOC-54 §5.1's Stores leg and widens it to three sub-surfaces rendered as one
pane:

| Sub-surface | Source | Today |
|---|---|---|
| **K-a — Store bindings** | `agent.toml` `[[stores]]` | LIVE (#3878). `GET /api/agents/:name/stores` → `StoreStatus` per binding with `connected`, `chunk_count`, `index_status`, `palace_connected`, `reason` |
| **K-b — Knowledge tools** | The agent's effective tool set, filtered to skills classified `kind = "knowledge"` (§5.3) | NEW. Derivable today only by eyeballing `[tools].allow` |
| **K-c — MCP knowledge connections** | Harness `[[tool_registry.endpoints]]` / `[[mcp.services]]` that back the K-b tools | NEW as a surface; the config exists |

The three are one pane because they answer one question. A user asking "what does
this agent know?" must not have to correlate a store card, a tool glob, and a
harness endpoint table across three files.

### 4.2 K-a — Store bindings

Unchanged wire format. `AgentStoreBinding { name, tree, index, palace }`
(`stores/config.rs:49`), both the array-of-tables spelling and the
`[stores] allow = [...]` shorthand (`stores/config.rs:208-235`). Exactly one
store per agent is the norm (DOC-54 §5.1); `validate()` warns rather than fails
on more than one (`stores/config.rs:181`).

**The posture that governs the whole section is inherited from #3878: declarative
data only, degrade never fail.** `resolve_store_statuses` never errors; malformed
TOML degrades to `config_error` rather than a 500. §4.4 and §4.5 hold K-b and
K-c to the same rule.

### 4.3 K-b — Knowledge tools

A tool is a *knowledge* tool iff the skill wrapping it (§5) declares
`kind = "knowledge"`. This is the only classification mechanism; there is no
hardcoded tool-name list, because a hardcoded list would drift exactly the way
the removed static `gworkspace` MCP tool list drifted
(`assets/config/default-config.toml:22-41`).

Tools that would carry `kind = "knowledge"` under §5.6's seeded manifests, from
the live registry: `vector_search`, `memory_recall`, `memory_search`,
`search_memory`, `retrieve_memory`, `store_memory`, `list_memory_keys`,
`okg_sources`, `okg_ingest_docstore`, `okg_ingest_drive`, `okg_ingest_gmail`,
`search_code`, `search_skills`. This list is **illustrative, not normative** —
the manifests are the authority.

`vector_search` is the seam that ties K-b back to K-a: its default index is the
agent's own store (`persona.rs:346-355`, the #3864 fix). The Knowledge pane MUST
show that linkage — a knowledge tool bound to a specific store binding renders
under it, not as a free-floating chip.

### 4.4 K-c — MCP knowledge connections

The knowledge services reachable over MCP/OpenRPC, with live status:

| Endpoint | Config | Declared scopes | Status on main |
|---|---|---|---|
| `trusty-memory` | `[[tool_registry.endpoints]]`, `driver = "direct"` | `["memory.read", "memory.write"]` | **`enabled = false`** — awaiting `--rpc` mode on the binary |
| `trusty-search` | `[[tool_registry.endpoints]]`, `driver = "direct"` | `["search.read"]` | **`enabled = false`** — same reason |
| `gworkspace` | `[[tool_registry.endpoints]]` | `google.*` families | `enabled = true`, self-discovering via `rpc.discover` |

> **Honest gap.** The two endpoints most obviously "MCP connections to knowledge
> stores" are disabled by default in `assets/config/default-config.toml:193-221`.
> The agent's memory and search capability today flows through *in-process*
> tools, not through those endpoints. The Knowledge pane MUST report this
> truthfully — an endpoint that is configured-but-disabled renders as
> **DISABLED** with its reason, never as connected, and never hidden. Rendering a
> disabled endpoint as absent is the same class of defect as #3891's fabricated
> listener pane.

### 4.5 Backend contract (NEW)

`GET /api/agents/:name/knowledge` returns all three sub-surfaces in one
response, so the pane makes one call and cannot render a half-correlated view:

```jsonc
{
  "stores":   [ /* StoreStatus, unchanged shape from /stores */ ],
  "tools":    [ { "tool": "vector_search", "skill": "knowledge-search",
                  "bound_store": "cto-assistant-kb", "available": true,
                  "reason": null } ],
  "mcp":      [ { "name": "trusty-memory", "kind": "openrpc",
                  "enabled": false, "connected": false,
                  "scopes": ["memory.read", "memory.write"],
                  "reason": "endpoint disabled in config.toml" } ],
  "issues":   [],
  "config_error": null
}
```

- **K-1** The route MUST NOT fail on a degraded sub-surface. Each of `stores`,
  `tools`, `mcp` is independently resolved; a probe timeout populates `reason`
  and leaves the other two intact. Mirrors `agent_stores.rs`'s two-phase load.
- **K-2** `GET /api/agents/:name/stores` is retained unchanged. `/knowledge`
  is additive; #3878's consumers keep working.
- **K-3** Write access to store bindings is **out of scope here** and remains
  #3890's contract (route shape, editable fields, and validation are undecided
  there). The Knowledge pane ships read-only (§10, Phase 1) and gains editing
  when #3890 lands.

### 4.6 Conformance

- **C-03.1** An agent with `[[stores]]` bound to a nonexistent index renders
  NOT CONNECTED with a reason in `/knowledge`, identically to `/stores`.
- **C-03.2** A disabled knowledge endpoint appears in `mcp[]` with
  `enabled: false` and a non-null `reason`; it is never omitted.
- **C-03.3** Every tool in `tools[]` names a skill that exists in the agent's
  resolved skill set (§5.5) — no orphans.
- **C-03.4** Killing the search daemon degrades `stores[].connected` to `false`
  without changing the HTTP status code.

---

## 5. SPEC-AGENTCFG-04 — Skills {#SPEC-AGENTCFG-04~draft}

> *"Instead of tools, let's show skills… each tool should be wrapped in a skill."*

### 5.1 The gap, stated honestly

trusty-agents has a real skill subsystem — and it is the wrong shape for this
directive. Both halves of that sentence matter.

**What exists** (`crates/trusty-agents/src/skills/`): `SkillEntry { name,
description, tags, path }` (`skills/types.rs:25-29`); a priority-ordered source
registry (`skills/sources/mod.rs`, `.trusty-agents/skill-sources.toml`) with
local and remote-git sources; a frontmatter scanner
(`skills/registry/scan.rs:188-212`); LLM-facing `load_skill` / `list_skills` /
`search_skills` tools; per-agent declaration `[system_prompt].skills`
(`agents/config.rs:566`), union-merged across `extends`; memory seeding
(`init/seed/skills.rs`); and an effectiveness-rating path (`skills/rating.rs`).

**What does not exist: any binding from a skill to the tools it describes.** A
skill today is *prompt text injected as a `# Skill: <name>` layer*
(`runtime/subagent_mode.rs:276-288`). The canonical demonstration is
`.trusty-agents/skills/web-search.md` — it documents the `web_search` tool, its
Tavily/Brave backends and its API keys, while `web_search` itself is granted
independently in `[tools].allow`. Delete the skill and the tool still works
unexplained; delete the tool from `allow` and the skill still promises a
capability the agent does not have. Two declarations, no link, either one
silently wrong.

Three further divergences a wrapping model has to survive:

1. **Two incompatible frontmatter dialects.** trusty-agents requires `name` +
   **non-empty `tags`** and skips a skill lacking tags (`scan.rs:204-206`);
   trusty-mpm's 51 bundled skills use `name`/`description`/`version`/
   `user-invocable`/`category`/`effort`/`tags` and only 24 carry `tags`. A
   trusty-mpm skill dropped into a trusty-agents source dir is **silently
   skipped**.
2. **Two on-disk shapes.** Flat `<name>.md` (trusty-agents) vs
   `<name>/SKILL.md` + `references/` (trusty-mpm/DOC-51). The
   `FsSkillResolver` reads both; the registry scanner reads only the flat form.
3. **No shared code.** trusty-agents does not consume
   `trusty_agents_common::skills` at all — no tiers, no `Shadow`, no checksum
   ownership ledger, no DOC-42 co-deployment.

This spec defines the wrapping model in the **trusty-agents dialect only**, and
records dialect convergence as an owner decision (OQ-3, §12).

### 5.2 The wrapping model (NORMATIVE)

**A skill is the unit of capability. A tool is an implementation detail beneath a
skill.** Granularity is **one skill per tool** (OQ-2, resolved 2026-07-25).

A skill manifest gains one new frontmatter key, `tools:`, plus two classifiers:

```yaml
---
name: Gmail Search                 # HUMAN name — never the tool identifier
description: Find messages in Gmail using Gmail query syntax.
tags: [gmail, email, gworkspace]
kind: action                       # NEW — action | knowledge | system
tools: [search_gmail_messages]     # NEW — the ONE tool this skill wraps
scopes: [google.gmail.*]           # NEW, optional — scopes that tool requires
---
```

| Key | Required | Meaning |
|---|---|---|
| `name` | yes | Existing. Skill identity; the allow-list unit (§5.4). |
| `description` | yes | Existing. Rendered as the skill card's subtitle. |
| `tags` | yes | Existing, and **already required** by `scan.rs:204-206`. |
| `kind` | no, default `action` | `action` \| `knowledge` \| `system` \| `function`. `knowledge` routes the skill's tools into the Knowledge pane (§4.3) **instead of** the Skills pane. `function` is the **bundling tier** (§5.7b, #4022): a group header naming member skill ids rather than a tool. `function` is **not authorable** — the frontmatter dialect has no `members` key and this parser requires `tools`, so a `kind: function` frontmatter value is read as `action` (S-14). |
| `tools` | no, default `[]` | The exact tool name this skill wraps — **at most one** (OQ-2). A skill with no `tools` is a **tool-less skill**: pure procedure or guidance with no executable member, a first-class shape and today's behavior, still valid. A manifest listing several tools is split into one skill per tool at load time rather than being rejected. |
| `scopes` | no | Scopes the wrapped tools require. Advisory in Phase 2, enforcement input in Phase 4 (§10). |

- **S-1** `tools` entries are **exact tool names, never globs.** The three glob
  dialects already in the tree (`[tools].allow` trailing-`*` only,
  `helpers.rs:146`; listener `from` leading-or-trailing, `wake.rs:62`; scope
  patterns trailing `.*` on a segment boundary, `scope.rs:68`) are a standing
  source of confusion. The skill layer adds no fourth dialect.
- **S-2** A tool is wrapped by **exactly one** skill (OQ-2). Uniqueness is
  enforced structurally at catalog-build time — a second manifest claiming an
  already-claimed tool does not create a second card; an authored manifest
  REPLACES the built-in one for that tool (S-9).
- **S-2b** A skill's `name` MUST be human and provider-recognisable, describing
  what invoking it accomplishes — *"MTA Train Time"*, not `get_train_schedule`.
  A name that echoes the tool identifier is a conformance failure (C-04.7).
- **S-3** A skill naming a tool that does not exist in the registry is **not** an
  error. It resolves to an unavailable capability, reported with a reason —
  matching how a missing store degrades rather than blocking boot
  (`stores/status.rs`). A hard failure here would make every skill authored
  against an optional MCP server un-loadable.
- **S-4** Skills carry **no executable payload.** See §5.8.

### 5.3 Skill kinds and pane routing

| `kind` | Renders in | Examples |
|---|---|---|
| `knowledge` | Knowledge pane (§4.3) | `knowledge-search`, `memory-recall`, `okg-ingest-gmail` |
| `action` | Skills pane | `gmail-search`, `gcal-events`, `git-status`, `ticket-create` |
| `system` | Skills pane, collapsed under "System" | `delegate-specialist`, `tmux-session-new` |

`kind` is the *only* mechanism routing capability between the Knowledge and
Skills panes. Both panes read one resolved skill set; neither hardcodes tool
names.

### 5.4 Allow-listing moves from tool names to skill names

New `agent.toml` section:

```toml
[skills]
allow = ["gmail-search", "gmail-compose", "gcal-events", "knowledge-search",
         "memory-recall", "git-status", "handoff-protocol"]
```

Entries are skill **ids** — stable, hyphenated, one per wrapped tool (plus the
tool-less ones, `handoff-protocol` above).

**Resolution (NORMATIVE):**

1. Resolve `[skills].allow` against the skill source registry
   (`skills/sources/mod.rs`, priority-descending, first-writer-wins).
2. Expand each resolved skill to its `tools` list.
3. `effective_tools = union(expanded_skill_tools, [tools].allow)` — see §9.3 for
   why this is a union and not a replacement.
4. Feed `effective_tools` into the **existing** gate:
   `filter_persona_tool_names(all_names, effective_tools, allowed_by_tier,
   tool_scopes, agent_scope_patterns)` (`persona.rs:65`).

**Step 4 is the whole compatibility story.** The skill layer is a *resolver in
front of* the enforcement path, not a change to it. `dispatch_gated`
(`tools/mod.rs:146`), the RBAC tier filter, the scope gate
(`agent_can_use`, `scope.rs:125`) and `persona_allowed_tools`'s deliberate
never-`None` behavior (`persona.rs:119-139`) are all untouched. A bug in skill
resolution can therefore narrow an agent's capability but can never widen it past
what the existing three gates permit.

- **S-5** Because expansion produces **exact names** and the existing gate takes
  glob patterns, an exact name is a degenerate glob under `match_any_glob`
  (`helpers.rs:146` — no trailing `*` ⇒ equality). No matcher change is needed.
- **S-6** The `[skills].allow`-absent case MUST be indistinguishable from today:
  `effective_tools == [tools].allow`, verbatim.

### 5.5 The built-in catalog, and derived skills for what it cannot see

The owner's directive is that *every* tool is wrapped, 1:1. Authoring ~170
Markdown files before the Skills pane can render a single honest card would
block the GUI slice on a content project. Two mechanisms close that gap without
grouping anything — the 1:1 base tier described here introduces **no** grouping
of any kind. (A separate, explicitly-declared **bundling tier** was added later
by owner directive; it sits *above* this tier and changes nothing in S-7…S-10.
See §5.7b.)

- **S-7** The crate ships a **compile-time catalog** — one named row per tool,
  covering every in-process tool trusty-agents registers plus the first-party
  `trusty-gworkspace` surface and the platform tools the checked-in roster
  grants. Rows are `&'static str` data, not files, so a tool gets a real human
  name at zero runtime cost.
- **S-8** A tool that no built-in or authored manifest claims — a live-discovered
  MCP/OpenRPC tool, which by definition is not knowable at compile time — is
  wrapped in a **derived skill**, 1:1, whose display name is the tool identifier
  title-cased. A derived skill carries **no description and no provider**:
  inventing plausible prose nobody wrote is the fabrication G-4 forbids. There is
  no prefix grouping anywhere in this model.
- **S-9** Authored always beats built-in, and built-in always beats derived. An
  authored manifest naming an already-wrapped tool REPLACES that skill rather
  than adding a second one (S-2).
- **S-10** Derived skills are marked `"origin": {"kind": "derived"}` in API
  responses. The GUI badges them (§8.3) so an unnamed surface is *visible* rather
  than indistinguishable from curated capability. Wrapping progress is thereby
  measurable, and the fix for a badge is one catalog row.

This makes the Skills pane complete on day one and turns naming into
incremental, independently-shippable content work.

### 5.6 Seeded manifests

Phase 2 (§10) ships the built-in catalog described in S-7 — one named skill per
tool across filesystem, shell, git, AST, static analysis, timers, delegation and
workflow, session/tmux control, ticketing, MCP administration, memory and
semantic search, OKG ingestion, the Google Workspace surface, web search, and the
platform-hosted personal tools (weather, MTA). It also ships a small set of
**tool-less** skills, since OQ-2 admits them explicitly.

`.trusty-agents/skills/web-search.md` is the migration exemplar — an existing
prose skill that gains `tools: [web_search]` and thereby stops being a promise
the config does not keep (§5.1).

### 5.7 Backend contract (NEW)

`GET /api/agents/:name/skills`:

```jsonc
{
  "skills": [
    { "id": "gmail-search", "name": "Gmail Search",
      "description": "Find messages in Gmail using Gmail query syntax.",
      "kind": "action",
      "origin": { "kind": "builtin" },      // builtin | authored{path} | derived
      "granted": true,
      "tools": ["search_gmail_messages"],   // exactly 0 or 1 (OQ-2)
      "provider": { "provider": "Google Workspace",
                    "requirement": "An authorized Google account profile…",
                    "env_var": null,
                    "configured": null },  // tri-state; null = NOT verified
      "members": [],                        // NEW (§5.7b) — [] on a leaf
      "granted_members": [],                // NEW (§5.7b) — [] on a leaf
      "granted_state": "all" },             // NEW (§5.7b) — restates `granted`
    { "id": "ticketing", "name": "Ticketing",
      "description": "Everything needed to work a tracker end to end…",
      "kind": "function",                   // NEW — the bundling tier
      "origin": { "kind": "builtin" },
      "granted": false,                     // === (granted_state == "all")
      "tools": [],                          // a bundle wraps no tool
      "provider": null,
      "members": ["ticket-create", "ticket-read", "…"],
      "granted_members": ["ticket-create"],
      "granted_state": "some" }             // all | some | none of the members
  ],
  "granted_count": 1,                       // capabilities; EXCLUDES bundles
  "groups": [ { "id": "ticketing", "name": "Ticketing", "description": "…",
                "members": ["ticket-create", "…"],
                "granted_members": ["ticket-create"],
                "granted_state": "some" } ],  // NEW (§5.7b) — index over the
                                              // kind:"function" cards
  "unresolved": [ { "id": "typo-skill", "reason": "no skill with this id…" } ],
  "unmatched_patterns": [ { "pattern": "granola_*", "reason": "…may still resolve
                            to an MCP tool discovered at dispatch time" } ],
  "declares_capability": true,
  "config_error": null
}
```

Every catalog skill is returned with a `granted` flag rather than only the
granted subset, so the Phase-3 editor renders the full choice from one route.

- **S-11** `unresolved[]` reports `[skills].allow` entries that resolved to
  nothing — a dangling reference is *surfaced*, never silently dropped. This is
  the trusty-agents analogue of DOC-42 §SPEC-AGENTSKILLS-03's doctor check.
  `unmatched_patterns[]` is its mirror for `[tools].allow`: a pattern no catalog
  skill matches is reported **with the honest reason** that it may still resolve
  to a live-discovered MCP tool, not as a broken grant.
- **S-11b** A credential is reported as configured only from evidence. `provider
  .configured` is `true`/`false` only when an environment variable backs it and
  the sidecar read it; it is `null` for an OAuth grant or MCP wiring the route
  does not verify, and `null` MUST render as "not verified", never as a check.
- **S-12** Write contract: `PATCH /api/agents/:name { skills_allow: [...] }`,
  mirroring the existing `tools_allow` field on `PatchAgentRequest`
  (`agent_patch.rs:128-148`), including its empty-array-clears semantics. Ships
  in Phase 3 (§10); Phase 1 is read-only.

### 5.7b Function skills — the bundling tier (NORMATIVE)

**Added 2026-07-26 by owner directive, one day after OQ-2 resolved the base tier
at 1:1.** The directive: *"Skills also need to be organized by function. So
'ticketing' is a skill with various capabilities including all the skills
[under] one."* Epic #4021's OQ-1 was resolved **grant-the-bundle** (owner,
2026-07-26): naming a bundle in `[skills].allow` grants every member. Implemented
by #4022 (primitive), #4023 (the `ticketing` exemplar) and #4025 (the route).

This tier sits **above** the 1:1 base and supersedes nothing. Every tool still
maps to exactly one leaf skill (OQ-2 stands); every leaf remains independently
grantable; S-1…S-12 are unchanged. It is also **not** the prefix grouping S-8
rejects — membership is an explicit, closed list, never a `ticket-*` scan, so a
future `ticket-`-prefixed skill is not silently absorbed into an existing grant.

- **S-13 A bundle resolves to its members, and compiles down before the gates.**
  A `kind: function` skill declares `members: [skill_id]` and wraps **no** tool
  (`tools` and `members` are mutually exclusive). `SkillCatalog::expand` replaces
  a bundle id with the union of its members' tools *before returning*, so the 1:1
  layer and all three permission gates (allow-glob, RBAC tier, scope) see **only
  exact leaf tool names — never a bundle id**. Every S-1…S-6 property therefore
  holds for a bundle by construction rather than through a second resolution
  rule: no new glob dialect, narrow-never-widen, and a bundle can only ever reach
  tools its members already wrap. Member resolution is memoised, making expansion
  linear in the catalog and cycle-safe.
- **S-14 Bundles are built-in only; a bundle id may not be redefined by a skill
  source.** Member lists are compile-time `const` data. The authored `.md`
  dialect has no `members` key and its parser requires `tools`, so no authored
  file can *declare* a bundle. Nor may one *displace* a built-in bundle by id:
  S-9's authored-beats-built-in is a **renaming** rule, and applied to a bundle id
  it becomes a hijack (`ticketing.md` with `tools: [execute_shell_command]` would
  turn `[skills].allow = ["ticketing"]` into a shell grant with nothing in
  `agent.toml` for a reviewer to see, and no `unresolved` entry). A bundle id is
  the one name whose meaning cannot be checked by reading the config, so it is the
  one name a skill source may not redefine; the attempt is refused and logged. A
  bundle's **members** are ordinary leaves and remain displaceable under S-9 —
  that path narrows the bundle and reports the lost member in `unresolved[]`.
- **S-15 An unknown member is a manifest error at build time, and narrows at
  runtime.** A bundle naming a member no manifest resolves fails a catalog
  validation check (`unknown_function_members`, asserted in debug builds and
  gated in CI) so a renamed leaf fails the build rather than quietly shrinking an
  agent's capability. If one reaches runtime regardless, expansion still narrows:
  that member contributes zero tools and is reported in `unresolved[]`, never
  "all tools". Partial resolution is reported, never dropped — revoking nine
  working capabilities over one typo is the worse failure.
- **S-16 The route is additive, and tri-state.** `GET /api/agents/:name/skills`
  gains no new route. Every field a pre-#4022 consumer reads keeps its name, type
  and meaning. Every card — leaf or bundle — carries `members`,
  `granted_members` and `granted_state` (`all` | `some` | `none` of the members),
  so a consumer needs no `kind` check before reading a field; on a leaf the first
  two are `[]` and the third restates `granted`. `groups[]` indexes the
  `kind: "function"` cards for the pane and MUST be built from the same
  computation as those cards, so a group and its card can never report different
  grant states. For a bundle, `granted` is exactly `granted_state == "all"` — the
  conservative reading an existing consumer gets for free. `granted_count` counts
  **capabilities** and therefore EXCLUDES bundles; the Skills pane's own counts
  MUST match it, since a header counted as an eleventh capability would print
  "11 of N" over ten rendered cards.

Rendering bundles as group headers in the Skills pane is #4024; until it lands a
`function` card is excluded from the pane's buckets rather than mis-filed.

### 5.8 Executable payload in skills (AMENDED 2026-07-27)

Agent skills may carry executable code — Python packages, binaries, or other implementations discovered and loaded by the skill plugin system (e.g., `install_discovered_skill_plugins`, `runtime/startup.rs:153`). The responsibility for vetting, auditing, and managing that code lies with the operator and agent owner.

**Historical context:** An earlier version of this spec proposed a platform-enforced ban on executable files in skill packages, grounded in DOC-41 §2.6's allowlist. That rule was never implemented — `load_agent_package` performs no file validation and `PackageContainsForeignFile` has zero occurrences in the codebase. Executable skill code is already in production: the `cto-db` skill bundles a full Python package. This amendment ratifies existing behavior and clarifies that the operator, not the platform, manages code provenance.

- **S-13** A skill MAY bundle executable code (scripts, binaries, or language-specific packages). The owner/operator is responsible for vetting its source, security, and compatibility.
- **S-14** Two sanctioned paths exist for "a skill that runs code":
  1. **Bundled implementation** — a skill package may include executable assets directly, discovered and loaded by the platform's plugin system at startup. The operator controls deployment and bears responsibility for auditing the code.
  2. **MCP server** — `McpService.command` in `~/.trusty-agents/config.toml`, operator-controlled platform infrastructure. A skill wraps such a server's tools exactly as it wraps any other tool.

**Future enforcement (planned):** A security review is underway to establish review-gated skill code loading. Epic #4128 will add a `build_plugin`-level check (#4137) that refuses to load executable skill code unless a recorded review verdict exists, via a canonical package hash (#4135) and fail-closed verdict store (#4136). This amendment describes the current permissive state; §5.8 will be revisited once the review gate lands to make the gating requirement normative.

### 5.9 Conformance

- **C-04.1** With no `[skills]` section, an agent's effective tool set is
  byte-identical to today's (`assert_eq!` against the pre-change resolution).
- **C-04.2** An agent declaring `[skills] allow = ["gmail"]` and no
  `[tools].allow` receives exactly the tools listed in the `gmail` manifest, and
  the dispatch gate rejects any tool outside that set.
- **C-04.3** Every registered tool maps to exactly one **authored, built-in or
  derived** skill — the pane has no gaps. A newly added tool with no catalog row
  FAILS this test; that failure is the mechanism, not a nuisance.
- **C-04.4** A skill naming a nonexistent tool renders as ungranted with a
  reason; the agent still boots (S-3).
- **C-04.5** A directory package may contain executable files; the operator is
  responsible for vetting and managing such code (§5.8, S-13, 2026-07-27).
- **C-04.6** An authored manifest naming an already-wrapped tool REPLACES the
  built-in skill rather than adding a second card for that tool (S-2, S-9).
- **C-04.7** No skill's `name` equals or machine-echoes its tool identifier
  (S-2b).
- **C-04.8** An unknown `[skills].allow` id expands to **zero** tools and is
  reported in `unresolved[]` — never to "all tools", and never to `None`
  (which downstream means *unrestricted*). Skill resolution can narrow an
  agent's capability; it can never widen it.
- **C-04.9** A manifest wrapping zero tools resolves successfully, grants no
  tools, and is NOT reported as unresolved (tool-less skills, OQ-2).

---

## 6. SPEC-AGENTCFG-05 — Listeners {#SPEC-AGENTCFG-05~draft}

### 6.1 Substance unchanged

Listeners keep DOC-54 §5.3's definition verbatim: inbound API bindings, **not**
MCP tools; an agent *acts* via skills and *reacts* via listeners; two-stage
filtering (harness-level ingestion filter → per-agent wake filter).

The backend shipped in #3820 and is untouched by this spec:
`ListenerConfig { name, connector, identity, transport, enabled, poll_interval_secs, filter }`
(`listeners/config.rs:91`, harness level, `enabled = false` by default,
poll interval clamped up to a 15s floor) and
`AgentListenerBinding { name, event_types, filter { from, exclude_labels } }`
(`listeners/config.rs:158`, per-agent). Wake matching is
`binding_matches_event` (`wake.rs:87`); the per-connector prompt overlay
`events/<connector>.md` loads at `wake.rs:317`.

### 6.2 What this spec adds

Only the section's membership and its ordering position (§2.1). Completing the
pane is **#3891's** work and this spec does not re-specify it — #3891 already
defines `GET /api/agents/:name/listeners`, the per-listener status fields, the
fail-soft rules, and the removal of the client-side scaffold.

- **L-1** The Listeners pane MUST NOT render fabricated data. Today it renders
  the `DEFINED_LISTENERS` literal (`ui/src/lib/agentConfig.ts:123-136`) with a
  hardcoded "not bound" badge for `gmail` and `google-calendar` regardless of
  configuration. A fabricated pane is worse than an empty one — it shows a
  listener that may not exist and hides one that is quietly failing.
- **L-2** Honest state of the tree, to be reflected in whatever the pane shows:
  exactly **one** checked-in per-agent binding exists
  (`agents/izzie/agent.toml:203-206`, `gmail-personal`); `cto-assistant` ships
  `events/slack.md` but declares **no** `[[listeners]]`; and
  `AgentBindingFilter.exclude_labels` is parsed but never enforced because
  `StoredEvent` carries no labels (`wake.rs:110-111`).
- **L-3** The only connector wired to a live poller is `gmail`; unknown
  connectors are logged and skipped.

### 6.3 Conformance

- **C-05.1** The pane's contents derive entirely from a backend response; the
  `DEFINED_LISTENERS` literal is deleted (#3891).
- **C-05.2** An agent with no `[[listeners]]` renders an explicit empty state,
  distinct from a loading state and from an error state.

---

## 7. SPEC-AGENTCFG-06 — Permissions {#SPEC-AGENTCFG-06~draft}

### 7.1 Why this is a section, not a footnote

Permissions is the section this spec creates rather than relabels. Today an
agent's permission posture is spread across **four unrelated mechanisms in three
locations, with three different failure polarities**:

| # | Mechanism | Where | Enforced? | Polarity on empty/absent |
|---|---|---|---|---|
| 1 | `[tools].allow` globs | `agent.toml` | Yes — `filter_persona_tool_names` (`persona.rs:65`) | **Absent ⇒ zero tools** on the persona path (`persona.rs:255,480`) |
| 2 | `[tools].scopes` (OpenRPC) | `agent.toml` | Yes — `agent_can_use` (`scope.rs:125`), **persona-chat path only** | **Empty ⇒ denies everything** (fail-closed, `scope.rs:225`) |
| 3 | `ServiceTier` RBAC | Rust + `SLACK_RBAC_USERS` env + `[rbac]` | Partly — `filter_tools_for_user` runs; `[rbac]` block has **no read site** | **`Default = All`** ⇒ degrades **open** (`trusty-agents-common/src/lib.rs:343-353`) |
| 4 | Endpoint scope filter | `~/.trusty-agents/config.toml` `[tool_registry]` | Yes — `scope_enforcement = "deny"` | Operator-level, not per-agent |

Three polarities (absent-denies, empty-denies, default-opens) across four
mechanisms is not a model a user can reason about from a GUI, and two of the four
are invisible in config entirely. Two more gaps compound it:

- **Scope enforcement is partial by construction.** Only tools whose
  `ToolExecutor::scope()` returns `Some` are gated — that is, tools discovered
  through the OpenRPC registry adapter (`tools/registry/adapter.rs:63`). Every
  in-process/native tool returns `None` (trait default) and **passes the scope
  gate unconditionally**. The subprocess/`--direct` path applies no scope
  enforcement at all (`scope_assistant_allowed_tools`, `tool_registry.rs:530`,
  filters by glob only).
- **`user_authority` does not exist yet.** DOC-41 §5.5 specifies the singleton in
  full — scan-based uniqueness, `authority_agent` config pointer, non-inheritance
  across `extends`, the reserved `user.*` credential namespace, and
  "delegation never grants authority". `AgentConfig` has no such field; the
  carve-out is a comment at `extends/mod.rs:252-262` and the test is `#[ignore]`d
  (`extends/tests.rs:619`). Tracked as #3074/#3075.

### 7.2 Data model (NORMATIVE, NEW)

```toml
[permissions]
# Declared OpenRPC/RBAC scopes. Supersedes `[tools].scopes` (§9.4).
scopes = ["memory.read", "memory.write", "search.read", "google.gmail.*"]

# DOC-41 §5.5. Reserved — parsed and surfaced now, enforced when #3074 lands.
user_authority = false

# Tier defaults (today: `[rbac]`, which has no read site).
default_tier = "all"                  # all | analytics | read_only
unauthenticated_tier = "read_only"

# DOC-54 §2.1's ask-first / learn-to-act posture. Declarative in M1.
autonomy = "ask-first"                # ask-first | learn-to-act

# Optional per-skill override. Union-merged by `skill` across `extends` (§2.3).
[[permissions.grants]]
skill = "gmail"
mode = "ask"                          # allow | ask | deny
```

| Field | Type | Default | Status |
|---|---|---|---|
| `scopes` | `Vec<String>` | inherited from `[tools].scopes` | Parsed **and enforced** today (path-limited, §7.1) |
| `user_authority` | `bool` | `false` | **Reserved** — surfaced read-only; enforcement is #3074 |
| `default_tier` | `ServiceTier` | `all` | Parsed today via `[rbac]`; **no read site** — wiring is Phase 4 |
| `unauthenticated_tier` | `ServiceTier` | `all` | Same. Note DOC-41's own warning that unauthenticated transports MUST set something stricter |
| `autonomy` | enum | `ask-first` | **Declarative only.** Mechanics belong to DOC-23 |
| `grants[]` | table array | `[]` | **Declarative only** in M1; enforcement is Phase 4 |

**Normative constraints:**

- **PM-1** `[permissions]` **describes** the union of mechanisms in §7.1; it does
  not invent new enforcement. Every field either maps to an existing enforced
  mechanism or is explicitly marked reserved/declarative in the table above. A
  field MUST NOT be rendered in the GUI as if enforced when it is not (§8.3).
- **PM-2** `autonomy` is an enum, not a policy engine. The designed
  approval/undo/audit model is **DOC-23** (`DecisionAdjudicator`,
  `DecisionRecord`, tiered thresholds, reversibility gates, audit trail) and
  DOC-54 §11 Q3 records that its trusty-agents decision tree is unspecified.
  This spec deliberately does not duplicate or fork it (OQ-5, §12).
- **PM-3** `user_authority` is **never inherited** across `extends` (DOC-41 §5.5).
  This is the one merge rule this spec fixes rather than inherits, and it is
  already the reserved intent at `extends/mod.rs:252-262`.
- **PM-4** Permissions is **read-only in every phase of this spec.** Granting
  capability from a GUI is a security-relevant write path; it requires its own
  review and is out of scope (§11).

### 7.3 A live defect this section surfaces

Making Permissions a real section exposes a latent bug worth recording, because
it is exactly the class of problem an invisible permission model produces:

`agents/cto-assistant/agent.toml` (the directory package, which **wins** over the
flat `cto-assistant.toml` — `loader.rs:163`) declares **no** `[tools].scopes`, so
it inherits the base assistant's `["memory.read", "memory.write", "search.read",
"google.read"]` (`assistant/agent.toml:183`) by union. Its `[tools].allow`
nonetheless grants `create_draft`, `list_tasks`, `complete_task` and
`create_task` — Gmail/Tasks tools requiring `google.gmail.*` / `google.tasks.*`.
The shadowed flat file `cto-assistant.toml:108` carries exactly those scopes and
its own comment (lines 105-107) warns they would otherwise "be silently denied at
dispatch the moment scope enforcement is active" — and scope enforcement **is**
active on the persona-chat path.

- **PM-5** This is filed separately rather than fixed inside a docs PR (OQ-6,
  §12). It is cited here as motivating evidence, not as spec content.
- **RESOLVED (historical record above).** The overlay half was filed as #3938
  and fixed in PR #3985 (the package now declares `google.gmail.*` /
  `google.tasks.*`). The base half — `google.read` matching nothing at all —
  was filed as #3987 and fixed in two parts: a dead-scope-pattern diagnostic
  (`tools::registry::dead_scope`, warning at registry build and surfaced in
  `GET /api/agents/:name/skills` as `dead_scope_patterns`), and explicit
  per-family grants on the base `assistant`. §7.3's quoted scope line is the
  pre-fix state, retained because it is what motivated this section.

### 7.4 Backend contract (NEW)

`GET /api/agents/:name/permissions`:

```jsonc
{
  "scopes":  [ { "pattern": "google.gmail.*", "source": "inherited:assistant",
                 "enforced": true } ],
  "user_authority": { "value": false, "enforced": false,
                      "reason": "field reserved — #3074 not landed" },
  "tiers":   { "default": "all", "unauthenticated": "all", "enforced": false },
  "autonomy": { "mode": "ask-first", "enforced": false,
                "reason": "declarative; see DOC-23" },
  "grants":  [],
  "config_error": null
}
```

- **PM-6** Every element carries an `enforced` boolean. Displaying an unenforced
  control as if it constrained the agent is the permissions-surface equivalent of
  #3891's fabricated listener pane, and is forbidden.
- **PM-7** `source` distinguishes `declared` from `inherited:<base>` so the §7.3
  class of defect is visible in the pane rather than requiring a reader to
  simulate `merge_extends`.

### 7.5 Conformance

- **C-06.1** An agent declaring `[permissions].scopes` and no `[tools].scopes`
  resolves to the same effective scope set as the reverse (§9.4).
- **C-06.2** A child agent does not inherit `user_authority = true` from its base
  (PM-3) — the currently-`#[ignore]`d `extends_does_not_inherit_user_authority`
  test is un-ignored.
- **C-06.3** Every field in the `/permissions` response carries `enforced`, and
  `enforced: true` appears only for mechanisms with a live enforcement site.
- **C-06.4** No route in this spec grants or widens a permission (PM-4).

---

## 8. SPEC-AGENTCFG-07 — GUI Section Mapping {#SPEC-AGENTCFG-07~draft}

### 8.1 The canvas

PR #3895 made agent configuration a **full-pane takeover**: the gear flips
`stores/configPane.ts`'s `configPaneOpen`, and `ChatPane` renders
`AgentConfigOverlay` as an absolutely-positioned sibling of the whole chat group,
covering the chat column and the recap rail while keeping `ChatView`/`InputArea`
**mounted** (so scroll offset and a half-typed message survive). That is the
canvas this lands on, and this spec does not alter it.

All five bodies live in one 547-line component today
(`ui/src/components/AgentConfigPanel.svelte`) via an `{#if}/{:else if}` chain.

### 8.2 Tab mapping (NORMATIVE)

| # | New tab id | New label | Today's tab id | Today's label | Change |
|---|---|---|---|---|---|
| 1 | `personality` | Personality | `personality` | Personality | none |
| 2 | `knowledge` | Knowledge | `okg` | OKG Stores | **rename + widen** (§4) |
| 3 | `skills` | Skills | `tools` | Tools | **replace** (§5) |
| 4 | `listeners` | Listeners | `listeners` | Listeners | position only (#3891 fills it) |
| 5 | `permissions` | Permissions | `permissions` | Permissions | **reorder + re-back** (§7) |

Two changes are mechanical and easy to under-estimate:

- **G-1 Reorder.** Today's source order is `personality, okg, tools,
  permissions, listeners` (`AgentConfigPanel.svelte:285`). Permissions moves to
  last. `AgentConfigPanel.test.ts:151-152` asserts the four non-personality tabs
  render and must be updated in the same change.
- **G-2 "OKG Stores" → "Knowledge" is not a label swap.** The pane gains two
  sub-surfaces (§4.3, §4.4). Keeping the label "OKG Stores" over a widened pane
  would misdescribe it; keeping the pane narrow under the label "Knowledge" would
  fail the directive.

### 8.3 Rendering rules (NORMATIVE)

- **G-3 Skills render as cards, not as a textarea.** Today the Tools pane is a
  single monospace `<textarea>` of raw globs, one per line, with no catalog, no
  validation and no grouping (`AgentConfigPanel.svelte:426-435`). A skill card
  shows: name, description, `kind`, the wrapped tool list (collapsed by default),
  availability, and a **`synthetic` badge** (S-10) when no manifest was authored.
- **G-4 Never fabricate.** Applies uniformly: the Listeners scaffold (L-1), a
  disabled knowledge endpoint (§4.4), an unenforced permission control (PM-6),
  and the Personality pane's hardcoded per-connector-instructions block (P-2).
  Every pane distinguishes **loading** / **empty** / **error** as three states,
  following the OKG pane's existing precedent
  (`AgentConfigPanel.svelte:372-384`).
- **G-5 Fail-soft per pane.** One degraded backend darkens one pane. The existing
  two-phase load (`load()` at `AgentConfigPanel.svelte:160-206`, where the stores
  fetch is separately caught so a slow daemon never blocks the panel) is the
  pattern; each new section fetch follows it.

### 8.4 Component decomposition

A 547-line component gaining three new data sources should be split. Following
the flat, colocated-test convention of `ui/src/components/`:

```
ui/src/components/AgentConfigPanel.svelte        (shell: tabs, header, exit guard)
ui/src/components/AgentConfigPersonality.svelte  + .test.ts
ui/src/components/AgentConfigKnowledge.svelte    + .test.ts
ui/src/components/AgentConfigSkills.svelte       + .test.ts
ui/src/components/AgentConfigListeners.svelte    + .test.ts
ui/src/components/AgentConfigPermissions.svelte  + .test.ts
ui/src/lib/agentConfig.ts                        (fetch wrappers, extended)
```

- **G-6** Two invariants must survive the split, both currently test-guarded:
  (a) **dirty tracking** — `configPaneDirty` is set by whoever owns editable
  state (`AgentConfigPanel.svelte:118`, cleared on destroy at :127) and every
  exit path funnels through `requestExitConfigPane` (`ChatHeader.handleGear`,
  `AgentConfigOverlay.onKeydown`, `App.svelte:69`); with editable state moving
  into children, dirty aggregation and the `confirmingExit` dialog need an
  explicit owner in the shell. (b) **layout** — the
  `flex min-h-0 flex-1 flex-col` chain with per-tab scrolling, guarded by
  `ui/tests/config-takeover.spec.ts` (`deadSpaceBelow < 4`, `maxHeight: none`,
  `minHeight: 0px`, no ancestor scrollers).

### 8.5 Phase-1 mapping with existing data

The GUI reshape ships **first**, before any new backend, by mapping the five
sections onto data that already exists:

| Section | Phase-1 data source |
|---|---|
| Personality | `GET /api/agents/:name/persona` (live, editable) |
| Knowledge | `GET /api/agents/:name/stores` (live) + a **read-only, statically-declared** MCP knowledge-endpoint list |
| Skills | `AgentDetail.tools_allow` rendered as **synthetic** skill cards, grouped per S-8 |
| Listeners | existing scaffold, explicitly labelled as such until #3891 |
| Permissions | `AgentDetail.scopes` (already returned by `parse_agent_toml`, `projects.rs:369-443`), rendered read-only with `enforced` annotations |

- **G-7** Phase 1 introduces **no** new HTTP route and **no** config-schema
  change. It is a pure front-end change and is independently revertable.

### 8.6 Conformance

- **C-07.1** The tab strip renders exactly five tabs in §2.1 order with the §8.2
  labels.
- **C-07.2** With every backend route absent or failing, all five panes still
  render with explicit empty/error states and no fabricated content (G-4).
- **C-07.3** `config-takeover.spec.ts`'s layout assertions pass unchanged after
  the decomposition (G-6b).
- **C-07.4** Editing persona text then pressing Esc still raises the
  unsaved-changes dialog after the split (G-6a).

---

## 9. SPEC-AGENTCFG-08 — Compatibility and Migration {#SPEC-AGENTCFG-08~draft}

### 9.1 The compatibility contract (NORMATIVE)

> **Every `agent.toml` that loads on `origin/main` @ `385aeff7` MUST continue to
> load, unchanged, with an identical effective tool set, identical store
> bindings, identical listener bindings and identical scope set, on every
> release described by this spec.**

This is a hard contract, not an aspiration. Agent definitions are **local
configuration, never committed to git** (DOC-54 §3.3) — the repo cannot see, let
alone migrate, the files that matter most. A change that requires users to
hand-edit `agent.toml` to keep an agent working is therefore a broken release,
not a migration.

| ID | Guarantee |
|---|---|
| **CC-1** | `[tools].allow`, `[tools].allowed`, `[tools].scopes`, `[[stores]]` (both spellings), `[[listeners]]`, `[system_prompt].skills` and `[rbac]` all keep parsing with unchanged semantics. None is removed by this spec. |
| **CC-2** | With no `[skills]` section present, tool resolution is byte-identical to today (§5.4 step 3 degenerates to `[tools].allow`). |
| **CC-3** | With no `[permissions]` section present, permission resolution is byte-identical to today (`[tools].scopes` ∪ inherited). |
| **CC-4** | New keys are **additive and optional**. `AgentConfig` does not use `deny_unknown_fields` (`agents/config.rs:24`), so a config carrying `[skills]`/`[permissions]` still loads on an **older** binary — the new sections are ignored, capability is unchanged, and nothing fails to parse. Forward and backward compatibility both hold. |
| **CC-5** | No new section is required. An agent declaring none of `[skills]`, `[permissions]`, `[[stores]]` or `[[listeners]]` remains valid. |
| **CC-6** | Every `extends` merge rule in §2.3 is the rule already implemented, with the single exception of `user_authority` non-inheritance (PM-3), which governs a field that does not yet exist and so cannot regress anything. |

> **CC-4 has a sharp edge worth naming.** The absence of `deny_unknown_fields` is
> what makes forward compatibility free — and it is also why a misplaced key
> vanishes silently. The `extends`-above-`[agent]` bug documented at
> `agents/izzie/agent.toml:25-38` is exactly that failure. Adding two new
> top-level tables enlarges the surface for it. §9.5 requires the mitigation.

### 9.2 Terminology compatibility

DOC-54 §5 names the model "the config triple". That phrase is superseded. Where
"the triple" appears in code comments — `agents/izzie.toml:36-37`,
`agents/cto-assistant/agent.toml:53-55`, `stores/config.rs` module docs, #3878's
and #3891's bodies — it remains accurate as *history* and MUST NOT be
retro-edited by this spec's PR. New comments use the five-section vocabulary.

### 9.3 Why Skills unions with, rather than replaces, `[tools].allow`

§5.4 step 3 computes `union(expanded_skill_tools, [tools].allow)`. The
alternative — `[skills]` replacing `[tools].allow` when present — was rejected:
adding a `[skills]` line would then **silently remove** every capability the
agent had from its existing `allow` list, and because agent files are local and
unversioned, the failure would surface as an agent that mysteriously stopped
being able to do things.

- **CC-7** The union is **transitional**. Narrowing an agent's capability
  continues to require editing `[tools].allow` (deny is still expressed as
  absence — `ToolsConfig` has no `deny` field). Migration (§9.4) removes
  `[tools].allow` outright, at which point the union is over an empty set.
- **CC-8** When both sections are present the GUI MUST say so — a skill card
  derived from `[tools].allow` rather than from `[skills].allow` is marked
  `synthetic` (S-10), which makes an un-migrated agent visually obvious.

### 9.4 Migration path

**Nothing is auto-rewritten.** Migration is opt-in, per agent, and reversible.

1. **Phase 1–2 (no user action).** Synthetic skills (§5.5) render every existing
   `[tools].allow` entry as a skill card. Every agent gains the five-section view
   with zero config edits. For most users this is the whole migration.
2. **Phase 3 (opt-in, GUI-driven).** `tagent agent migrate-skills <name>`
   proposes a rewrite: `[tools].allow` globs are matched against authored and
   synthetic manifests, and the equivalent `[skills].allow` is **printed as a
   diff for confirmation**, never written unprompted. `--write` applies it and
   leaves a timestamped `.bak`.
3. **Permissions.** `[tools].scopes` → `[permissions].scopes` and `[rbac]` →
   `[permissions].{default_tier, unauthenticated_tier}` follow the same
   propose-then-confirm flow.

- **CC-9 Precedence when both are declared.** `[permissions].scopes` **wins**
  over `[tools].scopes`; the union of the two is **not** taken, because unioning
  a legacy and a new declaration could only ever widen the scope set. A
  startup WARN names both keys and the file. Same rule for `[rbac]`.
- **CC-10** A glob in `[tools].allow` matching no known tool migrates to nothing
  and is **reported, not dropped** — the migration command exits non-zero listing
  unmatched patterns rather than silently narrowing the agent.

### 9.5 Required mitigation for CC-4's sharp edge

- **CC-11** Adding `[skills]` and `[permissions]` MUST be accompanied by a
  **known-section lint**: at load, any *top-level* table in `agent.toml` that is
  not a recognized section emits a WARN naming the key and the file. This is a
  warning, never an error (CC-4 depends on unknown keys remaining non-fatal), and
  it is the smallest change that would have caught the `izzie` `extends` bug.

### 9.6 Conformance

- **C-08.1** A corpus test loads every checked-in `agent.toml` under
  `crates/trusty-agents/.trusty-agents/agents/` and asserts the resolved
  effective tool set, store bindings, listener bindings and scope set are
  unchanged against a golden snapshot taken at `385aeff7` (CC-1, CC-2, CC-3).
- **C-08.2** A config carrying `[skills]` and `[permissions]` parses without
  error under a build with no knowledge of either (CC-4).
- **C-08.3** `migrate-skills` without `--write` mutates nothing on disk (§9.4).
- **C-08.4** An unrecognized top-level table produces exactly one WARN and does
  not fail the load (CC-11).

---

## 10. SPEC-AGENTCFG-09 — Phased Delivery {#SPEC-AGENTCFG-09~draft}

Each phase is independently shippable and independently revertable. Phase order
is chosen so the owner sees the five-section model **first**, before any backend
work, and so that no phase can regress an existing agent.

| Phase | Deliverable | Config change | Route change | Ticket |
|---|---|---|---|---|
| **1** | GUI reshape to five sections over existing data (§8.5) | none | none | (a) |
| **2** | Skill-wrapping runtime: `tools`/`kind` frontmatter, synthetic skills, `[skills].allow` resolution → existing gate | `[skills]` (optional) | `GET …/skills` | (b) |
| **3** | Knowledge backend: unified knows-surface | none | `GET …/knowledge` | (c) |
| **4** | Permissions backend: structured model, `enforced` reporting, `user_authority` surfacing | `[permissions]` (optional) | `GET …/permissions` | (d) |
| **5** | Listeners pane completion | none | `GET …/listeners` | (e) / #3891 |

- **D-1** Phase 1 has no dependency on Phases 2–5 and MAY ship alone.
- **D-2** Phases 2, 3 and 4 are mutually independent and MAY ship in any order or
  in parallel; each replaces one Phase-1 mapped pane with live data.
- **D-3** Phase 5 is #3891 and is referenced, not re-specified (§6.2).
- **D-4** Store *editing* (#3890) is not in any phase here; it lands on the
  Knowledge pane whenever #3890's own design questions are settled (K-3).
- **D-5** No phase in this spec adds a permission-granting write path (PM-4).

---

## 11. Non-Goals

Explicitly out of scope. Each is listed because it is a plausible reading of the
directive that this spec deliberately does **not** adopt.

1. **RESOLVED (OQ-1, 2026-07-27):** Skills may bundle executable code. The owner
   decided to permit bundled implementations (Python packages, etc.) with the
   operator bearing responsibility for vetting. Both bundled code and MCP servers
   are valid paths (§5.8, S-13/S-14).
2. **Unifying the trusty-agents and trusty-mpm skill implementations.** Two
   dialects and two on-disk shapes exist (§5.1). This spec governs the
   trusty-agents dialect only (OQ-3).
3. **A new enforcement engine.** Skills resolve *into* the existing
   `filter_persona_tool_names` gate (§5.4). No new gate, no change to
   `dispatch_gated`, `agent_can_use`, or the RBAC tier filter.
4. **Granting permissions from the GUI.** Read-only in every phase (PM-4).
5. **Specifying the autonomy decision tree.** `autonomy` is an enum; the designed
   model is DOC-23 and DOC-54 §11 Q3 records it as undecided for trusty-agents
   (PM-2, OQ-5).
6. **Fixing the partial scope-enforcement gap.** That native tools return
   `scope() = None` and that the `--direct` path applies no scope filtering are
   recorded as facts (§7.1), not repaired here.
7. **Store editing** — #3890 (K-3). **Listener pane completion** — #3891 (§6.2).
8. **Auto-rewriting any user's `agent.toml`.** Migration is propose-then-confirm
   (§9.4).
9. **Changing the agent template or creation flow** (#3816, #3818 §8.5).
10. **Retro-editing "config triple" from existing comments and issue bodies**
    (§9.2).

---

## 12. Open Questions for the Owner

Each blocks or reshapes a specific deliverable. Recommendations are the
engineering default if no decision is given.

**OQ-1 — May a skill carry executable code? — RESOLVED 2026-07-27 (owner): YES.**

**Resolution:** Skills may carry executable code (Python packages, binaries, or other implementations). The operator and agent owner are responsible for managing, vetting, and auditing that code. The platform imposes no automated validation. Both bundled implementation (discovered by `install_discovered_skill_plugins`) and MCP server hosting (operator-configured `McpService.command`) are valid paths. This amendment ratifies existing production behavior — the `cto-db` skill already bundles a Python package — and clarifies responsibility assignment rather than introducing new capability.

**Rationale:** The earlier proposed ban (DOC-41 §2.6's `PackageContainsForeignFile` error) was policy-only, never enforced in `load_agent_package` (`loader.rs:764-784`), with zero occurrences in the codebase. Executable skill code is already loading at runtime. The owner decision reflects this reality and makes responsibility explicit.

**Blocking resolved.** §5.8, Phase 2 scope, and the non-goal at §11.1 are amended accordingly.

**OQ-2 — Skill granularity: per tool, or per capability family? — RESOLVED
2026-07-25 (owner): PER TOOL.**
The question was whether "each tool should be wrapped in a skill" meant ~80+
single-tool cards or ~12 family cards (`gmail` = 6 tools). The owner ruled
first on *naming* — *"gworkspace is a Google Workspace skill, MTA Train Time is
that skill"* — and then, correcting the reading that this implied grouping:

> *"One skill per tool. There can be other skills without tools, but each
> [tool] needs an accompanying skill."*

The decision therefore has three parts, and the naming note is about **names**,
not about grouping:

1. **1:1.** Every registered tool is wrapped by exactly one skill. A tool with
   no skill is a defect, enforced by a coverage test that FAILS when a new tool
   is added without one (§5.9 C-04.3).
2. **Tool-less skills are first-class.** A skill MAY wrap zero tools — pure
   procedure or guidance with no executable member. This is a declared shape,
   not a degenerate case.
3. **Names are human and provider-recognisable.** `get_train_schedule` is
   named *"MTA Train Time"*; `search_gmail_messages` is named *"Gmail Search"*.
   A skill whose name echoes its tool identifier fails §5.9 C-04.7.

Implemented by #3933. This supersedes the family assumption this spec carried
in §5.5/S-8; those sections are rewritten accordingly.

**OQ-2 addendum — epic #4021 OQ-1, RESOLVED 2026-07-26 (owner): GRANT-THE-BUNDLE.**
One day after the ruling above, the owner directed that *"skills also need to be
organized by function"*. This does **not** reopen OQ-2: the 1:1 base is
unchanged and every tool still maps to exactly one leaf skill. What was added is
a separate **bundling tier** above it — a `kind: function` skill naming an
explicit list of member skill ids, where granting the bundle grants all members.
The alternative considered and rejected was presentation-only grouping (a
collapsible pane header with no resolution change); the owner's wording
(*"including all the skills under one"*) is a grant, not a display affordance.
Because a bundle compiles down to leaf ids before any permission gate runs, the
defence-in-depth this spec pins in S-1…S-6 is untouched. Specified in §5.7b
(S-13…S-16); implemented by #4022/#4023/#4025.

**OQ-3 — Converge the two skill implementations, or keep them separate?**
trusty-agents (flat `<name>.md`, **`tags` required**) vs trusty-mpm
(`<name>/SKILL.md`, 7-key frontmatter, tiers, checksum ledger, DOC-42
co-deployment). They share no code, and a trusty-mpm skill placed in a
trusty-agents source dir is silently skipped for want of `tags`.
*Recommendation: keep separate for M1; file convergence as its own epic.*
Affects: §5.1, §11 non-goal 2.

**OQ-4 — Should Permissions merge by UNION or REPLACE across `extends`?**
Scopes union today, so a child can only ever gain scopes and can never narrow
what it inherits — the opposite polarity from Knowledge (REPLACE) and a
privilege-escalation-shaped default. §7.3's cto-assistant defect is the mirror
image: inheriting *too few* scopes with no way to see it.
*Recommendation: keep UNION for M1 (changing it is a behavior change to live
agents), and make inheritance visible via `source` (PM-7).* Blocks: §2.3.

**OQ-5 — Is DOC-23 the autonomy model for the Permissions section?**
DOC-23 specifies a full `DecisionAdjudicator` / `DecisionRecord` / reversibility
/ audit-trail model on the trusty-mpm side. DOC-54 §11 Q3 leaves trusty-agents'
decision tree open. Adopt DOC-23, or is a simple `ask-first | learn-to-act` flag
sufficient for M1?
*Recommendation: flag only for M1; adopt DOC-23 when autonomy is actually
earned.* Blocks: §7.2 `autonomy`, Phase 4 scope.

**OQ-6 — Fix the cto-assistant scope defect now, or file it separately?**
The package inherits `google.read` only, while granting Gmail/Tasks tools that
require `google.gmail.*` / `google.tasks.*`; the correct scopes sit in the
shadowed flat file (§7.3). Scope enforcement is live on the persona-chat path.
*Recommendation: file as its own bug — it is a live defect, not spec content.*

**OQ-7 — Does the Knowledge pane get store editing in the same slice?**
#3890 (store PATCH) is open with its own undecided design questions (whole-array
replace vs per-binding, which fields are editable, validation before write).
Fold it into Phase 3, or keep Knowledge read-only until #3890 lands on its own?
*Recommendation: keep read-only; #3890 lands independently.* Blocks: K-3, D-4.

**OQ-8 — Is "Knowledge" the right user-facing label for a pane that includes
tools and MCP connections?**
The directive is explicit that Knowledge lists store bindings *and* knowledge
tools *and* MCP connections. Users may read "Knowledge" as documents only.
*Recommendation: keep "Knowledge"; the pane's three sub-headings disambiguate.*

---

## 13. References

**Supersedes:**
- DOC-54 [Trusty Agents Product Specification](./trusty-agents-product-spec.md)
  §5 `SPEC-AGENTS-04~draft` — "Agent Configuration: The Config Triple". The
  three legs are retained in substance and redistributed across five sections
  (§2.1). DOC-54 §8.4's GUI section list is superseded by §8.2.

**Related specs:**
- DOC-41 [Eve-Style Agent Framework](./trusty-agents-eve-style-agents-spec.md) —
  §2.3 manifest schema, §2.6 no-code enforcement (§5.8), §5.5 user-authority
  singleton (§7.1, PM-3), §6 config-key table. *(Referenced by section number
  rather than by `spec_refs:` frontmatter: DOC-41 declares its `SPEC-AGENTFW-NN`
  IDs in its header block but anchors none of its headings with `{#SPEC-…}`
  markers, so per DOC-38 §4.3 none of its sections is a resolvable SLD target
  yet. Retrofitting those anchors is out of scope here.)*
- DOC-42 [Agent-Bundled Skills](./agent-bundled-skills.md) — the trusty-mpm-side
  `skills:` declaration, co-deployment and dangling-reference validation (§5.1,
  S-11).
- DOC-23 [Learned-Autonomy Auto-Answer](./learned-autonomy-auto-answer.md) — the
  designed approval / reversibility / audit model (PM-2, OQ-5).
- DOC-31 [SYSTEM vs PROJECT Agents & Skills](./system-project-agents-skills.md) —
  3-tier skill precedence (trusty-mpm side).
- DOC-51 [trusty-code Plugin Support Phase 1](./DOC-51-tcode-plugin-support-phase1.md) —
  plugin skill tier and its hostile-input security model.
- DOC-55 [Universal OKG Importer](./okg-universal-importer.md) — what fills the
  stores the Knowledge section surfaces.
- DOC-56 [Agent Configuration Sync](./trusty-agents-agents-sync.md) — how these
  local, never-committed config files move between machines (§9.1).
- DOC-38 [Spec-Linked Documentation](./spec-linked-documentation.md) — the
  standard this document conforms to.

**Related issues:**
- #3052 — Assistant M1 epic.
- #3878 — OKG store binding (merged; the Knowledge section's K-a).
- #3890 — Store PATCH contract (open; K-3, D-4, OQ-7).
- #3891 — Listeners pane (open; §6.2, Phase 5).
- #3895 — Config-pane full-takeover (merged; the §8.1 canvas).
- #3857 — Slack RBAC parity (§7.1 mechanism 3).
- #3816 / #3818 — Declarative templates / GUI reshape.
- #3074 / #3075 — `user_authority` field and its `extends` test (§7.1, PM-3).
- #2791 — Declarative-only agents (§5.8).

---

## 14. Change Log

- **2026-07-25** — Initial spec (DOC-57, `SPEC-AGENTCFG-01~draft` …
  `-09~draft`). Records the owner's 2026-07-25 redefinition of the agent
  configuration model from DOC-54's three-leg config triple to five sections
  (Personality / Knowledge / Skills / Listeners / Permissions). Defines the
  tool-to-skill wrapping model with synthetic-skill coverage, the composed
  Knowledge surface, the structured Permissions model, the GUI section mapping
  onto PR #3895's takeover pane, a hard backward-compatibility contract for
  never-committed local `agent.toml` files, and five-phase delivery. Records
  eight open questions for the owner, including whether skills may carry
  executable code (DOC-41 §2.6 currently forbids it).
- **2026-07-27** — Amended §5.8 and resolved OQ-1. Owner decision: skills may
  carry executable code; the operator is responsible for managing and vetting it.
  The prior proposed ban (DOC-41 §2.6's `PackageContainsForeignFile` error) was
  never implemented and executable skill code is already in production (the
  `cto-db` skill). Amendment ratifies existing behavior and clarifies
  responsibility. Updated §5.8 from "No executable payload" to "Executable
  payload in skills", rewrote S-13/S-14 to permit bundled implementations and
  MCP servers as two valid paths, marked OQ-1 RESOLVED with the decision and
  rationale, and updated §11.1 non-goal 1 to reflect the resolution. Updated
  conformance C-04.5 to permit executables in skill packages with operator
  responsibility for vetting.
- **2026-07-27 (clarification)** — Sharpened §5.8's closing paragraph from
  noting that mandatory controls "may" be added to stating that they "will" be
  added. Epic #4128 will implement a `build_plugin`-level check (#4137) enforcing
  a recorded security verdict before skill code loads, via canonical package hash
  (#4135) and fail-closed verdict lookup (#4136). This amendment describes current
  permissive behavior; §5.8 will be revisited when the review gate lands to make
  enforcement normative. Harmonizes this spec with the stated intent that security
  review gates implementation.
