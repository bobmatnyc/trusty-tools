# tm-delegation-patterns — sizing and batching

## Task Complexity: One Delegation or Several

Size the task before choosing a shape. The signals:

| Signal | Simple (1 delegation) | Complex (multi-phase) |
|--------|----------------------|----------------------|
| Scope | <200 lines, 1 file type | >500 lines, multi-service |
| External deps | None or 1 framework | DB + APIs + Docker + scheduler |
| Endpoints | ≤6 | >6 with auth, roles, events |
| Time estimate | <30 min | >1 hour |

**Simple → ONE engineer delegation carrying the full scope:** "Build this, write
tests, create README, run linters, verify all tests pass, commit." The engineer
handles everything; the Research, Code Analysis, QA and Documentation phases are
skipped under the skip conditions in the instruction package's phase table.

**Complex → the normal multi-phase workflow.**

## Batching: the Anti-Patterns

The instruction package states the target (5-7 delegations per session, ~95K of
context reloaded by each). These are the specific splits to collapse:

| Anti-pattern | Fix |
|---|---|
| Research then implement (2 delegations) | `engineer` can research + implement (1) |
| Implement then fix lint (2) | Include "fix lint" in the impl task (1) |
| Implement then commit (2) | Include "commit when done" in the task (1) |
| Sequential fixes to the same agent (N) | One delegation with full scope (1) |
| A separate docs agent for a per-task README | Include the README in the engineer delegation |
| One PR per issue for same-module bugs | Group by crate: one dispatch/PR per crate cluster (≤~5 issues), one worktree, one gate run; each issue keeps its own regression test and `Refs #N` line |

Bug fixes and small changes are grouped by crate into one engineer dispatch
per crate cluster, up to about five issues per cluster, with one worktree and
one gate run after every fix in the batch lands. A per-issue dispatch is the
exception: reserve it for issues that collide on the same file, or for a
High-risk fix that needs its own `code-critic` round. Each fix inside a batch
still carries its own regression test and `Refs #N` line. A fix that turns
out to need a different crate, or a cross-crate contract change, is reported
back rather than absorbed into the batch (owner ruling 2026-09-16).
