# Research & Decision Documents — trusty-review

Investigation docs, decision documents, audits, and design specs that inform the
engineering trajectory of `trusty-review`.

These are distinct from `regression-testing/` (NUMBER snapshots per release) and
`sessions/` (NARRATIVE engineering trajectory).

## Naming Convention

- Investigation / audit / design docs: `<topic>-<YYYY-MM-DD>.md`
- Decision documents: `<topic>-decision-<YYYY-MM-DD>.md`

## Documents

- [`map-reduce-review-design-2026-06-03.md`](map-reduce-review-design-2026-06-03.md)
  — Design spec for per-file map-reduce review (map each changed file to an
  independent review, reduce into one synthesized verdict). DRAFT.
