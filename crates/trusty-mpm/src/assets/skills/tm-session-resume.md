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
   steps — restores the todo list, and continues under the Autonomous Execution
   rule (#8361), which is what decides whether anything is asked here.

## Usage

```
/tm-session-resume            # resume the latest session in this project
/tm-session-resume --select 2 # resume the 2nd-most-recent session (1-based)
```

With no argument and more than one paused session present, list them
newest-first (session id, time elapsed, topic) and ask which to resume — that
ambiguity is an observable condition, not a comfort check.

## Implementation: `mcp__trusty-mpm__session_context_catchup`

**Load the tool schema before calling it** — see `/tm-session-management`
("MCP Session-Tool Failure Diagnosis") for why absence from your loaded tool
list does not mean the tool is unavailable. Load it first:

```
ToolSearch(query: "select:mcp__trusty-mpm__session_context_catchup")
```

This is mandatory before attempting the call below — do not conclude the
tool is unavailable, and do not hand-parse `.trusty-mpm/sessions/*.md`
yourself as a substitute, just because it's missing from the loaded list.

If the load or the call still fails, follow the shared diagnosis procedure in
`/tm-session-management` ("MCP Session-Tool Failure Diagnosis"). Only once
that procedure has established that no `mcp__trusty-mpm__*` tool is
available, fall back to the CLI `tm sessions catchup` instead of hand-parsing
snapshot files yourself, since it implements the same merge/validation logic.
Spell it **plural** — `tm session` is a deprecated alias that prints
`warning: 'session' is deprecated; use 'sessions'` on every invocation (#2116).

Resume calls the MCP tool rather than shelling out to `git log`/`git status`
and hand-parsing snapshot files, so the merge/validation logic stays in one
place and returns typed JSON instead of scraped text:

```
mcp__trusty-mpm__session_context_catchup(
  project_dir: <absolute path to the current project root>,
  # session_id: OMIT IT — the tool derives the same id your pause was filed
  #             under (#6888). Pass one only to read a specific KNOWN session.
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
  "watermark_advanced": false,
  "session_refs": {
    "hydrated": true,
    "refs_seen": 0,
    "own_ref_found": false,
    "restored": 0,
    "error": "<why the cache was not refreshed, or null>"
  }
}
```

Present the digest from these fields directly — summary, completed/in-progress
work, next steps, repo context — restore the todo state from it, then continue
under the Autonomous Execution rule: a resume is not itself a reason to stop and
ask (#8361). Ask exactly one question, and only this one: when `sessions[]`
lists more than one candidate and `resolved_snapshot` is `null`, ask which to
resume from. A project that wants a resume to pause anyway sets that in the
`AUTONOMOUS-EXECUTION` marker section of its root `CLAUDE.md`. Cross-check
`recent_commits` against your own knowledge of the repo state if anything looks
stale.

> **Omit `session_id`; the tool resolves your own pause automatically** — it
> derives the id your pause was filed under, tries that first, then
> `tmux_window` only when it owns nothing (no "latest overall" fallback,
> #5272). `resolved_via` names the match; a window match is a claim, not a
> guarantee — window ids are reused after a kill/recreate. `null` means
> neither matched; pick from `sessions[]` deliberately. Never invent a
> `session_id`, or resumes report "no snapshot resolved" again (#6888).
> Ownership also gates `sessions[].owned`: an unowned entry keeps only
> `format`/`paused_at`/`summary` (#5272, #5386) — report it as "another
> session paused here", or pass its id to read it on purpose.

> **`sessions` and `resolved_snapshot` answer different questions, and can
> disagree under a recent watermark:** `sessions` is what paused since your
> last catch-up; `resolved_snapshot` is what to resume from. Resume from
> `resolved_snapshot`; treat `sessions` as the digest.

> **`sessions` is a page, not the full history (#5557).** `truncated: true`
> names what was withheld via `truncation_notice`/`sessions_offset`;
> `full: true` pages instead of dropping it; `over_budget: true` means one
> oversized record shipped intact. The offset is positional into a list
> rebuilt from disk each call — a mid-walk pause can duplicate a record, so
> de-duplicate on `source_file`/`paused_at`. Page 0 lists your own sessions
> first, so a normal resume needs only it; `undatable_sessions_dropped`
> non-zero means that many lacked a timestamp and were withheld — re-call
> with `full: true`.

> **`session_refs` says whether the cache was refreshed (ADR-0062, #7830)**
> from your own git ref `refs/tm/sessions/<user-id>/<session-key>` —
> `refs_seen`/`own_ref_found`/`restored` count remote refs, whether yours
> matched, and snapshots restored. `refs_seen > 0` with `own_ref_found:
> false` is permanent (a hostname or `gh` account change loses old refs for
> good); a non-null `error` means the cache was not refreshed though the
> catch-up still succeeded — report both rather than reading an empty
> digest as "nothing paused".

> **`watermark_advanced` is always `false` for a manual resume** — this is a
> read, not a state transition, so calling the tool repeatedly is always
> safe; only the automatic injection path advances the watermark.

## CLI Fallback: `tm sessions catchup`

For scripted / non-MCP callers the CLI still works, and the MCP tool is
additive rather than a replacement. Its full surface is two flags:

```bash
tm sessions catchup                  # this project, watermarked digest
tm sessions catchup --full           # ignore the watermark, full history
tm sessions catchup --all-projects   # also scan machine-wide registered projects
```

**Prefer the MCP tool whenever it is reachable.** The CLI prints rendered
markdown to stdout and has **no JSON mode and no paging**, so a project with
many paused sessions emits one unbounded blob — 163 KB for 23 sessions in the
run that filed #8017. The paged, machine-readable equivalents live only on
`session_context_catchup`:

| Want | MCP tool | CLI |
|---|---|---|
| Typed JSON | the tool's return value (schema above) | not available — rendered markdown only |
| One page at a time | `sessions_offset: <n>`, then `sessions_next_offset` | not available |
| Bounded output | default watermark + `truncated` / `truncation_notice` | not available |
| Full history | `full: true` | `--full` |
| Machine-wide scan | `all_projects: true` | `--all-projects` |

So when the MCP tool is unreachable and the CLI digest is too large to read,
narrow it at the source — resume from the newest snapshot in
`.trusty-mpm/sessions/` named by the digest rather than re-running the CLI —
and report that the paged read was unavailable, instead of loading the whole
blob into context.

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
