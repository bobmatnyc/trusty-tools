# tm-ticketing — trackers, phase issues and research

## Trackers and phase issues

🔴 **The full pattern lives in the project, at
`docs/reference/tracker-phases-pattern.md`** (a project that has not adopted it
yet copies it there), committed verbatim as the owner authored it: when the pattern earns its place,
both body templates, how to write acceptance criteria, and the anti-pattern
table. Read it before creating a tracker. This section states only the defaults.

**Use it when the work has a gate between stages** — one stage must be verified,
soaked, or deployed before the next starts. A tracker whose Ordering section is
empty is work that did not need the pattern; a task list inside one issue costs
less and does not drift.

**Naming.**

```
[EPIC <epic#>] <the outcome, in plain words>
[EPIC_<epic#> PHASE_<n>] <what this phase does>
```

`<epic#>` is the tracker's own issue number, so the tracker is created FIRST
titled `[EPIC] <outcome>`, its number read back, and the title renamed in
place before phase issues are filed — they cannot go in the same batch. Full
grammar and the manual `gh` sequence: `tm-epic`, which also states the phase
PR title format `<type>(<scope>): [EPIC_<epic#> PHASE_<n>] <subject>` — a PR
title, so version control's. Phase issues are native
GitHub sub-issues of the tracker, never a markdown task list — the link
survives body regeneration and stays queryable.

**Three zones, three maintenance rules.** Everything above the markers is
authored once. The `<!-- phases:start -->` block is regenerated wholesale from
child-issue state. The `<!-- deferred:start -->` block is amended deliberately.

```
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
```

🔴 **The `Gate` column is the justification for the whole pattern.** Without it
the table restates the sub-issue list, and the work should have been one issue.

**Four update triggers, and only these:** a phase issue opens, a phase issue
closes, a phase blocks or unblocks (the Gate column), or an item is deferred or
a deferred item lands. Not on PR open, merge, commit, or review — those are
phase-issue events, and a per-PR comment on the tracker buries the plan changes
that matter.

**Phase numbers are assigned once, never renumbered, never reused.** The number
sits in a title. A phase inserted later between 2 and 3 is `PHASE_6`; the
tracker's table says where it runs. Order is data; the number is an identifier.

**Split a phase when it can be scheduled, reviewed and reverted on its own;
fuse when one stage is meaningless without its neighbour.** Applied strictly
this yields fewer phases than the plan first suggests.

**Closing.** The tracker closes when every phase issue is closed and each
outcome is verified, with a closing comment mapping O1..On to evidence, one line
each.

## Research goes in committed docs, never in issues

🔴 **An epic is created from prior research, and the research document is what
the tracker LINKS to.** How the research happens stays flexible; what it
produces is a committed document under `research_docs_path` —
`docs/research/<effort>/<doc>.md` by default.

An issue body never carries findings, evidence dumps, or analysis. It carries
the outcome, the decisions already ratified, the ordering, and a link. That is
what makes "Epic #12345, work on phase 5" a complete instruction to any
contributor: the context is in the repository, at a path everyone can read, and
it survives the issue tracker.

Creating a tracker without that link is incomplete work — ask for the document
rather than pasting the findings in.
