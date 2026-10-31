---
name: tm-context-refresh
description: The Architect's context refresh for a watched trusty-mpm session or its own. Checkpoint with /tm-session-pause, verify the snapshot, /clear, then /tm-session-resume, each step verified before the next, when a session reaches the context bar (250k tokens by default). Architect only.
user-invocable: true
metadata:
  version: "0.1.0"
category: monitoring
tags: [architect, context, pause, resume, tmux]
effort: high
---

# tm-context-refresh — Refresh a Watched Session's Context

**Run it at the bar (250k tokens by default; set in `CLAUDE.md`).** The
poller raises a `ctx` alert marked `approaching` at 90% of the bar and `bar`
at the bar, and a `self_ctx` alert when your own session reaches it. The bar
applies to the supervised sessions and to you, and replaces the stock 70%
caution default. At the bar the poller only alerts; you run this procedure,
and nothing is cleared for you. This skill is the step-by-step procedure;
every step is verified before the next. A routine refresh that passes every
check needs no extra confirmation from the user.

Paths starting with `scripts/` or `local/` are relative to this project's
root. `.trusty-mpm/sessions/` is inside the WATCHED project's root.

## Preconditions

- The target pane is at a safe interactive boundary: no delegation or child
  agent running that a pause would interrupt, and no unsubmitted draft in the
  input box.
- When it is not safe, record the deferral and what you wait for in the
  context-refresh queue in `local/state.md`, and check again next pass.
  Never force it.

## Steps

1. **Revalidate.** Confirm the session, window, pane and project path with raw
   `tmux`, then classify the input box with
   `tm architect input-state '=<session>:'`. Continue only on `empty`
   or `suggestion`. Re-capture immediately before every send.
2. **Send `/tm-session-pause` alone**, with the two-step send: the literal
   text first (`tmux send-keys -t '=<session>:' -l '/tm-session-pause'`),
   confirm the box holds only that text, then Enter as a separate call. Never
   combine it with `/clear`. You may add a line asking it to keep the current
   tasks, agent ids, authorizations, blockers and next steps. Never a secret.
3. **Verify the checkpoint itself**, not an acknowledgement or a spinner:
   - The pause appends an entry for this session to
     `.trusty-mpm/sessions/sessions-log.jsonl` in the watched project. Its
     `snapshot` field is the path relative to `.trusty-mpm/sessions/`, for
     example `<session-id>/session-YYYYMMDD-HHMMSS.md`.
   - Confirm the snapshot file exists, is new, belongs to this session's id,
     and names the work in progress and the next steps.
   - Record the snapshot path and the session identity in
     `local/projects/<project>.md`.
   - Never invent a session id, and never accept another session's latest
     snapshot as proof.
4. **Only after that, send `/clear` alone**, once the input box is empty.
   Verify that the conversation reset before you go on.
5. **Send `/tm-session-resume`** in the same pane. Verify that it resolves the
   snapshot from step 3 and no older one, restores the task state, and
   continues the authorized work.
6. **Record** in `local/actions.md` and `local/state.md`: the context
   before and after, the snapshot path, the exact commands sent, the
   acknowledgements seen, and any restoration problem. When the restore is
   ambiguous, keep the state and ask the user. Never loop pause, clear and
   resume, and never assume an agent died.

## Pause and resume semantics

- `/tm-session-pause` writes `.trusty-mpm/sessions/<session-id>/session-*.md`,
  appends to `sessions-log.jsonl`, may prune stale worktrees, and publishes
  the snapshot to a session git ref. Record a publication error; never hide
  it.
- `/tm-session-resume` resolves the session's own snapshot first, then its
  own tmux window, never the latest snapshot overall. A missing or ambiguous
  checkpoint is a recovery decision for the user, not permission to clear
  again.
- Session crons survive `/clear`: the process keeps the jobs created before
  the clear firing, the cron list shows only the jobs created after it, and a
  resume that re-creates its crons doubles them. In every refresh, record the
  crons with their ids, keep them, and re-create none.
- A drain-for-refresh brief names the dispatches it still allows, for
  example: "a merge agent for work already cleared is allowed; no new
  builders, critics or research".
- A draining PM has no wake-up for the CI it waits on; only a finishing agent
  wakes it. While a PM drains on a PR's CI, check `gh pr checks <n>` on each
  pass and tell the PM when CI finishes, with any failing check and its link.

## The Architect's own session

- Your own pane shows no `ctx` field. The poller measures your context in
  tokens from this project's newest interactive transcript with
  `scripts/self-ctx.py` and raises a `self_ctx` alert at the same bar.
- Refresh yourself at the next safe boundary: no dialog open for the user
  and no relay half sent. Your Intent Ledger is the handoff, not a
  pause snapshot: update it (tasks, the session's next, waiting_on and owed,
  agents, crons and Monitors with their ids) so its digest (the `digest`
  verb of `scripts/datg.py`) summarises all current activity, then run
  `scripts/self-clear.sh --live`. Without `--live` it is a
  dry run. It runs `<ROOT>/scripts/datg.py --project <ROOT>/local/il
  ready` before the dry run and again after the idle wait.
  Only exit 0 allows a clear; 1 (not ready, reasons logged to
  `inbox/self-clear.log`), 2 (ledger missing or unreadable) or any other
  code refuses. Idle needs `turn-active.py` to exit 10; any other
  code is busy. Then it sends `/clear` and a resume prompt that reads the
  digest.
  Write no handoff block in `local/state.md`.
- Never let your session reach automatic compaction.

## Intent Ledger sessions

For a session the Intent Ledger schedule lists, the checkpoint is a fresh,
verified ledger digest, written and checked before `/clear`; steps 2, 3 and
5 above (pause, snapshot check, resume command) do not apply. Every other
session keeps the pause, clear and resume steps. Spec: SPEC-IL-01,
sections 8 and 11.3.

## Hard rules

- Never send `/tm-session-pause`, `/clear` and `/tm-session-resume` in one
  batch. Verify between each step.
- Never overwrite or submit a draft to make room for a checkpoint command.
- Never repeat a refresh that is in progress, and never `/clear` a session
  without a verified checkpoint.
- An accepted command or a spinner is not a finished pause. Wait for the
  snapshot and verify it.

## Related skills

- `tm-fleet-check`: the supervision pass that starts this procedure (step 9).
- `architect-tmux-ops`: the two-step send each step uses.
