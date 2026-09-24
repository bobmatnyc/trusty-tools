---
name: tm-epic
description: Author a GitHub epic — one tracker issue plus one native sub-issue per phase — when a gate sits between stages. The gate test, the four rules, how to write acceptance criteria, the five tracker-update triggers, and the tm issue epic verbs that maintain the tracker.
user-invocable: true
version: "0.1.0"
category: pm-workflow
tags: [tickets, github, epic, phases, tracker, pm-required]
effort: medium
---

# /tm-epic — Tracker + Phase Issues

One tracker issue owns the outcomes, the ratified decisions, the ordering and
the gates. One phase issue per phase owns that phase's scope, acceptance
criteria, risk and state. The phases are native GitHub sub-issues of the
tracker, never a task list. This skill carries the judgement; the mechanics —
the body templates, the ordered `gh` sequence, the anti-pattern table — sit in
[`references/`](references/) and are loaded on demand.

`tm-ticketing` still governs everything an epic shares with any other issue:
the promotion gate, the sparse-body rule and its bounded exceptions, the labels,
project and milestone standard, and the lifecycle labels. This skill adds only
what a tracker and its phases need on top. A project that ships a `TICKETING.md`
at its root may narrow these defaults; where none exists, this skill applies as
written.

## The `tm issue epic` verbs

The deterministic half is code ([#8445](https://github.com/bobmatnyc/trusty-tools/issues/8445)).
[`references/manual-procedure.md`](references/manual-procedure.md) remains the
`gh`-level description of what each verb does, for the adopt-an-existing-issue
cases the CLI does not cover.

| Verb | Behaviour |
|---|---|
| `tm issue epic create --from <plan-doc> --milestone … --component … [--project N] [--session …]` | Refuses until the plan document is on `origin/main`; files the tracker, reads its number back, edits it into the title, then files each phase as a native sub-issue; re-runnable |
| `tm issue epic sync <epic#>` | Regenerates the `phases` block wholesale from live child state; a no-op when it already matches |
| `tm issue epic defer <epic#> --item … --why … --where …` | Appends one row to the `deferred` block; the same row twice is a no-op |
| `tm issue epic close <epic#> --evidence "O<n>: …"…` | Refuses while any child is open, naming it; posts the closing comment mapping each declared outcome to its evidence; closes |

`tm issue transition` on a phase issue regenerates its tracker's `phases`
block as a side effect — a no-op transition included, so re-running the same
command after a killed or failed sync repairs the tracker. A sync that fails
there fails the command and names the tracker to `sync` by hand — the label
has already moved. `tm issue audit <epic#>` adds two set-level rows: the block
matches its children, and every `[EPIC_<epic#> PHASE_…]`-titled issue is a
native sub-issue. The linkage row reads GitHub's title search, which lags a
just-created issue; when it omits a linked phase the row FAILs with
`search index returned N of M known phases — re-run in a minute`, never PASS.

## The gate test

Use a tracker with phases only when a **gate** sits between stages: one stage
must be verified, soaked or deployed before the next starts. Multi-week
duration and multiple deploy units corroborate; the gate is the test.

Write the tracker's `## Ordering` section first. If it is empty — nothing to
say about why phase 2 cannot start before phase 1 is proven — the work does not
need this pattern. It fits one issue with a task list, or one PR.

## Four rules

1. **The phase issue is the only source of truth for phase state.** The
   tracker holds no state a child also holds, except inside the regenerated
   block.
2. **Derived content is regenerated wholesale, never patched.** A session
   rebuilds the `phases` block from child issue state and replaces it entirely.
   A hand-patched table drifts the first time a child closes without a
   session running.
3. **Phase numbers are assigned once, never renumbered, never reused.** The
   number sits in a title. A phase inserted later between 2 and 3 is
   `PHASE_6`; the tracker's table says where it runs. Order is data; the number
   is an identifier.
4. **Split a phase only when it can be scheduled, reviewed and reverted on its
   own.** Fuse when one stage is meaningless without its neighbour. Applied
   strictly, this yields fewer phases than the plan first suggests.

## Titles, labels, milestone, project

```
[EPIC <epic#>] <the outcome, in plain words>
[EPIC_<epic#> PHASE_<n>] <what this phase does>
```

`<epic#>` is the tracker's own issue number, so creation is two-step: file the
tracker with a placeholder title, read its number back, edit the number into
the title. Phases cannot go in the same batch as the tracker.

Every issue — tracker and phases alike — carries `ws/<session>`, its component
label(s), a project and a milestone, per "The Labels, Project, Milestone
Standard" in `tm-ticketing`. The tracker's type label is `epic`. Each phase
takes a type label from the same six-value set (`bug`, `enhancement`,
`refactor`, `chore`, `documentation`, `epic`) — the type of the work that
phase does; there is no seventh `phase` type. A phase takes its parent's
milestone.

## The plan document

An epic's plan lives under `docs/research/<effort>/`, one directory per epic.
Creation refuses until that document is on `origin/main`, and the tracker links
it by a blob permalink pinned to the commit SHA
(`https://github.com/<owner>/<repo>/blob/<sha>/docs/research/<effort>/<plan>.md`),
never by a branch path that moves. Issue numbers are never written back into
the plan document after creation — the tracker points at the plan; the plan
does not point at the tracker.

## Writing acceptance criteria

Before writing a criterion, ask: what would pass this and still be wrong?
Write the criterion that fails that.

Name the most likely wrong implementation — the obvious shortcut — and write at
least one criterion against it: "the shortcut drops X; assert X survives."
Those catch more than happy-path criteria do.

Rewrite any criterion phrased "works correctly" or "tests pass". Neither can
fail for a specific reason, so neither can be checked.

## When the tracker body changes

A session touches the tracker body on exactly five triggers:

| Trigger | What changes |
|---|---|
| A phase issue opens | `phases` block regenerated |
| A phase issue closes | `phases` block regenerated |
| A phase blocks or unblocks | `phases` block regenerated (Gate column) |
| An item is deferred, or a deferred item lands | `deferred` block amended (`tm issue epic defer`) |
| A phase's `status:*` label changes | `phases` block regenerated (State column) — `tm issue transition` on a phase does this itself |

The State cell reads `closed` for a closed phase; for an open one, its
`status:*` label without the prefix (`in-progress`, `coded`, `merged`,
`tested`), or `open` when it carries none. The prefix is the issue state
model's `label_config.status_prefix` (`status:` in trusty-tools).

**Not on PR open, merge, commit or review.** Those are phase-issue events and
belong on the phase issue or its PR. A finding discovered mid-execution is a
fifth kind of event and goes to the `followups` block, below.

## Deferred vs. follow-ups — two blocks, two rules

The tracker body carries three marker blocks
([`references/tracker-template.md`](references/tracker-template.md)):

| Block | Holds | Maintenance |
|---|---|---|
| `phases:start` / `phases:end` | One row per phase: number, name, issue, state, gate | Regenerated wholesale from child state; never hand-patched |
| `deferred:start` / `deferred:end` | Scope removed from the plan, and the gap it leaves | Amended deliberately, on the fourth trigger only |
| `followups:start` / `followups:end` | Findings surfaced during execution that are not this epic's scope | Appended as found; each row names the phase that surfaced it |

Deferred is scope that was in the plan and came out. Follow-ups were never in
the plan and were discovered doing it. They are not interchangeable: a row in
the wrong block misstates whether the plan changed.

**Follow-up budget:** at most two standalone follow-up issues per phase, and
only at severity HIGH or above. Everything else is a row in the `followups`
block or an entry in the project's prompt-feedback rollup — in trusty-tools,
[#8021](https://github.com/bobmatnyc/trusty-tools/issues/8021). A row that
later earns its own issue keeps its row and gains the link.

## Closing

The tracker closes when every phase issue is closed and each outcome is
verified. Its closing comment maps O1..On to evidence, one line each — the
only place the outcomes are re-asserted. Refuse to close while a phase is open
unless the user says to close anyway (`tm-ticketing`, "On closing a parent").

## References

- [`references/tracker-template.md`](references/tracker-template.md) — the
  tracker body with all three marker blocks.
- [`references/phase-template.md`](references/phase-template.md) — the
  five-heading phase body (Scope, Acceptance criteria, Non-goals, Risk, Gate).
- [`references/manual-procedure.md`](references/manual-procedure.md) — the
  ordered `gh` sequence: file, read back, retitle, label, project, phases,
  read children, regenerate.
- [`references/anti-patterns.md`](references/anti-patterns.md) — what breaks
  and why.

## Related Skills

- `tm-ticketing` — whether an issue exists, its body, labels, milestone,
  project and lifecycle; the bounded-exception row that lets a phase body
  carry its five headings
- `tm-workflow` — the delivery chain each phase's PR runs through, and why a
  PR event never touches the tracker
