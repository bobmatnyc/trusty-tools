# Architecture Decision Records (ADRs)

This directory holds the **workspace-wide** Architecture Decision Records for
trusty-tools.

## What is an ADR?

An ADR captures a single architecturally-significant decision: the context that
forced the decision, the decision itself, and its consequences. ADRs are
immutable once accepted — a decision is changed by writing a *new* ADR that
supersedes the old one, never by editing history. We use the
[Nygard format](https://cognitect.com/blog/2011/11/15/documenting-architecture-decisions)
(Title, Status, Context, Decision, Consequences).

## When to write one (the bar)

Write an ADR when a decision is **architecturally significant *and* costly to
reverse**. Examples: choosing an IPC protocol, a credential-routing model, an
MSRV/edition policy, where documentation lives. Do **not** write an ADR for
routine implementation choices, reversible refactors, or anything a code review
comment would cover. If in doubt, ask: *would a future maintainer be confused
about why we did this, and would undoing it be expensive?* If yes, write one.

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
Proposed ──► Accepted ──► Superseded (by ADR-NNNN)
       └────► Rejected
```

- **Proposed** — drafted, under discussion.
- **Accepted** — agreed and in force.
- **Rejected** — considered but not adopted (kept for the record).
- **Superseded** — replaced by a later ADR; note which one in the Status line.

## Writing a new ADR

1. Copy [`template.md`](./template.md) to the next free `NNNN-kebab-title.md`.
2. Fill in Title, Status, Context, Decision, Consequences.
3. Open it as **Proposed**; flip to **Accepted** when the decision is agreed.

## Index

| ADR | Title | Status |
|---|---|---|
| [0001](./0001-docs-live-top-level.md) | Design/research/ADR docs live in top-level `docs/` | Accepted |
| [0002](./0002-single-install-convention.md) | Single-install convention for main crates | Accepted |
| [0003](./0003-msrv-and-edition-policy.md) | MSRV 1.88 and per-crate Rust edition policy | Accepted |
| [0004](./0004-three-harnesses-shared-event-driven-common.md) | Three distinct harnesses on a shared event-driven trusty-common foundation | Proposed |
| [0005](./0005-harness-event-bus.md) | Shared HarnessEvent envelope + process-global event bus in trusty-agents-common | Accepted |
| [0006](./0006-trusty-controller-naming.md) | Name the stack control plane `trusty-controller` (binary `tctl`) | Accepted |
| [0007](./0007-tool-contract-versioning-and-verb-model.md) | Monotonic-integer `contract_version` + 3-layer extensible verb model | Accepted |
| [0008](./0008-project-identity-convention.md) | Project-identity convention: full-path slug of the nearest git root | Accepted |
