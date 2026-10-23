---
name: tm-session-resume
description: Resume from a paused PM session — scan project-local snapshots, validate the project matches, load the latest (or a selected) session, and restore todos and context
user-invocable: true
metadata:
  version: "2.0.0"
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
4. Reconciles against live git state — the tool returns the commits landed
   since the snapshot and a live `git status` — to compute what changed since
   pause.
5. Presents the snapshot — summary, in-progress items, next steps — restores
   the todo list, and continues under the Autonomous Execution rule (#8361),
   which is what decides whether anything is asked here.

The default answer is **lean** (#9883): the snapshot resolved for you plus live
git status, nothing else. The full digest — every paused session, recent
commits, memory titles — is one flag away (`full: true` / `--full`) when you
need it.

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
  tmux_session_created: <your own `tmux display-message -p '#{session_created}'`, an integer>,
  full: false,            # default: the lean answer; true returns the full digest
  all_projects: false,   # full digest only: also scan machine-wide registered projects
  sessions_offset: 0      # full digest only: which page of the session list to read
)
```

`project_dir` is **required** — the MCP transport forwards no cwd, so pass the
current project's absolute path explicitly. Pass `tmux_window` and
`tmux_session_created` whenever `$TMUX` is set; capture them with:

```bash
[ -n "$TMUX" ] && tmux display-message -p '#{session_name}:#{window_index}:#{window_id}'
[ -n "$TMUX" ] && tmux display-message -p '#{session_created}'
```

Every trusty-mpm tmux session is named `tm-<folder>`, so a later session
reuses the name. Without `tmux_session_created` the session-name route
resolves nothing (#8408).

<!-- rule:tm-session-resume/lean-default -->
By default (`full` omitted or `false`) the tool returns the lean answer:

```json
{
  "digest": "lean",
  "resolved_snapshot": "<path or null>",
  "resolved_via": "session_id" | "tmux_window" | "tmux_session" | null,
  "resolved_session": { "format", "paused_at", "summary", "in_progress",
                        "next_steps", "git_context", "tmux_window",
                        "source_file", "owned" } | null,
  "resolved_note": "<present only when the tmux-session route was skipped>",
  "resolved_snapshot_superseded": false,
  "commits_since_snapshot": [{ "sha", "msg", "author", "ts" }],
  "commits_since_snapshot_total": 0,
  "resume_verify": { "status", "checks" } | null,
  "git_status": {
    "available": true,
    "entries": [" M src/lib.rs", "?? notes.md"],
    "entries_total": 2,
    "truncated": false,
    "error": "<unavailable: why, or null>"
  },
  "watermark_advanced": false,
  "session_refs": { "hydrated", "refs_seen", "own_ref_found", "restored", "error" }
}
```

`resolved_session.git_context` is the tree at PAUSE time; `git_status` is the
tree NOW, read with a bounded `git status --porcelain` (at most 40 entries).
`available: false` means git failed or timed out — the `error` says which —
never a clean tree.

With `full: true` the tool returns the full digest instead — every paused
session, recent commits and recent memory over the whole history, with no
watermark — plus the same resolution fields (no `digest`, `resolved_session`
or `git_status`):

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
  "resolved_via": "session_id" | "tmux_window" | "tmux_session" | null,
  "resolved_note": "<present only when the tmux-session route was skipped>",
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

Present the snapshot from `resolved_session` directly — summary, in-progress
work, next steps, pause-time git context beside the live `git_status` — restore
the todo state from it, then continue under the Autonomous Execution rule: a
resume is not itself a reason to stop and ask (#8361). When `resolved_snapshot`
is `null`, re-call with `full: true` to list the candidates; ask exactly one
question, and only this one: when that `sessions[]` lists more than one
candidate, ask which to resume from. A project that wants a resume to pause
anyway sets that in the `AUTONOMOUS-EXECUTION` marker section of its root
`CLAUDE.md`. Read `commits_since_snapshot` before re-planning anything the
snapshot lists as next.

<!-- rule:tm-session-resume/tmux-session-route -->
> **Omit `session_id`; the tool resolves your own pause automatically** — it
> derives the id your pause was filed under, tries that first, then
> `tmux_window` only when it owns nothing (no "latest overall" fallback,
> #5272): its window id, then — because a relaunch recreates the window with a
> new id — its tmux session name (#8408). `resolved_via` names the match; a
> window or session match is a claim, not a guarantee — window ids and
> session names are reused after a kill/recreate. `null` means
> nothing matched; pick from `sessions[]` deliberately. A `resolved_note`
> means a route was skipped, not that nothing paused: pass
> `tmux_session_created` and call again. Never invent a
> `session_id`, or resumes report "no snapshot resolved" again (#6888).
> Ownership also gates `sessions[].owned`: an unowned entry keeps only
> `format`/`paused_at`/`summary` (#5272, #5386) — report it as "another
> session paused here", or pass its id to read it on purpose. The lean
> answer lists no other session at all.

> **In a `full: true` answer, `sessions` is a page, not the whole history
> (#5557).** `truncated: true` names what was withheld via
> `truncation_notice`/`sessions_offset`; the next page is one
> `sessions_offset` away, never dropped; `over_budget: true` means one
> oversized record shipped intact. The offset is positional into a list
> rebuilt from disk each call — a mid-walk pause can duplicate a record, so
> de-duplicate on `source_file`/`paused_at`. Page 0 lists your own sessions
> first, so a normal resume needs only it; `undatable_sessions_dropped`
> non-zero means that many lacked a timestamp and were withheld; a full
> answer applies no watermark, so it is 0 there.

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

<!-- rule:tm-session-resume/cli-fallback -->
For scripted / non-MCP callers the CLI still works, and the MCP tool is
additive rather than a replacement:

```bash
tm sessions catchup                  # lean: resolved snapshot + live git status (markdown)
tm sessions catchup --json           # the same lean answer as the MCP tool's JSON
tm sessions catchup --full           # the full digest as markdown, no watermark
tm sessions catchup --full --all-projects   # also scan machine-wide registered projects
tm sessions catchup --json --full    # the full digest's JSON, first page
tm sessions catchup --full --json --sessions-offset <n>   # the next page
```

<!-- rule:tm-session-resume/paged-full-digest -->
`--all-projects` and `--sessions-offset` select from the full digest, so they
need `--full`. **Prefer the MCP tool whenever it is reachable.** The
`--full` markdown has no paging, so a project with many paused sessions emits
one unbounded blob — 163 KB for 23 sessions in the run that filed #8017. When
the MCP tool is unreachable and you need the digest, use `--json --full`: it
prints the same paged payload `session_context_catchup` returns.

| Want | MCP tool | CLI |
|---|---|---|
| Lean resume (default) | `full` omitted | no flag; `--json` for JSON |
| Full digest, no watermark | `full: true` | `--full` |
| One page at a time | `full: true, sessions_offset: <n>`, then `sessions_next_offset` | `--full --json --sessions-offset <n>`, then `sessions_next_offset` |
| Bounded output | the lean default; `truncated` / `truncation_notice` in a full answer | `--json` (same fields) |
| Machine-wide scan | `full: true, all_projects: true` | `--full --all-projects` |

Never load the `--full` markdown digest whole into context; page the
`--json --full` form instead.

## No Tmux Window Re-alignment

Do not select, create or attach the snapshot's recorded tmux window (#8408).
A relaunch recreates the window, so its recorded `window_id` names a window
that no longer exists. When `resolved_via` is `tmux_window`, you are already
in that window. The recorded `tmux_window` is evidence of where the pause ran,
not a target.

## Session Store Location

```
<project-root>/.trusty-mpm/sessions/
├── sessions-log.jsonl          # append-only per-session pause/resume log
└── session-YYYYMMDD-HHMMSS.md  # human-readable snapshot (written by pause)
```

Resolution order for `resolved_snapshot`: the newest `pause` snapshot recorded
for the `session_id` you passed → the newest snapshot this project paused from
your `tmux_window`'s `@id` → the newest one paused from your tmux session name
(not a digits-only name) after `tmux_session_created` → null. Resume reads
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

## Update the Resume Slot (#9142)

Once resumed, rewrite `ws:<session>/resume` with `tm memory remember
--fact-key`, and each changed PR's `pr:<n>/state`, so recall returns the
current state. Slot rules: `/tm-session-management`.

## Token Budget

The lean default costs one snapshot plus a short status — a few thousand
tokens. A `full: true` page costs ~20–40k tokens (10–20% of context budget);
read it only when the snapshot did not resolve or you need the history.

## Related

- `/tm-session-pause` — pause the current session and write the snapshot
- `/tm-session-management` — policy, thresholds, and the full format reference
