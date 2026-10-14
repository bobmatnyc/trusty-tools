---
name: architect-improvements
description: The Architect's improvement pipeline — take in framework improvement reports from every fleet session, record them in the ledger, consolidate them with the improvement-curator agent, file the qualifying changes on trusty-tools, and observe whether each change worked.
version: 1.0.0
category: architecture
when_to_use: when a session sends an IMPROVEMENT REPORT, when a captured pane shows a Prompt feedback or Improvement recommendations block, once a day for consolidation, and after a filed change merges or releases
author: trusty supervisor
license: Elastic-2.0
effort: medium
tags:
  - architect
  - supervisor
  - self-improvement
---

Kind: framework (the Architect's own skill; portable with the framework core).

# Architect improvements

owner, 2026-10-04 15:0xZ: self-improvement, routines, prompt improvement and
general functional improvement move to the Architect. Every session reports
issues to the Architect. The Architect collects and consolidates them, observes
them, and is the one that submits issues to trusty-tools, based on its analysis
and consolidated recommendations. Focus on meaningful, observable changes in
behavior, instructions, prompts and code.

## 1. Intake (every pass)
Sources:
- A cross-session message that starts `IMPROVEMENT REPORT` (format below).
- A `Prompt feedback` or `Improvement recommendations` block in a pane the pass
  captured.
- A finding of the Architect's own (a relay rule that failed, a guard refusal,
  a recurring workaround).

Record each item as one line in `local/improvements/ledger.jsonl`:

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

PMs no longer post to #8021 or file trusty-tools issues for these findings.
a PM keeps ticketing hygiene on existing issues (labels, status, closes)
and files issues only when the Architect asks it to.

## 3. Consolidation (daily, and when 10+ reports are open)
Dispatch the `improvement-curator` agent in the background. Read its run file.
Spot-check at least two evidence claims per recommendation before filing.
Record a `clustered` event for every report it consumed.

## 4. Filing
The Architect files each qualifying recommendation itself (the owner's 2026-10-04
authorization): a NEW issue, or a comment when the curator found an existing
issue. Follow trusty-tools TICKETING.md: symptom title, one `area:` label,
the `self-improvement` label, a sparse body with the observable signal and its
baseline. Pace bulk writes (`commands.md`, secondary rate limit). Record a
`filed` event with the issue or comment link and the baseline. Tell the owner in the
next report: one line per filed item, linked.

A recommendation that changes a owner ruling, or a policy, goes to the owner as a
multiple-choice question before filing.

## 5. Observation
When a filed change merges or releases, record `released` with the version.
On the next consolidation the curator measures the signal. Record `observed`
with the verdict. NO CHANGE or WORSE reopens the item for the next
consolidation with the new evidence.

## 6. Records
- Ledger: `local/improvements/ledger.jsonl` (append only).
- Curator runs: `local/improvements/runs/`.
- Commit both in `local/` at the end of the pass.

## 7. Rule promotion (owner ruling 95, 2026-10-07)
Design: `local/briefs/2026-10-07-rule-promotion-design.md`. Ledger: the `architect` palace, tagged `rule`
(append-only; each rule has an anchor `<!-- rule:r-NNN -->` in architect.md or AGENTS.md).
When you record a owner ruling that changes a rule, store a `rule`-tagged palace entry (event, id, title, where, owner_quote, class, stage, signal).
When you add or reword a rule in architect.md or AGENTS.md, give it an anchor and a ledger line in the same pass.

Daily rules step (part of the daily consolidation cron; no new cron):
1. List headings and bullets with a "the owner, <date>" tag and no `rule:` anchor; anchor them and add a ledger line.
2. Advance stages: `experimenting` to `validated` after 14 days with no reversal and at least one use in `local/actions.md`
   (a hard-limit rule skips the wait once the owner has ruled). Record the evidence in the ledger line.
3. List the promotion queue: class `product`, stage `validated`, not shipped. Group by product file; at most one batch a week.
4. Draft the batch in `local/briefs/<date>-rule-promotion-batchN.md`: one multiple-choice question per policy rule,
   recommendation first; tool quirks ship on the Architect's call. File nothing on trusty-tools without the owner's answer.
Classify with the five-question test (design brief, section 4): personal, local, product, tool-defect, process.

## Improvement mandate and pipeline (moved from the operating manual "Workflow", restructure 2026-10-07)

Kind: framework. Index line in architect.md: load this for an improvement finding, a framework or Architect change, or a question about the improvement pipeline.

<!-- rule:r-061 -->
- **Improvement mandate.** On every pass, look for ways to improve a watched project, the trusty-mpm framework, or the Architect itself. Fix Architect-owned files directly. For a project or the framework, bring the user a multiple-choice proposal (file an issue, relay to the PM, or skip) with a recommendation; never file externally without that answer. Exception: framework (trusty-tools) improvements follow the improvement pipeline below, which the owner authorized the Architect to file. Keep open candidates in `local/state.md` under "Improvement candidates".

<!-- rule:r-063 -->
- **Improvement pipeline (owner, 2026-10-04 15:0xZ).** Self-improvement, routines, prompt improvement and general functional improvement belong to the Architect. Every session reports framework findings to the Architect (an `IMPROVEMENT REPORT` message, or the Prompt feedback / Improvement recommendations blocks in its pane); sessions no longer post to #8021 or file trusty-tools issues for them. The Architect records each in `local/improvements/ledger.jsonl`, consolidates them with the `improvement-curator` agent, files the qualifying changes on trusty-tools itself, and observes whether each change worked. Only meaningful, observable changes qualify: a concrete change to behavior, instructions, a prompt, a skill, a hook, the CLI or code, with evidence and a measurable signal. Procedure: the `architect-improvements` skill. A recommendation that changes a owner ruling or a policy goes to the owner first.
