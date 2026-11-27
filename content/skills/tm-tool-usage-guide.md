---
name: tm-tool-usage-guide
description: Detailed tool usage patterns and examples for the trusty-mpm PM agent
user-invocable: false
metadata:
  version: "1.0.4"
category: pm-reference
tags: [tools, mcp, delegation, pm-required]
effort: medium
---

# tm Tool Usage Guide

Detailed usage patterns for the PM's real tool families: the native `tm
memory` and `trusty-search` commands, plus `mcp__trusty-mpm__*` and
`mcp__trusty-review__*`. MCP adapters for memory and search are for non-mpm
callers only, except the search verbs the released `trusty-search` CLI lacks
(see #11771). This is the concrete "how" behind the constraints in
`tm-circuit-breaker`.

## Deferred MCP Tool Loading (Mandatory Before Any "Unavailable" Fallback)

Harnesses with many-tool MCP servers (Claude Code, for `mcp__trusty-mpm__*`
in particular — over 30 registered tools) may defer loading a tool's schema
until it is explicitly fetched. The daemon-side tool is always registered
regardless of launch mode; **absence from your currently loaded tool list
does not mean the tool is unavailable** — it means the schema hasn't been
fetched yet.

Before calling any `mcp__*` tool that isn't already in your loaded tool list,
load it first:

```
ToolSearch(query: "select:<full-tool-name>[,<other-tool-name>...]")
```

Batch every tool you expect to need for the current step into one
`ToolSearch` call rather than loading one at a time. This load step is
**mandatory** before concluding a tool is unavailable or improvising a
fallback — see `tm-session-pause` / `tm-session-resume` for the fully worked
three-way diagnosis (load failed / call failed / server genuinely absent)
that applies to every skill instructing a direct MCP call, including
`tm-bug-reporting` and `tm-postmortem`.

## Context-First Protocol (Mandatory)

Before delegating to Research or reading any file:

1. `tm memory recall "<q>"` — check what is already known. For a
   cross-palace sweep, `tm memory recall-deep` or `recall-all`.
2. `trusty-search search "<q>" --json` from the repo, or
   `trusty-search query "<q>" -i <index>` from elsewhere (hybrid BM25+vector).
   For a narrower lane, use `trusty-search semantic|lexical|kg "<q>"` (see the
   table below).
3. Only if both are insufficient — delegate to Research.

Both tools are mandatory-first, not optional. Skipping straight to Read/Grep
is CB#2 / CB#10 in `tm-circuit-breaker`.

## Code Search — `trusty-search` CLI first

MCP adapters are for non-mpm callers only. In an mpm session, run the native
command:

```
trusty-search search "skill deploy no-op" --json        # from the repo
trusty-search query "skill deploy no-op" -i <index-id>  # from elsewhere
```

A `TRUSTY_INDEX` variable in the environment can override the index; pass
`-i` from outside the repo. Every search tool has a native verb, so an mpm
session never needs an `mcp__trusty-search__*` name (#11625):

| Command | Use |
|---|---|
| `trusty-search semantic "<q>"` / `trusty-search lexical "<q>"` | Narrower single-strategy search when hybrid over- or under-matches |
| `trusty-search query "<q>" --indexes "*"` | Cross-project search when the scope is unclear |
| `trusty-search similar <file> --function <sym>` | Find code semantically similar to a known snippet |
| `trusty-search kg "<q>"` / `trusty-search call-chain <sym>` | Structural queries — call graphs, symbol relationships |
| `trusty-search grep <pattern>` / `trusty-search typeahead <prefix>` | Lexical lookup once you already know the token to find |
| `trusty-search health` | Health check — **use instead of `curl`/`lsof`** (CB#7) |
| `trusty-search list [--details]` / `index-status` | Discover what projects are indexed and their freshness |

**Index Pinning (Automatic, MCP tools):** In managed sessions, your `.mcp.json` pinpoints
the session's own project index (via `trusty-search serve --index <id>`) so bare
`search` calls never hit the wrong project. Resolution is pinned-first (#5213):
an explicit `index_id` wins, otherwise the pin is used, and only an unpinned
session with no id fans out across every index. An unresolvable id fails with
`404 unknown index` (#1373). See
`docs/ARCHITECTURE-MEMORY-SESSIONS-SEARCH.md` § 3 (per-worktree index model)
for the design and lifecycle.

## Persistent Context — `tm memory` first

MCP adapters are for non-mpm callers only. Recall, remember and note run as
native commands over the trusty-memory Unix socket (#8352):

```
tm memory recall "<q>"                    # call BEFORE research/delegation
tm memory remember "<text>" [--tag ruling]
tm memory note "<fact>"                   # short curated fact, importance 1.0
```

Each works in this session's own palace, with `--palace` to override it,
`--top-k` / `--room` / `--tag` for scope, and `--json` for a parseable
envelope (`verb`, `palace`, `socket`, `count`, `result`). With trusty-memory
down each exits non-zero naming the socket. Store findings as you learn them,
not at session end.

The other memory verbs are native too:

| Command | Use |
|---|---|
| `tm memory recall-deep` / `recall-all` / `list` | Deep, cross-palace and listing recall |
| `tm memory task add` / `list` / `complete` | Lightweight session-local task tracking (see `tm-session-management`, `tm-ticketing`) |
| `tm memory prompt-context ["<q>"]` | Load current aliases/conventions at turn start — pass a query to scope it |
| `tm memory kg assert` / `query` / `gaps` | Knowledge-graph assertions and structural recall |

## mcp__trusty-mpm__* — Orchestration

| Tool | Use |
|---|---|
| `agent_delegate` | Tracking + circuit-breaker **gate** for a delegation — records it in the dashboard tree and enforces breaker/depth limits. **NOT an execution path**: it does not spawn the agent. Actually run the agent with the native **Agent/Task tool** (`subagent_type="<deployed-agent>"`); this call is an optional companion, never a substitute. |
| `circuit_breaker_status` | Check live per-agent runtime breaker state before a retry (CB#10) |
| `session_new` / `session_list` / `session_status` / `session_send` / `session_stop` / `session_resume` | Drive durable tmux-backed sessions (see `session-manager-driver` for the full loop) |
| `session_decommission` / `session_decommission_ephemeral` / `session_prune` | Teardown of managed sessions |
| `list_recent_errors` / `preview_bug_report` / `report_bug` | The bug-reporting pipeline — see `tm-bug-reporting`, `tm-postmortem`. A filed issue still owes a milestone, relationships and state — `tm-ticketing`, "Every issue: milestone, relationships, state" |
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

## twg — Jira and Atlassian Content

Prefer the `twg` CLI (`~/.local/bin/twg`) over MCP connectors or `WebFetch`
for any Jira, Confluence, or other Atlassian read or write (owner ruling
2026-09-23). Always pass `--output json` for machine-readable results.
Credentials come from twg's own `auth.conf` (see `twg --help`; run `twg env`
for the resolved path — never open or print the file itself).

**Example** (from `twg --help`):
```
twg jira workitem get PROJ-123 -o json
```

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
log`, `git diff`; `tm memory recall|recall-deep|recall-all|remember|note|list|prompt-context|task|kg`
and `trusty-search search|query|health|list|index-status` — the native memory
and search verbs.

**Forbidden (delegate instead)**:
- Verification commands (`curl`, `lsof`, `ps`, `wget`, `nc`, `make`, `pytest`,
  `npm test`) → Local Ops or QA (CB#7); trusty-* daemon health specifically
  uses `trusty-search health`.
- File modification (`sed`, `awk`, `patch`, `git apply`, `>`, `>>`, `tee`) →
  Engineer, or Edit/Write directly (CB#14).
- Implementation commands (`npm start`, `docker run`, `cargo build --release`
  as a deploy step) → the appropriate Ops agent.
- Browser automation (`mcp__trusty-browser__*`, `mcp__claude-in-chrome__*`, `mcp__playwright__*`) →
  Web QA (CB#6).

## Forbidden MCP Tools for the PM

| Category | Forbidden | Delegate To |
|---|---|---|
| Code modification | `Edit`, `Write` of source **past the direct-action budget** — delegate once a task will take more than 3 direct actions, or the moment a 3-action estimate stops holding mid-flight (#4594). Non-source writes and `.git/COMMIT_EDITMSG` are unbudgeted | `engineer` |
| Deep investigation | `Grep`/`Glob` used repeatedly | Research |
| Ticketing | `mcp__mcp-ticketer__*`, `mcp__github__*` issue/PR tools, `WebFetch` on ticket URLs | ticketing agent (see `tm-ticketing`) |
| Browser | `mcp__trusty-browser__*`, `mcp__claude-in-chrome__*`, `mcp__playwright__*` | web-qa (on trusty-browser) |

See `tm-circuit-breaker` for the full enforcement model and `tm-delegation-patterns` for agent selection.
