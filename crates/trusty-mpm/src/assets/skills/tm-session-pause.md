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
├── sessions-log.jsonl              # append-only per-session pause/resume log
├── <session-id>/
│   └── session-YYYYMMDD-HHMMSS.md  # human-readable snapshot
└── session-YYYYMMDD-HHMMSS.md      # pre-#5272 snapshot, still resolvable
```

`sessions-log.jsonl` holds one JSON object per line — one per pause/resume
event. `snapshot` is the path relative to `sessions/`:

```json
{"session_id":"<id>","event":"pause","snapshot":"<id>/session-20260715-101500.md","timestamp":"2026-07-15T10:15:00Z"}
```

Because it is **append-only**, two `tm` sessions pausing in the same project
each keep their own history, and the log is what says which snapshot is whose.
The legacy global `LATEST-SESSION.txt` pointer is **no longer written** — a
single overwritten pointer let concurrent sessions clobber each other's resume
target.

🔴 **Resume resolves only your own session's snapshots (#5272).** The
latest-overall, `LATEST-SESSION.txt`, and mtime fallbacks are gone: with the PM
on the project's main checkout, several sessions share this store, and each of
those steps turns "no snapshot for me" into "someone else's snapshot". A flat
pre-#5272 file at the store root still resolves through its log line; one with
no log line resolves for nobody.

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

Reconcile the todo list against `git status` first — mark blockers explicitly —
so the snapshot reflects the *current* state, not a stale one. Then capture
the tmux window (this ONE step stays PM-side bash — only the PM's own shell
has tmux client access; the MCP tool never touches tmux):

```bash
# Capture the current tmux window ONLY when inside tmux; omit the field otherwise.
if [ -n "$TMUX" ]; then
  tmux display-message -p '#{session_name}:#{window_index}:#{window_id}'
fi
```

Then **load the tool schema before calling it**. In harnesses with deferred
MCP tool loading (Claude Code, when many tools are registered), the daemon
always registers `session_context_pause` — it is never gated by launch mode —
but the harness may not have fetched its schema yet, so it can be missing
from your currently loaded tool list. **Absence from that list does NOT mean
the tool is unavailable** — it means the schema needs to be fetched first:

```
ToolSearch(query: "select:mcp__trusty-mpm__session_context_pause")
```

This is mandatory before attempting the call and before considering any
fallback (see "If the Tool Call Fails" below) — do not skip straight to a
hand-written snapshot just because the tool isn't in your loaded list yet.

Then call the MCP tool to write the snapshot, append the pause-log entry, and
prune worktrees, all in one step:

```
mcp__trusty-mpm__session_context_pause(
  project_dir: <absolute path to the current project root>,
  session_id: <a stable id for this session — $TM_SESSION_ID, or the tmux
               session:window, or any other stable value>,
  summary: <human-readable current-state prose>,
  completed: [<completed todos/tasks this session>, ...],   # optional
  in_progress: [<in-progress todos with detailed state>, ...],  # optional
  next_steps: [<pending todos and recommended next actions>, ...],  # optional
  tmux_window: <the string captured above, when inside tmux>,   # optional
  prune_worktrees: true   # default; set false to skip the prune step
)
```

The tool returns
`{ snapshot_path, timestamp, pruned_worktrees, skipped_dirty_worktrees }`. It writes
`.trusty-mpm/sessions/<session-id>/session-YYYYMMDD-HHMMSS.md` in the same section format
`/tm-session-resume` already parses (`## Summary` / `## Completed` /
`## In Progress` / `## Next Steps` / `## Git Context` / `## Tmux Window`,
each omitted when empty), appends the matching `pause` line to the
append-only `sessions-log.jsonl` (never overwrites — concurrent `tm` sessions
in the same project keep their own history), and computes the
`## Git Context` section (branch, last commit, uncommitted-changes summary)
itself — no separate `git status`/`git log` shell-out needed.

Report the returned `snapshot_path` to the user.

**If `skipped_dirty_worktrees` is non-empty, you MUST report it** — do not let
it disappear into the tool result. Each entry is
`{ path, reason, dirty_files, unpushed_commits }` and names a worktree the
prune refused to delete because it still holds uncommitted or unpushed work.
List them for the user, with the path and reason, and say plainly that the
work is still on disk and needs a human decision (commit it, push it, or
delete the directory deliberately). Never summarise them as "some worktrees
were skipped" — the whole point of the #4091 guard is that unsaved work stops
being invisible.

## If the Tool Call Fails — Diagnose Before Falling Back

Never report a bare "MCP pause tool unavailable", and — this is the stricter
rule — **never assert a cause you have not tested**. Do not write "the
trusty-mpm MCP server isn't connected in this session" or "the daemon is
down" or "the project isn't registered" from a hunch — each of those has a
concrete, checkable basis (see step 3's deferred-tool-list check below for
"server connected at all"; `tm doctor` or
`mcp__trusty-mpm__project_get` for "project registered"), and you must
actually run that check before stating the conclusion it supports. If all
you know is that a load or a call did not succeed and you have not yet run
the check, say exactly that and nothing more: *"I could not load/call
`session_context_pause`; I have not yet determined whether the server is
absent or its schema is merely unloaded."* An invented explanation is worse
than no explanation — it sends whoever reads it chasing a diagnosis that was
never actually made. A *tested* explanation (step 3) is exactly what should
be reported, in the same concrete terms as the test itself.

Follow this exact ordered procedure — it is not optional, and no step may be
skipped or reordered:

1. **Attempt the `ToolSearch` load** (see above). Mandatory, every time,
   regardless of what you expect the outcome to be.
2. **If the tool is now loaded, attempt the call.** Do not skip straight to a
   fallback because the tool was merely absent from your list before loading
   — step 1 may have fixed that.
3. **If step 1 or step 2 fails, report the actual observed error text**, not
   an interpretation of it. Quote what `ToolSearch` or the tool call actually
   returned. Do not paraphrase a raw error into a diagnosis you have not
   tested — instead run the one concrete, checkable test available to you:
   **does any `mcp__trusty-mpm__*` name appear anywhere in your tool
   list — loaded *or* deferred** (deferred tools are the ones a system
   reminder lists as available-via-`ToolSearch`-but-not-yet-loaded)?
     - **Yes, some `mcp__trusty-mpm__*` name is present** (even only in the
       deferred list): the server IS registered and the tools are merely
       unloaded. Do not claim disconnection — the correct action is to
       `ToolSearch`-load the specific tool (step 1), not to fall back.
     - **No `mcp__trusty-mpm__*` name appears anywhere**, loaded or deferred:
       "the trusty-mpm MCP server does not appear to be available in this
       session" is a defensible statement — state the concrete basis for it
       exactly that way ("no `mcp__trusty-mpm__*` tools are present in
       either the loaded or deferred tool lists"), not a bare assertion.
   Never attribute a failure to "the daemon restarted" — `trusty-mpm serve
   --stdio` is a stateless proxy designed to survive a daemon restart and
   auto-reconnect transparently, so a mid-session daemon restart is not a
   valid explanation for a tool disappearing.
4. **Only after step 3, hand-write the snapshot as a fallback** — and label it
   as one (see "Self-Identifying Fallback Snapshot" below). This is the only
   step at which writing a file by hand is permitted.

When you do fall back at step 4, match the tool's own format exactly, not
free prose — reuse the shape from "Snapshot Content" above verbatim: RFC3339
timestamps (`date -u +%Y-%m-%dT%H:%M:%S.%6N+00:00` or equivalent), the exact
literal section headers with nothing appended after them on the same line
(`## Summary`, `## Next Steps`, etc. — never `## Next Steps (some
annotation)`, which corrupts what `/tm-session-resume`'s parser extracts, see
`extract_section` hardening in `session_finder.rs`), and the three-line `##
Git Context` shape (`Branch: … / Last commit: … / Uncommitted changes: …`).

### Self-Identifying Fallback Snapshot

A hand-written snapshot must never be indistinguishable from a tool-written
one — the only way this project's own history could tell them apart was a
forensic timestamp tell (real tool output carries fractional seconds and a
`+00:00` offset; hand-written snapshots drifted to a bare `Z` suffix), which
is how six drifted files went unnoticed for three days. Instead, mark it
explicitly. Add a line immediately under the `# Session Pause - {timestamp}`
title, before `## Summary`:

```markdown
# Session Pause - {timestamp}

> FALLBACK SNAPSHOT — hand-written, not produced by
> `mcp__trusty-mpm__session_context_pause`. Observed failure: {the exact
> error text from step 3 above, or "no mcp__trusty-mpm__* tools present in
> either the loaded or deferred tool lists" if that is what was actually
> observed}.

## Summary
...
```

This is additive prose above the parsed sections — `/tm-session-resume`'s
`extract_section` reads sections by header, so the marker line does not
interfere with parsing, but a human (or a future PM) resuming from it
immediately sees it was not tool-generated and why. State in your report to
the user, in the same terms, which step you stopped at and what you actually
observed — never the bare "MCP pause tool unavailable."

## Worktree Pruning

Decommissioned managed sessions (`tm session new` / `mcp__trusty-mpm__session_new`)
can leave orphaned per-session git worktree directories behind. By default,
`session_context_pause` prunes them as part of the call above (`prune_worktrees:
true`, the default) — the same in-process engine `tm session prune-worktrees`
uses. Only directories with **no** corresponding active session are ever
touched; the tool returns the list of paths removed as `pruned_worktrees`.

**A worktree holding unsaved work is never removed** (#4091). Before deleting
anything, the prune checks each candidate for uncommitted or staged changes,
untracked (non-ignored) files, and commits that exist on no remote — and
refuses to touch it if any are present, or if the check itself cannot
complete. Those are returned as `skipped_dirty_worktrees` (see above) rather
than deleted. `/tm-session-pause` has **no** option that can override this;
discarding unsaved work requires the deliberate CLI opt-in below.

Pass `prune_worktrees: false` to skip this step (e.g. to preview first with
the CLI):

```bash
tm session prune-worktrees          # dry-run by default (preview only)
tm session prune-worktrees --force  # remove the orphaned dirs, sparing dirty ones
```

`--force` still refuses to delete a worktree with unsaved work; it lists them
on stderr instead. Only `tm session prune-worktrees --force --discard-dirty`
destroys that work, and it should be reached for only after reading that list.

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
