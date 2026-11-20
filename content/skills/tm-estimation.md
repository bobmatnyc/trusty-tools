---
name: tm-estimation
description: Estimate work as two figures — engineer-hours for humans and agent wall-clock hours derived by task class from measured ratios — plus a separate human-wait line. Use when writing a plan, sizing a phase, quoting a time to the owner, or re-estimating after scope changes. Carries the class ratio table, the overrun checklist, and the time-tracking table every phase records.
user-invocable: true
metadata:
  version: "0.1.0"
category: pm-workflow
tags: [planning, estimation, time-tracking, pm-required]
effort: low
---

# /tm-estimation — Engineer-Hours, Agent-Hours, Human Waits

Owner ruling 2026-11-17: estimates are not trusted until they are measured.
Every estimate gives two figures and one separate wait line. Every phase
records its actual time. The measured rows live in
[`references/estimation-data.md`](references/estimation-data.md) and grow with
each plan.

## The three lines

| Line | Unit | For |
|---|---|---|
| Engineer-hours | h, at 8 h per engineer-day | A human doing the work. Size it as you always would. |
| Agent wall-clock | h, a median and a range | The dispatched agent. Derive it from engineer-hours by class (below). |
| Human wait | h, listed per cause | Owner dialogs, desk slots, screen lock, rate limits. Never add it to agent time. |

Quote all three. A single number is not an estimate.

## Step 1 — classify each phase

Pick one class per phase. A phase that mixes classes is two phases.

| Class | What makes it this class |
|---|---|
| Port or byte-oracle translation | An existing implementation gives exact expected output; the work is translation against captured fixtures. |
| New design with live checks | No oracle. New modules, a live system to probe, review findings to fix. |
| Spike, plan or docs | Read sources, write a plan, a README or a yes/no answer. |
| Small fix round | Scoped fixes after a review, a flag fix, a matcher fix. Estimate is 1 h or less. |
| Review round | Read-only `code-critic` or delta review. |
| Timed run | A soak or wait whose length is set by design. Agent time equals the set length. |

## Step 2 — apply the ratio

Agent hours = engineer hours × ratio. Medians and ranges come from the data
file; recompute them when rows are added.

| Class | Median ratio | Range | Rows |
|---|---|---|---|
| Port or byte-oracle translation | 0.8% | 0.4–1.3% | 8 |
| New design with live checks | 2.1% | 0.6–8.2% | 10 |
| Spike, plan or docs | 4% | 1.3–11% | 7 |
| Small fix round (of its own estimate) | 11% | 8–124% | 6 |
| Review round | 3 min | 1–5 min | 4 |

Read the table this way:

- A port with a byte oracle is about 1% of engineer time. The oracle removes
  design and debugging.
- New design sits at 2–6% for most phases. The top of the range is a phase
  with two review and fix cycles, or a build that grew.
- A fix round is usually 10–30% of its small estimate. Two of six rows took
  the whole estimate or more. A fix round that needs a live check or a
  rebase is not small; size it as new design.
- A review round costs minutes. Budget the fix round it causes, not the read.

## Step 3 — list the human waits

Write each expected wait as its own line with a cause and a limit. Measured
waits so far: a Keychain dialog with the owner away, 4.35 h; a locked screen,
1.95 h; a GitHub rate limit, about 1 h. A dialog-capable step needs a desk
slot booked with the owner and a wait limit. Report wait time apart from
agent time in the actual column, for example `4.52 wall (0.17 agent + 4.35
human wait)`.

## Step 4 — re-estimate when scope changes

Scope growth is the largest error source. #11470 was estimated at 0.3 agent
hours and took 1.83 h, 6× over, because its scope grew three times mid-task.
No row without scope growth overran its agent estimate by more than 1.3×.

Rule: when scope changes, stop, write a new estimate row for the added
scope, and record the change in the cause column. Do not let the original
estimate absorb new work.

## Overrun checklist

Check each item while estimating. A hit adds a wait line or widens the range.

- [ ] macOS dialogs and TCC prompts (Keychain, Accessibility, signing). They
      need the owner at the desk.
- [ ] A locked screen blocks AX. Live UI checks wait until it unlocks.
- [ ] Linux CI lanes that were not run locally. Run them on EVO before push.
- [ ] Release preflight: the changelog must be assembled and the pre-publish
      gate run at the release SHA.
- [ ] GitHub rate limits on bulk `gh` calls.
- [ ] Full Mac test suites hit `lsof` and file-descriptor limits. Use EVO.
- [ ] The fix-round and pin cycle when a branch falls behind main: rebase,
      re-run gates, re-pin the review verdict.

## Time-tracking table

Every phase of every plan records a row. Fill start when the phase starts,
end and actual when it ends, and the cause whenever actual differs from the
agent estimate by more than 2× in either direction.

```markdown
### Time tracking

Estimate in hours at 8 h per engineer-day. Actual hours are agent wall-clock time.

| Phase | Estimate (h) | Agent est. (h) | Start (UTC) | End (UTC) | Actual (h) | Cause of gap |
|---|---|---|---|---|---|---|
| 0 | 8 | 0.15 (0.05–0.5) | | | | |
| H human waits | n/a | n/a | | | | |
```

When a plan closes, copy its rows into
[`references/estimation-data.md`](references/estimation-data.md) and update
the ratio table above if a median moved.
