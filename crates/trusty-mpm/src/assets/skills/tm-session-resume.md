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
  session_id: <the current session id — the first thing resolved_snapshot is tried against>,
  tmux_window: <your own `tmux display-message -p '#{session_name}:#{window_index}:#{window_id}'`>,
  all_projects: false,   # true also scans machine-wide registered projects
  full: false,            # true ignores the watermark, returns full history
  sessions_offset: 0      # which page of the ordered session list to read — yours first, then newest
)
```

`project_dir` is **required** — the MCP transport forwards no cwd, so pass the
current project's absolute path explicitly. Pass `tmux_window` whenever `$TMUX`
is set; capture it in the same bash step you would use for realignment:

```bash
[ -n "$TMUX" ] && tmux display-message -p '#{session_name}:#{window_index}:#{window_id}'
```

The tool returns:

```json
{
  "sessions": [{ "format", "paused_at", "summary", "in_progress", "next_steps",
                 "git_context", "tmux_window", "source_file", "owned" }],
  "sessions_total": 31,
  "sessions_offset": 0,
  "sessions_next_offset": 6,
  "recent_commits": [{ "sha", "msg", "author", "ts" }],
  "recent_commits_total": 50,
  "recent_memory": [{ "title", "tags" }],
  "recent_memory_total": 0,
  "truncated": true,
  "over_budget": false,
  "page_bytes": 47812,
  "truncation_notice": "<what was withheld and how to get it, or null>",
  "resolved_snapshot": "<path or null>",
  "resolved_via": "session_id" | "tmux_window" | null,
  "undatable_sessions_dropped": 0,
  "watermark_advanced": false
}
```

Present the digest from these fields directly — summary, completed/in-progress
work, next steps, git context — confirm which session to resume from if more
than one is listed, restore the todo state from it, and confirm with the user
before continuing work. Cross-check `recent_commits` against your own
knowledge of the repo state if anything looks stale.

> **`sessions` is a page, and `truncated` says so.** The response is fitted to
> a size you can read in one tool result, so on a project with a long pause
> history `sessions` holds a page rather than all of them (#5557). When
> `truncated` is `true`, `truncation_notice` names what was withheld and the
> `sessions_offset` that retrieves it — re-call with that value to walk the
> rest. This does not weaken `full: true`: full history is paged, never
> dropped, and `sessions_next_offset` is `null` once you have all of it. Page 0
> is ordered with the sessions you own first, so it carries your own entry;
> that is why a resume normally needs only page 0.
>
> Two things the page does NOT promise. `over_budget: true` means nothing was
> withheld but one record is larger than a whole page — it ships intact, and
> `page_bytes` says how big the response got; no offset can shrink it. And the
> offset is positional into a list rebuilt from disk on each call, so if a
> session pauses while you are walking pages, a later page can repeat a record
> you already have — de-duplicate on `source_file` or `paused_at` if you are
> collecting them. Neither is a dropped record.

> **`sessions` and `resolved_snapshot` answer different questions** and
> legitimately disagree under a recent watermark: `sessions` is "what paused
> since your last catch-up", `resolved_snapshot` is "what should I resume
> from". Resume from `resolved_snapshot`; treat `sessions` as the digest.

> **You only see a session's detail if you own it.** Each `sessions[]` entry
> carries `owned`. It is `true` when your `session_id` paused that snapshot, or
> when you are sitting in the tmux window that did. For a session you do NOT
> own, the entry keeps `format`, `paused_at` and `summary` and nothing else —
> `source_file`, `tmux_window`, `in_progress`, `next_steps` and `git_context`
> come back null. That is the correct response, not missing data: those fields
> are what would let you load or restore another session's state, and handing
> them to any caller reconstructed by hand the cross-session resume #5272
> removed (#5386). Report an unowned session as "another session paused here"
> and move on. To read one on purpose, pass ITS `session_id` — the explicit
> opt-in — and it becomes owned for that call.

> **`resolved_snapshot` belongs to the `session_id` you passed, or to your
> tmux window — nothing else.** Several sessions share one
> `.trusty-mpm/sessions/` store, so there is still no "latest overall"
> fallback (#5272). The tool tries your `session_id` first; only when that
> owns nothing does it try `tmux_window`, matching the `@id` component against
> snapshots this project paused. Pass neither, or a `session_id` that never
> paused from a window that never paused, and you get `null` — that is the
> correct answer, not a failure; pick a snapshot out of `sessions[]` and
> resume from it deliberately. To read another session's state on purpose,
> pass that session's id.

> **Check `resolved_via` before you call it yours.** `"session_id"` means the
> id you passed owns that snapshot. `"tmux_window"` means it was paused from
> the window you are sitting in — which is why a relaunch (new harness session
> id, same window) still resolves — but the id differs, so say so when you
> report what you resumed from. Window ids are reused after a window is killed
> and recreated, so a `tmux_window` match is an ownership claim, not a
> guarantee.

> **Empty is not always empty.** An empty `sessions` array means "nothing
> paused since last catch-up" only when `undatable_sessions_dropped` is `0`.
> Non-zero means that many paused sessions exist but carried no derivable pause
> timestamp and were withheld — re-call with `full: true` to see them.

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

Resolution order for `resolved_snapshot`: the newest `pause` snapshot recorded
for the `session_id` you passed → the newest snapshot this project paused from
your `tmux_window`'s `@id` → null. Resume reads
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
