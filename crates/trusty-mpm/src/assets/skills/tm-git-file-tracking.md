---
name: tm-git-file-tracking
description: Protocol for tracking files immediately after agent creation, before marking work complete
user-invocable: false
version: "1.0.0"
category: pm-workflow
tags: [git, file-tracking, workflow, pm-required]
effort: medium
---

# Git File Tracking Protocol

**Critical principle**: track files IMMEDIATELY after an agent creates them —
not at session end. This is CB#4 in `tm-circuit-breaker`; the PM cannot mark a
todo complete until tracking is verified.

## File Tracking Decision Flow

```
Agent completes work and returns to PM
    |
Did the agent create/modify files?  -- NO --> mark todo complete, continue
    | YES
MANDATORY FILE TRACKING (BLOCKING)
    |
1. `git status`        -- see new/modified files
2. Check decision matrix (deliverable vs. temp/ignored)
3. `git add <files>`    -- stage deliverables only
4. `git commit -m "..."` -- with proper context
5. `git status`         -- verify clean tree
    |
ONLY NOW: mark the todo complete
```

## Decision Matrix: What to Track

| File type | Track? | Reason |
|---|---|---|
| New/modified source files (`.rs`, `.ts`, `.py`, ...) | YES | production code must be versioned |
| Config files (`.toml`, `.json`, `.yaml`) | YES | configuration changes must be tracked |
| New tests (`_test.rs`, `*.test.ts`, `test_*.py`) | YES | tests are critical artifacts |
| Docs under `docs/` | YES | documentation is a deliverable |
| New `.md` at repo root | NO (usually) | only core docs (README, CHANGELOG) belong at root |
| Scripts under `scripts/` | YES | automation must be versioned |
| `target/`, `dist/`, `build/`, `node_modules/` | NO | generated or dependency, not source |
| `.gitignore`d files, `/tmp/` output | NO | intentionally excluded |
| `.claude/worktrees/*` scratch state | NO | disposable per the worktree-discipline convention |

## Commit Message Format

Follow this repo's conventional-commit style (see the root `CLAUDE.md` git
workflow section). A typical agent-tracked commit:

```
feat(trusty-mpm): add skill-source doctor probe (A2)

- Adds check_skill_source() mirroring the check_agents pattern
- Warns via tracing::warn! on missing/empty skill source
- Part of the /tm- skills portfolio epic

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
```

## Before Ending Any Session

Final verification checklist:

```bash
git status   # should show untracked deliverables, ideally none left
# if any deliverable files remain untracked:
git add <files>
git commit -m "..."
git status   # must now show a clean working tree
```

**Ideal state**: `git status` shows no untracked deliverable files, because
the PM tracked them immediately after each agent — not batched at the end.

## Example Workflow

```bash
# After the language-appropriate engineer adds a new feature
# (paths shown are placeholders — substitute the files your engineer actually
#  touched, e.g. src/handlers/auth.ts, app/services/user.py, src/core/foo.rs)
git status
#   modified:   src/<module>/<file-a>
#   modified:   src/<module>/<file-b>

git add src/<module>/<file-a> src/<module>/<file-b>
git commit -m "fix(<scope>): warn instead of silent no-op on empty input

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"

git status   # clean
```

## Integration with Worktree Discipline

Per this repo's worktree convention (see root `CLAUDE.md`), all write-side
work — including this tracking sequence — happens inside a dedicated
`.claude/worktrees/<name>` worktree branched off `origin/main`. Never run
`git add`/`git commit` from the main checkout.

## Integration with Todo Workflow

**Blocking sequence**: agent completes → PM checks for created/modified files
→ if any, run the tracking protocol (cannot proceed until complete) → only
then mark the todo complete. This ensures no deliverable is ever lost between
an agent's completion and the end of the session.
