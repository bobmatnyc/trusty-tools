---
name: tm-issues-prune
description: Prune and prioritize a project's GitHub issue backlog — natural-language PM delegation pattern (gh-first, JIRA deferred)
user-invocable: true
version: "1.0.0"
category: pm-workflow
tags: [tickets, github, backlog, pm-required]
effort: medium
---

# /tm-issues-prune — Backlog Prune & Prioritize

A natural-language PM-delegation pattern for keeping a project's GitHub
issue backlog healthy: closing stale/duplicate/obsolete issues and
correcting priority labels on the ones that remain open. This skill
describes a workflow, not a new tool or command — every mechanical `gh`
operation is delegated, never run by the PM directly.

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
| `/tm-issues-prune prioritize` | Prioritize pass — assess open issues, propose label changes, surface top-N |
| `/tm-issues-prune` (no args) | Run scan, then offer to proceed with close and/or prioritize |

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

## JIRA (Future)

JIRA is not yet supported: `TicketSystemKind::Jira` is stubbed in
`trusty-agents`, so there is no working JIRA backend today. A JIRA backend
is a future follow-up — when it lands, the same scan/close/prioritize shape
applies, with the `ticketing` agent choosing the backend by project
configuration instead of always shelling `gh`.

## Related Skills

- `tm-ticketing` — the general ticket-driven-development protocol and the
  GitHub-issues-vs-ticketing-agent routing note this skill deliberately
  extends for bulk backlog hygiene
- `tm-delegation-patterns` — where this fits in the broader agent matrix
- `tm-circuit-breaker` — CB#6 (forbidden direct tool usage) applies here too:
  the PM delegates, it never runs `gh issue` itself
