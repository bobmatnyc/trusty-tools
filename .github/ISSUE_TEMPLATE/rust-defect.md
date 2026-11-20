---
name: Rust Defect
about: A defect or invariant violation in a Rust crate — outcome-scoped, not per failing test
title: "[DEFECT] "
labels: bug
---

<!--
File ONE issue per independently prioritizable behavior or invariant — not per
failing test, module, crate touched, reviewer observation, or implementation
step. Group failures that share a root cause and acceptance test.

Before filing, search open and recently closed issues by test name, panic text,
affected symbol, and crate. If a canonical issue exists, add this occurrence
there instead of opening a new one.
-->

## Outcome / impact

What user or system behavior is wrong or desired?

## Confidence

<!-- Delete the rest. Inferred/Speculative usually belongs on the parent
issue/PR as a note, not as a standalone ticket, unless severity justifies it. -->

Observed | Reproduced | Inferred | Speculative

## Evidence / reproduction

Minimal command, inputs, failure signature, and affected SHA/environment.
Attach or link raw logs for failures, flakes, and performance claims.

## Root-cause relationship

Duplicate search performed? Same root cause as another symptom? Parent/epic?

## Acceptance criteria

<!--
**Acceptance criteria.** Every bug, feature, task and epic issue states one to
four testable acceptance criteria, each a check that can fail for a named
reason; an epic states them on its phase issues. They set the good-enough bar:
the issue closes when each criterion has evidence. Anything beyond the bar
becomes a follow-up issue filed at close, with its own criteria and the
`stretch` label; it never widens this issue's scope. The closing note lists
every criterion with its evidence, plus the follow-up issue numbers or "none".
-->

Externally observable behavior and the regression test required for closure.

## Test level

Targeted | crate | dependents | workspace | integration/e2e

---

**Labels:** add `P0`/`P1`/`P2` and the affected crate label (e.g. `trusty-search`).
