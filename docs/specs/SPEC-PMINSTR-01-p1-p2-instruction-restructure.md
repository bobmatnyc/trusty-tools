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
**Last-updated:** 2026-07-27
**Spec ID:** `SPEC-PMINSTR-01~draft` … `SPEC-PMINSTR-07~draft` (DOC-59)
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
> `check_claude_md_not_tracked.sh`) was **not** contradicted — that guard
> still runs unmodified and still only checks that *this repo's own*
> root `CLAUDE.md` stays untracked (issue #2299/#2647 self-referential
> concern); it was never a guard against a target project's `CLAUDE.md`
> carrying override markers, so #4324 shipping a target-project override
> reader required no change to it.
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

   > **Still accurate.** The guard
   > (`.github/workflows/claude-md-guard.yml`,
   > `scripts/check_claude_md_not_tracked.sh`) runs unmodified and was never
   > loosened. It checks only that *this repo's own* root `CLAUDE.md` stays
   > untracked (the #2299/#2647 duplicate-context-load concern); it does not
   > and never did restrict a *target* project's `CLAUDE.md` from carrying
   > override markers, so #4324 required no change to it. Recommendation 1
   > above is the one superseded — this one was not.
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
