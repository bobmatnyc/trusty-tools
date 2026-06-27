---
name: mpm-session-resume
description: Load context from paused session
user-invocable: true
version: "1.1.0"
category: mpm-command
tags: [mpm-command, session, pm-recommended]
effort: medium
---

# /mpm-session-resume

Load and display context from a paused session to restore full work context.
Handles both the native trusty-mpm format and the legacy claude-mpm format
(DOC-28 cutover bridge, #1762).

## What This Does

When invoked, this skill:
1. Delegates cross-format session discovery to `tm sessions catchup`
2. Renders a unified catch-up digest (newest-first) covering both
   `.trusty-mpm/sessions/` (native) and `.claude-mpm/sessions/` (legacy)
3. Optionally scans machine-wide projects via `--all-projects`
4. Displays a formatted resume prompt so the PM can continue with full context

## Usage

```
/mpm-session-resume                    # catch-up from current project
/mpm-session-resume --all-projects    # also scan machine-wide claude-mpm projects
```

## PM Instructions for Resuming a Session

When invoked, the PM MUST:

1. **Run the catch-up command** to get the unified session digest:
   ```bash
   tm sessions catchup
   # or, to include all machine-wide projects:
   tm sessions catchup --all-projects
   ```

2. **Check current git state** to reconcile with session state:
   ```bash
   git log --oneline -5
   git status
   ```

3. **Present the catch-up output** to the user and confirm which session to
   resume from (when multiple sessions are listed).

4. **Restore todo state** from the session snapshot.

5. **Confirm with user** before proceeding with work.

## Session Storage Locations

**trusty-mpm native format (current):**
```
<project-root>/.trusty-mpm/sessions/
├── LATEST-SESSION.txt          # Pointer to most recent session
└── session-YYYYMMDD-HHMMSS.md  # Human-readable session state
```

**claude-mpm legacy format (cutover bridge):**
```
<project-root>/.claude-mpm/sessions/
├── LATEST-SESSION.txt          # Pointer to most recent session
└── session-YYYYMMDD-HHMMSS.json  # JSON digest
```

Sessions from both locations are merged and sorted newest-first by the
`tm sessions catchup` command.

## What Gets Loaded

**Common across both formats:**
- Pause timestamp
- Git context (branch, recent commits, file status)
- Summary / resume instructions
- Task state (pending/in-progress tasks at pause time)
- Important reminders and open questions

**NOT loaded (even if present):**
- Raw `conversation` history (too large; the digest fields are sufficient)

## No Sessions Found

If no sessions exist:
```
No paused sessions found.
```

To create a paused session, use: `/mpm-session-pause`

## Notes

- Sessions are read-only at resume time.
- Auto-pause at 90% context creates sessions automatically; this skill reads them.
- Multiple sessions are listed most-recent-first; the latest is the default.
- Session files are project-scoped by default; `--all-projects` expands the scan.

## Related Commands

- `/mpm-session-pause` — Pause current session and save state
- `tm sessions catchup --all-projects` — Scan all machine-wide projects
- See `mpm-session-management` skill for full context management guide
