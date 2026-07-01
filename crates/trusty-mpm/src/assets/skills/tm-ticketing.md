---
name: tm-ticketing
description: Ticket-driven development protocol and high-level ticketing orchestration for the trusty-mpm PM
user-invocable: true
version: "1.0.0"
category: pm-workflow
tags: [tickets, workflow, pm-required]
effort: medium
---

# /tm-ticket — Ticketing Protocol

Consolidates ticket-driven-development (TkDD) enforcement and the
high-level `/tm-ticket` orchestration commands. **The PM never calls
ticketing tools directly — always delegate to the `ticketing` agent.**

## Verified Tool Reality

The bundled `ticketing` agent (`crates/trusty-mpm/src/assets/agents/ticketing.md`)
has this real priority order — verified against the agent source, not
assumed:

1. **Primary**: `mcp__mcp-ticketer__*` MCP tools, when configured.
2. **Fallback**: `aitrackdown` CLI (`aitrackdown create issue/task`,
   `aitrackdown transition`, `aitrackdown status tasks`) when no ticketing
   MCP server is available.

Separately, this repo's own convention (see root `CLAUDE.md`) defaults to
**GitHub Issues** for the actual delivery chain: spec → issue → worktree
branch → PR → trusty-review gate → squash-merge. When no dedicated ticketing
MCP is configured, route GitHub issue operations to the **Version Control**
agent (`gh issue create/view/list`, or `mcp__github__*` tools), not the
`ticketing` agent's `mcp-ticketer`/`aitrackdown` path — they serve different
scopes (external ticket-tracker vs. this repo's own GitHub issues).

For lightweight, session-local task tracking that isn't a formal ticket at
all, use `mcp__trusty-memory__task_add` / `task_list` / `task_complete`
directly — these are cheap in-session TODOs, not a ticketing system
replacement, and do not require delegation (they are not one of the
forbidden MCP families in `tm-circuit-breaker` CB#6).

## Delegation Pattern (CB#6 in `tm-circuit-breaker`)

**Wrong:**
```
PM: mcp__mcp-ticketer__ticket_list()   # PM using ticketing tools directly
```

**Correct:**
```
PM: "I'll have ticketing organize the board..."
[PM constructs a delegation prompt for the ticketing agent]
[ticketing agent uses mcp-ticketer or aitrackdown internally]
PM: [presents results]
```

## Ask Before Creating

If the user references a ticket/issue but no matching one is found:
ticketing (or Version Control, for GitHub issues) MUST NOT auto-create.
Ask: "I didn't find an existing issue for [topic]. Create one, or did you
mean a different one?" Auto-create only on explicit "create a
ticket/issue for X."

## Ticket-Driven Development Protocol (TkDD)

When a ticket/issue reference is detected (an ID pattern, a URL, "work on
issue #123"), the PM executes:

1. **Work start** — delegate: transition to in-progress, comment "work
   started."
2. **Each phase** — delegate a progress comment with deliverables and
   links to commits/PRs.
3. **Work complete** — delegate: transition to done/closed, comprehensive
   completion comment, link the PR.
4. **Blockers** — delegate: transition to blocked, comment with blocker
   detail and impact.

Every delegation in this chain includes the ticket/issue context so
downstream agents (Engineer, QA) know the work is ticket-driven and can
reference it in their own output.

## `/tm-ticket` Subcommands

High-level orchestration over the ticketing agent (for whichever tracker is
configured):

| Subcommand | Purpose |
|---|---|
| `/tm-ticket organize` | Review, transition states, update priorities, flag stale tickets |
| `/tm-ticket proceed` | Analyze the board, recommend the top 3 next actions |
| `/tm-ticket status` | Health metrics, ticket counts, high-priority work, blockers |
| `/tm-ticket project <url>` | Set the default project/tracker context |

Every subcommand is a PM delegation to the ticketing agent with a specific
task description — the PM constructs the prompt and presents the result, it
never calls the underlying tools itself.

## Documentation Routing With Ticket Context

When a ticket context is present, delegate to attach research findings and
specs as ticket comments (or linked files); still create a local backup doc
under `docs/research/` (or the configured `documentation.docs_path`). Without
ticket context, everything goes to the local docs path only, named
`{topic}-{date}.md`.

## Violation Prevention

Directly using ticketing tools is CB#6 (Forbidden Tool Usage) in
`tm-circuit-breaker`: Violation #1 WARNING, #2 ESCALATION, #3 FAILURE.

## Related Skills

- `tm-circuit-breaker` — CB#6 enforcement detail
- `tm-pr-workflow` — the PR side of the same delivery chain
- `tm-delegation-patterns` — where ticketing fits in the broader agent matrix
