---
name: tm-session-resume
description: Resume from a paused PM session — scan project-local snapshots, validate the project matches, load the latest (or a selected) session, and restore todos and context
user-invocable: true
version: "1.0.0"
category: pm-workflow
tags: [session, resume, context, pm-recommended]
effort: medium
---

# /tm-session-resume — Load & Restore

Focused entry point for loading context from a session paused with
`/tm-session-pause`. This is the *action*; the policy and full format reference
live in `/tm-session-management`.

## What This Does

When invoked, this skill:
1. Scans the project-local session store at `.trusty-mpm/sessions/` for paused
   snapshots.
2. Loads the most recent session — for the current session id when known, via
   the append-only `sessions-log.jsonl`; otherwise the latest overall (last
   `pause` line), with `LATEST-SESSION.txt` / mtime as back-compat fallbacks —
   **or** a specific one chosen with `--select`.
3. **Validates** that the session belongs to the *current* project — snapshots
   whose recorded project path does not match the working directory are skipped,
   so you never accidentally resume another checkout's state.
4. Reconciles against live git state (`git log --oneline -5`, `git status`) to
   compute what changed since pause.
5. Presents the digest — summary, completed work, in-progress items, next
   steps — restores the todo list, and confirms before continuing.

## Usage

```
/tm-session-resume            # resume the latest session in this project
/tm-session-resume --select 2 # resume the 2nd-most-recent session (1-based)
```

With no argument and more than one paused session present, list them
newest-first (session id, time elapsed, topic) and confirm which to resume.

## Implementation: `mcp__trusty-mpm__session_context_catchup`

**Load the tool schema before calling it.** In harnesses with deferred MCP
tool loading (Claude Code, when many tools are registered), the daemon
always registers `session_context_catchup` — it is never gated by launch
mode — but the harness may not have fetched its schema yet, so it can be
absent from your currently loaded tool list even though it is fully
available. **Absence from that list does NOT mean the tool is unavailable.**
Load it first:

```
ToolSearch(query: "select:mcp__trusty-mpm__session_context_catchup")
```

This is mandatory before attempting the call below — do not conclude the
tool is unavailable, and do not hand-parse `.trusty-mpm/sessions/*.md`
yourself as a substitute, just because it's missing from the loaded list.

If the load or the call still fails, **never assert a cause you have not
tested.** Run the one concrete, checkable test available: does any
`mcp__trusty-mpm__*` name appear anywhere in your tool list, loaded *or*
deferred (deferred = listed by a system reminder as available-via-`ToolSearch`
but not yet loaded)?

- **Yes** (even only deferred): the server IS registered — the tools are
  merely unloaded. Do not claim disconnection; go back and `ToolSearch`-load
  the specific tool instead.
- **No** `mcp__trusty-mpm__*` name appears anywhere: "the trusty-mpm MCP
  server does not appear to be available in this session" is a defensible
  statement — report the concrete basis exactly that way ("no
  `mcp__trusty-mpm__*` tools present in either the loaded or deferred tool
  lists"), and only then fall back to the CLI `tm session catchup` instead of
  hand-parsing snapshot files yourself, since it implements the same
  merge/validation logic.

Report the exact error text `ToolSearch` or the call returned rather than
interpreting it, and state in your report to the user which of the two cases
above you observed. Never attribute a failure to "the daemon restarted" —
`trusty-mpm serve --stdio` is a stateless proxy designed to survive a daemon
restart and auto-reconnect transparently, so a mid-session restart is not a
valid explanation for a tool disappearing.

Resume calls the MCP tool rather than shelling out to `git log`/`git status`
and hand-parsing snapshot files, so the merge/validation logic stays in one
place and returns typed JSON instead of scraped text:

```
mcp__trusty-mpm__session_context_catchup(
  project_dir: <absolute path to the current project root>,
  session_id: <the current session id, when known — narrows resolved_snapshot>,
  all_projects: false,   # true also scans machine-wide registered projects
  full: false             # true ignores the watermark, returns full history
)
```

`project_dir` is **required** — the MCP transport forwards no cwd, so pass the
current project's absolute path explicitly. The tool returns:

```json
{
  "sessions": [{ "format", "paused_at", "summary", "in_progress", "next_steps",
                 "git_context", "tmux_window", "source_file" }],
  "recent_commits": [{ "sha", "msg", "author", "ts" }],
  "recent_memory": [{ "title", "tags" }],
  "resolved_snapshot": "<path or null>",
  "watermark_advanced": false
}
```

Present the digest from these fields directly — summary, completed/in-progress
work, next steps, git context — confirm which session to resume from if more
than one is listed, restore the todo state from it, and confirm with the user
before continuing work. Cross-check `recent_commits` against your own
knowledge of the repo state if anything looks stale.

> **Watermark note:** a manual `/tm-session-resume` is a *read*, not a state
> transition — `watermark_advanced` in the tool's response is always `false`.
> It does **not** advance the internal watermark used by
> auto-inject-on-session-start. Only the automatic injection path does. This is
> intentional — calling the tool repeatedly is always safe.

The CLI `tm session catchup` command still works unchanged for scripted /
non-MCP callers — the tool is additive, not a replacement.

## Re-aligning the Tmux Window

If the resumed session's `tmux_window` field is non-null (recorded at pause
time as `session_name:window_index:window_id`, e.g. `main:2:@7`), realign to
the originating window so resumed work lands where it left off. This is a PM
bash step — only the PM's own shell has tmux client access, the MCP tool
never touches tmux. Parse the string on `:` and use only the `session_name`
and `window_index`:

```bash
# tmux_window field value: main:2:@7
if [ -n "$TMUX" ]; then
  # inside tmux → select the recorded window (idempotent no-op if already there)
  tmux select-window -t 'main:2'   # <session_name>:<window_index> from the field
else
  # not inside tmux → just report it; do not attempt to attach
  echo "Recorded tmux window: main:2:@7 (start tmux to re-align)"
fi
```

This step is a **no-op** when `tmux_window` is `null` (older snapshots or
sessions paused outside tmux). `tmux select-window` is safe and idempotent —
if you are already on that window it does nothing. Never force-create windows or
attach sessions here; only align within the current tmux client.

## Session Store Location

```
<project-root>/.trusty-mpm/sessions/
├── sessions-log.jsonl          # append-only per-session pause/resume log
└── session-YYYYMMDD-HHMMSS.md  # human-readable snapshot (written by pause)
```

Resolution order for "latest": the newest `pause` snapshot for the current
session id in `sessions-log.jsonl` → the last `pause` line overall → the legacy
`LATEST-SESSION.txt` pointer → an mtime scan of `session-*.md`. Resume reads
existing snapshots only — it never creates snapshot files. It MAY append a
`resume` line to `sessions-log.jsonl` for audit, but snapshots are kept after
resume so you can resume more than once.

## No Sessions Found

```
No paused sessions found.
```

Direct the user to `/tm-session-pause` first if they expected one.

## Differentiating from Native Claude Code Resume

This is distinct from Claude Code's native `claude --resume`/`--continue` and
checkpoint-rewind (Esc-Esc / `/rewind`):

- **Native resume** replays the full raw conversation transcript and continues
  the exact same thread in the same working directory.
- **`/tm-session-pause` + `/tm-session-resume`** capture a condensed textual
  summary (git state, todos, accomplishments) into `.trusty-mpm/sessions/*.md`
  and load it into a **fresh** conversation — useful for long-form work spanning
  many separate conversations, or a clean context window carrying just the
  essential summary.

## Token Budget

~20–40k tokens (10–20% of context budget) to load the summary, next steps, git
history, and pending todos so the PM can continue without rediscovering state.

## Related

- `/tm-session-pause` — pause the current session and write the snapshot
- `/tm-session-management` — policy, thresholds, and the full format reference
