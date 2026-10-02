---
name: tm-context-refresh
description: The Architect's context refresh for a watched trusty-mpm session. Checkpoint with /tm-session-pause, verify the snapshot, /clear, then /tm-session-resume, each step verified before the next. Default threshold 50% context. Architect only.
user-invocable: true
version: "0.1.0"
category: monitoring
tags: [architect, context, pause, resume, tmux]
effort: high
---

# tm-context-refresh — Refresh a Watched Session's Context

**Threshold: 50% context by default**, with 45 to 49% counted as approaching.
This project's `CLAUDE.md` may set another value. The threshold applies to the
supervised sessions and replaces the stock 70% caution default. This skill is
the step-by-step procedure; every step is verified before the next.

Paths starting with `scripts/` or `records/` are relative to this project's
root. `.trusty-mpm/sessions/` is inside the WATCHED project's root.

## Preconditions

- The target pane is at a safe interactive boundary: no delegation or child
  agent running that a pause would interrupt, and no unsubmitted draft in the
  input box.
- When it is not safe, record the deferral and what you wait for in the
  context-refresh queue in `records/state.md`, and check again next pass.
  Never force it.

## Steps

1. **Revalidate.** Confirm the session, window, pane and project path with raw
   `tmux`, then classify the input box with
   `python3 scripts/input-state.py '=<session>:'`. Continue only on `empty`
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
     `records/projects/<project>.md`.
   - Never invent a session id, and never accept another session's latest
     snapshot as proof.
4. **Only after that, send `/clear` alone**, once the input box is empty.
   Verify that the conversation reset before you go on.
5. **Send `/tm-session-resume`** in the same pane. Verify that it resolves the
   snapshot from step 3 and no older one, restores the task state, and
   continues the authorized work.
6. **Record** in `records/actions.md` and `records/state.md`: the context
   before and after, the snapshot path, the exact commands sent, the
   acknowledgements seen, and any restoration problem. When the restore is
   ambiguous, keep the state and ask the user. Never loop pause, clear and
   resume, and never assume an agent died.

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
