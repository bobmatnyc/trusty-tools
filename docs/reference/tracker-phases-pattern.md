# Tracker + phase issue pattern

Domain-independent. Placeholders in `<angle brackets>`.

## When to use it

Use a tracker with phase issues when the work has a gate between stages: one stage must be verified, soaked, or deployed before the next starts. Multi-week duration and multiple deploy units corroborate; the gate is the test.

Do not use it when the work fits one PR, or when the stages form a checklist with no gate between them. A task list inside one issue costs less and does not drift.

## Anatomy

| Artifact | Count | Owns |
|---|---|---|
| Tracker | 1 | Outcomes, decisions, ordering, gates, deferred items |
| Phase issue | 1 per phase | That phase's scope, acceptance, risk, and its own state |

Link them as native GitHub sub-issues, not a markdown task list. The link survives body regeneration and stays queryable.

## Naming

```
[EPIC] <the outcome, in plain words>
[EPIC_<epic#> PHASE_<n>] <what this phase does>
```

`<epic#>` is the tracker's issue number. Create the tracker first and read its number back; phase issues cannot go in the same batch.

## Four rules

**1. The phase issue is the only source of truth for phase state.** The tracker holds no state a child also holds, except inside the regenerated block.

**2. Derived content is regenerated wholesale, never patched.** A session rebuilds the phases block from child issue state and replaces it entirely. A hand-patched table drifts the first time someone closes a child without a session running.

**3. Phase numbers are assigned once, never renumbered, never reused.** The number sits in a title. A phase inserted later between 2 and 3 is `PHASE_6`; the tracker's table says where it runs. Order is data; the number is an identifier.

**4. Split a phase when it can be scheduled, reviewed and reverted on its own. Fuse when one stage is meaningless without its neighbour.** Applied strictly, this test yields fewer phases than the plan first suggests.

## Tracker body template

````markdown
<One paragraph: the problem, and what changes when this is done.>

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

<!-- deferred:start -->
| Item | Why deferred | Where it went |
|------|--------------|---------------|
| <gap this work creates or scope removed> | <reason> | <issue or "unscheduled"> |
<!-- deferred:end -->

## Maintenance

The phases block is regenerated from child issue state — never patched
by hand. The deferred block is amended deliberately. The sections above
it are authored once.

Comments here are limited to changes of plan: a decision made, a phase
re-scoped, a risk realized, an item deferred, a gate lifted. Progress
belongs in the phases block; evidence belongs in the phase issue or its PR.
````

Three zones, three maintenance rules: authored once above the markers, regenerated in the phases block, amended in the deferred block.

The `Gate` column justifies the block. Without it the table restates the sub-issue list.

## Phase body template

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

## Writing acceptance criteria

Before writing a criterion, ask: what would pass this and still be wrong? Write the criterion that fails it.

Name the most likely wrong implementation and write at least one criterion against it: "the obvious shortcut drops X — assert X survives." Those catch more than happy-path criteria do.

Rewrite any criterion phrased as "works correctly" or "tests pass".

## Lifecycle

**Creation** — tracker first, read back its number, then the phase issues with that number in each title. Research findings go in one tracker comment at creation, or in a committed doc linked from the tracker.

**Updates** — a session touches the tracker body on exactly four triggers:

| Trigger | What changes |
|---|---|
| A phase issue opens | phases block regenerated |
| A phase issue closes | phases block regenerated |
| A phase blocks or unblocks | phases block regenerated (Gate column) |
| An item is deferred, or a deferred item lands | deferred block amended |

Not on PR open, merge, commit or review; those are phase-issue events.

**Closing** — the tracker closes when every phase issue is closed and each outcome is verified. Its closing comment maps O1..On to evidence, one line each. That is the only place the outcomes are re-asserted.

## Anti-patterns

| Anti-pattern | Why it breaks |
|---|---|
| Patching the phases block by hand | Drifts the first time a child closes without a session running |
| Restating the tracker's decisions in each phase issue | Two copies diverge; the phase issue points instead |
| Commenting on the tracker per PR | Buries the plan changes that matter |
| Renumbering phases mid-flight | Breaks every existing title and reference |
| A phase with no independent acceptance | It is not a phase; fuse it with its neighbour |
| Evidence pasted into the tracker body | The body is a plan, not a record; evidence goes in a comment, a phase issue, or a doc |
| A tracker whose Ordering section is empty | The work does not need this pattern |
