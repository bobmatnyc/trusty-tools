---
name: architect-improver
description: Architect skill. Load on a scheduled improver day, when a session sends an improvement report the improver should weigh, or when reporting on improver runs. Runs the improver agent over all self-improvement signals, has its fixes adversarially reviewed, lets the Architect or the PM choose what to ticket under one self-improvement epic per project, and measures the effective run frequency. Architect only.
user-invocable: true
metadata:
  version: "0.1.0"
category: monitoring
tags: [architect, self-improvement, review]
effort: medium
---

# architect-improver — The Improver

The improver agent consolidates every self-improvement signal (improvement
reports, prompt feedback, tool failures, token alerts) into a few ranked
fixes. On a day the improver runs, it replaces the `improvement-curator`
consolidation and the daily tool-failure review in `architect-improvements`
section 3. Intake (section 1), filing (section 4) and observation (section 5)
still apply.

## Logging

- PMs send prompt feedback and improvement findings with
  `scripts/pm-notify.sh --kind fyi` and text that starts "FEEDBACK:" (held for
  the digest; it never wakes the Architect).
- The Architect records its own feedback as ledger reports.
- Tool failures come from transcripts and logs; nothing extra to log.

## Schedule

Run at most once a day, with growing gaps between runs (1, 2, 2, 2 days); the
first run covers the last 72 h, each later run the time since the run before.
Run in the first full-poll pass of the day, after drift-guard on a shared
day. No new cron.

## One run

1. Dispatch the `improver` agent in the background. Brief: the window, the
   previous run file, the projects with live PMs, and the gh account per
   GitHub owner (`repos` in `local/architect.json`).
2. Adversarial review: dispatch a `research` agent (Opus) with the run file. It
   tries to refute each fix: wrong cause, wrong owner, duplicate, no baseline, a
   fix that hides a real fault, conflict with a ruling. Verdict per fix: keep,
   revise or drop, written into the run file under "Review".
3. Choose: an Architect-owned fix -> do it now (r-061). A PM-owned fix -> relay
   to that PM as "Supervisor question: ticket this under your
   self-improvement epic?"; the PM chooses. A trusty-tools fix -> the Architect
   chooses and files per `architect-improvements` section 4. Every ticket is a
   sub-issue of that project's single self-improvement epic; create the epic
   once per project, when its first item is ticketed.
4. Ledger: one line to `ledger.jsonl` in `local/improver/`: `{run, date,
   window_h, duration_s, agent_tokens, review_tokens, signals, fixes, kept,
   revised, dropped, ticketed, done_now, declined}`.

## Report (after five runs)

Write a brief in `local/briefs/`: per-run table, fixes kept after review
against fixes proposed, ticketed against kept, cost per ticketed fix, overlap
with drift-guard, and the frequency the data supports. Then bring the owner one
decision item: (a) keep it at frequency F, (b) change the frequency, (c) stop.

## Related skills

- `architect-improvements`: intake, filing and observation.
- `architect-drift-guard`: the instruction-drift run that shares its day.
