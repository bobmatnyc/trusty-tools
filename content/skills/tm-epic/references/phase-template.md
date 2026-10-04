# Phase body template

Title: `[EPIC_<epic#> PHASE_<n>] <what this phase does>`. The number `<n>` is
assigned once and never reused; a phase added later takes the next free number
regardless of where it runs in the order.

Filed with `--parent <epic#>` so it is a native sub-issue, with a type label
from the six-value set (`bug`, `enhancement`, `refactor`, `chore`,
`documentation`, `epic`) naming the kind of work the phase does, the component
label(s), `ws/<session>`, the parent's milestone, and the project.

The five headings below are the bounded exception `tm-ticketing` grants a phase
body ("What a Ticket Says", bounded-exceptions table). The exception buys
length for the scope, criteria, risk and gate — never for narrative, and never
for restating the tracker's decisions.

````markdown
Part of #<epic#>, phase <n> of <N>.

<One paragraph: what this phase does and why it sits at this point in
the order. Do not restate the tracker's decisions — point at them.>

## Scope

<Explicit list of what changes. Files, modules, or surfaces by name.>

## Acceptance criteria

- **AC1** <criterion>
- **AC2** <criterion>

## Non-goals

<What a reader might reasonably assume is in scope and is not. Name the
things this phase deliberately leaves standing.>

## Risk

<What could go wrong, and why this phase is the right place to absorb it.>

## Gate

<What must be true before this phase starts, or "None.">
````

## Writing the criteria

Before each criterion, ask what would pass it and still be wrong, then write
the criterion that fails that. Name the most likely wrong implementation and
write at least one criterion against it. Rewrite anything phrased "works
correctly" or "tests pass" — neither names a way to fail.

## What the phase issue owns

The phase issue is the only source of truth for the phase's state. Its
lifecycle labels (`status:in-progress` → `status:coded` → `status:merged` →
`status:tested`), its PR's `Refs #<n>`, and its evidence all live here or on
the PR — never in the tracker body. The tracker's `phases` row for this phase
is derived from this issue's state, and is regenerated, not edited.
