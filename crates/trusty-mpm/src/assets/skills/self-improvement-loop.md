---
name: self-improvement-loop
description: "Self-analysis and improvement reporting for a dispatched agent: the post-mortem-routed recommendation block, plus the per-task fast-loop hypothesis record that changes behavior now"
user-invocable: false
version: "1.0.0"
category: agent-reference
effort: low
---

# Self-Improvement Loop

#7723: moved out of `BASE-AGENT.md` (resident on every agent turn) — load
this skill only at the point BASE-AGENT's "Self-Improvement Reporting"
section names: before your final report.

## Self-Analysis and Improvement Reporting

Every task ends with an analysis of your own run. This is a core property of
every agent, not a step reserved for runs that went badly.

Three questions, and a finding answers all three:

1. What went wrong, or took longer than it should have?
2. Which instruction, skill, tool, or harness gap caused it?
3. What concrete change would prevent it?

A symptom with no named cause and no proposed change files nothing.

**The destination is fixed: issues in `bobmatnyc/trusty-tools`.** That holds
whatever project you ran in. A recommendation about a bundled skill, a bundled
agent, the delivery workflow, or the framework goes to trusty-tools, never to
the target project's own repository.

**Routing depends on what you are.** A dispatched subagent never files the
issue itself — "No Subagent Fan-Out" forbids reaching for `ticketing`. End
your report with an **Improvement recommendations** block instead, one entry
per finding:

- **Symptom** — what you observed.
- **Cause** — the instruction, skill, tool, or harness gap.
- **Change** — the concrete edit that prevents it.
- **Evidence** — command output, `file:line`, or elapsed time.

The PM routes that block to trusty-tools issues through the `ticketing` agent.
An agent running top-level files through `ticketing` directly.

**Search before filing.** Search open trusty-tools issues carrying the
`self-improvement` label first. A match gets a comment on that issue, never a
second issue. Every issue filed this way carries the `self-improvement` label
and links #6933.

**A clean run reports nothing.** No findings means no block and no issue.
Never emit an empty block, and never file an issue to show that you looked.

These recommendations feed the recurring harness post-mortem in #6933 (#6935).

## Continuous Self-Improvement — the Fast Loop

The section above reports upward on the post-mortem's schedule. This one runs
on every task and changes your behavior now. See #6937.

**Detection heuristics.** Check these at the end of every task. Each is
observable from your own transcript or a tool result:

- A retry on the same failure — you re-ran a command and it failed the same way.
- A gate failing after a "done" claim — your `fmt`, test, `clippy`, or script
  gate went red after you called the work finished.
- A review round beyond the first — a critic or reviewer returned a second round.
- A user or PM correction — a message redirected work you had already started.
- A circuit-breaker trip — `mcp__trusty-mpm__circuit_breaker_status`, or a
  breaker notice in your transcript.
- Repeated tool errors — the same tool failed twice on the same argument shape.
- An overrun of your own estimate — you named a duration, a file count, or an
  action count, and exceeded it.

A run that trips none of them records nothing. A run that trips one where you
cannot name a different way to try records nothing either: a symptom with no
alternative is a report, not an experiment.

**The tag is `self-improvement-hypothesis`.** One tag, in the project's memory
palace, and it is the contract the post-mortem queries by —
`memory_list(tag: "self-improvement-hypothesis")`. Add descriptive tags beside
it, never in place of it.

**The record shape**, seven fields:

- **Trigger heuristic** — which one fired, and what you observed.
- **What was tried** — the approach that produced the trigger.
- **Hypothesis** — what to do differently, and why it should help.
- **Metric and baseline** — the one number the change moves, its value before
  the change, and where that number came from. Cite a harness-emitted source
  over your own estimate: the daemon's delegation records, `session_activity`,
  `console_metrics`, the `agent_cost` context reading, or the session
  transcript. Name the source in the record.
- **Judgement rule** — the sample size, the test, and the thresholds that make
  the outcome improved or regressed.
- **Status** — `open`, `improved`, `regressed`, or `inconclusive`, with the n so
  far.
- **Evidence** — command output, `file:line`, or the tool result the numbers
  came from.

**Consult before you start; measure after you finish.**

1. Before starting a task, recall the open hypotheses under the tag for your
   area and apply the ones that fit. An open hypothesis is an approach to try,
   not a note to read.
2. After finishing, record the measurement against the SAME hypothesis — same
   metric, same source.
3. Promote to `improved`, or retire as `regressed`, only on a statistically
   significant difference at the sample size the record named. Below that the
   status stays `open` with the n so far, or `inconclusive` when the experiment
   cannot run again.
4. Retire a regressed hypothesis by re-recording it with the measurement and the
   `regressed` status. Never delete it silently — the measurement that killed it
   is what stops the next agent retrying it.

**A behavioral tweak that works needs no issue.** It stays in memory and the
record is the whole deliverable. Only a change that needs an edit to the
framework, a bundled skill, a bundled agent, or the workflow goes to
trusty-tools, through the Improvement recommendations block above.

**The post-mortem coalesces; it does not replace this.** The scheduled
post-mortem (#6933, #6934) reads every record under the tag across registered
projects, groups them by metric, and files only the significant, fix-needing
results as trusty-tools issues. Its cadence is unchanged. This loop runs every
task.

**Worked example — guard denials.** A PM dispatched a read-only agent from a
main checkout without declaring `isolation: "worktree"` while `version-control`
was working there. The ADR-0048 guard denied it and the PM re-dispatched, which
is a retry on the same failure. The same lesson was already in memory from
2026-08-28 and had not changed behavior. Hypothesis: declaring isolation on
every dispatch from a main checkout except `version-control` (ADR-0056),
read-only agents included, drives denials to zero. Metric: guard-denied
dispatches per 30 dispatches, from the daemon's delegation records. Baseline: 1
in 14 that session, and 3 in one session on 2026-08-28. Judge: 0 in the next 30
is `improved`, 2 or more is `regressed`. Status: `open`, n=0.

**Worked example — monitor parks.** An agent handed back twice with its goal
unmet, saying a background monitor would wake it. The monitor watched
`pgrep -f <pattern>`, and `pgrep -f` matched the monitor's own command line, so
the exit condition could never become true. Hypothesis: waiting on your own
conditions with `tm wait --for run|file`, or where `pgrep` is unavoidable the
self-excluding `pgrep -f '[c]argo install'` form or a pid captured at spawn,
drives parks to zero. Metric: hand-backs with the goal unmet that needed a PM
`SendMessage`, per 20 dispatches, counted from the session transcript. Baseline:
2 in 1 dispatch. Judge: 0 in the next 20 is `improved`, 2 or more is
`regressed`. Status: `open`, n=0.
