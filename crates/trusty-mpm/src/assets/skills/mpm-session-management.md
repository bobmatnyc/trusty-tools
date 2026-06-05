---
name: mpm-session-management
version: "1.0.0"
description: Session pause/resume capabilities for PM context limit management
when_to_use: context limit approaching, session resume, token limits, session continuity
category: pm-framework
tags: [session, context, pause, resume, pm-required]
effort: medium
---

# PM Session Management

**Applies To**: Project Manager Agent
**Load Priority**: On-demand (context limit or session resume)

## Purpose

This skill provides session pause/resume capabilities for the PM when context limits are approaching or when resuming from a previous session. These protocols are only needed when hitting token limits or starting a session with existing pause state.

## When This Skill Is Loaded

- Context usage reaches 70%+ thresholds
- Session starts with `.trusty-mpm/sessions/ACTIVE-PAUSE.jsonl`
- Session starts with `.trusty-mpm/sessions/LATEST-SESSION.txt`
- User runs `/mpm-session-resume`

## Auto-Pause System

trusty-mpm automatically tracks context usage and pauses sessions when approaching limits:

### Threshold Levels

| Level | Usage | Behavior |
|-------|-------|----------|
| Caution | 70% | Warning displayed |
| Warning | 85% | Stronger warning |
| **Auto-Pause** | **90%** | **Session pause activated, actions recorded** |
| Critical | 95% | Session nearly exhausted |

### Auto-Pause Behavior (at 90%)

When context usage reaches 90%:

1. Creates `.trusty-mpm/sessions/ACTIVE-PAUSE.jsonl`
2. Records all subsequent actions (tool calls, responses) incrementally
3. Displays warning to user about context limits
4. On session end, finalizes to full session snapshot

The incremental recording ensures all work is captured even if the session hits hard limits.

## Session Resume Protocol

### At Session Start, PM Checks For

**1. Active Incremental Pause**: `.trusty-mpm/sessions/ACTIVE-PAUSE.jsonl`

If found:
- Display warning with action count and context percentage
- Options:
  - **Continue**: Resume work from pause state
  - **Finalize**: Run `tm session --finalize` to create snapshot
  - **Discard**: Start fresh (previous work still in git)

**Example Response**:
```
Active session pause detected

Actions recorded: 47
Context usage: ~92%

Options:
1. Continue working (actions will be recorded)
2. Finalize pause: tm session --finalize
3. Discard pause: Delete .trusty-mpm/sessions/ACTIVE-PAUSE.jsonl

Would you like to continue from the paused session?
```

**2. Finalized Pause**: `.trusty-mpm/sessions/LATEST-SESSION.txt`

If found:
- Display resume context with accomplishments and next steps
- Load context from the session snapshot
- Continue where previous session left off

**Example Response**:
```
Resuming from previous session

Last session accomplished:
- Implemented OAuth2 authentication (Engineer)
- Deployed to staging (vercel-ops)
- QA verification in progress

Next steps:
- Complete QA verification of auth flow
- Update documentation
- Deploy to production

Continue with QA verification?
```

## PM Response to Context Warnings

When PM sees context warnings (70%+), follow this protocol:

### Immediate Actions

1. **Wrap up current work phase**
   - Complete the current delegation cycle
   - Don't start new major tasks

2. **Document all in-progress tasks**
   - Ensure all todos are updated with current status
   - Mark BLOCKED todos with specific blockers
   - Add context to in_progress todos

3. **Delegate remaining work with clear handoff**
   - Provide detailed context to agents
   - Include acceptance criteria
   - Reference relevant files and commits

4. **Create summary**
   - What was completed
   - What remains to be done
   - Any blockers or important context

### Example Wrap-Up Sequence

```
Context at 85% - wrapping up current phase

Completed:
- OAuth2 implementation (commit abc123)
- Staging deployment verified

In Progress:
- QA verification (api-qa testing login flow)

Remaining:
- Documentation update (auth flow docs)
- Production deployment

Creating session snapshot for clean resume...
```

## Git-Based Session Continuity

Git history provides additional session context that complements session snapshots:

### Useful Git Commands

```bash
# Recent commits (what was delivered)
git log --oneline -10

# Uncommitted changes (work in progress)
git status

# Recent work (last 24 hours)
git log --since="24 hours ago" --pretty=format:"%h %s"

# Files changed recently
git log --name-status --since="24 hours ago"
```

### Integration with Session Resume

When resuming a session, PM should:

1. Load session snapshot (if available)
2. Check git log for additional context
3. Verify git status for uncommitted work
4. Reconcile session state with git state

## Session Files Structure

```
.trusty-mpm/sessions/
├── ACTIVE-PAUSE.jsonl      # Incremental actions during auto-pause
├── LATEST-SESSION.txt      # Pointer to most recent finalized session
├── session-*.json          # Machine-readable session snapshots
└── session-*.md            # Human-readable markdown
```

## Best Practices

### When Context Limits Approach

1. **Don't panic**: Auto-pause system will capture your work
2. **Finish current phase**: Complete the delegation in progress
3. **Update todos**: Ensure all todos reflect current state
4. **Create handoff context**: Next PM session needs to understand state

### When Resuming Sessions

1. **Review session snapshot**: Understand what was accomplished
2. **Check git history**: Verify actual state matches snapshot
3. **Validate uncommitted work**: Any WIP that wasn't tracked?
4. **Continue from clear state**: Don't duplicate completed work

### Avoiding Session Bloat

- Keep delegations focused and atomic
- Don't load unnecessary context (use skills on-demand)
- Complete and close todos regularly
- Commit work incrementally (easier to resume)

## Trigger Keywords

- "context", "pause", "resume", "session"
- "token", "limit", "usage"
- "continue", "previous session"
- Auto-loaded at 70%+ context usage
- Auto-loaded when session files exist

## Related Skills

- `mpm-git-file-tracking` — File tracking during pause/resume
- `mpm-verification-protocols` — Verification state during pause
- `mpm-delegation-patterns` — Resuming delegations mid-workflow
- `mpm-session-pause` — Detailed pause protocol
- `mpm-session-resume` — Detailed resume protocol
