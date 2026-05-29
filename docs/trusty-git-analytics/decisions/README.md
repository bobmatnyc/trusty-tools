# Architecture Decision Records

This directory contains Architecture Decision Records (ADRs) for
`trusty-git-analytics`.

## What is an ADR?

An ADR is a short, immutable document capturing a single significant
architectural decision: the context in which it was made, what was decided,
and the consequences that follow. ADRs are written when the decision is
made, not retrospectively, and are never edited after acceptance — instead,
a superseding ADR is written.

This project uses ADRs because:

- Future contributors should not have to reverse-engineer *why* a library,
  schema, or trade-off was chosen.
- Decisions made under pressure or with incomplete information are easier
  to revisit when the original reasoning is on the record.
- A pull request that touches an architectural seam can link to an ADR
  rather than re-litigate the decision in review comments.

## Format

The project uses a Nygard-style ADR format (the same one used by
[Documenting Architecture Decisions][nygard]):

```markdown
# ADR NNNN: Title

- **Status**: Proposed | Accepted | Deprecated | Superseded by ADR-XXXX
- **Date**: YYYY-MM-DD
- **Deciders**: trusty-git-analytics core team

## Context

What is the problem or opportunity? What forces are at play?

## Decision

What was decided? State it clearly and directly.

## Consequences

### Positive
### Negative
### Neutral

## References
```

See [`TEMPLATE.md`](./TEMPLATE.md) for a copy-paste starter.

## Numbering

ADRs are numbered with a **4-digit zero-padded sequential integer**,
starting at `0001`. Numbers are never reused or reordered. The filename
follows the pattern:

```
NNNN-short-kebab-case-slug.md
```

Examples:

- `0001-sqlite-tuning.md`
- `0002-performance-hotspots.md`
- `0003-llm-provider-selection.md`

To allocate the next number, run `ls docs/adr/` and pick `N+1` where `N`
is the highest existing number. If two ADRs are proposed simultaneously,
the first to merge gets the lower number.

## When to Write an ADR

Write an ADR when the decision is:

- **A library or dependency choice** with non-trivial alternatives
  (e.g. `git2` vs `gitoxide`, `rusqlite` vs `sqlx`).
- **A schema or data-model decision** that future migrations will have
  to live with.
- **A performance or correctness trade-off** (e.g. `synchronous = NORMAL`
  in SQLite — see ADR 0001).
- **An architectural seam** — module boundary, error-handling convention,
  async runtime choice.
- **A "no" decision** worth recording — e.g. *not* adding a connection
  pool, *not* supporting 32-bit targets.

Do **not** write an ADR for routine implementation choices, bug fixes,
or reversible refactors. If in doubt, ask whether a future contributor
would otherwise have to reverse-engineer the reasoning. If yes, write
the ADR.

## Lifecycle

1. Copy `TEMPLATE.md` to `NNNN-slug.md` with the next number.
2. Set **Status** to `Proposed`. Fill in Context, Decision, Consequences.
3. Open a pull request. Discussion happens in PR review.
4. On merge, change **Status** to `Accepted`. Do not edit the body after
   acceptance.
5. If a later ADR overrides this one, set **Status** to
   `Superseded by ADR-XXXX` and add a back-reference in the superseding
   ADR's References section. Never delete the old ADR.

## Existing ADRs

| ADR | Title | Status |
|-----|-------|--------|
| [0001](./0001-sqlite-tuning.md) | SQLite Tuning Pragmas | Accepted |
| [0002](./0002-performance-hotspots.md) | Performance Hotspots | Accepted |

[nygard]: https://cognitect.com/blog/2011/11/15/documenting-architecture-decisions
