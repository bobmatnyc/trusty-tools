# Architecture Decision Records (ADRs)

This directory holds the **workspace-wide** Architecture Decision Records for
trusty-tools — a **first-class documentation artifact, peer to Specs (DOC-38)
and Requirements (DOC-43)**.

See **[DOC-31](../specs/DOC-31-adr-standard.md)** for the formal ADR standard,
including the consistency-vetting protocol and governance rules.

## What is an ADR?

An ADR captures *why* a single architecturally-significant decision was made:
the context that forced the decision, the decision itself, and its consequences.
ADRs are immutable once accepted — a decision is changed by writing a *new* ADR
that supersedes the old one, never by editing history. We use the
[Nygard format](https://cognitect.com/blog/2011/11/15/documenting-architecture-decisions)
(Title, Status, Context, Decision, Consequences) plus a **"Related Decisions"**
section documenting consistency vetting (see DOC-31 §3).

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
  [`docs/open-mpm/decisions/`](../open-mpm/decisions/).

A crate-specific ADR may reference a workspace ADR, and vice versa.

## Numbering & filenames

`NNNN-kebab-title.md`, zero-padded to four digits, monotonically increasing
within the directory. Workspace ADRs and each crate's `decisions/` directory
maintain **independent** numbering sequences. Never renumber an existing ADR.

## Status lifecycle

```
Proposed ──► Accepted ──► Superseded by NNNN
       └────► Rejected

       ┌────────────────┐
       │ Amended by NNN │
       └────────────────┘
       (refined, not replaced, by later ADR)
```

- **Proposed** — drafted, under discussion. Consistency vetting is optional while Proposed, but required before acceptance.
- **Accepted** — agreed and in force. All Accepted ADRs form the current decision set.
- **Rejected** — considered but not adopted (kept for the record).
- **Superseded by NNNN** — replaced by a later ADR; links to the new ADR. Old decision no longer in force.
- **Amended by NNNN** — refined (not replaced) by a later ADR. Prior decision still in force, but qualified by the amendment.

## Writing a new ADR

1. Copy [`template.md`](./template.md) to the next free `NNNN-kebab-title.md`.
2. Fill in Title, Status, Context, Decision, Consequences.
3. **Critical: Add a "Related Decisions" section** (see DOC-31 §3) — sweep `docs/adr/INDEX.md` and prior decisions; record verdict codes (Consistent, Extends, Supersedes, Conflict) for each relevant prior ADR. **Do not accept an ADR with an empty "Related Decisions" section.**
4. Open it as **Proposed**; flip to **Accepted** when the decision is agreed and consistency vetting is complete.

See [`INDEX.md`](./INDEX.md) for the current decision corpus and a quick reference for vetting.

## Index

See [`INDEX.md`](./INDEX.md) for the complete, machine-readable index of all ADRs
(Accepted, Proposed, Rejected, and Superseded). Use it as a quick reference for
consistency vetting when writing new ADRs.
| [0014](./0014-native-mcp-support.md) | Ship full native MCP support (ticketing, gworkspace, Slack/Telegram, and more) | Accepted |
| [0015](./0015-three-product-agent-composition-model.md) | Unified agent composition: shared `.md`+YAML+`extends` format across trusty-agents, trusty-mpm, trusty-code | Proposed |
