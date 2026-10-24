---
name: architect-drift-guard
description: Architect skill. Load on a scheduled drift-guard day, when the owner asks for a drift review of the instructions, or when reporting on drift-guard runs. Runs the drift-guard agent over the instruction layer, records each run, stores each finding's remediation in memory, routes its findings, and measures the effective run frequency. Architect only.
user-invocable: true
metadata:
  version: "0.1.0"
category: monitoring
tags: [architect, drift, instructions, memory]
effort: medium
---

# architect-drift-guard — Instruction Drift

The drift-guard agent reviews the instruction layer (the Architect's files, the
framework instructions and agents, owner rulings, dashboard decisions and
ADRs) for logical consistency, and lists rules that are baked in and belong in
a spec. Not the same as `architect-drift-check`: that skill compares a
project's code with its architecture baseline. This one checks the rules
themselves.

## Memory (agent tag `agent:drift-guard`)

Findings and remediations live in trusty-memory so the agent, the Architect
and each PM can see what drift-guard found and what was done.

- **Palace by subject:** `supervisor` for Architect files and rulings;
  `trusty-tools` for trusty-tools instructions, agents, specs and ADRs; a
  project's palace for that project's PM instructions.
- **Findings:** the agent stores one drawer per new finding (tags
  `agent:drift-guard`, `finding`, `<id>`, `kind:<kind>`, `status:open`).
- **Remediations:** the Architect stores one drawer per routed finding in the
  same palace, in the pass that routes it: text `drift-guard remediation <id>:
  <fixed|rejected|routed|pending>. <what was done, file:line or issue link, or
  the rejection reason>. By <Architect|PM|owner> <date>.`; tags
  `agent:drift-guard`, `remediation`, `<id>`, `status:<status>`. A later status
  change gets a new drawer; never forget the old one.
- **PMs:** a finding routed to a PM carries its id in the relay; the PM can
  recall `drift-guard` in its project palace.
- No `fact_key` on either kind: those facts expire, and this is history.
- Name a run by its time (`run 2026-10-12T0345Z`), never by file path: the
  secret filter rejects the path and `force` does not bypass it. Similar
  remediation texts trip the dedup filter; add the finding id and pass
  `force=true`. Never `allow_secret_like`.

## Schedule

Run at most once a day, with growing gaps between runs (1, 1, 2, 3 days), so
the yield per run can be compared by gap. No new cron: run it in the first
full-poll pass of a scheduled day, with the daily goal brief. In user state
Sleeping, skip a run only if plan usage is above 75% weekly; record the skip.

## One run

1. Dispatch the `drift-guard` agent in the background. Brief: run date, scope
   (default: all inputs in the agent file), previous run file path, and the
   trusty-tools checkout to read.
2. When it reports, spot-check two findings against their sources, and one
   stored finding drawer, before acting.
3. Route each new finding: an Architect-owned file -> fix it now (r-061),
   remediation `fixed`; a PM's instructions -> relay to that PM with the finding
   id, remediation `routed`; a trusty-tools spec or instruction change -> the
   improvement ledger (`architect-improvements`), remediation `routed`; a
   conflict between two owner rulings, or a policy change -> a decision item for
   the owner, remediation `pending` (then `fixed` or `rejected` on the answer).
   A finding that is wrong -> remediation `rejected` with the reason.
4. Store the remediation drawers (see Memory).
5. Append one line to `ledger.jsonl` in `local/drift-guard/`: `{run, date,
   gap_days, duration_s, agent_tokens, sources, new, repeat, blocking,
   accepted, rejected, routed, drawers}`. `agent_tokens` and `duration_s` come
   from the agent's usage line.

## Report (after five runs)

Write a brief in `local/briefs/`: per-run table from the ledger; new findings
and accepted findings per run against the gap; precision (accepted /
(accepted + rejected)); repeat rate; cost per accepted finding; the frequency
the data supports; whether it is effective. Then bring the owner one decision
item: (a) keep it at frequency F, (b) change the frequency, (c) stop.

## Related skills

- `architect-improvements`: where a trusty-tools change is recorded.
- `architect-drift-check`: architecture drift in a project's code.
