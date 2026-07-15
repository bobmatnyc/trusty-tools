---
name: tm-session-pause
description: Pause the current PM session — snapshot todos, git state, and context to a project-local session file, prune stale worktrees, and print the resume path
user-invocable: true
version: "1.0.0"
category: pm-workflow
tags: [session, pause, worktree, context, pm-recommended]
effort: medium
---

# /tm-session-pause — Pause & Snapshot

Focused entry point for pausing the current session and saving all work state
for a later `/tm-session-resume`. This is the *action*; the policy, thresholds,
and full format reference live in `/tm-session-management`.

## What This Does

When invoked, this skill:
1. Captures current work state — todos (`TodoWrite`), git status, and a short
   context summary (plus any message you pass after the command).
2. Writes a project-local snapshot at
   `.trusty-mpm/sessions/session-{timestamp}.md`.
3. **Appends** a `pause` line to the append-only
   `.trusty-mpm/sessions/sessions-log.jsonl` (never overwrites — so concurrent
   `tm` sessions in the same project don't clobber each other's resume target).
4. **Prunes stale git worktrees** left behind by decommissioned managed
   sessions (see below).
5. Prints the snapshot path so you can resume later.

## Usage

```
/tm-session-pause [optional message describing current work]
```

Examples:
```
/tm-session-pause
/tm-session-pause Working on the auth refactor, about to test the login flow
/tm-session-pause Need to context-switch to an urgent bug fix
```

## Session File Location (Project-Local)

Snapshots are stored **project-local**, never under the user's home directory,
so pausing in project A and opening project B never loads project A's state:

```
<project-root>/.trusty-mpm/sessions/
├── sessions-log.jsonl          # append-only per-session pause/resume log
└── session-YYYYMMDD-HHMMSS.md  # human-readable snapshot
```

`sessions-log.jsonl` holds one JSON object per line — one per pause/resume
event:

```json
{"session_id":"<id>","event":"pause","snapshot":"session-20260715-101500.md","timestamp":"2026-07-15T10:15:00Z"}
```

Because it is **append-only**, two `tm` sessions pausing in the same project
each keep their own history: resume resolves the latest snapshot for the
*current* session id, and "latest overall" is simply the last `pause` line. The
legacy global `LATEST-SESSION.txt` pointer is **no longer written** (a single
overwritten pointer let concurrent sessions clobber each other's resume target);
resume still reads it, and an mtime scan of `session-*.md`, as back-compat
fallbacks when no log is present.

Add `.trusty-mpm/sessions/` to `.gitignore` — this is machine-local state, not
a deliverable. No git commit is created by pausing.

## Snapshot Content

```markdown
# Session Pause - {timestamp}

## Summary
{user-provided message, or auto-generated from current todos}

## Completed
{completed todos/tasks this session}

## In Progress
{in-progress todos with detailed state}

## Next Steps
{pending todos and recommended next actions}

## Git Context
Branch: {current branch}
Last commit: {hash and message}
Uncommitted changes: {git status summary}

## Tmux Window
{session_name:window_index:window_id, e.g. main:2:@7 — omit this section entirely when not inside tmux}
```

The `## Tmux Window` section records the originating tmux window so
`/tm-session-resume` can re-align you to it with `tmux select-window`. Only
include the section when the capture step below produced a value; snapshots
created outside tmux simply omit it (resume treats absence as a no-op).

## Procedure

```bash
git status                       # working-tree state for the Git Context block
git log --oneline -10            # recent commits for context
mkdir -p .trusty-mpm/sessions    # ensure the store exists

# Capture the current tmux window ONLY when inside tmux; skip the section otherwise.
if [ -n "$TMUX" ]; then
  tmux display-message -p '#{session_name}:#{window_index}:#{window_id}'
fi

# Resolve a stable session id for the log: prefer $TM_SESSION_ID, else the tmux
# session:window (stable within a session), else "default".
SID="${TM_SESSION_ID:-$([ -n "$TMUX" ] && tmux display-message -p '#{session_name}:#{window_index}' || echo default)}"
SNAP="session-$(date +%Y%m%d-%H%M%S).md"
TS="$(date -u +%Y-%m-%dT%H:%M:%SZ)"

# write $SNAP using the format above — include a `## Tmux Window` section holding
# the captured string ONLY if the command above printed one, then APPEND (never
# overwrite) one pause line to the log:
printf '{"session_id":"%s","event":"pause","snapshot":"%s","timestamp":"%s"}\n' \
  "$SID" "$SNAP" "$TS" >> .trusty-mpm/sessions/sessions-log.jsonl
```

**Do not** write `LATEST-SESSION.txt` — the append-only log replaces it. Use
`>>` (append), never `>` (truncate), so a concurrent session's history is
preserved.

Reconcile the todo list against `git status` first — mark blockers explicitly —
so the snapshot reflects the *current* state, not a stale one. Then report the
written path to the user.

## Worktree Pruning

Decommissioned managed sessions (`tm session new` / `mcp__trusty-mpm__session_new`)
can leave orphaned per-session git worktree directories behind. As part of
pause wrap-up, prune them:

```bash
tm session prune-worktrees          # dry-run by default (preview only)
tm session prune-worktrees --force  # actually remove the orphaned dirs
```

Only directories with **no** corresponding active session are ever touched.
`tm doctor`'s `worktrees` probe reports the orphan count and suggests this
command; run it whenever that probe is non-zero, and always before ending a
long session that spawned managed sessions.

## Durable Tasks vs. Snapshot Prose

The snapshot captures the PM's own `TodoWrite` state as prose. For work items
that must survive as durable, queryable tasks visible to *any* future session
(not just a resumed one), promote them via `mcp__trusty-memory__task_add` /
`task_list` / `task_complete` before pausing.

## Token Budget

~5–10k tokens to execute (2–5% of context budget). The payoff is that all
remaining context is freed for a clean resume, a context switch, or a break.

## Resume Later

```
/tm-session-resume
```

or from the CLI:

```bash
tm session catchup
```

## Related

- `/tm-session-resume` — load context from a paused session
- `/tm-session-management` — policy, the 70% auto-pause threshold, full format
  reference, and the task-list integration this action is a focused slice of
