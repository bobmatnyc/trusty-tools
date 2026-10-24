---
name: architect-improvements
description: Architect skill. Load for an improvement finding, a framework or Architect change, or a question about the improvement pipeline - when a session sends an IMPROVEMENT REPORT, when a captured pane shows a Prompt feedback or Improvement recommendations block, once a day for consolidation, and after a filed change merges or releases. Covers intake into the ledger, consolidation by a research agent, filing, and observing whether each change worked. Architect only.
user-invocable: true
metadata:
  version: "0.1.0"
category: monitoring
tags: [architect, self-improvement, ledger]
effort: medium
---

# architect-improvements — The Improvement Pipeline

<!-- rule:r-061 -->
**Improvement mandate.** On every pass, look for ways to improve a watched
project, the trusty-mpm framework, or the Architect itself. Fix
Architect-owned files directly. For a project or the framework, bring the
user a multiple-choice proposal (file an issue, relay to the PM, or skip) with
a recommendation; never file externally without that answer, unless a
standing authorization covers it (section 4). Keep open candidates in
`local/state.md` under "Improvement candidates".

<!-- rule:r-063 -->
**Improvement pipeline.** Self-improvement, routines, prompt improvement and
general functional improvement belong to the Architect. Every session reports
framework findings to the Architect (an `IMPROVEMENT REPORT` message, or the
Prompt feedback / Improvement recommendations blocks in its pane). The
Architect records each in the ledger, consolidates them, files the qualifying
changes, and observes whether each change worked. Only meaningful, observable
changes qualify: a concrete change to behavior, instructions, a prompt, a
skill, a hook, the CLI or code, with evidence and a measurable signal. A
recommendation that changes an owner ruling or a policy goes to the owner
first.

## Records

`local/improvements/` holds the pipeline's records:
- `ledger.jsonl`: one line per event, append only;
- `runs/`: one file per consolidation or review run.
Commit both in `local/` at the end of the pass.

## 1. Intake (every pass)

Sources:
- A cross-session message that starts `IMPROVEMENT REPORT` (format below).
- A `Prompt feedback` or `Improvement recommendations` block in a pane the pass
  captured.
- A finding of the Architect's own (a relay rule that failed, a guard refusal,
  a recurring workaround).

Record each item as one ledger line:

```json
{"ts":"<date -u>","event":"report","id":"i-<YYYYMMDD>-<nn>","session":"tm-x","kind":"prompt|skill|hook|cli|code|behavior|instructions|routine","symptom":"...","cause":"...","change":"...","evidence":"...","label":"reported"}
```

Do not file anything at intake. Answer the session only if it asked a question.
A report about the session's own project code (not the framework) goes back to
that PM's own tracker; record it as `dropped` with the reason `project-local`.

## 2. Report format sessions use

Every PM sends findings to the Architect with SendMessage, one message per
batch, at most one per hour unless a finding blocks work:

```
IMPROVEMENT REPORT <session> <HH:MMZ>
- kind: <behavior|instructions|prompt|skill|hook|cli|code|routine>
  symptom: <what was observed>
  cause: <the instruction, skill, tool or harness gap>
  change: <the concrete edit that prevents it>
  evidence: <command output, file:line, elapsed time, issue/PR link>
```

PMs do not file framework issues for these findings themselves. A PM keeps
ticketing hygiene on existing issues (labels, status, closes) and files
issues only when the Architect asks it to.

## 3. Consolidation (daily, and when 10+ reports are open)

Dispatch the read-only `improvement-curator` agent in the background over
the open ledger reports. Its brief asks it to cluster the reports by cause, check each
cluster's evidence, search the target tracker for an existing issue, and
write one recommendation per cluster (symptom, cause, change, evidence,
signal, existing issue if any) to a run file in `runs/`. Read its run file.
Spot-check at least two evidence claims per recommendation before filing.
Record a `clustered` event for every report it consumed.

**Tool-failure review (standard daily task).** Run it with the daily
consolidation; no new cron. Dispatch one read-only `research` agent over the
last 24 h of:
- the Architect's and every PM's transcripts (tool results with `is_error`,
  or text such as "missing", "InputValidationError", "unknown tool",
  "denied", "timed out");
- `mcp__trusty-mpm__list_recent_errors`, and the hook and poller logs in
  `inbox/`.
It groups the failures by tool and cause, with counts. For each group it
decides one fix: a schema or alias change in the tool (an issue for the
owning PM), clearer error text, an instruction or skill line, or caller error
with no tool change. It writes `runs/<date>-tool-failures.md`. Each fix goes
into the ledger as a report and follows sections 4-5. A tool change goes to
the owning PM as a relay.

## 4. Filing

File only under a standing owner authorization recorded in `local/user.md`;
otherwise bring each qualifying recommendation to the user first (r-061).
Under that authorization the Architect files each one itself: a NEW issue, or
a comment when the research agent found an existing issue. Follow the target
repository's ticketing conventions: a symptom title, its area label, a sparse
body with the observable signal and its baseline. Pace bulk writes
(`architect-github`). Record a `filed` event with the issue or comment link
and the baseline. Tell the user in the next report: one line per filed item,
linked.

A recommendation that changes an owner ruling, or a policy, goes to the owner
as a multiple-choice question before filing.

## 5. Observation

When a filed change merges or releases, record `released` with the version.
On the next consolidation the research agent measures the signal. Record
`observed` with the verdict. NO CHANGE or WORSE reopens the item for the next
consolidation with the new evidence.

## Related skills

- `architect-github`: filing and bulk-write pacing.
- `architect-host-admin`: subsystem-health checks that feed tool failures.
