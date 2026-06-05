---
name: mpm-session-resume
description: Load context from paused session
user-invocable: true
version: "1.0.0"
category: mpm-command
tags: [mpm-command, session, pm-recommended]
effort: medium
---

# /mpm-session-resume

Load and display context from a paused session to restore full work context.

## What This Does

When invoked, this skill:
1. Scans the project-local session store at `.trusty-mpm/sessions/` for paused sessions
2. Loads the most recent session (by file modification time) or a specific session
3. Validates that the session belongs to the current project
4. Calculates time elapsed since pause and git changes since pause
5. Displays a formatted resume prompt with summary, accomplishments, and next steps
6. Loads the session data so the PM can continue work with full context

## Usage

```
/mpm-session-resume                    # resume most recent session
/mpm-session-resume <session-id>       # resume by specific session ID
```

## PM Instructions for Resuming a Session

When invoked, the PM MUST:

1. **Check for session files:**
   ```bash
   ls .trusty-mpm/sessions/ 2>/dev/null || echo "No sessions found"
   cat .trusty-mpm/sessions/LATEST-SESSION.txt 2>/dev/null
   ```

2. **Load the session file** (most recent or specified):
   ```bash
   cat .trusty-mpm/sessions/session-{YYYYMMDD-HHMMSS}.md
   ```

3. **Check current git state** to reconcile with session state:
   ```bash
   git log --oneline -5
   git status
   ```

4. **Display resume context** to the user:
   ```
   Resuming from previous session

   Paused: {time elapsed} ago
   Branch: {git branch}

   Last session accomplished:
   - {completed item 1}
   - {completed item 2}

   In progress at pause:
   - {in-progress item 1}

   Next steps:
   - {next step 1}
   - {next step 2}

   Git changes since pause:
   - {commits added since pause}

   Continue from: {recommended next action}
   ```

5. **Restore todo state** from the session snapshot

6. **Confirm with user** before proceeding with work

## Session Storage Location

**Session location:** project-local `.trusty-mpm/sessions/`

```
<project-root>/.trusty-mpm/sessions/
├── LATEST-SESSION.txt                  # Pointer to most recent session
└── session-YYYYMMDD-HHMMSS.md          # Human-readable session state
```

Resume reads the `.md` file. Sessions are project-scoped — you will never
accidentally load a session from a different project.

## What Gets Loaded

**From the paused session:**
- Session ID and pause timestamp
- Git branch, recent commits, and file status at pause
- Summary, accomplishments, and next steps
- Task state (pending/in-progress tasks at pause time)
- Context message (if provided at pause)

**Calculated at resume time:**
- Human-readable time elapsed
- Git commits added since pause
- File changes since pause

## No Sessions Found

If no sessions exist:
```
No paused sessions found in .trusty-mpm/sessions/

To create a paused session, use: /mpm-session-pause
```

## Notes

- Sessions are read-only at resume time — the file is not deleted after loading.
- Auto-pause at 90% context creates sessions automatically; this skill reads them.
- Multiple sessions are listed most-recent-first; the latest is loaded by default.
- Session files are project-scoped — never loads sessions from a different project directory.

## Related Commands

- `/mpm-session-pause` — Pause current session and save state
- See `mpm-session-management` skill for full context management guide
