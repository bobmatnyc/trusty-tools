---
name: tm-workflow
description: Manage and customize the trusty-mpm PM workflow via CLAUDE.md named-section overrides
user-invocable: true
version: "2.0.0"
category: pm-workflow
tags: [workflow, customization, phase-configuration, verification-gates, agent-routing]
effort: medium
---

# /tm-workflow

trusty-mpm has no separate "workflow engine" a project configures at
runtime — the PM's phase/gate behavior comes from the bundled instruction
sections, and a project customizes it by marking out a **named section**
inside its own `CLAUDE.md`. This skill documents that mechanism, which is
implemented (not aspirational) in `core/claude_md_sections.rs` +
`core/bundled_pm_package.rs` + `core/instruction_overrides.rs`.

## How the PM Prompt Is Assembled

`core/instruction_pipeline.rs` embeds one markdown source per section under
`assets/instructions/sections/`: `identity.md`, `core.md`, `memory.md`,
`search.md`, `workflow.md`, `agent-delegation.md`,
`non-overridable-rules.md`, `framework-guaranteed-conventions.md`. (The four
monolithic assets this crate used to embed — `PM_INSTRUCTIONS.md`,
`WORKFLOW.md`, `AGENT_DELEGATION.md`, `BASE_PM.md` — are gone as of #4183.)

`core/bundled_pm_package.rs` declares those sections as a typed
`InstructionPackage` carrying a `customization_tier` per section, plus an
ordered block stream that composes to the delivered prompt. Two blocks are
*generated* at composition time rather than authored: the detected stack
profile and the LIVE deployed-agent roster.

At session start `core/instruction_overrides.rs::resolve_pm_prompt` scans the
project for named-section overrides (`core/claude_md_sections.rs`) and applies
them to that package.

## Making a Change: a Named Section in `CLAUDE.md`

Marker grammar, matched whole-line. Both markers must name the same section:

```text
<!-- TRUSTY-MPM: WORKFLOW START v=1 -->
# Workflow (project override)

Two phases only: implement, then verify.
<!-- TRUSTY-MPM: WORKFLOW END -->
```

Everything strictly between the marker lines becomes that section, trimmed.
Text outside markers is ordinary `CLAUDE.md` prose and is ignored by this
mechanism. `v=1` is the only format version this build implements; omitting
`v=` is accepted as v=1 with a warning. Tokens match case-insensitively.
Blocks do not nest — a `START` while a block is open discards the outer one.

| Section token | Replaces | Tier |
|---|---|---|
| `CORE` | Core PM instructions | project |
| `MEMORY` | Memory protocol | project |
| `SEARCH` | Code search protocol | project |
| `WORKFLOW` | Workflow phases | project |
| `AGENT-DELEGATION` | The routing doctrine only | project |
| `IDENTITY` | — refused | fixed |
| `NON-OVERRIDABLE-RULES` | — refused | fixed |
| `FRAMEWORK-GUARANTEED-CONVENTIONS` | — refused | fixed |

An `AGENT-DELEGATION` override replaces the authored routing doctrine but
**cannot** suppress the live agent roster: the roster is a *generated* block,
and `InstructionPackage::with_overrides` only ever replaces authored text.
That asymmetry is deliberate (#4196 in override form).

Overridability is decided by the package's own `customization_tier` — the
reader holds no second list — which is what makes the floor structurally
unreachable from `CLAUDE.md` rather than merely unlisted.

Host files, highest precedence first:

| Host | Notes |
|---|---|
| `<project>/CLAUDE.md` | The surface #4183 moves customization to; wins a same-section collision |
| `<project>/.trusty-mpm/INSTRUCTIONS.md` | Also the additive project addendum; a block marked out here is delivered as the override, not twice |

**The PM does not write a project's `CLAUDE.md` unasked.** When the user wants a
persistent project rule, show them the marked block and let them place it.
Overrides take effect at the NEXT session start — the prompt is assembled at
session-prepare time, never hot-reloaded mid-session.

## Nothing Can Blank a Section

Every failure degrades toward MORE framework instruction, never toward a blank
section or a failed launch. An absent or unreadable host, a `START` with no
`END`, an `END` naming a different section, an unknown token, an unsupported
`v=`, an empty body, a duplicate section, and an override aimed at the floor
all keep the bundled section and log the reason
(`core/claude_md_sections.rs::REASON_*`).

## Legacy `.trusty-mpm/` Override Files (Deprecated)

Projects that already carry `.trusty-mpm/PM_INSTRUCTIONS_DEPLOYED.md`,
`AGENT_DELEGATION.md`, `WORKFLOW.md`, or `MEMORY.md` keep working — those files
are still read. They are deprecated and **must not be created**: while any of
them is present, `resolve_pm_prompt` falls back to the unsectioned legacy
string assembly, so named-section overrides cannot be applied. Each one is then
logged as unapplied (`claude_md_sections::warn_unapplied`) rather than dropped
in silence — an advertised-but-unread override is issue #381.

`.trusty-mpm/INSTRUCTIONS.md` is unaffected: it stays the additive addendum and
composes through the packaged path normally.

The packaged path also requires a deployed-agent roster to consume, so a project
with no agents deployed at all composes through the legacy assembly too. Check
the `resolved the PM system prompt` log line for which composer ran.

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

The committed snapshots in `crates/trusty-mpm/src/core/testdata/pm-prompt-*.md`
show the fully composed prompt for each configuration, including one with
`CLAUDE.md` named sections applied.

## The Bundled 5-Phase Model

The bundled `workflow.md` section (replaceable via a `WORKFLOW` named section)
documents: Research (conditional) → Code Analysis review (mandatory gate) →
Implementation → QA (mandatory gate) → Documentation. See the Core section's
`## Workflow (5-phase)` table for the condensed version and skip conditions.
This is genuinely tm's bundled default, not a claude-mpm holdover — but a
project is free to replace the whole section if its delivery process differs
(e.g. no Code Analysis gate, an extra Security phase, a different QA routing
rule).

## Verification Gates

Regardless of which phases are overridden, the verification-gate contract in
`tm-verification-protocols` is not itself an override target — it is a
project-independent invariant enforced by CB#8. A custom `WORKFLOW` section can
change *when* QA runs but not *whether* a completion claim requires evidence.

## Related Skills

- `tm-delegation-patterns` — the agent-selection matrices this workflow model routes into
- `tm-circuit-breaker` — CB#5 (Delegation Chain) enforces phase completeness
- `tm-verification-protocols` — the QA evidence standard every phase gate uses
- `tm-agent-architecture` — how the agents this workflow delegates to are built
