---
name: tm-adr
description: Architecture Decision Records (ADRs) — opt-in convention for documenting significant, hard-to-reverse architectural decisions using the Nygard template
user-invocable: true
version: "1.0.0"
category: pm-optional
tags: [documentation, architecture, adr, pm-optional]
effort: medium
---

# /tm-adr

Architecture Decision Records (ADRs) are short, structured documents that
capture *why* a significant architectural decision was made — not just what
was decided. They live alongside the code so future contributors understand
the constraints and trade-offs that shaped the system.

This skill is **opt-in**. Only use it for decisions that are architecturally
significant AND costly to reverse. Over-forcing ADRs on routine work is the
documented adoption-killer.

---

## When to Write an ADR

Write an ADR **only** when all three conditions are true:

1. **Architecturally significant** — shapes how major parts of the system are
   structured, constrains future options, or affects multiple crates.
2. **Costly to reverse** — undoing it later requires substantial rework,
   migration, or coordination across crates.
3. **Not obvious from the code** — the rationale is not apparent from reading
   the implementation alone.

**Write an ADR for**: choosing an IPC protocol between daemons, a
credential-routing model, MSRV/edition policy, workspace layout conventions,
adopting a new persistence strategy, defining service boundaries between
trusty-* crates, or deprecating a foundational component.

**Do NOT write an ADR for**: routine bug fixes, small features, style/lint
config, decisions already obvious from the codebase, or a library version
bump.

If unsure, ask: "Would a new contributor in six months need to understand
*why* this choice was made?" If no — skip it.

---

## File Location Convention (this repo)

| Scope | Location |
|---|---|
| Workspace-wide decisions | `docs/adr/NNNN-kebab-title.md` |
| Crate-specific decisions | `docs/<crate>/decisions/NNNN-kebab-title.md` (only if that crate maintains its own decisions log) |

`docs/adr/` already exists in this repo (see `docs/adr/README.md` and
`docs/adr/template.md`) with decisions numbered through the 0013 range as of
this writing — always check the actual latest file before picking a number.

---

## Numbering Convention

Sequential, zero-padded four-digit integers. Find the next number:

```bash
ls docs/adr/*.md | grep -E '^docs/adr/[0-9]{4}' | sort | tail -3
```

Copy `docs/adr/template.md` to `docs/adr/NNNN-your-title.md` with the next
available number.

---

## Status Lifecycle

| Status | Meaning |
|---|---|
| `Proposed` | Draft under discussion; not yet adopted |
| `Accepted` | Adopted — current approach |
| `Deprecated` | No longer recommended; superseded or abandoned |
| `Superseded by [NNNN]` | Replaced by a later ADR (link to it) |

Update the old ADR's status when superseded; never delete or rewrite history.

---

## Nygard Template

```markdown
# NNNN. Title (short, imperative: "Use X for Y")

Date: YYYY-MM-DD

## Status

Proposed | Accepted | Deprecated | Superseded by [NNNN](NNNN-replacement.md)

## Context

What is the issue that is motivating this decision? Describe the forces at
play: technical constraints, workspace constraints, product requirements.
Be factual.

## Decision

The change being proposed or agreed. State it clearly in active voice:
"We will use X."

## Consequences

What becomes easier or harder as a result? List both positive and negative
consequences, including known risks. Be honest about trade-offs — this
section is the most valuable part.
```

---

## Workflow

### Creating a new ADR

1. Copy `docs/adr/template.md` to `docs/adr/NNNN-your-title.md` with the next
   sequential number.
2. Fill in Context, Decision, and Consequences. Set Status to `Proposed`.
3. Submit for review (PR per the worktree-discipline convention). Once
   agreed, change Status to `Accepted`.
4. Commit with: `docs: ADR-NNNN adopt X for Y`.

### Superseding an ADR

1. Create a new ADR (`MMMM-new-approach.md`) with Status `Accepted`.
2. Edit the old ADR's Status to `Superseded by [MMMM](MMMM-new-approach.md)`.
3. Commit both files together.

---

## References

- Michael Nygard, "Documenting Architecture Decisions" (2011)
- `docs/adr/README.md` — the authoritative convention doc for this repo
- `docs/adr/template.md` — the actual template file to copy
