---
name: tm-init
description: Initialize or intelligently refresh a project for trusty-mpm — analyze the repo and scaffold or update CLAUDE.md (project instructions), register the project with the daemon, and offer update/context/catchup modes
user-invocable: true
version: "1.0.0"
category: pm-workflow
tags: [init, setup, scaffolding, claude-md, pm-recommended]
effort: medium
---

# /tm-init — Project Initialization & Refresh

Analyze the current repository and scaffold or intelligently update its
**project documentation** — primarily `CLAUDE.md` — then register the project
with the trusty-mpm daemon so it is tracked and resumable.

## What This Is NOT: Trusty Config Deployment

The trusty **config** — agents, skills, `INSTRUCTIONS.md`, `settings.json`,
output-style, and `.mcp.json` — is **not** the responsibility of this skill.
That config is deployed **automatically** every time trusty-mpm launches a
managed session, and can be (re)deployed to any directory on demand:

```bash
tm install            # (re)deploy the trusty config into the current directory
tm install --force    # overwrite existing config artifacts
```

So `/tm-init` never touches `.claude/agents/`, `.claude/skills/`,
`.claude/settings.json`, `INSTRUCTIONS.md`, the output style, or `.mcp.json`.
If a session is missing its config, run `tm install` (or just relaunch the
managed session) — not `/tm-init`.

## What `/tm-init` Actually Does

`/tm-init` focuses on **project** initialization and refresh:

1. **Analyze the repository** — languages, frameworks, build/test/lint
   commands, directory layout, entry points, and existing conventions.
2. **Scaffold or update `CLAUDE.md`** — the project-instructions file that
   tells any future session how to build, test, and navigate this repo. New
   projects get a fresh priority-ranked (🔴🟡🟢⚪) file; existing projects get a
   smart, non-destructive refresh that preserves custom sections.
3. **Register the project** with the trusty-mpm daemon (`tm project init`) so it
   is tracked, listable, and resumable.
4. Optionally surface recent work context (`update` / `context` / `catchup`
   modes below).

## Usage

```
/tm-init [update|context|catchup] [message]
```

Examples:

```
/tm-init                # Auto-detect: create CLAUDE.md if missing, else offer a refresh
/tm-init update         # Refresh CLAUDE.md against recent repo activity
/tm-init context        # Deep git-history analysis of active work streams
/tm-init catchup        # Fast work-context digest via `tm session catchup`
```

## Modes

### (default) — Scaffold or Refresh

With no mode argument:

- **No `CLAUDE.md` present** → analyze the repo and scaffold a new one:
  project overview, single-path build/test/lint commands, key conventions,
  directory map, and priority-ranked instructions.
- **`CLAUDE.md` present** → offer an intelligent update: merge in newly
  discovered commands/conventions, archive stale guidance, and **preserve any
  custom sections** the operator has written. Always confirm before writing.

Register the project as part of either path:

```bash
tm project init            # register the cwd as a trusty-mpm project
tm project init --dir PATH # register a specific directory
tm project info            # show the current project's registered info + config
tm project list            # list all registered projects and their status
```

### `update` — Documentation Refresh

Re-derive build/test/lint commands and conventions from the current state of
the repo (recent commits, changed manifests, new tooling) and reconcile them
into `CLAUDE.md`. Non-destructive: custom sections are preserved, stale ones
archived, and every change is confirmed before writing.

### `context` — Deep Work-Stream Analysis

Analyze recent git history to surface active work streams, intent, risks, and
recommended next actions. Delegate the heavy analysis to the **research** agent
(via the Agent/Task tool) rather than doing it inline; present its digest.

### `catchup` — Fast Work-Context Digest

Map directly to the real CLI command, consistent with `/tm-session-resume`:

```bash
tm session catchup                  # current project only
tm session catchup --all-projects   # also scan machine-wide registered projects
```

`catchup` renders a unified, newest-first work-context digest for the current
project — instant, no LLM analysis. After running it, reconcile against
`git log --oneline -5` / `git status` and present the digest.

## What Gets Written

`/tm-init` only ever writes **project documentation** — never trusty config:

- ✅ `CLAUDE.md` — project instructions (created or smart-merged)
- ✅ project registration under `.trusty-mpm/` (via `tm project init`)
- ❌ never `.claude/agents/`, `.claude/skills/`, `.claude/settings.json`,
  `INSTRUCTIONS.md`, output styles, or `.mcp.json` (those are `tm install`'s job)

Project-local trusty-mpm state (registration, sessions) lives under
`.trusty-mpm/` at the project root. Add `.trusty-mpm/sessions/` to `.gitignore`
(machine-local session snapshots); the registered `.trusty-mpm/config.toml` is
version-controllable and is never clobbered on re-`init`.

## Delegation

- **CLAUDE.md scaffold/refresh** → keep with the PM, or delegate the repo
  analysis to the **research** agent and the file write to an **engineer** agent
  for a large repo; confirm the result before writing.
- **`context` deep analysis** → **research** agent (structured analysis).
- **`catchup`** → direct CLI (`tm session catchup`), no delegation.

## Related

- `tm install` — (re)deploy the trusty **config** (agents/skills/settings/mcp);
  this is the config path `/tm-init` deliberately does not cover.
- `/tm-session-management` — session pause/resume policy, the auto-pause
  threshold, project-local `.trusty-mpm/sessions/` format, and worktree pruning.
- `/tm-session-pause` and `/tm-session-resume` — the focused pause/resume actions.
