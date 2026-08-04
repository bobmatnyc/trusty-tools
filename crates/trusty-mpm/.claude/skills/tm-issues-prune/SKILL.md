---
name: tm-issues-prune
description: Prune, organize, prioritize, and suggest next tasks from a project's GitHub issue backlog — natural-language PM delegation pattern (gh-first, JIRA deferred)
user-invocable: true
version: "1.1.0"
category: pm-workflow
tags: [tickets, github, backlog, pm-required, triage, prioritization]
effort: medium
---

# /tm-issues-prune — Backlog Prune, Organize, Prioritize & Suggest Next

A natural-language PM-delegation pattern for keeping a project's GitHub
issue backlog healthy and actionable: closing stale/duplicate/obsolete
issues, correcting priority labels on the ones that remain open, grouping
the survivors into a navigable map, producing a deterministic priority
ranking, and surfacing a short list of next tasks for the user to choose
from. This skill describes a workflow, not a new tool or command — every
mechanical `gh` operation is delegated, never run by the PM directly.

The full sweep runs in four phases, in this order: **Prune → Organize →
Prioritize → Suggest Next**. Each phase depends on the one before it —
Organize/Prioritize/Suggest Next operate on the *surviving* open issues
after any prune-close pass, so stale/duplicate/obsolete noise doesn't
pollute the ranking or the suggestions.

## Scope: gh-first, JIRA deferred

This skill operates on **GitHub Issues only**, via the `gh` CLI. JIRA is
**not yet supported** — `TicketSystemKind::Jira` is stubbed in
`trusty-agents`, so there is no working JIRA backend to prune/prioritize
against today. The workflow below is written so a future JIRA backend can
slot in without changing the PM-facing shape (prune candidates → confirm →
close; prioritize → apply labels), but until that backend exists, only
GitHub-repo projects can use `/tm-issues-prune`.

## Delegation Pattern

**The PM never runs `gh issue` commands directly.** Delegate the mechanical
work to the `ticketing` agent, which shells `gh issue list/close/edit`
against the active project's GitHub repo. This is a deliberate, scoped
extension of the ticketing agent's remit for bulk backlog-hygiene passes —
distinct from `tm-ticketing.md`'s routing note that single-issue,
PR-linked TkDD transitions go through the Version Control agent. A prune/
prioritize sweep is workflow-state intelligence across the whole backlog
(the ticketing agent's actual specialty), just backed by `gh` instead of
`mcp-ticketer`/`aitrackdown` for this repo's own issues.

**Wrong:**
```
PM: gh issue list --state open --json number,title,updatedAt,labels
PM: gh issue close 1234 --comment "stale"
```

**Correct:**
```
PM: "I'll have ticketing survey the open backlog for prune/priority candidates..."
[PM constructs a delegation prompt for the ticketing agent, scoped to this repo]
[ticketing agent runs gh issue list/close/edit, returns structured findings]
PM: [presents the prune/prioritize summary, asks for confirmation before closing]
```

## `/tm-issues-prune` Subcommands

| Subcommand | Purpose |
|---|---|
| `/tm-issues-prune scan` | Survey the backlog, report prune candidates AND priority gaps without changing anything |
| `/tm-issues-prune close` | Prune pass — present candidates with reasons, close only after user confirmation |
| `/tm-issues-prune organize` | Organize pass — group surviving open issues by epic/component/theme, flag orphans |
| `/tm-issues-prune prioritize` | Prioritize pass — assess open issues, propose label changes, surface a ranked list |
| `/tm-issues-prune suggest-next` | Suggest-next pass — recommend the top 3-7 next tasks as a selectable table |
| `/tm-issues-prune` (no args) | Run scan, then offer to proceed with close, organize, prioritize, and/or suggest-next, in that order |

## Prune Workflow

1. **Delegate the survey.** Ask the `ticketing` agent to pull the open-issue
   list for the active repo:
   ```bash
   gh issue list --state open --limit 500 \
     --json number,title,updatedAt,labels,body,comments
   ```
2. **Classify candidates** against these criteria (the ticketing agent
   applies them, the PM does not re-derive them):
   - **Stale** — no activity (comments, commits, label changes) in more
     than N days (default N=90; PM confirms or overrides N with the user
     before delegating).
   - **Duplicate** — title/body substantially overlaps an existing open
     issue; link the surviving issue number.
   - **Obsolete/superseded** — references code, a design, or a milestone
     that no longer exists (e.g. a removed crate, a merged/abandoned spec).
   - **Won't-fix** — valid report but explicitly out of scope or rejected
     in a prior comment thread.
3. **Present the candidate list to the user WITH reasons**, one line per
   issue: `#1234 "Old title" — stale, last activity 214 days ago`. Group by
   category. **Never close anything silently or in bulk without explicit
   user confirmation** — this is the conservative default and it is not
   optional.
4. **On confirmation**, delegate the close pass. Each close carries a
   reason comment, not a bare state change:
   ```bash
   gh issue close 1234 --comment "Closing as stale — no activity since \
   <date>. Reopen if still relevant."
   gh issue close 1235 --comment "Duplicate of #1200 — consolidating \
   discussion there."
   ```
5. **Report** the final close list back to the user with issue numbers and
   reasons, so the action is auditable after the fact.

## Prioritize Workflow

1. **Delegate the assessment.** Ask the `ticketing` agent to pull open
   issues with their current labels:
   ```bash
   gh issue list --state open --limit 500 --json number,title,labels,createdAt
   ```
2. **Classify against priority signal** (age, linked PR activity, explicit
   user/maintainer escalation, blocking relationship to other open issues,
   security/data-loss impact) and produce a proposed P0/P1/P2/… label per
   issue, distinguishing:
   - **Unlabeled** — no priority label at all.
   - **Mislabeled** — current label looks inconsistent with the issue's
     actual signal (e.g. a security bug labeled P3).
   - **Correctly labeled** — no change needed.
3. **Present the top-N** (default N=10) most important open issues to the
   user, with the proposed label change and a one-line justification for
   each.
4. **On confirmation** (or for a lower-stakes "add labels, don't remove
   anything" pass, which does not need the same close-level confirmation
   gate — but still summarize before/after), delegate the label edits:
   ```bash
   gh issue edit 1234 --add-label "P1" --remove-label "P3"
   gh issue edit 1235 --add-label "P0"
   ```
5. **Report** the final label changes and the surfaced top-N list.

## Organize Workflow

Runs after the prune-close pass (if any) so grouping only covers issues
that actually survive. Ask the `ticketing` agent to pull the surviving
open-issue set with labels and bodies, then group into a map along three
axes:

1. **Epic** — group by parent issue where the repo has an epic/parent-issue
   convention. Detect this from: an `epic` label, a body reference like
   `Part of #N` / `Epic: #N`, or a parent issue whose body lists child issue
   numbers. If the repo has no established epic convention, say so rather
   than inventing one.
2. **Component/crate** — infer from conventional-commit-style title
   prefixes (`feat(trusty-mpm): …`, `fix(trusty-search): …`, etc.) and from
   any `crate:*` / component labels already applied.
3. **Theme** — when an issue has no inferable epic and no inferable
   component, cluster it with similar issues by subject matter (e.g.
   "installer/signing", "session lifecycle") based on title/body content.

**Orphans** — issues with no epic, no labels, and no inferable component or
theme — must be called out explicitly in their own section, e.g.
`Orphans (needs labeling): #1234, #1240`. Do not guess a component/theme
for an orphan just to force it into a bucket; ask the user to label it
instead.

Present the result as a grouped map (epic → component → theme → orphans),
one line per issue with its number and title, so the user can see the
backlog's shape before ranking.

## Prioritize (Ranked List) Workflow

This is a deterministic ranking pass over the organized, surviving open
issues — distinct from the label-correction "Prioritize Workflow" above,
which proposes label *changes*. This pass produces an **ordered list**
using only facts pulled via `gh` (labels, cross-references, dates,
milestone state) — never subjective judgment alone. Apply these heuristics
in order, most authoritative first:

1. **Existing priority labels** — P0 > P1 > P2 > P3 > unlabeled.
2. **Blocks other work** — issues referenced by open PRs or other open
   issues via `Closes #`, `Blocks #`, `Depends on #`, or cross-references
   visible in `gh issue view --json` / `gh api` linked-PR data. An issue
   that blocks active work outranks one that doesn't, within the same
   priority tier.
3. **Active epic/milestone membership** — an issue belonging to an open
   milestone with a due date, or an open parent epic issue that is itself
   still active, outranks an otherwise-similar issue with no active epic.
4. **Staleness** — old and untouched (no comments/updates in a long window,
   default 90 days unless the user specified another window during the
   prune pass) is a signal to **demote** or flag as a close-candidate for
   the next prune pass — never a reason to promote.
5. **Size hints** — if the issue is labeled/estimated for size, use it only
   as a final tiebreaker between otherwise-equal issues.

**Output format:** an ordered list, one line per issue, ending in a short
rationale citing which heuristic(s) fired, e.g.:

```
1. #1234 "Fix daemon restart race" — P0 label; blocks #1240, #1255
2. #1201 "Add retry backoff to proxy" — P1 label; active epic #1100
3. #1188 "Refactor config loader" — P2 label; stale 94d — demote candidate
```

State explicitly which `gh`-observable fact justified each ranking
decision. If two issues tie on every heuristic, say so rather than
inventing a tiebreak.

## Suggest Next Workflow

Runs last, over the ranked list, to recommend a short, concrete slate of
next tasks:

1. **Select 3-7 candidates** from the top of the ranked list, adjusted for
   dependency sequencing: this repo's layer priority is API/daemon routes
   before CLI before TUI/GUI — i.e. prefer lower-layer work that unblocks
   higher-layer work over higher-layer work that has no such leverage, even
   if the higher-layer item ranks slightly higher in raw priority.
2. **Size each candidate** S/M/L (rough effort, not a strict estimate).
3. **Write a one-line why-now rationale** per candidate (why this, why
   now — cite the ranking heuristic(s) that put it near the top).
4. **Note what it unblocks** — which other open issues or PRs become easier
   or possible once this lands (cross-reference the Organize/Prioritize
   output; don't invent unblocks that aren't visible in the `gh` data).
5. **Present as a markdown table:**

   | # | Title | Size | Why now | Unblocks |
   |---|---|---|---|---|
   | #1234 | Fix daemon restart race | M | P0, blocks 2 other issues | #1240, #1255 |

6. **Do not auto-start any of these tasks.** Present the table to the user
   and wait for them to explicitly choose one (or more) before delegating
   any implementation work.

## Output Format (full sweep)

When running the full `/tm-issues-prune` sweep (no args, all phases), report
back to the user in this sequence, each section clearly labeled:

1. **Prune summary** — candidates presented, confirmations received, final
   close list with reasons (empty if nothing was closed).
2. **Organized map** — epic → component → theme → orphans, one line per
   surviving open issue.
3. **Ranked list** — the ordered list from the Prioritize (Ranked List)
   Workflow, each line with its rationale.
4. **Next-task table** — the Suggest Next markdown table, ending with an
   explicit prompt for the user to pick a task (or none).

Any phase can also be run standalone via its subcommand (`organize`,
`prioritize`, `suggest-next`) without running the full sweep, in which case
only that section is reported.

## Safety Rules

- **Closing is destructive and requires explicit user confirmation** —
  present candidates first, close second, never combine the two steps into
  one delegation.
- **Priority label changes are additive/correctable**, not destructive, but
  still summarize proposed changes before applying them at scale (more than
  a handful of issues).
- If the user has not specified a staleness window, ask before assuming a
  default N.
- Never delegate a prune/prioritize sweep against a repo other than the
  one the active project is registered against — confirm the target repo
  first if it is ambiguous.
- **Organize, ranked-list Prioritize, and Suggest Next are read-only** — they
  never close issues or edit labels, so they carry no confirmation gate.
  Suggest Next's only guardrail is behavioral, not destructive: never
  auto-start a suggested task without the user explicitly picking it.

## JIRA (Future)

JIRA is not yet supported: `TicketSystemKind::Jira` is stubbed in
`trusty-agents`, so there is no working JIRA backend today. A JIRA backend
is a future follow-up — when it lands, the same prune/organize/prioritize/
suggest-next shape applies, with the `ticketing` agent choosing the backend
by project configuration instead of always shelling `gh`.

## Related Skills

- `tm-ticketing` — the general ticket-driven-development protocol and the
  GitHub-issues-vs-ticketing-agent routing note this skill deliberately
  extends for bulk backlog hygiene
- `tm-delegation-patterns` — where this fits in the broader agent matrix
- `tm-circuit-breaker` — CB#6 (forbidden direct tool usage) applies here too:
  the PM delegates, it never runs `gh issue` itself
