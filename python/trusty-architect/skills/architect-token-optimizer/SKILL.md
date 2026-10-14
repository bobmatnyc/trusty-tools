---
name: architect-token-optimizer
description: The Architect's token-optimization loop — read the deterministic watcher's `token` alerts, dispatch the token-optimizer agent (Sonnet, read-only) for ranked suggestions, record them, relay or propose the changes, and observe whether each worked.
version: 1.0.0
category: architecture
when_to_use: when a pass reads a `token` alert in inbox/alerts.jsonl, once a day with the goal brief, and when plan usage crosses a threshold (75% weekly, or a projected 5-hour exhaustion)
author: trusty supervisor
license: Elastic-2.0
effort: low
tags:
  - architect
  - supervisor
  - tokens
---

Kind: framework (the Architect's own skill; portable with the framework core).

# Architect token optimizer

owner, 2026-10-07 17:2xZ: "an architect token-optimizer agent ... both a watcher
and suggester ... part of the architect deployment." The watcher
(`scripts/token_watch.py`, spec `local/briefs/2026-10-07-token-watch-spec.md`)
is deterministic and runs inside the poller. This skill is the suggester half.
the owner's rule: inference only when action is needed.

## 1. When to run
- A `[poll ...]` wake lists `token` alerts: run the steps below for those alerts only.
- Daily, as a step of the goal-brief cron (no new cron): run once with `--since 24h`.
- Plan usage crossing 75% weekly: run once with `--since 6h`.
Skip the run when the window holds no `token` alert and the daily brief is not due.
Never run the agent more than once per hour.

## 2. Read the alerts first (no agent)
`grep '"kind": "token"' inbox/alerts.jsonl | tail -20`. Handle these yourself,
without the agent, when the rule is routine:
- `ctx_main` at 250k: the existing refresh procedure (`tm-context-refresh`).
- `fanout` over the cap while pacing b5 is active: relay pacing rule 1 to that PM.
Everything else goes to step 3.

## 3. Dispatch the agent
Run the `token-optimizer` agent in the foreground (one turn of work, no wake).
Brief it with: the window, the alert lines, the ledger path, and the sessions
parked or running (from `local/state.md`). Model is Sonnet by definition; pass no
`model` override.

## 4. Review the result
Spot-check one evidence number per item with `scripts/token_watch.py report`
or a bounded transcript read. A suggestion that fails the check is dropped.
Items marked "the owner decides" go to "Pending for the owner" in `local/state.md` with a
D id (architect.md "Ids"); open no dialog.

## 5. Act within authority
- Pacing a PM and relaying an existing ruling: send the draft relay with the
  verified two-step send and the `[<Name> Architect HH:MMZ]` marker.
- A framework change (BASE-AGENT text, agent definitions, tm defaults): record in
  `local/improvements/ledger.jsonl` as kind `behavior`/`prompt`/`code` with
  source `token-optimizer`, then follow `architect-improvements` for filing.
- A change to a model assignment, config or rule: the owner first.

## 6. Observe
Record each acted suggestion in `local/actions.md` with its signal and baseline.
On the next daily run, give the agent the earlier items; it reports IMPROVED,
NO CHANGE, WORSE or TOO EARLY. NO CHANGE or WORSE for two runs: tell the owner.

## 7. Records
- Suggestions: one line each in `local/improvements/ledger.jsonl` when filed.
- Trend: `local/usage/token-hourly.jsonl` (written by the watcher).
- Commit both in `local/` at the end of the pass.
