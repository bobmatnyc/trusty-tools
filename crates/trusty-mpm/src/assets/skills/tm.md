---
name: tm
description: trusty-mpm orchestration model overview — agents, skills, delegation
user-invocable: true
version: "1.0.0"
category: pm-reference
tags: [overview, orchestration, framework]
effort: low
---

# /tm — trusty-mpm Overview

trusty-mpm is a Rust daemon + MCP-server platform (not the Python
`claude-mpm` project — see `docs/WHAT-IS-TRUSTY-MPM.md` if that distinction
ever matters). A **PM agent** coordinates specialist agents through
structured delegation; it does not implement, investigate, or verify
directly. See `tm-circuit-breaker` for the full enforcement model behind
that rule.

## How Delegation Works

The PM breaks a request into subtasks and delegates each to the right agent
via the Agent/Task tool. Independent subtasks run in parallel; dependent
ones run sequentially. An agent that needs another specialty reports back to
the PM rather than self-delegating.

**Typical chain**: Research → Engineer → Ops (deploy) → Ops (verify) → QA →
Documentation. See `tm-delegation-patterns` for the concrete chains and
`tm-workflow` for how a project customizes the phase model.

## Agent Roster (Bundled)

Verified against `bundle_all.rs`'s `ALL` table — this is not exhaustive of
every possible custom agent a project may add, but every name below is
really bundled.

**Core**: `engineer`, `research`, `qa`, `ops`, `security`, `documentation`,
`ticketing`, `code-analyzer`, `data-engineer`, `version-control`

**Language engineers**: `rust-engineer`, `python-engineer`,
`typescript-engineer`, `javascript-engineer`, `golang-engineer`,
`java-engineer`, `php-engineer`, `ruby-engineer`, `dart-engineer`

**Framework specialists**: `react-engineer`, `nextjs-engineer`,
`svelte-engineer`, `tauri-engineer`, `phoenix-engineer`, `web-ui-engineer`

**QA**: `web-qa`, `api-qa`, `code-critic`

**Ops**: `local-ops`, `vercel-ops`, `gcp-ops`

**Specialized**: `refactoring-engineer`, `prompt-engineer`,
`memory-manager`, `mpm-agent-manager`, `mpm-skills-manager`

See `tm-agent-architecture` for how these are built (the `extends:` compose
chain) and how to update one safely.

## Skills System

Skills are Markdown files at `.claude/skills/<name>/SKILL.md` providing
reusable knowledge/procedures. User-invocable skills respond to `/skill-name`;
others load automatically on context triggers. The full `/tm-*` portfolio:
`tm-circuit-breaker`, `tm-verification-protocols`, `tm-tool-usage-guide`,
`tm-git-file-tracking`, `tm-adr`, `tm-workflow`, `tm-agent-architecture`,
`tm-postmortem`, `tm-bug-reporting`, `tm-teaching-templates`, `tm-ticketing`,
`tm-pr-workflow`, `tm-delegation-patterns`, `tm-session-management`,
`tm-doctor`.

## Memory System

`mcp__trusty-memory__*` provides persistent cross-session context —
`memory_recall`/`memory_recall_deep` to read, `memory_remember`/`memory_note`
to write, `task_add`/`task_list`/`task_complete` for durable task tracking.
Not a per-agent file on disk (unlike claude-mpm) — a live MCP-backed memory
palace. Check before research or delegation, per `tm-tool-usage-guide`.

## Health and Diagnostics

```bash
tm doctor                              # full stack diagnostic (see tm-doctor)
mcp__trusty-mpm__supervisor_status     # fleet-level health
mcp__trusty-search__search_health      # trusty-search liveness
```

No dashboard on `:8765` — that's claude-mpm's Socket.IO monitor, which does
not exist here. Use the TUI (`tm tui`) or the MCP health tools above.

## Available Commands

| Command | Description |
|---|---|
| `/tm` | This overview |
| `/tm-doctor` | Full diagnostic (`tm doctor`) |
| `/tm-workflow` | Workflow/phase customization |
| `/tm-ticket` | Ticketing orchestration |
| `/tm-session` | Pause/resume session management |
| `/tm-adr` | Architecture Decision Records |
| `/tm-postmortem` | Session-error analysis and reporting |

## For Agents: Working Within trusty-mpm

Focus on your delegated task; return results with evidence (file paths,
test counts, commit hashes — see `tm-verification-protocols`). Report back
to the PM if you need another specialty rather than self-delegating.
Escalate blockers immediately rather than silently producing a partial
result.
