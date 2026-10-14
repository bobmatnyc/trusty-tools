---
name: tm-context-refresh
description: The Architect's context refresh for a watched trusty-mpm session or its own. Checkpoint with /tm-session-pause, verify the snapshot, /clear, then /tm-session-resume, each step verified before the next. Bar about 250k tokens. Also own-context measurement and the crons and Monitors that must survive a clear. Architect only.
user-invocable: true
version: "0.2.0"
category: monitoring
tags: [architect, context, pause, resume, tmux]
effort: high
---

# tm-context-refresh — Refresh a Session's Context

**Threshold: about 250k tokens of context** (25% of a 1M window), with 220k
to 249k counted as approaching. Every turn re-reads the whole context, so cache
reads grow with its size. The bar applies to the watched sessions and to your
own, and replaces the stock 70% caution default. A routine verified refresh
needs no extra confirmation. This skill is the step-by-step procedure; every
step is verified before the next.

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
   Session crons survive `/clear`: tell the PM before the clear to
   `CronDelete` its own crons, and to re-create them after the resume.
3. **Verify the checkpoint itself**, not an acknowledgement or a spinner:
   - The pause writes `.trusty-mpm/sessions/<session-id>/session-*.md` and
     appends an entry for this session to
     `.trusty-mpm/sessions/sessions-log.jsonl` in the watched project. Its
     `snapshot` field is the path relative to `.trusty-mpm/sessions/`, for
     example `<session-id>/session-YYYYMMDD-HHMMSS.md`.
   - Confirm the snapshot file exists, is new, belongs to this session's id,
     and names the work in progress and the next steps.
   - Record the snapshot path and the session identity in `local/actions.md`.
   - Never invent a session id, and never accept another session's latest
     snapshot as proof.
4. **Only after that, send `/clear` alone**, once the input box is empty.
   Verify that the conversation reset before you go on.
5. **Send `/tm-session-resume`** in the same pane. It resolves the session's
   own snapshot first, then its own tmux window, never the latest overall.
   Verify that it resolves the snapshot from step 3 and no older one,
   restores the task state, and continues the authorized work. A missing or
   ambiguous checkpoint is a recovery decision for the user, not permission
   to clear again.
6. **Record** in `local/actions.md` and `local/state.md`: the context
   before and after, the snapshot path, the exact commands sent, the
   acknowledgements seen, and any restoration problem. When the restore is
   ambiguous, keep the state and ask the user. Never loop pause, clear and
   resume, and never assume an agent died.

## Draining a PM before a refresh

- A drain-for-refresh brief says which dispatches it still allows, for
  example: "a merge agent for work already cleared is allowed; no new
  builders, critics or research".
- A draining PM has no wake-up for CI it waits on. While it drains on a PR's
  CI, check `gh pr checks <n> --json bucket` on each wake and tell the PM when
  CI finishes, with any failing check and its link.

## Your own session

- Measure your own context on every wake: `tm architect self-ctx` prints the
  tokens of the last assistant turn in this session's transcript. Your status
  line has no `ctx` field, and its `⤵ A→B` field shows the last compaction,
  not the current use.
- Over about 250k tokens, refresh yourself at the next safe boundary: no
  dialog open for the user and no relay half sent. Verify the snapshot first.
- After every resume, `TaskStop` the pre-clear Monitor ids and re-arm the
  Monitors and crons once. Never let the session reach automatic compaction.

## Hard rules

- Never send `/tm-session-pause`, `/clear` and `/tm-session-resume` in one
  batch. Verify between each step.
- Never overwrite or submit a draft to make room for a checkpoint command.
- Never repeat a refresh that is in progress, and never `/clear` a session
  without a verified checkpoint.
- An accepted command or a spinner is not a finished pause. Wait for the
  snapshot and verify it.

## Related skills

- `tm-fleet-check`: the supervision pass that starts this procedure (step 8).
- `architect-tmux-ops`: the tmux send and verify commands each step uses.
