# Architecture Decision Records (ADRs)

This directory holds the **workspace-wide** Architecture Decision Records for
trusty-tools — a **first-class documentation artifact, peer to Specs (DOC-38)
and Requirements (DOC-43, future)**.

See **[DOC-46](../specs/DOC-46-adr-standard.md)** for the formal ADR standard,
including the consistency-vetting protocol and governance rules.

## What is an ADR?

An ADR captures *why* a single architecturally-significant decision was made:
the context that forced the decision, the decision itself, and its consequences.
ADRs are immutable once accepted — a decision is changed by writing a *new* ADR
that amends or supersedes the old one, never by editing its normative history.
The old record's Status and backlink metadata are updated to point at the new
ADR; its Context, Decision, and Consequences remain historical. We use the
[Nygard format](https://cognitect.com/blog/2011/11/15/documenting-architecture-decisions)
(Title, Status, Context, Decision, Consequences) plus a **"Related Decisions"**
section documenting consistency vetting (see DOC-46 §3).

ADR-0001..0013 predate DOC-46's modern metadata and Related Decisions
template. They are structurally grandfathered: their historical sections are
not rewritten for template conformity, while their statuses, successor links,
numbering, and index entries remain governed. ADR-0014 and later must satisfy
the current template fields and core-section checks.

## When to write one (the bar)

Write an ADR when a decision is **architecturally significant *and* costly to
reverse**. Examples: choosing an IPC protocol, a credential-routing model, an
MSRV/edition policy, where documentation lives, defining service boundaries
between crates. Do **not** write an ADR for routine implementation choices,
reversible refactors, bug fixes, or anything a code review comment would cover.
If in doubt, ask: *would a future maintainer be confused about why we did this,
and would undoing it be expensive?* If yes, write one.

**ADRs are not optional** for architectural decisions — they are mandatory. The
decision itself is required; recording it formally is also required. This ensures
consistency vetting happens before decisions land.

## Hybrid scope rule

- **Workspace-wide decisions** (affecting multiple crates or the whole repo)
  live **here**, in `docs/adr/`.
- **Crate-specific decisions** live in **`docs/<crate>/decisions/`** — e.g.
  [`docs/trusty-agents/decisions/`](../trusty-agents/decisions/).

A crate-specific ADR may reference a workspace ADR, and vice versa.

## Numbering & filenames

`NNNN-kebab-title.md`, zero-padded to four digits, unique and monotonically increasing
within the directory. Workspace ADRs and each crate's `decisions/` directory
maintain **independent** numbering sequences. Never renumber an existing ADR.

## Status lifecycle

```
Proposed ──► Accepted ──► Superseded by linked NNNN
       └────► Rejected

       ┌────────────────┐
       │ Amended by NNN │
       └────────────────┘
       (refined, not replaced, by later ADR)
```

- **Proposed** — drafted, under discussion. Consistency vetting is optional while Proposed, but required before acceptance.
- **Accepted** — agreed and in force. All Accepted ADRs form the current decision set.
- **Rejected** — considered but not adopted (kept for the record).
- **Superseded by linked NNNN** — replaced by a later ADR; the Status value
  links to the new ADR. Old decision no longer in force.
- **Amended by linked NNNN[, NNNN…]** — refined (not replaced) by one or more later
  ADRs. The prior decision remains in force together with every listed amendment.

## Writing a new ADR

1. Copy [`template.md`](./template.md) to the next free `NNNN-kebab-title.md`.
2. Fill in Title, Status, Context, Decision, Consequences.
3. **Critical: Add a "Related Decisions" section** (see DOC-46 §3) — sweep `docs/adr/INDEX.md` and prior decisions; record verdict codes (Consistent, Extends, Supersedes, Conflict) for each relevant prior ADR. **Do not accept an ADR with an empty "Related Decisions" section.**
4. Open it as **Proposed**; flip to **Accepted** when the decision is agreed and consistency vetting is complete.
5. Run `bash scripts/check_adr.sh` before opening the PR. The check enforces
   numbering, status grammar, index parity, successor links, and required
   consistency-vetting sections.

See [`INDEX.md`](./INDEX.md) for the current decision corpus and a quick reference for vetting.

## Index

See [`INDEX.md`](./INDEX.md) for the complete, machine-readable index of all ADRs
(Accepted, Proposed, Rejected, and Superseded). Use it as a quick reference for
consistency vetting when writing new ADRs.
