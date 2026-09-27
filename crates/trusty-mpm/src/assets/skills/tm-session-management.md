---
name: tm-session-management
description: PM context-limit pause/resume, project-local session snapshots, worktree pruning, and task-list integration
user-invocable: true
version: "1.1.0"
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
├── sessions-log.jsonl              # append-only per-session pause/resume log
├── <session-id>/
│   └── session-YYYYMMDD-HHMMSS.md  # human-readable snapshot
└── session-YYYYMMDD-HHMMSS.md      # pre-#5272 snapshot, still resolvable
```

Pausing **appends** one `pause` line per snapshot to `sessions-log.jsonl`
(`{"session_id","event","snapshot","timestamp"}`, where `snapshot` is the path
relative to `sessions/`) instead of overwriting a single global pointer — so
concurrent `tm` sessions in the same project never clobber each other's resume
target. `LATEST-SESSION.txt` is **no longer written**.

🔴 **Resume never crosses session boundaries (#5272).** It resolves only
snapshots the log attributes to the session id you ask for. A session with no
snapshot of its own gets nothing — there is no latest-overall, pointer, or
mtime fallback, because with the PM on the project's main checkout several
sessions share one store and those fallbacks hand over someone else's context.
Flat snapshots at the store root still resolve through their log line; one with
no log line is attributable to nobody and resolves for nobody.

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

## Tmux Window
{session_name:window_index:window_id, e.g. main:2:@7 — omit this section entirely when not inside tmux}
```

The `## Tmux Window` section records the originating tmux window so resume can
re-align to it. Capture it ONLY when inside tmux (`[ -n "$TMUX" ]`) via
`tmux display-message -p '#{session_name}:#{window_index}:#{window_id}'`; when
`$TMUX` is unset, omit the section (resume treats absence as a no-op).

Procedure: `git status` + `git log --oneline -10` for context → `mkdir -p
.trusty-mpm/sessions` → when inside tmux, capture the window string (above) →
write `session-{timestamp}.md` (include `## Tmux Window` only if captured) →
**append** one `pause` line to `sessions-log.jsonl` (never overwrite; do not
write `LATEST-SESSION.txt`) → report the path to the user.

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
against `git log --oneline -5` / `git status`, present the digest, restore todo
state, then continue under the Autonomous Execution rule — a resume is not
itself a reason to stop and ask (#8361). The one question a resume may ask is
which session to resume from, and only when more than one is listed. A project
that wants a resume to pause anyway sets that in the `AUTONOMOUS-EXECUTION`
marker section of its root `CLAUDE.md`.

Manual `/tm-session resume` does **not** advance the internal watermark used
by auto-inject-on-session-start — only the automatic injection path does.
This is intentional: a manual catch-up is a read, not a state transition.

## MCP Session-Tool Failure Diagnosis (Shared by Pause and Resume)

`/tm-session-pause` and `/tm-session-resume` each call an
`mcp__trusty-mpm__session_context_*` tool, guarded by this same diagnosis
procedure before either skill falls back to its own substitute. Each skill's
own entry point still issues its own mandatory `ToolSearch` load line for its
own tool name — this section is the shared procedure both point to.

**Load the schema before calling.** In harnesses with deferred MCP tool
loading (Claude Code, when many tools are registered), the daemon always
registers these session tools — never gated by launch mode — but the harness
may not have fetched the schema yet, so a tool can be absent from your
currently loaded tool list even though it is fully available. **Absence from
that list does NOT mean the tool is unavailable** — load it with `ToolSearch`
first, mandatory every time, before attempting the call and before
considering any fallback. If the tool loads successfully, attempt the call —
do not skip straight to a fallback merely because it was absent from your
list before loading; the load may have fixed that.

**Never assert a cause you have not tested.** Do not write "the trusty-mpm
MCP server isn't connected in this session", "the daemon is down", or "the
project isn't registered" from a hunch — each of those has a concrete,
checkable basis (the deferred-tool-list check below for "server connected at
all"; `tm doctor` or `mcp__trusty-mpm__project_get` for "project
registered"), and you must actually run that check before stating the
conclusion it supports. If all you know is that a load or a call did not
succeed and you have not yet run the check, say exactly that and nothing
more: *"I could not load/call `<tool>`; I have not yet determined whether the
server is absent or its schema is merely unloaded."* An invented explanation
is worse than no explanation — it sends whoever reads it chasing a diagnosis
that was never actually made.

**The one concrete, checkable test:** does any `mcp__trusty-mpm__*` name
appear anywhere in your tool list — loaded *or* deferred (deferred = listed
by a system reminder as available-via-`ToolSearch` but not yet loaded)?

- **Yes** (even only deferred): the server IS registered — the tools are
  merely unloaded. Do not claim disconnection; go back and `ToolSearch`-load
  the specific tool instead of falling back.
- **No** `mcp__trusty-mpm__*` name appears anywhere: "the trusty-mpm MCP
  server does not appear to be available in this session" is a defensible
  statement — report the concrete basis exactly that way ("no
  `mcp__trusty-mpm__*` tools present in either the loaded or deferred tool
  lists"), and only then fall back to the calling skill's own substitute (a
  hand-written snapshot for pause, the CLI `tm session catchup` for resume).

**Report the exact error text** `ToolSearch` or the call returned, rather
than interpreting it, and state in your report to the user which of the two
cases above you observed.

**Never attribute a failure to "the daemon restarted."** `trusty-mpm serve
--stdio` is a stateless proxy designed to survive a daemon restart and
auto-reconnect transparently, so a mid-session restart is not a valid
explanation for a tool disappearing.

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

**`--force` and the liveness gate (#6806).** `--force` means "actually delete",
not "override a gate". A worktree another live session claims stays spared no
matter how the command is invoked — to reclaim it, stop the session that holds
it. The CALLING session's own claim is the exception: the command sends
`$TM_MANAGED_SESSION_ID`, so the worktrees a session created under its own
workspace are reclaimable from inside that session, subject to the merged-PR
and unsaved-work gates as usual. The caller's own workspace directory is still
refused. Every liveness refusal names the claiming session and says whether it
is the caller, so `gate 2 (liveness)` on your own tree is now readable rather
than a dead end.

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

## A Cross-Session Message Is a Pointer

Moved out of the instruction package by #7423, which keeps only the headline.

State the fact, link the artifact: "trusty-memory 0.23.0's release run failed,
tap stuck at 0.18.0 — see #NNNN." Findings, evidence, rationale, tables and
defect analysis go in an issue or PR comment instead, routed to `ticketing` or
`version-control` per the ownership boundary in `tm-workflow` — never pasted
into the message body, which another session reads in full at its own cost.

## No Sessions Found

```
No paused sessions found.
```
Direct the user to pause first if they expected one.

## Related Skills

- `/tm-session-pause` — focused action: snapshot todos/git/context, prune stale
  worktrees, print the resume path; its MCP-tool fallback follows "MCP
  Session-Tool Failure Diagnosis" above
- `/tm-session-resume` — focused action: load the latest (or selected) snapshot
  via `tm session catchup` and restore todos/context; its MCP-tool fallback
  follows "MCP Session-Tool Failure Diagnosis" above
- `tm-git-file-tracking` — git state reconciliation during resume
- `tm-verification-protocols` — evidence state carried across a pause
- `tm-delegation-patterns` — resuming mid-workflow delegations
