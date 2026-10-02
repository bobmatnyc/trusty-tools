# Tracker body template

The tracker is a plan, not a record. Everything above the first marker is
authored once; the three marker blocks each have their own maintenance rule
(stated in the `## Maintenance` section the body itself carries, so a reader of
the issue sees it without this skill).

Title: `[EPIC <epic#>] <the outcome, in plain words>` — set in a second step
after the number is read back (`manual-procedure.md`).

````markdown
<One paragraph: the problem, and what changes when this is done.>

Plan: <blob permalink pinned to the commit SHA that landed
docs/research/<effort>/<plan>.md on origin/main>

Supersedes #<n>.        <- only if it does

## Outcomes

- **O1** <observable end state>
- **O2** <observable end state>

## Ratified decisions

| # | Decision |
|---|---|
| D1 | <a decision already made, not a proposal> |
| D2 | <...> |

<Optional: one short paragraph if the original framing contained a
wrong premise the plan corrects. Say what was wrong and point at
the evidence.>

## Ordering

<Why the phases run in this order. Name each gate and what lifts it.
This is the section that justifies the pattern's existence — if there
is nothing to write here, the work does not need phase issues.>

<!-- phases:start -->
| # | Phase | Issue | State | Gate |
|---|-------|-------|-------|------|
| 1 | <name> | #<n> | <state> | <what blocks it> |
<!-- phases:end -->

## Deferred

<!-- deferred:start -->
| Item | Why deferred | Where it went |
|------|--------------|---------------|
| <gap this work creates or scope removed> | <reason> | <issue or "unscheduled"> |
<!-- deferred:end -->

## Follow-ups

<!-- followups:start -->
| Finding | Surfaced by | Severity | Where it went |
|---------|-------------|----------|---------------|
| <something discovered during execution, outside this epic's scope> | PHASE_<n> | <HIGH/MEDIUM/LOW> | <issue, rollup, or "this row"> |
<!-- followups:end -->

## Maintenance

The phases block is regenerated from child issue state — never patched
by hand. The deferred block is amended deliberately, only when scope
leaves the plan or a deferred item lands. The follow-ups block is
appended as findings surface; at most two of them per phase become
standalone issues, and only at severity HIGH or above. The sections
above the markers are authored once.

Comments here are limited to changes of plan: a decision made, a phase
re-scoped, a risk realized, an item deferred, a gate lifted. Progress
belongs in the phases block; evidence belongs in the phase issue or its PR.
````

## The three blocks

| Block | Holds | Rule |
|---|---|---|
| `phases` | one row per phase — number, name, issue link, state, gate | regenerated wholesale from `gh issue view <epic#> --json subIssues`; a hand edit is overwritten by the next regeneration |
| `deferred` | scope that was in the plan and came out, and the gap it leaves | amended deliberately, on the "item deferred / deferred item lands" trigger only |
| `followups` | findings discovered while executing a phase, outside the plan | appended as found; each row names the phase that surfaced it |

The `Gate` column justifies the phases block. Without it the table restates
the sub-issue list GitHub already renders.

Every issue reference in a rendered block is a clickable link, `#<n>` inside a
table being the one bare form GitHub auto-links; anywhere else use the full
`[#n](https://github.com/<owner>/<repo>/issues/n)` shape.
