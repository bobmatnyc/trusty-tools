---
name: tm-adr
description: Architecture Decision Records (ADRs) — formal first-class documentation artifact for significant, hard-to-reverse architectural decisions with consistency vetting
user-invocable: true
version: "2.0.0"
category: documentation
tags: [documentation, architecture, adr, governance]
effort: medium
---

# /tm-adr

Architecture Decision Records (ADRs) are a **first-class documentation artifact**,
peer to Specs (DOC-38) and Requirements (DOC-43, future). They capture *why* a significant
architectural decision was made — not just what was decided. They live alongside
the code so future contributors understand the constraints and trade-offs that
shaped the system.

**ADRs are formal and mandatory** for architectural decisions. When a decision is
significant enough to shape the system, it is significant enough to record. The
decision itself is required; creating a record of it is also required.

See **[DOC-46 — ADR Standard](../../docs/specs/DOC-46-adr-standard.md)** for the
complete formal specification, including the consistency-vetting protocol, governance,
and CI gates.

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
| `Accepted` | Adopted — current approach; consistency vetting complete |
| `Rejected` | Considered but not adopted (kept for record) |
| `Superseded by NNNN` | Replaced by a later ADR (link to it) |
| `Amended by NNNN` | Refined (not replaced) by a later ADR (link to it) |

Update the old ADR's status when superseded or amended; never delete or rewrite
history. All status changes are tracked in the ADR file and git history.

---

## ADR Template

```markdown
# NNNN. Title (short, imperative: "Use X for Y")

- **Status:** Proposed | Accepted | Rejected | Superseded by NNNN | Amended by NNNN
- **Date:** YYYY-MM-DD
- **Scope:** Workspace-wide (or: crate `<name>` or subsystem `<name>`)
- **Reversibility Cost:** (Low | Medium | High) — cost to undo this choice
- **Decision Drivers:** (comma-separated: "MSRV constraint, performance ceiling, cross-crate boundary")
- **Supersedes / Superseded by:** — (link if applicable)

## Context

What is the issue that is motivating this decision? Describe the forces at
play: technical constraints, workspace constraints, product requirements,
and alternatives considered. Be factual.

## Decision

The change being proposed or agreed. State it clearly in active voice:
"We will use X."

## Consequences

What becomes easier or harder as a result? List both positive and negative
consequences, including known risks. Be honest about trade-offs — this
section is the most valuable part.

## Related Decisions

**Vetting required before acceptance.** Sweep `docs/adr/INDEX.md` and prior ADRs;
record verdict codes (Consistent, Extends, Supersedes, Conflict) for each affected
prior decision (see DOC-46 §3).

Vetted against prior ADRs on [DATE]:

- **ADR-NNNN (Title):** [Consistent | Extends | Supersedes | Conflict(resolved)] — explanation
- ...

If no prior decisions are affected: "No prior decisions to vet against."
```

---

## Workflow

### Creating a new ADR

1. Copy `docs/adr/template.md` to `docs/adr/NNNN-your-title.md` with the next
   sequential number.
2. Fill in Context, Decision, and Consequences. Set Status to `Proposed`.
3. **Critical: Add "Related Decisions" section** — sweep `docs/adr/INDEX.md` and
   prior ADRs; record verdict codes (Consistent, Extends, Supersedes, Conflict)
   for each affected prior decision. **Do not submit an ADR with an empty
   "Related Decisions" section.**
4. Submit for review (PR per the worktree-discipline convention).
5. Reviewer checks: Does "Related Decisions" vetting look complete? Are there
   any silent contradictions? If all clear, approval proceeds.
6. Once agreed, change Status to `Accepted`.
7. Commit with: `docs: ADR-NNNN adopt X for Y — vetting complete; [brief vetting summary]`.

### Superseding an ADR

1. Create a new ADR (`MMMM-new-approach.md`) with Status `Accepted`.
2. In the new ADR's "Related Decisions", document: **Supersedes ADR-NNNN.**
3. Edit the old ADR's Status to `Superseded by [MMMM](MMMM-new-approach.md)`.
4. Commit both files together: `docs: ADR-MMMM supersedes ADR-NNNN (reason)`.

### Amending an ADR

1. Create a new ADR (`MMMM-refinement.md`) with Status `Accepted`.
2. In the new ADR's "Related Decisions", document: **Amends ADR-NNNN.**
3. Edit the old ADR's Status to `Amended by [MMMM](MMMM-refinement.md)`.
4. Commit both files together: `docs: ADR-MMMM amends ADR-NNNN (reason)`.

---

## References

- **[DOC-46 — ADR Standard](../../docs/specs/DOC-46-adr-standard.md)** — the formal spec
- Michael Nygard, "Documenting Architecture Decisions" (2011)
- `docs/adr/README.md` — convention doc for this repo
- `docs/adr/INDEX.md` — index of all ADRs (consistency vetting surface)
- `docs/adr/template.md` — the actual template file to copy
