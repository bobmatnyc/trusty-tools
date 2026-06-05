---
name: mpm-session-pause
description: Pause session and save current work state for later resume
user-invocable: true
version: "1.0.0"
category: mpm-command
tags: [mpm-command, session, pm-recommended]
effort: medium
---

# /mpm-session-pause

Pause the current session and save all work state for later resume.

## What This Does

When invoked, this skill:
1. Captures current work state (todos, git status, context summary)
2. Creates session file at `.trusty-mpm/sessions/session-{timestamp}.md` (project-local)
3. Updates `.trusty-mpm/sessions/LATEST-SESSION.txt` pointer
4. Shows user the session file path for later resume

## Usage

```
/mpm-session-pause [optional message describing current work]
```

**Examples:**
```
/mpm-session-pause
/mpm-session-pause Working on authentication refactor, about to test login flow
/mpm-session-pause Need to context switch to urgent bug fix
```

## PM Instructions for Pausing a Session

When invoked, the PM MUST:

1. **Capture current state:**
   ```bash
   git status
   git log --oneline -10
   ```

2. **Create the sessions directory:**
   ```bash
   mkdir -p .trusty-mpm/sessions
   ```

3. **Write session file** to `.trusty-mpm/sessions/session-{YYYYMMDD-HHMMSS}.md`:

   ```markdown
   # Session Pause - {timestamp}

   ## Summary
   {user-provided message or auto-generated from current todos}

   ## Completed
   {list of completed todos/tasks from current session}

   ## In Progress
   {list of in-progress todos with detailed state}

   ## Next Steps
   {list of pending todos and recommended next actions}

   ## Git Context
   Branch: {current branch}
   Last commit: {last commit hash and message}
   Uncommitted changes: {git status summary}

   ## Context
   {any additional context useful for resume}
   ```

4. **Update the pointer file:**
   ```
   .trusty-mpm/sessions/LATEST-SESSION.txt → session-{timestamp}.md
   ```

5. **Report to user:**
   ```
   Session paused successfully!

   Session file: .trusty-mpm/sessions/session-{timestamp}.md

   Quick resume: /mpm-session-resume
   ```

## What Gets Saved

**Session State:**
- Session ID and timestamp
- Current git branch, recent commits, and file status
- Primary task and current phase
- Context message (if provided)
- Current todo/task state (pending/in-progress/completed)

**Resume Instructions:**
- Quick-start summary
- Files to review
- Next recommended actions

## Session File Location

All session files are stored in the **project-local** directory:
```
<project-root>/.trusty-mpm/sessions/
├── LATEST-SESSION.txt          # Pointer to most recent session
└── session-YYYYMMDD-HHMMSS.md
```

Add `.trusty-mpm/sessions/` to `.gitignore` — session state is machine-specific.

## Use Cases

**Context switching:**
```
/mpm-session-pause Switching to urgent production bug
```

**End of work session:**
```
/mpm-session-pause Completed API refactor, ready for testing tomorrow
```

**When approaching context limit:**
```
/mpm-session-pause Hit context limit, starting fresh session
```

## Related Commands

- `/mpm-session-resume` — Resume from most recent paused session
