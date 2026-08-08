---
name: tm-workflow
description: Manage and customize the trusty-mpm PM workflow via named-section overrides in the project's CLAUDE.md
user-invocable: true
version: "1.0.0"
category: pm-workflow
tags: [workflow, customization, phase-configuration, verification-gates, agent-routing]
effort: medium
---

# /tm-workflow

trusty-mpm has no separate "workflow engine" a project configures at
runtime — the PM's phase/gate behavior comes from the bundled instruction
package, and a project customizes it via named-section marker blocks in its
root `CLAUDE.md`. This skill documents that mechanism, which is implemented
(not aspirational) in `core/instruction_overrides.rs`,
`core/instruction_pipeline.rs`, and `core/claude_md_sections.rs`.

The project-level `.trusty-mpm/{INSTRUCTIONS,AGENT_DELEGATION,WORKFLOW,MEMORY,
PM_INSTRUCTIONS_DEPLOYED}.md` per-file override system described in older
docs is RETIRED (#4286) — no code reads those files anymore, and a leftover
one fails `tm doctor`'s `legacy_overrides` check. `CLAUDE.md` is the sole
project customization surface.

## How the PM Prompt Is Assembled

The bundled PM prompt has one source of truth: the JSON manifest
`assets/instructions/pm-instruction-package.json` (schema v2), embedded at
compile time via `bundled_pm_package.rs`. It declares section order and
composition; the prose for each section is stored separately in
`assets/instructions/sections/*.md`, pulled in as `include_str!` constants
registered in the `SECTION_SOURCES` table (`core/instruction_pipeline.rs`)
— a missing section file is a compile error, not a launch-time surprise.
The nine marker tokens (`core/claude_md_sections.rs::section_token`) are
`IDENTITY`, `CORE`, `MEMORY`, `SEARCH`, `WORKFLOW`, `AGENT-DELEGATION`,
`ENFORCEMENT`, `NON-OVERRIDABLE-RULES`, and
`FRAMEWORK-GUARANTEED-CONVENTIONS`. `CORE` is the only one a project cannot
replace; every other section, including `NON-OVERRIDABLE-RULES` and
`FRAMEWORK-GUARANTEED-CONVENTIONS`, can be overridden — there is no separate
"floor" concept anymore (the `is_floor()`/`instruction_floor.sha256` machinery
was itself retired by #4286, being the appearance of a control rather than a
control a project-owned `CLAUDE.md` can actually enforce).

At session start, `core/instruction_overrides.rs::resolve_pm_prompt` (reached
via `build_system_prompt_for*`) composes the final prompt. It is not a file a
user edits — it's composed fresh per launch. See "Inspecting the Resolved
Prompt" below for where that composed prompt is stashed.

A project customizes any non-`CORE` section one way: a named-section marker,
`<!-- TRUSTY-MPM: <TOKEN> START v=1 -->` … `<!-- TRUSTY-MPM: <TOKEN> END -->`,
in the project's root `CLAUDE.md` — the sole marker host
(`core/claude_md_sections.rs::HOST_FILES`). This replaces exactly the
matching section, nothing else.

The five legacy per-file overrides
(`.trusty-mpm/{INSTRUCTIONS,AGENT_DELEGATION,WORKFLOW,MEMORY,
PM_INSTRUCTIONS_DEPLOYED}.md`) are RETIRED (#4286) and are never read. Never
create one; if a project still has one, move its contents into `CLAUDE.md`
(plain prose, or a marker block for a specific section) and delete the file
— `tm doctor`'s `legacy_overrides` check fails until it is gone.

Robustness: a missing `CLAUDE.md`, a missing marker block, or an empty marker
body all fall back silently to the bundled default — a customization attempt
never blanks a section or crashes launch.

The agent-delegation roster is DYNAMIC, not authored prose: it comes from
`deployed_roster_section` → `roster_from_dirs`, a union of the project tier,
`$CLAUDE_CONFIG_DIR/agents`, and `~/.claude/agents`, rendered by
`generate_authority`. It is non-droppable — `validate_roster` rejects a
package where the roster generator is optional or absent (#4069).

## Making a Change

Trigger phrases the PM should act on immediately — all of them land in the
project's root `CLAUDE.md`:

| User says | PM writes to |
|---|---|
| "remember/always/never/for this project" | Plain prose in `CLAUDE.md` (no marker needed) |
| "use X agent for Y" / "route/change agent" | `<!-- TRUSTY-MPM: AGENT-DELEGATION START v=1 -->` block in `CLAUDE.md` |
| "add/change workflow phase" | `<!-- TRUSTY-MPM: WORKFLOW START v=1 -->` block in `CLAUDE.md` |
| "memory behavior" | `<!-- TRUSTY-MPM: MEMORY START v=1 -->` block in `CLAUDE.md` |

After writing an override: confirm the marker (or the added prose) to the
user and note it "takes effect at next session startup" — the resolved
prompt is only assembled at session-prepare time, not hot-reloaded
mid-session.

## Inspecting the Resolved Prompt

Never guess what the PM is actually running under. Two equivalent ways to
check:

```bash
tm sessions instructions       # prints the resolved prompt on stdout
cat .trusty-mpm/last-instructions.md   # the exact stash resolve_pm_prompt wrote
```

`tm sessions instructions` reports every applied, declined, and shadowed marker
on **stderr**, so `tm sessions instructions >/dev/null` alone answers "why
didn't my override apply?".

`last-instructions.md` is written by `prepare_session` every time a prompt is
assembled, specifically so the inspectable copy can never diverge from what
the PM actually received (issue #382). `tm doctor`'s `instructions` probe
checks for this file's presence as a proxy for "has the pipeline run".

## The Bundled 5-Phase Model

The bundled workflow section (`assets/instructions/sections/workflow.md`,
replaceable via the override above) documents Research → Code Analysis review →
Implementation → QA → Documentation. **Every phase is CONDITIONAL**: it runs
unless its skip condition holds, and the instruction package's CORE phase table
is canonical for that decision. Where a phase runs, its gate is blocking —
"conditional" governs entry, never rigour (#4594). This is genuinely tm's
bundled default, not a claude-mpm holdover — but a project is free to replace
the whole section via a `WORKFLOW` named-section marker in `CLAUDE.md` if its
delivery process differs (e.g. no Code Analysis gate, an extra Security phase, a
different QA routing rule).

### Dispatch Briefs Per Phase

**Phase 1 — Research** (`research`). Required for ambiguous requirements,
multiple possible approaches, or an unfamiliar codebase. Skipped when the user
gave an explicit command or the task is simple operational work
(start/stop/build/test).

```
Task: Analyze requirements for [feature]
Return: Technical requirements, gaps, measurable criteria, approach
```

Output: requirements, constraints, success criteria, risks.

**Phase 2 — Code Analysis** (`code-analyzer`, sonnet — NOT `code-critic`, which
is a separate agent).

```
Task: Review proposed solution
Use: think/deepthink for analysis
Return: Approval status with specific recommendations
```

Decision: APPROVED → Implementation. NEEDS_IMPROVEMENT → back to Research.
BLOCKED → escalate to the user.

**Phase 3 — Implementation** (the language-specific engineer where one exists).
Requirements: complete code, error handling, basic test proof, and a changelog
entry for the changed package — a per-PR fragment file if the project uses one,
otherwise its `CHANGELOG.md`. Skip only for docs-only/CI-only changes.

**Phase 4 — QA.** Routing: `api-qa` for APIs, `web-qa` for UI, `qa` otherwise.
Requirements: real-world testing with evidence. The gate itself is
`tm-verification-protocols`.

**Phase 5 — Documentation** (`documentation`). Output: updated docs, API specs,
README. Skipped for an internal refactor with no public API change.

## Sprint, then Harden — the Rest of the Doctrine

The instruction package states the two phases, where to spend the verification
budget, and the hard line (never turn red green by deleting coverage). Two
derived rules live here because they only apply at a specific moment:

- **A branch that has drawn 3+ review rounds is evidence to close and fold**,
  not to attempt round 4. Worked example: #4202 → #4207.
- **Branch = workstream, and it is durable. Worktree = writer, and it is
  ephemeral.** Keep worktrees short-lived; keep branches workstream-scoped.

The causal claim behind the doctrine: slow feature release *causes* too many
things in flight — it is not a separate problem. Shortening time-to-land is the
fix; managing WIP count directly (caps, purges) treats the symptom.

## Override Commands

The user can bypass a gate by saying so explicitly:

| User says | Effect |
|---|---|
| "Skip workflow" | Bypass the phase sequence |
| "Go directly to [phase]" | Jump to that phase |
| "No QA needed" | Skip phase 4 (not recommended) |
| "Emergency fix" | Bypass Research |

Honour the override and name the bypassed gate in the completion report, so the
missing evidence is visible rather than implied.

## Verification Gates

Regardless of which phases are overridden, the verification-gate contract in
`tm-verification-protocols` is not itself an override target — it is a
project-independent invariant enforced by CB#8. A custom `WORKFLOW` override
can change *when* QA runs but not *whether* a completion claim requires
evidence.

## Related Skills

- `tm-delegation-patterns` — the agent-selection matrices this workflow model routes into
- `tm-circuit-breaker` — CB#5 (Delegation Chain) enforces phase completeness
- `tm-verification-protocols` — the QA evidence standard every phase gate uses
- `tm-agent-architecture` — how the agents this workflow delegates to are built
