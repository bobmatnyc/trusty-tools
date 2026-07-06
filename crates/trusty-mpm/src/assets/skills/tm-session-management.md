---
name: tm-session-management
description: PM context-limit pause/resume, project-local session snapshots, worktree pruning, and task-list integration
user-invocable: true
version: "1.0.0"
category: pm-workflow
tags: [session, context, pause, resume, worktree, pm-recommended]
effort: medium
---

# /tm-session — Session Management

Overview and policy reference for PM session pause/resume. For the focused
per-action entry points, use `/tm-session-pause` (snapshot + prune) and
`/tm-session-resume` (load + restore) — this skill stays as the thresholds,
format, and integration reference they build on.

This is a PM self-monitoring convention,
not a hard-coded token counter — the PM must judge its own context usage and
act on the thresholds below; native Claude Code has no equivalent
conversation-level auto-pause, so this behavior is tm-specific, not a
duplicate of anything Claude Code already does.

## The 70% Auto-Pause Threshold

Per `PM_INSTRUCTIONS.md`'s `## Session Management` section, this skill loads
on-demand once context usage crosses **70%**, when a pause-state file
already exists, or when the user explicitly asks to resume.

| Level | Usage | PM behavior |
|---|---|---|
| Caution | 70% | Load this skill; start watching for a natural wrap-up point |
| Warning | 85% | Finish the current delegation cycle; don't start new major tasks |
| Wrap-up | 90%+ | Create a session snapshot proactively, even without a user request |

At 70%+: wrap up the current phase, ensure every todo reflects current
status (mark blockers explicitly), delegate any remaining work with a
complete handoff (context + acceptance criteria + relevant files/commits),
then create the snapshot below.

## Pausing: Session File Format (Project-Local)

Sessions are stored **project-local**, not under the user's home directory:

```
<project-root>/.trusty-mpm/sessions/
├── LATEST-SESSION.txt          # pointer to the most recent session
└── session-YYYYMMDD-HHMMSS.md  # human-readable snapshot
```

Add `.trusty-mpm/sessions/` to `.gitignore` — this is machine-local state,
not a deliverable.

Snapshot content:

```markdown
# Session Pause - {timestamp}

## Summary
{user-provided message or auto-generated from current todos}

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
```

Procedure: `git status` + `git log --oneline -10` for context → `mkdir -p
.trusty-mpm/sessions` → write `session-{timestamp}.md` → update
`LATEST-SESSION.txt` → report the path to the user.

## Resuming: `tm session catchup`

Resume delegates to the real CLI command rather than manually parsing
session files:

```bash
tm session catchup                  # current project only
tm session catchup --all-projects   # also scan machine-wide claude-mpm/trusty-mpm projects
```

This renders a unified, newest-first digest across both the native
`.trusty-mpm/sessions/` format and the legacy `.claude-mpm/sessions/` format
(a cutover bridge — see `core/catchup/`, issue #1762 — that will be removed
once migration off claude-mpm is complete). After running catchup: reconcile
against `git log --oneline -5` / `git status`, present the digest, confirm
which session to resume from if more than one is listed, restore todo state,
and confirm with the user before continuing work.

Manual `/tm-session resume` does **not** advance the internal watermark used
by auto-inject-on-session-start — only the automatic injection path does.
This is intentional: a manual catch-up is a read, not a state transition.

## Worktree Pruning Integration

Paused/resumed PM sessions are conceptually distinct from *managed*
session-manager (SM) tmux sessions (`tm session new`, driven via
`mcp__trusty-mpm__session_new`/`session_list`/etc.) but share the same
worktree hygiene concern: orphaned per-session git worktrees left behind by
decommissioned SM sessions. `tm doctor`'s `worktrees` probe flags these; the
cleanup command is:

```bash
tm session prune-worktrees          # dry-run by default
tm session prune-worktrees --force  # actually remove
```

Run this as part of session wrap-up when `tm doctor` reports orphaned
worktrees, and always before ending a long working session that spawned
managed sessions.

**Worktree Architecture:** For the design of the session↔worktree 1:1 model,
semantic naming, and per-worktree search-index lifecycle, see
`docs/ARCHITECTURE-MEMORY-SESSIONS-SEARCH.md` § 2 (session↔worktree model)
and § 3 (search index pinning). Managed sessions isolate work via git worktrees
with semantic names (e.g., `tm-trusty-tools-01`); decommission removes the
worktree, its branch, and its associated search index atomically.

## Task-List Integration

Pause/resume snapshots capture the PM's own `TodoWrite` state, but for
work items that should survive across the pause/resume boundary as durable,
queryable tasks (not just prose in a snapshot), use
`mcp__trusty-memory__task_add` / `task_list` / `task_complete` — these
persist independent of the session snapshot file and are visible to any
future session, not just a resumed one. Prefer `TodoWrite` for
in-session-only progress tracking; promote a todo to
`mcp__trusty-memory__task_add` when it needs to survive past this session's
resume boundary.

## No Sessions Found

```
No paused sessions found.
```
Direct the user to pause first if they expected one.

## Related Skills

- `/tm-session-pause` — focused action: snapshot todos/git/context, prune stale
  worktrees, print the resume path
- `/tm-session-resume` — focused action: load the latest (or selected) snapshot
  via `tm session catchup` and restore todos/context
- `tm-git-file-tracking` — git state reconciliation during resume
- `tm-verification-protocols` — evidence state carried across a pause
- `tm-delegation-patterns` — resuming mid-workflow delegations
