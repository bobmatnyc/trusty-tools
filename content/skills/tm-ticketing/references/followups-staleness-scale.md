# tm-ticketing — follow-ups, staleness and scale

## Follow-ups — the sprawl control

A repository at 400+ open issues got there through follow-ups, one reasonable
filing at a time. Two phrasings sound disciplined and are both wrong defaults:
"open no follow-up issues" loses the critical ones, and "critical only" with no
linkage loses which epic they belong to. The default is **critical or
independently schedulable, always linked, always dated, always budgeted.**

A standalone follow-up issue must carry all four:

1. Its trigger — `Refs #<phase issue>` and the PR that surfaced it.
2. A native sub-issue link to the epic.
3. A severity or value signal — a `P0`–`P3` label, or the project's equivalent.
4. A milestone, or a `due-by` date within `followups.due_within_days`, so it
   cannot silently go stale.

Past `followups.budget_per_phase`, or below
`followups.severity_floor_for_standalone`, it is a checklist line on the epic's
Follow-ups tracker — or on the project's `rollup_issue` when there is no epic —
and not an issue at all.

## Staleness

At `staleness.stale_after_days` with no activity and no milestone, the agent
labels the issue `stale` and posts ONE triage comment naming the disposition it
recommends, with the evidence for it:

| Disposition | Evidence it carries |
|---|---|
| CLOSE | The code path is gone, or a merged PR superseded it — cite the PR |
| SUPERSEDE | The newer issue, linked; close this one against it |
| KEEP | Why it still matters, and a re-dating |

At `staleness.close_stale_after_days` it closes with a note, unless the issue is
pinned to a milestone or carries a `staleness.exempt_labels` label.

🔴 **A sweep closure carries `closed:sweep`** (owner ruling 2026-09-27) — age,
staleness, duplicate, or obsolete dispositions all count. A fix closure comes
from a merged PR and never carries `closed:sweep`.

🔴 **A human decision is requested as a digest, never per issue.** Group the
recommendations per epic and post them as one comment on that epic's tracker.
The sweep that runs this is `tm-issues-prune`'s Prune phase; this section is the
policy it applies.

## Scale rules

At twenty contributors the epic tracker is the single view a human reads, and
the board is filtered by epic. Two rules keep that view complete:

- Every open issue belongs to an epic or to a `Backlog · <area>` milestone. One
  with neither is a standard violation the audit reports.
- The four numbers that say whether the policy is holding — open count per epic,
  follow-ups per phase issue, stale count, median age of open follow-ups — are
  the file's `metrics:` block. They are the contract for the `tm issue audit`
  integration, not something implemented here.
