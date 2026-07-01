---
name: tm-tool-usage-guide
description: Detailed tool usage patterns and examples for the trusty-mpm PM agent
user-invocable: false
version: "1.0.0"
category: pm-reference
tags: [tools, mcp, delegation, pm-required]
effort: medium
---

# tm Tool Usage Guide

Detailed usage patterns for the PM's real tool families:
`mcp__trusty-mpm__*`, `mcp__trusty-memory__*`, `mcp__trusty-search__*`, and
`mcp__trusty-review__*`. This is the concrete "how" behind the constraints in
`tm-circuit-breaker`.

## Context-First Protocol (Mandatory)

Before delegating to Research or reading any file:

1. `mcp__trusty-memory__memory_recall` (or `memory_recall_deep` for a
   cross-palace sweep) — check what is already known.
2. `mcp__trusty-search__search` (hybrid BM25+vector) or `search_semantic` /
   `search_lexical` — pass `index_id` matching the project name (e.g.
   `trusty-mpm`, `trusty-tools`). Use `search_all` when the scope spans
   multiple indexed projects, `search_similar` to find analogous code, and
   `list_indexes` to discover what is indexed.
3. Only if both are insufficient — delegate to Research.

Both tools are mandatory-first, not optional. Skipping straight to Read/Grep
is CB#2 / CB#10 in `tm-circuit-breaker`.

## mcp__trusty-search__* — Code Search

| Tool | Use |
|---|---|
| `search` | Hybrid BM25 + vector search; first choice for "find code that does X" |
| `search_semantic` / `search_lexical` | Narrower single-strategy search when hybrid over- or under-matches |
| `search_all` | Cross-project search when the scope is unclear |
| `search_similar` | Find code semantically similar to a known snippet |
| `search_kg` / `get_call_chain` | Structural queries — call graphs, symbol relationships |
| `grep` / `typeahead` | Lexical lookup once you already know the token to find |
| `search_health` | Liveness check — **use instead of `curl`/`lsof`** (CB#7) |
| `list_indexes` / `index_status` | Discover what projects are indexed and their freshness |

**Example:**
```
mcp__trusty-search__search(query="skill deploy no-op", index_id="trusty-mpm", limit=5)
```

## mcp__trusty-memory__* — Persistent Context

| Tool | Use |
|---|---|
| `memory_recall` | Recall relevant context by query — call BEFORE research/delegation |
| `memory_recall_deep` / `memory_recall_all` | Cross-palace / exhaustive recall |
| `memory_remember` / `memory_note` | Store important findings immediately, not at session end |
| `task_add` / `task_list` / `task_complete` | Lightweight session-local task tracking (see `tm-session-management`, `tm-ticketing`) |
| `get_prompt_context` | Load current aliases/conventions at turn start — pass a `query` to scope it |
| `kg_assert` / `kg_query` / `kg_gaps` | Knowledge-graph assertions and structural recall |

## mcp__trusty-mpm__* — Orchestration

| Tool | Use |
|---|---|
| `agent_delegate` | Programmatic delegation surface (mirrors the Agent tool for MCP-driven flows) |
| `circuit_breaker_status` | Check live per-agent runtime breaker state before a retry (CB#10) |
| `session_new` / `session_list` / `session_status` / `session_send` / `session_stop` / `session_resume` | Drive durable tmux-backed sessions (see `session-manager-driver` for the full loop) |
| `session_decommission` / `session_decommission_ephemeral` / `session_prune` | Teardown of managed sessions |
| `list_recent_errors` / `preview_bug_report` / `report_bug` | The bug-reporting pipeline — see `tm-bug-reporting`, `tm-postmortem` |
| `project_register` / `project_get` / `project_list` / `project_resolve` | Project registry lookups |
| `supervisor_status` | Fleet-level supervisor health |
| `config_read` / `config_write` | Read/write `~/.trusty-mpm/config.toml` |
| `hook_event` | Emit a hook event (internal wiring; rarely called directly) |
| `memory_protect` | Guard a memory entry from eviction/consolidation |

## mcp__trusty-review__* — Code Review Gate

| Tool | Use |
|---|---|
| `review_diff` | Review an in-progress diff for correctness/design issues before PR |
| `review_pr` | Review an open PR — pairs with the QA gate (CB#8) |
| `review_health` | Liveness check for the review daemon |

## Read Tool — Strict Limit

The PM must never read source code files directly (CB#2 in
`tm-circuit-breaker`). Source extensions (`.rs`, `.ts`, `.py`, `.go`, `.js`,
`.java`, ...) always delegate to Research. The single exception is up to a
handful of small config/manifest files (`Cargo.toml`, `package.json`,
`.mcp.json`) read once for delegation context — never for understanding the
implementation.

Pre-flight check before any Read call: is this a source file? have I already
read this turn? does my task contain investigation keywords ("check",
"find", "analyze", "investigate")? Any "yes" → delegate to Research instead.

## Bash Tool — Navigation and Git Tracking Only

**Allowed**: `ls`, `pwd`, `cd`; `git status`, `git add`, `git commit`, `git
log`, `git diff`.

**Forbidden (delegate instead)**:
- Verification commands (`curl`, `lsof`, `ps`, `wget`, `nc`, `make`, `pytest`,
  `npm test`) → Local Ops or QA (CB#7); trusty-* daemon health specifically
  uses `mcp__trusty-search__search_health` / `mcp__trusty-memory__memory_recall`,
  not Bash.
- File modification (`sed`, `awk`, `patch`, `git apply`, `>`, `>>`, `tee`) →
  Engineer, or Edit/Write directly (CB#14).
- Implementation commands (`npm start`, `docker run`, `cargo build --release`
  as a deploy step) → the appropriate Ops agent.
- Browser automation (`mcp__claude-in-chrome__*`, `mcp__playwright__*`) →
  Web QA (CB#6).

## Forbidden MCP Tools for the PM

| Category | Forbidden | Delegate To |
|---|---|---|
| Code modification | `Edit`, `Write` (beyond the `.git/COMMIT_EDITMSG` exception) | Engineer |
| Deep investigation | `Grep`/`Glob` used repeatedly | Research |
| Ticketing | `mcp__mcp-ticketer__*`, `mcp__github__*` issue/PR tools, `WebFetch` on ticket URLs | ticketing agent (see `tm-ticketing`) |
| Browser | `mcp__claude-in-chrome__*`, `mcp__playwright__*` | web-qa |

See `tm-circuit-breaker` for the full enforcement model and `tm-delegation-patterns` for agent selection.
