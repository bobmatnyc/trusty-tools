---
name: tm-capabilities
description: Auto-generated exhaustive harness capability catalog — every tm CLI command, MCP tool, bundled agent, bundled skill, and doctor check, verbatim and always current. Complements (does not replace) the conceptual `tm` skill.
user-invocable: false
version: "1.0.0"
category: pm-reference
tags: [reference, generated, cli, mcp, agents, skills, doctor]
effort: low
---

# tm-capabilities — Generated Harness Capability Catalog

> **AUTO-GENERATED — do not hand-edit.** This file and its `references/
> {cli,mcp-tools,agents,skills,doctor}.md` siblings are produced by
> `tm generate capabilities` from the harness's own in-process data (clap's
> command-tree introspection, the MCP tool catalog, the bundled agent/skill
> roster, and a maintained doctor-check list cross-checked against
> `run_doctor`'s real output). A CI drift gate
> (`scripts/check_capabilities.sh`, wired into `.github/workflows/`) fails
> the build if the committed files ever fall out of sync with the generator's
> output. To change what this skill says, change the harness surface it
> describes, then run `tm generate capabilities` and commit the diff.
> `references/workflows.md` is the one exception — it is hand-authored.

## What This Is

An exhaustive, exact reference for "what exact surface exists, verbatim,
right now" — every CLI command, MCP tool schema, agent description, skill
description, and doctor-check meaning. This is deliberately NOT the same
document as the `tm` skill: `tm` is a *conceptual* overview (the delegation
model, memory system, "how it works" prose) for building intuition;
`tm-capabilities` is a *reference* catalog for looking up an exact name,
signature, or meaning. Load `tm` first to understand the system; load the
relevant `references/*.md` file here when you need an exact answer.

## When to Consult Which Reference

| Question | Load |
|---|---|
| "What are the exact `tm <command>` subcommands / flags?" | `references/cli.md` (52 top-level commands) |
| "What MCP tools exist and what parameters do they take?" | `references/mcp-tools.md` (31 tools, plus sibling-daemon pointers) |
| "What agents can I delegate to, and what do they declare?" | `references/agents.md` (37 concrete agents) |
| "What skills exist, and are they user-invocable?" | `references/skills.md` (50 bundled skills) |
| "What does a `tm doctor` check name actually mean?" | `references/doctor.md` (15 checks) |
| "How do the pieces fit into an end-to-end flow?" | `references/workflows.md` (hand-authored, not generated) |

## Relationship to Other Skills

- **`tm`** — conceptual overview; keeps its own prose roster/skill list and
  points here for the exhaustive, generated version (issue #2913 brief §C:
  complement, not supersede — avoids two hand/auto-maintained lists
  disagreeing).
- **`tm-doctor`**, **`tm-cli-operations`**, **`tm-agent-architecture`** —
  narrowly procedural companions; this skill is the data they operate on,
  not a replacement for their guidance.

## Regenerating

```bash
tm generate capabilities          # write the regenerated files
tm generate capabilities --check  # CI drift gate: diff only, nonzero on mismatch
```
