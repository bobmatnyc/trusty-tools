---
name: tm-workflow
description: Manage and customize the trusty-mpm PM workflow via project-level .trusty-mpm/ override files
user-invocable: true
version: "1.0.0"
category: pm-workflow
tags: [workflow, customization, phase-configuration, verification-gates, agent-routing]
effort: medium
---

# /tm-workflow

trusty-mpm has no separate "workflow engine" a project configures at
runtime — the PM's phase/gate behavior comes from the bundled instruction
package, and a project customizes it via marker blocks in `CLAUDE.md` or
override files in `<project>/.trusty-mpm/`. This skill documents that
mechanism, which is implemented (not aspirational) in
`core/instruction_overrides.rs`, `core/instruction_pipeline.rs`, and
`core/claude_md_sections.rs`.

## How the PM Prompt Is Assembled

The bundled PM prompt has one source of truth: the JSON manifest
`assets/instructions/pm-instruction-package.json` (schema v2), embedded at
compile time via `bundled_pm_package.rs`. It declares section order and
composition; the prose for each section is stored separately in
`assets/instructions/sections/*.md`, pulled in as `include_str!` constants
registered in the `SECTION_SOURCES` table (`core/instruction_pipeline.rs`)
— a missing section file is a compile error, not a launch-time surprise.
Three sections (identity, non-overridable-rules,
framework-guaranteed-conventions — the "floor") are tier `fixed` and cannot
be overridden by any project.

At session start, `core/instruction_overrides.rs::resolve_pm_prompt` (reached
via `build_system_prompt_for*`) composes the final prompt. It is not a file a
user edits — it's composed fresh per launch. See "Inspecting the Resolved
Prompt" below for where that composed prompt is stashed.

A project can customize the non-floor sections two ways:

| Channel | Where | Effect |
|---|---|---|
| Named-section marker | `<!-- TRUSTY-MPM: <TOKEN> START v=1 -->` … `<!-- TRUSTY-MPM: <TOKEN> END -->` in `CLAUDE.md` or `.trusty-mpm/INSTRUCTIONS.md` (first host wins, `core/claude_md_sections.rs`) | Replaces exactly that section |
| Per-file override | `.trusty-mpm/INSTRUCTIONS.md` (additive) / `AGENT_DELEGATION.md` / `WORKFLOW.md` / `MEMORY.md` / `PM_INSTRUCTIONS_DEPLOYED.md` (full-body replacement) | Replaces the matching section, or the whole body |

Both channels are live. `CLAUDE.md` is the surface project customization is
consolidating onto (#4183/#4286), but the five per-file overrides still work
exactly as before, and a same-section per-file override always wins over a
named marker for that section.

**The floor is never overridable.** Even `PM_INSTRUCTIONS_DEPLOYED.md`'s
full-body replacement still gets the floor appended last — a hard invariant
enforced by `resolve_pm_prompt`, not a convention.

Robustness: a missing `.trusty-mpm/` directory, a missing override file, an
empty override file, or an unreadable override file all fall back silently
to the bundled default (with a `tracing::warn!` for the empty/unreadable
cases) — a customization attempt never blanks a section or crashes launch.

The agent-delegation roster is DYNAMIC, not authored prose: it comes from
`deployed_roster_section` → `roster_from_dirs`, a union of the project tier,
`$CLAUDE_CONFIG_DIR/agents`, and `~/.claude/agents`, rendered by
`generate_authority`. It is non-droppable — `validate_roster` rejects a
package where the roster generator is optional or absent (#4069).

## Making a Change

Trigger phrases the PM should act on immediately:

| User says | PM writes to |
|---|---|
| "remember/always/never/for this project" | `.trusty-mpm/INSTRUCTIONS.md` |
| "use X agent for Y" / "route/change agent" | `.trusty-mpm/AGENT_DELEGATION.md` |
| "add/change workflow phase" | `.trusty-mpm/WORKFLOW.md` |
| "memory behavior" | `.trusty-mpm/MEMORY.md` |

After writing an override: confirm the file path to the user and note it
"takes effect at next session startup" — the resolved prompt is only
assembled at session-prepare time, not hot-reloaded mid-session.

## Inspecting the Resolved Prompt

Never guess what the PM is actually running under. Two equivalent ways to
check:

```bash
tm session instructions        # prints the resolved prompt
cat .trusty-mpm/last-instructions.md   # the exact stash resolve_pm_prompt wrote
```

`last-instructions.md` is written by `prepare_session` every time a prompt is
assembled, specifically so the inspectable copy can never diverge from what
the PM actually received (issue #382). `tm doctor`'s `instructions` probe
checks for this file's presence as a proxy for "has the pipeline run".

## The Bundled 5-Phase Model

The bundled workflow section (`assets/instructions/sections/workflow.md`,
replaceable via the override above) documents: Research (conditional) →
Code Analysis review (mandatory gate) → Implementation → QA (mandatory
gate) → Documentation, with skip conditions. This is genuinely tm's bundled
default, not a claude-mpm holdover — but a project is free to replace the
whole section via a `WORKFLOW` override (named marker or
`.trusty-mpm/WORKFLOW.md`) if its delivery process differs (e.g. no Code
Analysis gate, an extra Security phase, a different QA routing rule).

## Verification Gates

Regardless of which phases are overridden, the verification-gate contract in
`tm-verification-protocols` is not itself an override target — it is a
project-independent invariant enforced by CB#8. A custom `WORKFLOW.md` can
change *when* QA runs but not *whether* a completion claim requires evidence.

## Related Skills

- `tm-delegation-patterns` — the agent-selection matrices this workflow model routes into
- `tm-circuit-breaker` — CB#5 (Delegation Chain) enforces phase completeness
- `tm-verification-protocols` — the QA evidence standard every phase gate uses
- `tm-agent-architecture` — how the agents this workflow delegates to are built
