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
assets, and a project customizes it by dropping override files into
`<project>/.trusty-mpm/`. This skill documents that mechanism, which is
implemented (not aspirational) in `core/instruction_overrides.rs` +
`core/instruction_pipeline.rs`.

## How the PM Prompt Is Assembled

`core/instruction_pipeline.rs` bundles four compile-time assets:
`PM_INSTRUCTIONS` (the canonical prohibitions/CB/QA-gate/workflow
summary), `WORKFLOW` (the 5-phase execution detail), `AGENT_DELEGATION`
(the routing table), and `BASE_PM` (the non-overridable floor). At session
start, `core/instruction_overrides.rs::resolve_pm_prompt` reads
`<project>/.trusty-mpm/` for override files and layers them onto those
bundled defaults:

| Override file | Effect | Replaces |
|---|---|---|
| `.trusty-mpm/INSTRUCTIONS.md` | **Appended** (additive) after the PM body | nothing — pure addition |
| `.trusty-mpm/AGENT_DELEGATION.md` | **Replaces** the bundled agent-routing section | `AGENT_DELEGATION.md` |
| `.trusty-mpm/WORKFLOW.md` | **Replaces** the bundled workflow-phase section | `WORKFLOW.md` |
| `.trusty-mpm/MEMORY.md` | **Slotted** as a delimited block right after PM_INSTRUCTIONS | (no standalone bundled memory asset) |
| `.trusty-mpm/PM_INSTRUCTIONS_DEPLOYED.md` | **Full replacement** of the entire PM body — short-circuits WORKFLOW/AGENT_DELEGATION | everything except `BASE_PM.md` |

**`BASE_PM.md` is never overridable.** Even a full `PM_INSTRUCTIONS_DEPLOYED.md`
replacement gets the `BASE_PM` floor appended last — this is a hard
invariant enforced by `resolve_pm_prompt`, not a convention.

Robustness: a missing `.trusty-mpm/` directory, a missing override file, an
empty override file, or an unreadable override file all fall back silently
to the bundled default (with a `tracing::warn!` for the empty/unreadable
cases) — a customization attempt never blanks a section or crashes launch.

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
tm sessions instructions        # prints the resolved prompt
cat .trusty-mpm/last-instructions.md   # the exact stash resolve_pm_prompt wrote
```

`last-instructions.md` is written by `prepare_session` every time a prompt is
assembled, specifically so the inspectable copy can never diverge from what
the PM actually received (issue #382). `tm doctor`'s `instructions` probe
checks for this file's presence as a proxy for "has the pipeline run".

## The Bundled 5-Phase Model

The default `WORKFLOW.md` (replaceable via the override above) documents:
Research (conditional) → Code Analysis review (mandatory gate) →
Implementation → QA (mandatory gate) → Documentation. See
`PM_INSTRUCTIONS.md`'s `## Workflow (5-phase)` table for the condensed
version and skip conditions. This is genuinely tm's bundled default, not a
claude-mpm holdover — but a project is free to replace the whole section via
`.trusty-mpm/WORKFLOW.md` if its delivery process differs (e.g. no Code
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
