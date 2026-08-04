---
spec_refs:
  - id: SPEC-SLD-01~draft
    path: docs/specs/spec-linked-documentation.md
    anchor: SPEC-SLD-01~draft
  - id: SPEC-PROVISION-01~draft
    path: docs/specs/system-project-agents-skills.md
    anchor: SPEC-PROVISION-01~draft
  - id: SPEC-SHAREDWS-01~draft
    path: docs/specs/DOC-52-shared-workstream-definition.md
    anchor: SPEC-SHAREDWS-01~draft
---

# DOC-59 — P1/P2 Instruction Restructure: Tiered, Cache-Stable, Customizable PM System Prompt Composition

**Status:** Draft (design proposal only — see §8)
**Subsystem:** trusty-mpm — PM instruction pipeline (`crates/trusty-mpm/src/core/instruction_pipeline.rs`, `instruction_overrides.rs`, `stack_profile.rs`); session-manager (workstream/session persistence)
**Owner:** Engineering (trusty-mpm) / Bob Matsuoka
**Last-updated:** 2026-08-03
**Spec ID:** `SPEC-PMINSTR-01~draft` … `SPEC-PMINSTR-09~draft` (DOC-59)
**Motivating incident:** issue #4071 (contradiction between `PM_INSTRUCTIONS.md:70` and `stack_profile.rs`'s generated rule, ~400 resolved lines apart)
**Builds on:** DOC-38 [Spec-Linked Documentation](./spec-linked-documentation.md) (numbering/anchor convention); DOC-31 [SYSTEM vs PROJECT Agents & Skills](./system-project-agents-skills.md) (the project-custom > user-custom > bundled precedence this design's tier ordering mirrors); DOC-52 [Shared Workstream Definition](./DOC-52-shared-workstream-definition.md) (the workstream/session binding §7 makes the resolved corpus immutable against); issue #2299 / PR #2300 (root-`CLAUDE.md` removal — directly informs §6)

---

> **Supersession note (2026-08-01).** §5.5 recommendation 1 ("do not make
> `CLAUDE.md` the core project-override mechanism, in either its current
> Markdown form or a hypothetical restructured JSON form") is **superseded by
> [#4324](https://github.com/bobmatnyc/trusty-tools/pull/4324)**, merged
> 2026-07-29 — a *third* form neither this section nor the JSON-in-Markdown
> alternative it argues against anticipated: named-section markers
> (`<!-- TRUSTY-MPM: <TOKEN> START v=1 -->` … `END`) inside a project's own
> `CLAUDE.md`, read by
> [`claude_md_sections.rs`](https://github.com/bobmatnyc/trusty-tools/blob/8abf30962863e143ed405e8d6cabe33f6b0f0b6d/crates/trusty-mpm/src/core/claude_md_sections.rs#L72).
> `CLAUDE.md` is `HOST_FILES[0]` and wins a same-section collision over
> `.trusty-mpm/INSTRUCTIONS.md` (`HOST_FILES[1]`) — see
> [`claude_md_sections.rs:72`](https://github.com/bobmatnyc/trusty-tools/blob/8abf30962863e143ed405e8d6cabe33f6b0f0b6d/crates/trusty-mpm/src/core/claude_md_sections.rs#L72),
> [`:34`](https://github.com/bobmatnyc/trusty-tools/blob/8abf30962863e143ed405e8d6cabe33f6b0f0b6d/crates/trusty-mpm/src/core/claude_md_sections.rs#L34).
> Recommendation 2 (do not loosen `claude-md-guard.yml` /
> `check_claude_md_not_tracked.sh`) held **until a further owner ruling on
> 2026-08-03 retired it outright — see §9.6.** The guard was never loosened
> for a "small disciplined format" (the risk this recommendation actually
> argued against); instead the owner ruled the underlying ban itself
> obsolete, and [#4660](https://github.com/bobmatnyc/trusty-tools/pull/4660)
> deletes both the workflow and the script. Do not read this recommendation
> as still standing — §9.6 has the current position and the retirement
> rationale.
>
> Beyond the mechanism, the **direction** is also settled, not just added
> alongside the old one: project customization is named sections in the
> root `CLAUDE.md`. The `.trusty-mpm/` per-file override surface remains in
> the current binary's read paths — it has not been removed and nothing
> here should be read as claiming otherwise — but
> [#4286](https://github.com/bobmatnyc/trusty-tools/issues/4286) tracks its
> removal, not its indefinite parallel existence. See §5.5 inline for the
> per-item detail. This note records the reversal; §5 below is left as
> originally written.
>
> **Sharpened by a further owner ruling (2026-08-03) — see §9.** The
> direction above is no longer merely "settled, not yet removed": the
> owner has now ruled that `.trusty-mpm/INSTRUCTIONS.md`'s current
> unconditional, additive read is itself a **defect**, not a tolerated
> transitional state, and that the `#2299` tracked-root-`CLAUDE.md` ban
> is **retired**. §9 records both rulings verbatim, the canonical
> section-token list, the deprecated-file inventory, the code/spec gap,
> and the migration path. §5's argument that a tracked `CLAUDE.md` risks
> reintroducing the #2299 double-load is superseded by §9.6, not by this
> note — read §9 for the current position.

---

## 1. Problem Statement {#SPEC-PMINSTR-01~draft}

**ID:** SPEC-PMINSTR-01~draft
**Status:** Draft

### 1.1 The motivating incident — issue #4071

Two sections of the *same* resolved PM system prompt contradict each other on
whether a generic `engineer` agent is acceptable for language-specific code
work:

- **Section A (early, permissive, wins in practice).**
  `crates/trusty-mpm/src/assets/instructions/PM_INSTRUCTIONS.md:70`, inside the
  `## Agent Routing` quick-reference table (heading at line 63):

  ```
  | Engineer (all langs) | code changes, impl, refactor | sonnet |
  ```

  This names "Engineer (all langs)" as a valid single choice for ANY language,
  with no steering toward a language-specific engineer. In the resolved prompt
  this lands near the top of the document (`.trusty-mpm/last-instructions.md:70`
  in the reporting session's snapshot).

- **Section B (late, correct, loses in practice).**
  `crates/trusty-mpm/src/core/stack_profile.rs` — `stack_profile_section()` (the
  function body spans roughly lines 55-90; the heading constant
  `STACK_PROFILE_HEADING` is declared at line 37) — renders the auto-derived
  `## Detected Project Stack (auto-derived)` block:

  ```
  Route hands-on code work to the matching language engineer(s) — prefer the
  most specific — and never a generic `engineer` when one of these fits:
  - `rust-engineer`
  ```

  This is the *correct* rule for this repo. In the reporting session's
  resolved prompt it landed at `.trusty-mpm/last-instructions.md:470-474` —
  **roughly 400 lines after** the contradicting row at line 70.

In a long PM session, the early ambiguous rule is what gets internalized and
acted on; the correct late rule gets deprioritized. This was confirmed via two
independent, concurrent PM sessions in this same repo dogfooding trusty-mpm
itself: both dispatched a generic `engineer`/`Agent()` call for Rust work
instead of `rust-engineer`, ruling out "one careless session" as the
explanation and pointing at the shared instruction source's ambiguity and
ordering.

This is a **structural** bug, not a wording bug: nothing in the current
instruction pipeline enforces that a load-bearing routing rule appears once,
early, and cannot be contradicted by a later-generated section. There is no
tiering, no priority, and no mechanism preventing exactly this shape of
collision from recurring for any other rule.

### 1.2 Current state: an undifferentiated 1521-line corpus

The resolved PM system prompt for this repo is assembled, in order, from:

| Source | Lines | Role |
|---|---|---|
| `PM_INSTRUCTIONS.md` | 466 | PM identity, prohibitions, allowlist, routing, delegation mechanics, model selection, workflow summary, circuit breakers, commits/issues, skills/agent deployment |
| `stack_profile_section()` (generated, not a file) | ~15-35 (varies by detected stack) | Per-project detected-stack routing rule (the Section B above) |
| `WORKFLOW.md` | 151 | 5-phase workflow detail, verification gates, publish/release workflow |
| `AGENT_DELEGATION.md` | 63 | Full agent routing table + make/mise command routing |
| `.trusty-mpm/INSTRUCTIONS.md` (this project's own, additive) | 714 | Project overview, build/test commands, conventions, worktree discipline, abbreviations, environment, pitfalls |
| `BASE_PM.md` (non-overridable floor, always last) | 107 | Non-overridable prohibitions pointer, override-file table, framework-guaranteed conventions (footer/docs/ticket rules), trusty tool priority |

**Total: 1521 lines**, of which this project's own `.trusty-mpm/INSTRUCTIONS.md`
is **714 lines — 47% of the entire resolved corpus** — every one of those
lines injected into every PM turn regardless of whether the current task
touches release workflow, TCC/FDA signing, or the 68-line Parallel Worktree
Discipline block (§3, finding "Mis-tiering #2").

None of this is tiered by load-bearingness. A one-line routing-table
contradiction and a 68-line worktree-safety block and a 15-line release-note
convention all carry the same weight: "somewhere in the 1521 lines." #4071 is
the predictable consequence of that flat structure at scale.

---

## 2. Goals {#SPEC-PMINSTR-02~draft}

**ID:** SPEC-PMINSTR-02~draft
**Status:** Draft

1. **P1 (always-injected, load-bearing) vs P2 (situational, discoverable-on-demand) classification.** Every instruction unit is tagged `priority: P1 | P2`. P1 units are always inlined into the resolved system prompt, in a fixed, deliberately-ordered position. P2 units are not inlined by default (§5).

2. **Estimated P1-only corpus: ~480-500 lines — a 65-68% reduction from 1521.** Driven mainly by:
   - Demoting the 714-line project reference file (`.trusty-mpm/INSTRUCTIONS.md`) to P2 almost in its entirety — build commands, environment setup, abbreviations, TCC/FDA runbooks, and reference-doc pointers are situational, not needed on every turn. Only the ~68-line Parallel Worktree Discipline block (safety-critical, see finding "Mis-tiering #2") and a short project-identity capsule stay P1.
   - Collapsing the commit/PR attribution footer rule, which today is stated in full **four separate times** (finding "Duplication," below), down to one P1 statement (in `BASE_PM.md`) with the others reduced to a one-line cross-reference or removed outright.

3. **Instructions immutable within a workstream.** The resolved P1 corpus is persisted once at first workstream launch and reused verbatim across every resume — not re-resolved from live disk state on every resume, which is what happens today (§4f) — so every subagent spawned within that workstream shares one byte-stable system-prompt prefix for prompt-cache reuse.

4. **Structured user/project customization via unit-addressable JSON operations** (`override` / `add` / `disable`, addressed by unit `id`) — not today's whole-file replacement, which silently drops everything in the replaced file (finding "Mis-tiering #1").

5. **A `customization_tier: fixed | project | user` axis, independent of `priority`.** Load-bearingness (P1/P2) and override permission (fixed/project/user) are orthogonal — see §4a.

---

## 3. Current-State Findings {#SPEC-PMINSTR-03~draft}

**ID:** SPEC-PMINSTR-03~draft
**Status:** Draft

### Finding: Duplication — the attribution footer rule appears in 4 places

The commit/PR attribution footer rule (`🤖🤖🤖 Generated with trusty-mpm — …`)
is stated in full, independently, in:

1. `PM_INSTRUCTIONS.md:319-326` ("Commits & Issues (shipped defaults…)")
2. `WORKFLOW.md:116-119` ("Commits, Issues & PRs (Shipped Defaults)" — itself says "See `PM_INSTRUCTIONS.md` § 'Commits & Issues' (canonical)" at line 113, then restates it anyway)
3. `BASE_PM.md:58-61` ("Framework-Guaranteed Conventions (Non-Overridable)")
4. This project's own `CLAUDE.md` root stub, "Commit & PR Attribution" section (restated a fourth time specifically because a bare `claude` session launched outside `tm` orchestration only sees this file, not `BASE_PM.md`)

Three of these four openly acknowledge redundancy in their own prose (WORKFLOW.md points back at PM_INSTRUCTIONS.md; the project CLAUDE.md stub explains it's restating BASE_PM.md for a different launch path) — the duplication is not an oversight, it's a symptom of not having a single P1 unit with one `enforced_by` mechanism that every consumer can point at instead of re-stating.

### Finding: Mis-tiering #1 — the authority model is only *contingently* fixed

The Prohibitions (P1-P11) and Circuit Breakers tables are the delegation
authority model Bob has explicitly named as "must be fixed, not
customizable." Today they live in `PM_INSTRUCTIONS.md` (Prohibitions at
`PM_INSTRUCTIONS.md:11-34`; Circuit Breakers at `PM_INSTRUCTIONS.md:268-288`)
— a file that is **fully replaceable** by a project's
`.trusty-mpm/PM_INSTRUCTIONS_DEPLOYED.md` override, confirmed directly in
`crates/trusty-mpm/src/core/instruction_overrides.rs::resolve_pm_prompt`,
Branch 1:

```rust
// instruction_overrides.rs:153-161
if let Some(body) = read_override(&dir, FILE_PM_DEPLOYED) {
    let mut sections: Vec<String> = vec![body, stack];
    if let Some(extra) = read_override(&dir, FILE_INSTRUCTIONS) {
        sections.push(extra);
    }
    sections.push(floor.trim().to_string());
    return join_sections(sections);
}
```

`PM_INSTRUCTIONS` is not merely overridden here — it is **excluded from
`sections` entirely**; the resolved prompt in this branch contains `body`
(the project's replacement), the stack profile, optionally
`INSTRUCTIONS.md`, and the `BASE_PM.md` floor. Nothing from the bundled
`PM_INSTRUCTIONS.md` survives.

Only `BASE_PM.md` is truly compile-time-fixed: it is `include_str!`'d and
`read_override` is never called with a `BASE_PM` file name anywhere in this
module. But `BASE_PM.md:18` only *points at* the authority model —
"All prohibitions defined in PM_INSTRUCTIONS.md § Prohibitions are BINDING"
— it does not restate the table. Today, a project's
`PM_INSTRUCTIONS_DEPLOYED.md` override can silently drop the entire
Prohibitions/Circuit-Breaker authority model, leaving only a dangling,
now-meaningless pointer sentence in the one section that is supposed to be
the non-overridable floor. The unit tests in `instruction_overrides.rs`
(e.g. `pm_deployed_replaces_body_but_keeps_base_floor`,
`framework_guaranteed_conventions_survive_every_override_combination`) verify
that the *footer/docs/ticket conventions* survive full replacement (they
live in `BASE_PM.md`'s own body) — but nothing verifies that the
Prohibitions table itself survives, because it doesn't; it isn't in
`BASE_PM.md` at all.

> **Accuracy note (2026-08-01).** `PM_INSTRUCTIONS.md` and `BASE_PM.md` as
> monolithic files are gone (#4183); the current bundled manifest is
> [`pm-instruction-package.json`](https://github.com/bobmatnyc/trusty-tools/blob/8abf30962863e143ed405e8d6cabe33f6b0f0b6d/crates/trusty-mpm/src/assets/instructions/pm-instruction-package.json)
> (schema v2), whose `sections[]` each carry an explicit
> `customization_tier`. The Prohibitions table now lives under the heading
> `## Prohibitions (CANONICAL -- single source of truth)` inside
> [`sections/core.md:11`](https://github.com/bobmatnyc/trusty-tools/blob/8abf30962863e143ed405e8d6cabe33f6b0f0b6d/crates/trusty-mpm/src/assets/instructions/sections/core.md#L11)
> — and the `core` section is tagged
> [`"customization_tier": "project"`](https://github.com/bobmatnyc/trusty-tools/blob/8abf30962863e143ed405e8d6cabe33f6b0f0b6d/crates/trusty-mpm/src/assets/instructions/pm-instruction-package.json#L14-L16),
> not `fixed`. This finding's recommendation — promote Prohibitions to
> `fixed` — was **not** adopted; only three sections ship `fixed`:
> `identity`, `non-overridable-rules`, `framework-guaranteed-conventions`
> ([`pm-instruction-package.json:8-10,42-44,48-50`](https://github.com/bobmatnyc/trusty-tools/blob/8abf30962863e143ed405e8d6cabe33f6b0f0b6d/crates/trusty-mpm/src/assets/instructions/pm-instruction-package.json#L8-L10)).
> The gap this finding describes is real and current, not merely historical
> — tracked by
> [#4573](https://github.com/bobmatnyc/trusty-tools/issues/4573)
> ("Prohibitions and Circuit Breakers tables are tier 'project' — deletable,
> unguarded at merge", milestone 1.3.2, open). Promoting the tier is a code
> change tracked there, not made by this doc.

### Finding: Mis-tiering #2 (inverse) — safety-critical project rules sit in the least-protected tier

This project's own Parallel Worktree Discipline rules
(`.trusty-mpm/INSTRUCTIONS.md:543-611`, ~68 lines) are destructive-git-op
prevention rules: "the main checkout is inspection-only," forbidding
`git reset --hard` / `git checkout .` / `git stash` / `cargo build` from the
main checkout, mandating every worktree branch off `origin/main`. These are
safety-critical in exactly the sense P1-fixed content should be. Yet they sit
in `.trusty-mpm/INSTRUCTIONS.md` — today's freely project-editable, additive,
whole-file-replaceable prose tier, the *least*-protected location in the
current system. Under the new model this content is a strong `promote to
fixed` candidate (§6 illustrative table), even though it is project-specific
content (most `fixed` units are framework-wide; this shows `fixed` is a
tier a *project* can also populate, not solely a framework prerogative —
see §4b).

### Finding: user-tier override doesn't exist for instruction text

`AGENT_DELEGATION.md`'s own header comment
(`AGENT_DELEGATION.md:3-6`) claims:

```
> Override at project level: .trusty-mpm/AGENT_DELEGATION.md
> Override at user level:    ~/.trusty-mpm/AGENT_DELEGATION.md
```

No code reads any `~/.trusty-mpm/*.md` instruction file — confirmed by
grepping `instruction_overrides.rs` (the entire override-resolution module):
there is no `home_dir()` call, no `dirs::home_dir()`, nothing that resolves a
path outside `project_dir`. The user-level override line is
advertised-but-unimplemented, the same class of gap the historical #381 bug
(cited in this module's own doc comment at `instruction_overrides.rs:1-10`)
was filed to fix for the project level.

**A real precedent for user-tier override does exist for a different
setting**, however: `~/.trusty-mpm/config.toml`'s `[models.agents]` table
(`crates/trusty-mpm/src/core/config.rs:72` `ModelsConfig`, `:79`
`pub agents: HashMap<String, String>`, resolved at `:499`; documented and
demonstrated in `PM_INSTRUCTIONS.md:117-124`). This establishes the
*mechanism* of a real `~/.trusty-mpm/` user-scope file being read and
honored isn't unprecedented in this codebase — it's just never been
extended to instruction *text*, only to model-selection config.

### Finding: resume-time cache-instability

The resolved instruction corpus is recomputed from live disk state on
**every** session resume, not just at first launch — `resolve_pm_prompt`
re-reads `.trusty-mpm/*.md` from whatever is currently on disk each time it
runs, with no persisted snapshot of what a given workstream's PM actually
received at launch. A paused-and-resumed workstream can therefore receive a
byte-different system prompt across resumes (e.g. because a teammate edited
`.trusty-mpm/INSTRUCTIONS.md` in the interim) with zero signal to the
resuming session that anything changed. This silently breaks prompt-cache
continuity for that workstream and undermines the "immutable within a
workstream" goal (§2.3) unless resolution is explicitly pinned at first
launch (§4f).

---

## 4. Proposed Design {#SPEC-PMINSTR-04~draft}

**ID:** SPEC-PMINSTR-04~draft
**Status:** Draft

### 4a. Two independent axes per unit

Every instruction unit carries two axes that must not be conflated:

| Axis | Values | Governs |
|---|---|---|
| `priority` | `P1` (always-injected) / `P2` (situational, on-demand) | **Injection behavior** — is this unit in the base system prompt every turn, or fetched only when relevant? |
| `customization_tier` | `fixed` / `project` / `user` | **Override permission** — who is allowed to override, add, or disable this unit? |

These are genuinely orthogonal, not a 2x2 where one implies the other:

- **P1 + project-customizable**: the 5-phase Workflow sequence
  (`WORKFLOW.md`'s `## Mandatory 5-Phase Sequence`) and the Agent Routing
  table (`PM_INSTRUCTIONS.md:63-77` / `AGENT_DELEGATION.md`'s full table).
  Both are load-bearing every turn (P1) — the PM needs the routing table on
  every delegation decision — yet a project legitimately customizes them
  today (a project with no QA agent might skip phase 4; a project with a
  bespoke agent roster needs its own routing table). Today's
  `.trusty-mpm/AGENT_DELEGATION.md` / `WORKFLOW.md` override files are
  exactly this: P1-injected, project-tier-customizable content.

- **P2 + project-customizable**: Cross-Workstream Coordination
  (`PM_INSTRUCTIONS.md:386-398`, the memory-claim-drawer protocol) and
  ticketing mechanics (`PM_INSTRUCTIONS.md:368-370`). These matter only when
  a multi-agent dispatch or a ticket reference is actually in play this turn
  — not every turn — yet a project can still reasonably override how it
  wants ticketing routed.

A unit's tier is never inferable from its priority, and vice versa; both
must be declared explicitly.

### 4b. Tier ordering

`fixed < project < user`, by increasing permissiveness/breadth of who can
override:

- **`fixed`** — no override mechanism touches this unit at any tier. Analogous
  to today's `BASE_PM.md` intent, but actually enforced per-unit (§4d) rather
  than per-file.
- **`project`** — a project's own override file may `override`/`add`/`disable`
  this unit. **Explicitly excludes user-tier override** — a unit tagged
  `project` cannot be overridden by a personal `~/.trusty-mpm/` config,
  because that would let one operator's personal preference leak into every
  OTHER project they touch, silently, via their own machine config. This is
  the structural fix that would have prevented a hypothetical "I set a
  personal override and it silently changed behavior on a shared/team
  project" class of bug.
- **`user`** — the most permissive: both project-tier AND user-tier
  operations may target it. `user`-tagged units imply project-tier can also
  override them (most-specific-wins), matching the skill-deployment
  precedence convention already established in this codebase
  (`PM_INSTRUCTIONS.md:413`: "Precedence on name collision: **project-custom
  > user-custom > bundled**").

### 4c. JSON schema

Each unit:

```json
{
  "id": "prohibitions-table",
  "priority": "P1",
  "customization_tier": "fixed",
  "title": "Prohibitions (canonical, single source of truth)",
  "source": "PM_INSTRUCTIONS.md#Prohibitions",
  "content_ref": "bundled://pm_instructions/prohibitions.md",
  "enforced_by": {
    "mechanism": "pm_guard hook",
    "code_ref": "crates/trusty-mpm/src/hooks/pm_guard.rs",
    "covers": ["P1", "P5"]
  },
  "unenforced_ids": ["P2", "P3", "P4", "P6", "P8", "P9", "P10", "P11"],
  "tags": ["authority", "delegation", "non-negotiable"],
  "triggers": null
}
```

A P2 unit additionally carries `triggers` (used for the on-demand index,
§5) instead of `null`:

```json
{
  "id": "worktree-discipline",
  "priority": "P1",
  "customization_tier": "fixed",
  "title": "Parallel Worktree Discipline",
  "source": ".trusty-mpm/INSTRUCTIONS.md#Parallel-Worktree-Discipline",
  "content": "🔴 SOURCE OF TRUTH = origin/main:HEAD. ...",
  "tags": ["safety", "destructive-git-ops", "project-specific"]
}
```

```json
{
  "id": "release-workflow-tcc-fda",
  "priority": "P2",
  "customization_tier": "project",
  "title": "macOS Full Disk Access / App Data TCC re-grant after cargo install",
  "source": ".trusty-mpm/INSTRUCTIONS.md#macOS-Full-Disk-Access",
  "content_ref": "project://trusty-mpm/instructions.json#release-workflow-tcc-fda",
  "tags": ["release", "macos", "tcc", "signing"],
  "triggers": ["cargo install", "code signing", "FDA", "daemon restart"]
}
```

`content` is used for short, stable units authored inline; `content_ref` is
used for larger or generated bodies (mirrors today's split between
hand-authored files and `stack_profile_section()`'s generated block).
`enforced_by`/`unenforced_ids` make explicit which prohibitions are actually
backed by a hook (`pm_guard`) versus prose-only — directly actionable for
open question §7.3.

**Overrides block** (a project's or user's own file):

```json
{
  "tier": "project",
  "source_path": ".trusty-mpm/instructions.json",
  "operations": [
    { "op": "override", "id": "agent-routing-table", "content_ref": "..." },
    { "op": "add", "id": "custom-release-note", "priority": "P2", "customization_tier": "project", "content": "..." },
    { "op": "disable", "id": "some-bundled-p2-unit" }
  ]
}
```

### 4d. Merge/resolution algorithm

1. Build the unit set from bundled system defaults.
2. Apply `user`-tier operations — **only to units tagged `user`**.
3. Apply `project`-tier operations — to units tagged `user` OR `project`.
4. Any operation targeting a `fixed` unit is **rejected and warned**, never
   applied, regardless of which file-level mechanism supplied it.

This is the structural fix for **Mis-tiering #1**: a unit's `fixed` tag is
checked *per-unit*, at merge time, independent of which file-level override
mechanism a project supplies. A project's own
`instructions.json` cannot bulk-replace the Prohibitions/Circuit-Breaker
units by supplying a whole-body replacement the way
`PM_INSTRUCTIONS_DEPLOYED.md` does today — because there is no whole-body
replacement primitive anymore, only per-unit operations, and per-unit
operations against `fixed` units are structurally rejected rather than
silently accepted. The authority model can no longer be dropped by a
project-level file, full stop.

### 4e. P2 delivery mechanism

P1 units are always inlined into the resolved system prompt, in their
assigned position. P2 units are **not** inlined — instead exposed via a
short, always-present index (one line per unit: `id` + `title` + trigger
description), with full content fetched on-demand via a mechanism analogous
to how skills already work today: a tool call at the point of need. This is
cache-safe because tool *results* do not need to be part of the stable
cached system-prompt prefix — only the system prompt and tool *definitions*
do.

**Explicit anti-pattern**: dynamically splicing different P2 content into
different agents' *base* system prompts depending on task type. This WOULD
break prompt-cache reuse, because it makes the cached prefix itself vary
per-agent/per-task rather than staying byte-identical across every subagent
in the workstream. P2 content must always arrive via a tool-call boundary,
never via base-prompt splicing.

### 4f. Immutable-per-workstream resolution

Resolve the merged P1 corpus (system + project + user, per §4d) **exactly
once**, at first workstream launch. Persist the resolved bytes — not just
references to the source files — alongside the session/workstream record.
On resume, reuse the persisted snapshot rather than re-running resolution
against current (possibly-changed) disk state. Only re-resolve on an
explicit, visible refresh action (e.g. a hypothetical `tm session
refresh-instructions`), which is a deliberate operator choice, never a
silent resume side-effect.

**Regression test to add** (analogous to this codebase's existing
`pipeline_claude_md_left_byte_identical`-style tests): launch a workstream,
snapshot the resolved P1 prompt, mutate a project override file
(`.trusty-mpm/instructions.json`), resume the workstream, and assert the
resumed prompt is still byte-identical to the ORIGINAL snapshot — not to
what current disk state would now resolve to.

---

## 5. Project-Tier Override Input: `CLAUDE.md` vs. a bespoke `.trusty-mpm/instructions.json` {#SPEC-PMINSTR-05~draft}

**ID:** SPEC-PMINSTR-05~draft
**Status:** Draft

Bob's question: should project-tier overrides in the new design use the
standard `CLAUDE.md` file — the convention Claude Code itself natively
auto-loads — instead of, or alongside, the bespoke JSON file proposed above?
This section investigates the repo's own history with `CLAUDE.md` before
answering.

### 5.1 What issue #2299 / PR #2300 actually found (root cause, not headline)

Issue #2299 traced a real, measured ~11k-tokens/session duplicate-context
load to the repo's root `CLAUDE.md`. The **exact mechanism**:

1. `tm`'s managed-session provisioner
   (`crates/trusty-mpm/src/provisioner/workspace.rs::provision_in`) always
   nests the session worktree under the project root:
   `project_dir/.base/.worktrees/<session-id>/`.
2. Because that worktree checks out the same branch as the main checkout, it
   carries its **own git-tracked copy** of the root `CLAUDE.md`.
3. Claude Code's own native memory loader ascends parent directories and
   auto-loads **every** `CLAUDE.md` file on the ancestor path. For a session
   running inside the nested worktree, that's both `<worktree>/CLAUDE.md`
   (the worktree's own tracked copy) **and** `<project_dir>/CLAUDE.md` (the
   ancestor main checkout's copy) — same content, loaded twice.

Critically, issue #2299 **explicitly ruled out** the hypothesis that
`tm`'s own prompt-injection mechanism was also embedding a copy (which would
have made it a triple-load, and would have implicated trusty-mpm's own
resolver, not just Claude Code's native loader): the investigation checked
the actual stashed prompt (`.trusty-mpm/last-instructions.md`, guaranteed
byte-identical to what `claude --append-system-prompt-file` receives) for a
live session and found **zero occurrences** of the project CLAUDE.md
content. `instruction_pipeline::build_instructions`'s `PipelineOutput.merged`
field did still concatenate the project CLAUDE.md body internally, but
grepping every call site showed it was dead code — never wired into the
actual delivered prompt (issue #382 had already routed the real prompt
through `resolve_pm_prompt` instead). PR #2300 removed that dead code as
regression-proofing, not as the fix — the fix was step 4:

4. **Remove the tracked root `CLAUDE.md` entirely**, moving its content
   (verbatim, `git mv`) to `.trusty-mpm/INSTRUCTIONS.md` — trusty-mpm's own
   existing, harness-agnostic "project rules" convention, already appended
   once by `resolve_pm_prompt`. With no tracked `CLAUDE.md` at either the
   main checkout or the nested worktree, Claude Code's native ancestor loader
   has nothing duplicate to find.

So: **the double-load was caused by Claude Code's own native ancestor-path
auto-load colliding with `tm`'s nested-worktree-under-project-root
provisioning layout — not by content size, and not by trusty-mpm's own
resolver.** #2299 explicitly flagged the underlying structural collision
(nested worktree + native ancestor-loader) as still applying to *any other*
project that keeps a root `CLAUDE.md` under `tm` managed-session
provisioning, and named the real structural fix — provisioning worktrees
outside the project's own directory tree — as a **larger architectural
change, out of scope** for #2299/#2300. It was never actually fixed; it was
sidestepped by removing the one tracked file that triggered it in this repo.

### 5.2 The CI guard: `claude-md-guard.yml`

`.github/workflows/claude-md-guard.yml` ("Root CLAUDE.md must stay
untracked") runs `scripts/check_claude_md_not_tracked.sh` on every push/PR
to main. The script is a pure `git ls-files --error-unmatch CLAUDE.md`
check scoped to the repo-root path only (nested `crates/*/CLAUDE.md` files
are explicitly out of scope and unaffected). If the root `CLAUDE.md` is
ever git-tracked, the build fails. The guard's own comment cites a **prior
regression**, #2647, where the file got re-tracked and the duplicate
loading came back — this is not a hypothetical risk, it has already
recurred once.

### 5.3 What's actually sitting in this worktree's root `CLAUDE.md` right now

The untracked root `CLAUDE.md` present in this session's own worktree is a
30-line stub: a "Project Context" placeholder, a restated Commit & PR
Attribution rule (explicitly noted in its own text as existing "so a
`claude` session launched directly in this project — outside `tm`
orchestration — still sees it"), and a "Preferences" placeholder. This is
`load_or_create_claude_md`'s per-workspace stub-seeding side effect
(referenced in #2299/#2300 as unchanged by that fix) — a tiny,
harness-native scratch file, **not** a copy of the real 714-line project
instruction corpus, which lives entirely and only in the tracked
`.trusty-mpm/INSTRUCTIONS.md`.

This confirms the division of labor already implicit in the current system:
**`.trusty-mpm/INSTRUCTIONS.md` is the one tracked, authoritative source
trusty-mpm's own resolver reads; `CLAUDE.md` is an untracked, thin,
harness-native courtesy stub for a bare, non-`tm`-orchestrated `claude`
session, and must never carry substantive project content or be
git-tracked at the root.**

### 5.4 Answering the design question

**Does using `CLAUDE.md` for the new P1/P2 project-tier override reintroduce
the #2299 bloat?** Partially, and for a reason narrower than content size.
The double-load mechanism is a property of three things being simultaneously
true: (a) the file is git-tracked, (b) it sits at a path Claude Code's
native ancestor-loader recognizes (i.e., literally named `CLAUDE.md`, or
reachable via its native `@import`), and (c) `tm` provisions the session
worktree nested under the project root. Content size is irrelevant to
*whether* the duplication happens — a tiny JSON-in-Markdown file would
double-load exactly as reliably as the original 546-line file did; size only
changed the original's token cost, not the mechanism. So "make it small and
disciplined" does not avoid the collision, it only shrinks the cost of not
avoiding it.

Separately, and importantly: this risk is specific to Claude Code's **own
native harness-level loader**, not to trusty-mpm's resolver. #2299 already
proved trusty-mpm's own prompt-assembly path (`resolve_pm_prompt`) never
double-counted the project CLAUDE.md — the dead `.merged` field never
reached the delivered prompt. A resolver-side read of a project override
file (JSON or otherwise) that trusty-mpm itself controls the read-once
semantics of is not at risk of this class of duplication; the risk is purely
"does Claude Code's OWN unrelated ancestor-scan ALSO independently discover
and load a second tracked copy of literally the same path segment."

**Is there a version of "use CLAUDE.md" that's fine?** Only the version
already in place: `CLAUDE.md` stays untracked, thin, and harness-native —
never the authoritative input to trusty-mpm's own resolver, and never
re-tracked at the repo root. The proposal in the question — CLAUDE.md as
the ONE project-override file, replacing `.trusty-mpm/instructions.json`
entirely, tracked normally, with the CI guard loosened specifically for a
disciplined new JSON format — does not survive scrutiny: the guard's
condition (`git ls-files` sees it) and the loader's condition (Claude Code
walks ancestor directories for anything literally named `CLAUDE.md`) are
both indifferent to whether the tracked content is 714 lines of prose or 40
lines of structured JSON-in-Markdown. Loosening the guard "for a small
disciplined format" would be loosening it based on a property (size) that
has nothing to do with the actual failure condition (tracked + native-named
+ nested-worktree-ancestor-collision) — the exact class of gap this spec is
trying to eliminate elsewhere (advertised behavior that doesn't match the
real mechanism). The one variable that WOULD actually neutralize the risk —
`tm` provisioning workstream worktrees outside the project's own directory
tree, so the ancestor chain never reaches a project-tracked file at two
depths — is precisely the fix #2299 named and explicitly deferred as a
larger, separate architectural change. This spec does not resolve that
follow-up; it is out of scope here (tracked as an implicit dependency of
"revisit CLAUDE.md as project-tier input," should that ever be pursued).

**Is `CLAUDE.md` fundamentally the wrong mechanism regardless of format,
independent of the #2299 mechanics?** Yes, on a second and independent
ground: it is a Claude-Code-specific harness convention, and trusty-mpm's
own design intent is explicitly harness-independent (trusty-mpm ≠
claude-mpm; the `tcode` product line is being built as a harness-independent
implementation *around* trusty-mpm's PM+instructions+subagent model, per
this project's own prior architecture decisions). Making a Claude-Code-only
auto-load file the canonical carrier of trusty-mpm's own project-tier
override semantics would coactively couple a supposedly harness-neutral
mechanism to one specific harness's native conventions — any other harness
trusty-mpm runs atop (a future `tcode` runtime, or any non-Claude agent
runtime) would need either a parallel redundant mechanism or would simply
not see the override at all, unless it separately reimplemented Claude
Code's own ancestor-directory `CLAUDE.md` semantics.

### 5.5 Recommendation

1. **Keep `.trusty-mpm/` (extended with the new unit-addressable JSON
   format, e.g. `.trusty-mpm/instructions.json`) as the ONE tracked,
   authoritative project-tier override input trusty-mpm's own resolver
   reads.** Do not make `CLAUDE.md` the core project-override mechanism, in
   either its current Markdown form or a hypothetical restructured JSON
   form.

   > **Superseded (2026-08-01) by [#4324](https://github.com/bobmatnyc/trusty-tools/pull/4324).**
   > This did not happen. #4324 added named-section markers
   > (`<!-- TRUSTY-MPM: <TOKEN> START v=1 -->` … `END`) read directly out of
   > a project's `CLAUDE.md` by
   > [`claude_md_sections.rs`](https://github.com/bobmatnyc/trusty-tools/blob/8abf30962863e143ed405e8d6cabe33f6b0f0b6d/crates/trusty-mpm/src/core/claude_md_sections.rs#L72),
   > and `CLAUDE.md` is scanned *first* —
   > `HOST_FILES = ["CLAUDE.md", ".trusty-mpm/INSTRUCTIONS.md"]`
   > ([`claude_md_sections.rs:72`](https://github.com/bobmatnyc/trusty-tools/blob/8abf30962863e143ed405e8d6cabe33f6b0f0b6d/crates/trusty-mpm/src/core/claude_md_sections.rs#L72)),
   > with `CLAUDE.md` winning a same-section collision
   > ([`:34`](https://github.com/bobmatnyc/trusty-tools/blob/8abf30962863e143ed405e8d6cabe33f6b0f0b6d/crates/trusty-mpm/src/core/claude_md_sections.rs#L34)).
   > **The direction is also settled, not merely added alongside the old
   > one.** Project customization is named sections in the root `CLAUDE.md`.
   > The five `.trusty-mpm/` per-file overrides (`INSTRUCTIONS.md`,
   > `WORKFLOW.md`, `AGENT_DELEGATION.md`, `MEMORY.md`,
   > `PM_INSTRUCTIONS_DEPLOYED.md`; constants at
   > [`instruction_overrides.rs:52-60`](https://github.com/bobmatnyc/trusty-tools/blob/8abf30962863e143ed405e8d6cabe33f6b0f0b6d/crates/trusty-mpm/src/core/instruction_overrides.rs#L52-L60))
   > remain in the current binary's read paths — nothing has removed them,
   > and this note does not claim otherwise — but
   > [#4286](https://github.com/bobmatnyc/trusty-tools/issues/4286) tracks
   > their removal, not their standing as a co-equal, indefinitely-supported
   > alternative to `CLAUDE.md`.
2. **Do not loosen `claude-md-guard.yml` / `check_claude_md_not_tracked.sh`.**
   The guard's condition is correct and orthogonal to file format or size;
   loosening it for "a small disciplined JSON file" doesn't address the
   actual failure mode and reopens a path to the #2647-class regression.

   > **Superseded (2026-08-03) — see §9.6.** This recommendation was
   > accurate through #4324 (a target project's `CLAUDE.md` override
   > markers never needed the guard loosened). It no longer holds: the
   > owner separately ruled the guard's underlying ban obsolete —
   > "The ban is obsolete. CLAUDE.md can be tracked, it's up to the
   > user." —
   > [#4660](https://github.com/bobmatnyc/trusty-tools/pull/4660) deletes
   > both `.github/workflows/claude-md-guard.yml` and
   > `scripts/check_claude_md_not_tracked.sh`. §9.6 has the full ruling,
   > rationale, and citations.
3. **Preserve the existing untracked-stub division of labor explicitly** as
   a named design principle in the new system, not an incidental side
   effect: `CLAUDE.md` remains the harness-native, untracked, per-workspace
   courtesy file for a bare `claude` session launched outside `tm`
   orchestration; it must never carry the authoritative project-tier
   instruction corpus and must never be git-tracked at a project root that
   `tm` provisions nested worktrees under.
4. **Flag, but do not attempt to resolve here**, the actual structural fix
   that would neutralize the underlying collision entirely — provisioning
   `tm` managed-session worktrees outside the project's own directory tree —
   as a prerequisite follow-up should `CLAUDE.md`-as-project-tier ever be
   revisited. Carried forward as an open question in §7.

---

## 6. Illustrative Before/After Classification Table {#SPEC-PMINSTR-06~draft}

**ID:** SPEC-PMINSTR-06~draft
**Status:** Draft

Condensed from the full section-by-section audit; enough real examples to
make the two axes concrete.

| Source (today) | Content | `priority` | `customization_tier` | Reasoning |
|---|---|---|---|---|
| `PM_INSTRUCTIONS.md:11-34` | Prohibitions table (P1-P11) | P1 | **fixed** | Authority model; today silently droppable via full-body override (Mis-tiering #1) — must become structurally un-droppable |
| `PM_INSTRUCTIONS.md:268-288` | Circuit Breakers table | P1 | **fixed** | Same authority-model class as Prohibitions |
| `PM_INSTRUCTIONS.md:63-77` | Agent Routing quick-reference | P1 | project | Load-bearing every delegation decision, but a project's own agent roster legitimately overrides it |
| `stack_profile_section()` (generated) | Detected-stack routing rule | P1 | fixed (generated, not user-authored) | Must sit adjacent to / above the Agent Routing table, not ~400 lines below it — the direct #4071 fix |
| `PM_INSTRUCTIONS.md:319-326`, `BASE_PM.md:58-61`, `WORKFLOW.md:116-119` | Attribution footer rule | P1 | **fixed** (one copy) | Collapse 3 in-repo restatements to one `enforced_by`-tagged unit; others become a one-line pointer |
| `WORKFLOW.md` — 5-phase sequence | Workflow phases/gates | P1 | project | Load-bearing, but "skip QA," "no research phase," etc. are legitimate per-project overrides today |
| `.trusty-mpm/INSTRUCTIONS.md:543-611` | Parallel Worktree Discipline | P1 | **fixed** (project-populated) | Safety-critical destructive-git-op prevention; Mis-tiering #2 — promote out of freely-editable prose |
| `.trusty-mpm/INSTRUCTIONS.md` — build/test commands, TCC/FDA runbooks, release steps | Reference material | **P2** | project | Situational — needed only when the matching task type is in play |
| `PM_INSTRUCTIONS.md:386-398` | Cross-Workstream Coordination (memory claim drawers) | **P2** | project | Only relevant when dispatching multi-agent work on a shared area |
| `PM_INSTRUCTIONS.md:368-370` | Ticketing Integration | **P2** | project | Only relevant when a ticket reference is in play |
| `.trusty-mpm/INSTRUCTIONS.md` — Abbreviations & Aliases | Project glossary | **P2** (see open question §7.1) | project | Frequently looked up, but pure project data — see open question |
| `AGENT_DELEGATION.md` header comment | Advertised user-level override | n/a today | **user** (once implemented) | Currently advertised-but-unimplemented (no `home_dir()` call); this design makes it real |
| `~/.trusty-mpm/config.toml` `[models.agents]` | Per-agent model override | P1 (consulted every delegation) | **user** | Existing, WORKING precedent for a real user-tier mechanism — the template this design generalizes to instruction text |

---

## 7. Open Questions / Low-Confidence Items {#SPEC-PMINSTR-07~draft}

**ID:** SPEC-PMINSTR-07~draft
**Status:** Draft — explicitly NOT resolved by this document

1. Whether the Abbreviations & Aliases table
   (`.trusty-mpm/INSTRUCTIONS.md#Abbreviations-Aliases`) is P1 or P2 —
   frequently-looked-up but pure project data, and the two pulls (load-bearing
   frequency vs. "just data, fetch on demand") don't obviously resolve one way.

2. Whether the Model Selection Protocol's current lack of mechanical
   enforcement (`PM_INSTRUCTIONS.md:101-127` is prose-only; nothing structurally
   prevents a PM from omitting `model:`) should be fixed via a `pm_guard`-style
   hook rather than, or in addition to, prompt restructuring alone.

3. Whether the Circuit Breakers table should be visually split into
   "hook-backed" (mechanically enforced, e.g. via `pm_guard`) vs. "prose-only"
   entries, rather than kept as one undifferentiated P1 block — the
   `enforced_by`/`unenforced_ids` schema fields (§4c) make this distinction
   representable; whether it should also be *visible* to the PM in the
   rendered prompt is undecided.

4. Whether `WORKFLOW.md`'s worked Example block
   (`WORKFLOW.md#Example`, the pytest transcript) needs to stay P1 as a
   calibration anchor for evidence formatting, or can safely move to P2 as a
   fetch-on-demand reference.

5. Whether the skill-deployment precedence convention
   (`PM_INSTRUCTIONS.md:410-424`: project-custom > user-custom > bundled)
   should be the direct template for the new instruction-JSON precedence
   verbatim, or whether instruction text's `fixed`/`project`/`user` semantics
   (which have no `fixed` analogue in the skill system) need their own,
   independently-designed model.

6. (From §5) The structural fix that would neutralize the `CLAUDE.md`
   native-ancestor-loader collision entirely — provisioning `tm` managed-session
   worktrees outside the project's own directory tree — is named but not
   designed here; it is a prerequisite for ever safely reconsidering
   `CLAUDE.md` as a project-tier override input, and is out of scope for this
   document.

---

## 8. Explicitly Out of Scope {#SPEC-PMINSTR-08~draft}

**ID:** SPEC-PMINSTR-08~draft
**Status:** Draft

**This is a design proposal, not an implementation plan and not an
authorization to build.** No code should be written against this spec
without a separate, explicit go-ahead from Bob, given how foundational a
change to the PM instruction system this represents — it touches the system
prompt every `tm`-orchestrated session receives, the override contract every
existing project's `.trusty-mpm/*.md` files rely on, and the persistence
model for workstream/session resumption. This document records the problem,
the goals, the audit findings, and a proposed shape; it does not sequence
migration, does not specify backward-compatibility handling for existing
`.trusty-mpm/*.md` override files, and does not estimate implementation
cost.

---

## 9. Owner Ruling (2026-08-03): CLAUDE.md Named Sections Are Canonical; the Legacy `.trusty-mpm/` Overrides Are Deprecated {#SPEC-PMINSTR-09~draft}

**ID:** SPEC-PMINSTR-09~draft
**Status:** Ruling — owner-decided, binding (not a design proposal; §8 does not gate this section)
**Ruling date:** 2026-08-03
**Owner:** Bob Matsuoka

Unlike §1-§8, this section is not a design proposal awaiting a go-ahead — it
records a decision the owner has already made, in the same way the
2026-08-01 supersession note at the top of this document records #4324's
shipped reversal of §5.5 recommendation 1. Do not re-litigate it by
re-deriving intent from source comments; the ruling is the authority.

### 9.1 The ruling, verbatim

> "User overrides should now be in CLAUDE.md in named sections.
> [`.trusty-mpm/INSTRUCTIONS.md` being read additively] is a bug. This
> should be in the spec." — Bob, 2026-08-03

This sharpens, rather than merely repeats, the 2026-08-01 supersession note
on §5.5: that note recorded that #4324 shipped named-section markers and
that the *direction* was project customization moving to `CLAUDE.md`, while
explicitly leaving the `.trusty-mpm/` five-file surface as "remains in the
current binary's read paths ... nothing here should be read as claiming
[removal]." This ruling goes further: `.trusty-mpm/INSTRUCTIONS.md`'s
current unconditional, additive read is itself a **defect**, not a
tolerated transitional state. It also supersedes the framing PR #4597 used
("No doc in this PR uses 'deprecated'/'obsolete' language about
[the `.trusty-mpm/` surface]") — this section uses that language
deliberately, on this specific point, per the ruling above.

### 9.2 Canonical surface: named sections in `CLAUDE.md`

The grammar, matched whole-line (`claude_md_sections.rs:22-26`):

```text
<!-- TRUSTY-MPM: <TOKEN> START v=1 -->
…override content, verbatim…
<!-- TRUSTY-MPM: <TOKEN> END -->
```

The nine accepted section tokens, quoted exactly as
`section_token()` spells them (`claude_md_sections.rs:111-121`):

```rust
SectionId::Identity => "IDENTITY",
SectionId::Core => "CORE",
SectionId::Memory => "MEMORY",
SectionId::Search => "SEARCH",
SectionId::Workflow => "WORKFLOW",
SectionId::AgentDelegation => "AGENT-DELEGATION",
SectionId::Enforcement => "ENFORCEMENT",
SectionId::NonOverridableRules => "NON-OVERRIDABLE-RULES",
SectionId::FrameworkGuaranteedConventions => "FRAMEWORK-GUARANTEED-CONVENTIONS",
```

Four of the nine are `fixed` tier (`pm-instruction-package.json:8-56`) and a
marker aimed at one is **always declined and logged**, never applied
(`claude_md_sections.rs::apply_one`, `Rejection::NotOverridable`); the
bundled floor asset states this identically to the PM
(`sections/non-overridable-rules.md:36-40`, quoted verbatim):

> "Four tokens are `fixed` tier and can never be overridden: `IDENTITY`,
> `ENFORCEMENT` (the Prohibitions and Circuit Breakers tables),
> `NON-OVERRIDABLE-RULES`, `FRAMEWORK-GUARANTEED-CONVENTIONS`. A marker
> aimed at one of these is declined and logged as a warning — the bundled
> section stays in force."

The remaining five are `project` tier and may be overridden by a marker
block: `CORE`, `MEMORY`, `SEARCH`, `WORKFLOW`, `AGENT-DELEGATION`.

**Markers are REPLACE, not merge.** `apply_one` (`claude_md_sections.rs`)
replaces the section's first authored block with the override body
wholesale and drops the section's other authored blocks — it does not
append to or blend with the bundled section. Only three of the five
project-tier tokens are addressable at all on the legacy,
`.trusty-mpm/`-forced composition path — `WORKFLOW`, `MEMORY`,
`AGENT-DELEGATION` (`instruction_overrides.rs:327-336`,
`named_workflow`/`named_memory`/`named_delegation`); `CORE` and `SEARCH`
have no slot there and are reported unapplied unless the packaged composer
(the no-legacy-file path) runs.

**Plain, unmarked prose in `CLAUDE.md` is not a section override at all** —
it is read as ordinary project context every session by Claude Code's own
native per-directory memory loader, independent of any marker mechanism.
The bundled floor asset's own table names this as the home for "Project
facts/preferences": *"(none — plain `CLAUDE.md` prose) | Read as project
context every session"* (`sections/non-overridable-rules.md:29`). This is
the relevant row for §9.5's migration path — bulk project content is
unmarked prose, not a marker block.

### 9.3 Deprecated: the five `.trusty-mpm/` override files

| File | Current behavior | Source | Ruling implication |
|---|---|---|---|
| `PM_INSTRUCTIONS_DEPLOYED.md` | Full-body replacement short-circuit: replaces PM body + WORKFLOW + AGENT_DELEGATION + MEMORY; `BASE_PM` floor still appended | `instruction_overrides.rs:52` (constant), `:286-294` (branch) | Deprecated — tracked by #4286 |
| `AGENT_DELEGATION.md` | Replaces the whole delegation section AND suppresses the live agent roster | `:54`, `:338-345` | Deprecated — tracked by #4286 |
| `WORKFLOW.md` | Replaces the whole bundled workflow section | `:56`, `:297`, `:412-414` | Deprecated — tracked by #4286 |
| `MEMORY.md` | Slotted as a delimited addendum block immediately after `PM_INSTRUCTIONS`, not a section replacement | `:58`, `:299`, `:465-467` | Deprecated — tracked by #4286 |
| `INSTRUCTIONS.md` | Dual role: (a) unconditional additive addendum, appended after every other section (`:60`, `:304-306`, `:471-474`); (b) a marker host in its own right (`HOST_FILES[1]`, `claude_md_sections.rs:72`), lower precedence than `CLAUDE.md` on a same-section collision | see §9.4 | Role (a) is the specific defect named in the 2026-08-03 ruling (§9.1) — deprecated, tracked by #4286 |

None of the five has been removed from the current binary's read paths —
#4286 ("Retire the project-level `.trusty-mpm/` PM instruction override
mechanism," open, milestone 1.3.2) tracks that removal. The bundled floor
asset already tells every PM never to create a new one
(`sections/non-overridable-rules.md:57-60`, quoted verbatim):

> "The `.trusty-mpm/` override files (`.trusty-mpm/INSTRUCTIONS.md`,
> `.trusty-mpm/AGENT_DELEGATION.md`, `.trusty-mpm/WORKFLOW.md`,
> `.trusty-mpm/MEMORY.md`, `.trusty-mpm/PM_INSTRUCTIONS_DEPLOYED.md`) are
> still read by the current binary; #4286 removes them — never create
> one."

So the PM-facing floor asset already states this ruling's direction
correctly — the gap is in the override-resolution code, not in what the
PM is told (§9.4).

### 9.4 The gap between spec/floor-asset and implementation

The override-resolution code still, unconditionally:

- Reads `.trusty-mpm/INSTRUCTIONS.md` — `instruction_overrides.rs:60`
  (`pub const FILE_INSTRUCTIONS: &str = "INSTRUCTIONS.md";`)
- Strips marker blocks from it and treats the remainder as an addendum —
  `instruction_overrides.rs:304-306`:
  ```rust
  let addendum = read_override(&dir, FILE_INSTRUCTIONS)
      .map(|body| crate::core::claude_md_sections::strip_marker_blocks(&body))
      .filter(|body| !body.is_empty());
  ```
- Appends that addendum into every resolved prompt, unconditionally, with
  no gate, no warning, and no `tm doctor` check —
  `instruction_overrides.rs:471-474`:
  ```rust
  // Additive project rules.
  if let Some(extra) = addendum {
      sections.push(extra);
  }
  ```
- Emits **no deprecation signal anywhere** in the read path. Verified:
  `grep -rni "deprecat" crates/trusty-mpm/src | grep -i instructions`
  returns zero hits against this file or its call sites (the only two
  matches in the crate are unrelated — the deprecated generic `ops` agent,
  and a CLI alias note in `tracing_setup.rs`).

**Measured cost** (verified against this worktree, which is `origin/main`
at the time of this ruling): `.trusty-mpm/INSTRUCTIONS.md` in this repo is
**49,699 bytes** (~12,425 estimated tokens at bytes/4) — the largest single
sub-piece of a 1.3.2 launch's compiled prompt — appended in full, every
session, with no opt-out.

**Tracked by:** #4286 (open, milestone 1.3.2), which now carries this owner
ruling and its supporting evidence directly, posted 2026-08-03. A duplicate
issue (#4659) covering the same defect was filed and same-day closed as a
duplicate of #4286 with the comment "Duplicate of #4286. Posting owner
ruling and evidence there instead." — reference **#4286 only**; #4659 is
closed and superseded.

### 9.5 Migration path for a project currently using `INSTRUCTIONS.md`

**This is not a token-reduction migration — only the delivery channel
changes.** Content volume is unchanged: a measured overlap audit found only
~4.7% duplication (2,350 of 49,699 bytes) between `INSTRUCTIONS.md` and the
bundled sections, and zero contradictions between the two. A project
migrating should not expect to shrink its per-session context by doing so,
and should not treat the migration as license to drop content as
"already covered" — nearly all of it is genuinely project-specific and
must move, not be deleted.

The move is **whole-content, as plain prose — not marker-wrapped**:

1. Copy `.trusty-mpm/INSTRUCTIONS.md`'s content into the project's
   `CLAUDE.md` as ordinary, **unmarked** prose (§9.2's "Project
   facts/preferences" row). Do **not** wrap bulk project content (build
   commands, conventions, pitfalls, environment setup) in a marker block:
   markers are REPLACE semantics against exactly one of the five
   project-tier sections (§9.2), so wrapping, say, the whole file in a
   `WORKFLOW` marker would **delete** the entire bundled workflow section
   (the 5-phase templates, QA gate table, security review, and publish
   workflow) rather than merge with it. Reserve a marker block for the
   narrow case of genuinely wanting to replace one of the five addressable
   sections wholesale (e.g., a materially different phase sequence) — that
   is what markers are for, not the bulk-content vehicle.
2. Once in `CLAUDE.md`, the content is read every session by Claude Code's
   own native per-directory memory loader — independently of `tm`'s
   `.trusty-mpm/` resolver, which is exactly what
   `instruction_pipeline.rs:474-478` and `:564-569` already state (quoted
   in §9.6).
3. Verify with `tm sessions instructions` (or read
   `.trusty-mpm/last-instructions.md`), which reports every applied,
   declined, and shadowed marker on stderr — this answers "did my move
   land" and "did a marker I did add get accepted."
4. Delete `.trusty-mpm/INSTRUCTIONS.md` once satisfied. The framework's
   own floor asset already instructs the PM never to re-create it (§9.3,
   `non-overridable-rules.md:57-60`).

### 9.6 The `#2299` tracked-root-`CLAUDE.md` ban is retired

**Ruling, verbatim:** "The ban is obsolete. CLAUDE.md can be tracked, it's
up to the user." — Bob, 2026-08-03

Whether a project tracks its own `CLAUDE.md` is now the project's/user's
choice. The framework takes no position on it and must not enforce one.

**Stated rationale:** issue #2299 (closed 2026-07-10) existed to eliminate
a measured ~11k-tokens/session double-load traced to a tracked root
`CLAUDE.md` (Claude Code's own native ancestor-directory loader reading it
twice — once from a nested session worktree, once from the ancestor main
checkout). The rationale given for retiring the ban is that `tm`'s own
prompt composer has, since, deliberately excluded the project `CLAUDE.md`
body from the prompt it builds:

> "Deliberately excludes the project `CLAUDE.md` body (removed as dead
> code — see the module docs): Claude Code loads `CLAUDE.md` natively, and
> the actual launch prompt is built by
> [`crate::core::instruction_overrides::resolve_pm_prompt`], not this
> field." — `instruction_pipeline.rs:474-478`

> "Side effect only: ensure the project CLAUDE.md stub exists so a fresh
> workspace always has a place for project notes. The content is
> deliberately discarded here rather than folded into `merged` — Claude
> Code already memory-loads `CLAUDE.md` natively, and the real launch
> prompt is built by `resolve_pm_prompt`/`build_system_prompt_for`, not
> this pipeline's `merged` output." — `instruction_pipeline.rs:564-569`

> **Citation-drift note.** An earlier draft of this ruling (and the
> parallel bug-issue thread) cited this exclusion at
> `instruction_pipeline.rs:264-265`. That line span was checked against
> the current `origin/main` build of this file and does **not** contain
> this content at the time of this ruling (it falls inside the unrelated
> `base_pm()` function). The two citations quoted above were verified
> directly against this file in this worktree and are the accurate current
> locations; this note exists so the drift is recorded rather than
> silently re-cited.

**Consequence:**

- `scripts/check_claude_md_not_tracked.sh` and
  `.github/workflows/claude-md-guard.yml` — the CI gate enforcing the now
  retired ban — are **obsolete**. Their removal is not this docs-only
  PR's own change: [#4660](https://github.com/bobmatnyc/trusty-tools/pull/4660)
  ("chore(trusty-mpm): retire the root CLAUDE.md tracked-guard", open at
  the time of this writing, checks green/in-progress) deletes both files
  plus the now-dangling `claude_md` case in
  `scripts/check_scan_floor_selftest.sh`'s mutation self-test.
- Issue #2299's state as of this writing: **CLOSED** (closed 2026-07-10,
  for its original fix — removing the tracked root `CLAUDE.md`). Neither
  this ruling nor #4660 changes that state; #4660 deliberately does not
  use an auto-closing keyword against it (verified:
  `closingIssuesReferences` for #4660 is empty), because the issue
  documents a fix that was real and remains historically accurate — only
  the *enforcement* built on top of it is what's retiring. Do not write
  or imply that #2299 is open, or that it was (re-)closed by this
  ruling or by #4660; the recommended follow-up is an owner-authored
  comment annotating it as superseded, not a state change.
- Do not reference #4659 — it was filed in error as a duplicate of #4286
  and is closed.

### 9.7 Obsolete: the "BASE_PM" framing — there is no separate floor tier

**Ruling, verbatim:** "we don't use BASE_PM any more. All instructions
begin in instructions/sections, any overrides are in named sections in
CLAUDE.md" — Bob, 2026-08-03.

**Verified current state (checked directly, not taken from the ruling's
description):**

- No standalone `BASE_PM.md` file exists anywhere in the tree
  (`find . -iname "BASE_PM*"` returns nothing). This matches this
  document's own §3 accuracy note: monolithic `PM_INSTRUCTIONS.md` /
  `BASE_PM.md` files were removed by #4183.
- The compiled prompt **still literally renders** a "BASE_PM" heading and
  framing, sourced from `sections/identity.md:1-3` (the `identity`
  section, `fixed` tier, `pm-instruction-package.json:8-10`):
  ```markdown
  # BASE_PM Framework Floor

  > Always appended to PM prompt. Cannot be overridden.
  ```
- The code still names the function that concatenates the four `fixed`
  sections (`Identity`, `Enforcement`, `NonOverridableRules`,
  `FrameworkGuaranteedConventions`) `base_pm()`
  (`instruction_pipeline.rs:246-266`), and roughly thirty test assertions
  across `instruction_overrides.rs`, `instruction_pipeline_tests.rs`,
  `bundled_pm_package_tests.rs`, `claude_md_sections_tests.rs`, and
  `session_launch/tests.rs` assert on the literal string
  `"# BASE_PM Framework Floor"`.
- **Correction to an earlier characterization of this issue.** It was
  claimed that the "Customizing PM Behavior" table inside
  `non-overridable-rules.md` (rendered as part of the same `base_pm()`
  concatenation) still instructs the PM to CREATE the five legacy
  `.trusty-mpm/*.md` files on trigger phrases like
  "remember/always/never/for this project." Checked directly against this
  worktree's `origin/main`: that is **not currently true**. Commit
  `0aec8866` ("floor docs point at CLAUDE.md named sections, dedupe
  attribution footer", #4592) already rewrote that trigger-phrase table —
  every trigger now maps to `CLAUDE.md` (plain prose or a marker block,
  §9.2), and the section explicitly instructs "never create one"
  regarding the five legacy files (quoted in full at §9.3). The remaining
  obsolete element is the **"BASE_PM" branding and floor framing itself**
  (the heading, the "Always appended ... Cannot be overridden" tagline,
  and the `base_pm()` name), not the customization table's content, which
  is already correct.

**The canonical model this ruling states:**

- All instructions originate in
  `crates/trusty-mpm/src/assets/instructions/sections/` — there is no
  separate "BASE_PM" tier alongside it.
- Project overrides are named sections in `CLAUDE.md` (§9.2).
- Non-overridable rules are **not retired** by this ruling — only the
  "BASE_PM" label/framing is. The Prohibitions and Circuit Breakers
  tables (`sections/enforcement.md`) and the Framework-Guaranteed
  Conventions (`sections/framework-guaranteed-conventions.md` — the
  commit/PR attribution footer, proportional-documentation policy, and
  ticket-attribution convention) remain fully binding, exactly as
  `CustomizationTier::Fixed` already enforces them (§9.2): a marker aimed
  at `IDENTITY`, `ENFORCEMENT`, `NON-OVERRIDABLE-RULES`, or
  `FRAMEWORK-GUARANTEED-CONVENTIONS` is still always declined. The
  mechanism that makes these rules non-overridable does not need a
  separate "floor" concept layered on top of `fixed` tier — `fixed` tier
  already is that guarantee.

**Deviation, tracked as follow-up, not fixed here:** removing the
"BASE_PM Framework Floor" heading and tagline from `sections/identity.md`,
renaming `base_pm()`, and updating the ~30 test assertions that pin the
literal string is a **code change**. Per this ruling's own scope note, it
lands in the #4286 core PR, not this docs-only one. This section records
the target model and the current gap; it does not perform the rename.

## 10. Owner Ruling (2026-08-04): the Compiled Prompt Owns a Distinct Path, Written Before Launch {#SPEC-PMINSTR-10~draft}

Issue [#4752](https://github.com/bobmatnyc/trusty-tools/issues/4752). This
section supersedes any earlier text in this document that treats
`~/.trusty-mpm/framework/instructions/INSTRUCTIONS.md` as the home of the
compiled PM system prompt.

### 10.1 The defect

One path carried three roles at once:

- `instruction_pipeline.rs`'s `install_system_prompt` / `install_system_prompt_to`
  wrote the **full compiled prompt** to `framework/instructions/INSTRUCTIONS.md`.
- `bundle_all.rs` used to write a **4-line stub** to that same path. (Already
  removed by #4286 split A — see §10.4; the `ALL` table and the
  `FRAMEWORK_INSTRUCTIONS` constant no longer carry the entry.)
- `build_instructions` **reads** that path as an optional framework section —
  i.e. the compiled output was simultaneously a pipeline input.

Last writer won, so the artifact an operator inspects to answer "what
instructions is my session actually running?" was non-deterministic. #383 fixed
one instance of this by ordering the writes; ordering is not a fix when the real
problem is that two roles share one name.

### 10.2 The ruling

- The compiled prompt is written **per project**, to
  `<project>/.trusty-mpm/framework/INSTRUCTIONS-COMPILED.md` (§10.6(b)).
  Resolved by `instruction_pipeline::compiled_prompt_path(project_dir)`; the
  filename constant is `instruction_pipeline::COMPILED_PROMPT_FILE`.
  `framework/instructions/` stays the bundled-input directory.
- **Nothing else writes that path.** Every writer goes through one function,
  `write_compiled_prompt_to`, so the file can only ever hold a full compiled
  prompt — never a stub. `tm install` writes no compiled prompt at all
  (§10.6(b)).
- A pre-#4752 `instructions/INSTRUCTIONS.md` is a **stale leftover**: no writer
  can refresh it, yet `build_instructions` would keep reading it. `tm install`
  removes it (`remove_stale_bundled_instructions`).

### 10.3 Ordering: the write is fatal, and it blocks the spawn

**Ruling (2026-08-04, revised after the #4759 review):** the compiled prompt is
**per project**, at `<project>/.trusty-mpm/framework/INSTRUCTIONS-COMPILED.md`,
and a failure to write it **refuses the launch**.

Owner's reasoning, verbatim: *"If we can't write a simple file to this
directory, we have a bigger issue."* That holds because `.trusty-mpm/` is a
directory `tm` already writes on every launch (it holds `last-instructions.md`
and `sessions/`), and because the prompt file `--append-system-prompt-file`
points at is already a mandatory write — a session that cannot write its own
prompt cannot launch. The compiled file is a second copy alongside a write that
is fatal by necessity, so a loud, diagnosable refusal beats a session running
against a silently stale artifact.

**Where the fatal write happens — both pre-spawn provisioning steps:**

| Path | Fatal write | Error |
|---|---|---|
| start / connect / in-project / cloned | `session_launch::prepare_session_inner`, as its LAST step | `PrepError::CompiledPrompt` |
| resume / guided-resume / crash recovery | `daemon::managed_routes::lifecycle::resume_managed`, immediately before `spawn_resume` | `ResumeManagedError::Other` |

`resume_managed` needs its own write because it never calls `prepare_session*`
on its healthy path (`resume_self_heal` → `ensure_status_line` →
`ensure_deployment_complete` → `spawn_resume`).

**Only this one preparation failure is fatal.** #2149 deliberately made
preparation non-fatal so a roster or skill deploy hiccup could not stop a
session launching, and that still holds: `PrepError::is_fatal()` returns `true`
for `CompiledPrompt` and nothing else. All seven spawning call sites consult it
— `commands/launch.rs::connect`, the managed-clone block in
`commands/launch.rs`, `commands/meta/launch.rs`,
`commands/session/start.rs::start_session_in_place`,
`client/http_client/session_connect.rs`,
`daemon/managed_routes/lifecycle.rs::prepare_inproject_session` (now
`Result`-returning, propagated by both in-project spawns), and
`provisioner/workspace.rs` (→ `ProvisionError::PrepareSession`). A blanket
"every prep error aborts" would have reversed #2149 wholesale.

**Operator-facing failure.** A refusal never surfaces a bare `io::Error`.
`instruction_pipeline::compiled_prompt_failure_message` is the single formatter:
it names the path, carries the cause, states that the session was NOT started,
and points at permissions/free space. Pinned by
`compiled_prompt_failure_message_names_the_path_and_a_remedy`.

**Position is load-bearing and must stay last.** An earlier revision placed the
write immediately after the `last-instructions.md` stash, upstream of
`write_output_style`, `write_project_hooks`, the workspace trust pre-seed, and
all four MCP injectors — including `inject_trusty_mpm_mcp` /
`inject_trusty_review_mcp`, the content-pinning defense against the
#3918/#3950 MCP name-squatting class. That was a security regression: a failed
write skipped the injectors while the (then non-fatal) callers launched anyway.
Under a FATAL write the placement matters more, not less — a refusal must not
also mean provisioning was half-applied. Pinned by
`compiled_write_failure_does_not_skip_the_mcp_injectors`.

**One best-effort write remains, deliberately.**
`runtime::claude_code::build_prompt_file` refreshes the file from the exact
bytes it hands to `--append-system-prompt-file`. It is NOT fatal, because it
sits *inside* the spawn, past the provisioning gate — every path reaching it has
already had a fatal compiled write succeed, so it is a belt-and-braces refresh,
never the sole guarantee. Making it fatal would also invert that function's own
priority: a failure of the strictly more important write beside it (the actual
system-prompt file) already degrades to spawning without it (#2173). Refusing to
launch over the inspection copy while shrugging at the real prompt would be
backwards.

**Cost.** Every write reuses an already-composed string; none adds a composition
pass. Not benchmarked — no no-regression claim is made.

### 10.4 Corrections to earlier characterizations of #4752

Recorded because the issue text and this document previously said otherwise, and
both were checked directly against the code rather than taken from the report:

- **The `bundle_all.rs` stub was already gone.** #4286 split A removed the
  `instructions/INSTRUCTIONS.md` entry from `bundle::ALL` and deleted the
  `FRAMEWORK_INSTRUCTIONS` constant it backed (`bundle_tests.rs`,
  `bundle_table_is_complete`, pins `ALL.len() == 178`). Finding #1 in
  `sections/README.md` described a two-writer collision whose first writer no
  longer existed. The surviving half of the collision — compiled output sharing
  a path with a pipeline input — is real, and is what §10.2 fixes.
- **`FrameworkPaths::framework_instructions` / `framework_instructions_path`
  are NOT dead** and were retained: `session_launch::prepare_session_inner` and
  `commands/session.rs` both still construct a `PipelineInput` from them.

### 10.5 CLAUDE.md pointer — deferred, not delivered

The ruling that a project's `CLAUDE.md` should point at
`INSTRUCTIONS-COMPILED.md` is **not implemented here**, superseded mid-flight by
a second ruling: **all `CLAUDE.md` modification is reserved to the scaffolding
agent**, which owns that file exclusively.

Two code paths mutate a project `CLAUDE.md` today, neither of which is that
agent — recorded as a defect to resolve separately, not fixed here:

- `instruction_pipeline::load_or_create_claude_md` seeds `CLAUDE_MD_STUB` on
  first session start (reached via `build_instructions`).
- `session_launch::worktree_sync::self_heal_claude_md` strips the legacy #2170
  delegation block on every session resume, via `strip_delegation_block`.

The scaffolding path itself is the `tm-init` skill, which is agent-driven prose
— it has no code component and no knowledge of the
`<!-- TRUSTY-MPM: <TOKEN> START v=1 -->` marked-block format. Teaching it to
author marked section-override blocks is unbuilt capability, and where the
pointer lives is an open owner decision.

### 10.6 Rulings on the two questions this change raised (2026-08-04)

Both were referred to the owner during the #4759 review and are now settled.

**(a) Should a failed compiled write block the spawn? RULED: yes.** The
implementer's proposal to keep it non-fatal was overruled. That proposal reasoned
about the OLD global path; under project-local scoping it does not hold — see
§10.3 for the reasoning and the implementation.

**(b) Per-project or per-session? RULED: per project, project-local.** Not
`framework/compiled/<slug>.md` under the global framework dir, which was the
implementer's proposal. The file lives inside the project's own `.trusty-mpm/`,
beside `last-instructions.md` and `sessions/`. That removes the collision **by
construction** rather than by slug disambiguation: no shared root, no `$HOME`
special case for managed workspaces, no slugs to collide. Owner: *"it's only
used on startup, shouldn't be an issue to update it on startup."*

Two consequences worth recording:

- **The accessor moved off `FrameworkPaths`.** That type models the global
  framework INSTALL layout; an accessor there would have to choose between the
  shared managed root and the real `$HOME` — the very ambiguity that caused the
  collision. The compiled prompt is now resolved by
  `instruction_pipeline::compiled_prompt_path(project_dir)`.
- **`tm install` no longer writes a compiled prompt at all.** Install has no
  project, so it has nothing meaningful to compile; writing the bundled assembly
  to a shared path was exactly the "wrong content kind on a shared path"
  collision this issue closes. This eliminates the second writer rather than
  relocating it. `tm install` still deletes a stale pre-#4752
  `instructions/INSTRUCTIONS.md`.
