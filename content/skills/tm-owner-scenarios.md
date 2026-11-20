---
name: tm-owner-scenarios
description: Write owner scenarios a non-technical owner can review — intent, situation, observable result, and a trace to the team's test IDs, with no click-by-click steps — and check them with the scenario lint. Use when a plan, spec or acceptance set must be reviewed by an owner who does not read code.
user-invocable: true
metadata:
  version: "0.1.0"
category: pm-workflow
tags: [owner, scenarios, acceptance, non-technical, pm-recommended]
effort: low
---

# tm-owner-scenarios — Outcome Scenarios for a Non-Technical Owner

A click-by-click scenario form once made a month of owner work and would have
frozen the screens before they were built (#10576). An owner scenario says what
the owner wants and what they will see. The team's tests carry the clicks.

## The Form

<!-- rule:tm-owner-scenarios/four-fields -->
Every scenario is a Markdown heading followed by four fields:

- `Intent:` what the owner wants, in the owner's words.
- `Situation:` who, where, and what state things are in.
- `Result:` what the owner observes when it works.
- `Trace:` the test IDs from the team's test matrix that prove it, such as
  `T3, T7`.

```markdown
## S1 Add a product with a price
Intent: The owner wants a new product on sale today.
Situation: The owner is signed in; the shop has 40 products.
Result: The new product shows in the shop list with its price, and customers
can buy it.
Trace: T1, T4
```

<!-- rule:tm-owner-scenarios/no-numbered-ui-steps -->
**No numbered UI steps.** "1. Click Add. 2. Type the name. 3. Press Save." is a
test script, not a scenario. It fixes the screen design in the owner's review
and makes every later screen change an owner edit. Two or more numbered steps
that start with a UI verb (click, tap, press, select, type, open, drag, scroll)
fail the lint. A numbered list of outcomes the owner checks is fine.

Length is not a measure. A scenario is as long as its situation needs.

## The Trace

`Trace:` names test IDs, never prose. Each ID must exist in the team's test
matrix (for example `tests/matrix.md`). A scenario with no test yet gets one
added to the matrix first; never invent an ID the matrix lacks.

## The Owner's Cost

<!-- rule:tm-owner-scenarios/owner-cost-in-review-minutes -->
State the owner's cost in the plan as review time in minutes ("about 10
minutes to read eight scenarios"). Never ask the owner to write scenarios
step by step, write scripts, design or fix screens, or run commands. The team
drafts; the owner reviews outcomes.

## Check With the Lint

```bash
python3 scripts/check_owner_scenarios.py --matrix tests/matrix.md docs/scenarios.md
```

It fails on a missing or empty field, a `Trace:` ID absent from the matrix, two
or more numbered UI steps in one scenario, or a file with no scenario. Exit 0
passes, 1 lists each finding as `<file>:<line>: <heading>: <reason>`, 2 is a
usage or read error, so CI can run it as is. The script lives in the
trusty-tools repo; a project without it copies the one file, which needs only
Python 3.

## Reporting a Scenario Set as Done

A scenario set is a non-code deliverable: its completion report follows the
`tm-workflow` Completion Standard, "Non-Code Deliverables" — criteria that
quote the owner's request, and an evidence map. A lint pass is evidence of
form, not of meaning; say which scenarios you read after the last write.
