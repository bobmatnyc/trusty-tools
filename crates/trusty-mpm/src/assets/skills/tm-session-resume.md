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
2. Loads the most recent session (by the `LATEST-SESSION.txt` pointer / file
   modification time) **or** a specific one chosen with `--select`.
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

## Implementation: `tm session catchup`

Resume delegates to the real CLI command rather than hand-parsing snapshot
files, so the merge/validation logic stays in one place:

```bash
tm session catchup                  # current project only
tm session catchup --all-projects   # also scan machine-wide registered projects
```

`catchup` renders a unified, newest-first work-context digest for the current
project. After running it: reconcile against `git log --oneline -5` /
`git status`, present the digest, confirm which session to resume from if more
than one is listed, restore the todo state, and confirm with the user before
continuing work.

> **Watermark note:** a manual `/tm-session-resume` is a *read*, not a state
> transition — it does **not** advance the internal watermark used by
> auto-inject-on-session-start. Only the automatic injection path does. This is
> intentional.

## Session Store Location

```
<project-root>/.trusty-mpm/sessions/
├── LATEST-SESSION.txt          # pointer to the most recent session
└── session-YYYYMMDD-HHMMSS.md  # human-readable snapshot (written by pause)
```

Resume reads existing snapshots only — it never creates files, and snapshots are
kept after resume so you can resume more than once.

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
