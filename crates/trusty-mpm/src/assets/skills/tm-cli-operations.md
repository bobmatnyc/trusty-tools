---
name: tm-cli-operations
description: Operate the tm / trusty-mpm CLI — set up and manage MCP servers, drive session lifecycle, and run health diagnostics
user-invocable: true
version: "1.0.0"
category: pm-reference
tags: [cli, mcp, sessions, services, doctor, auth, operations, pm-recommended]
effort: medium
---

# tm CLI Operations

How to drive the `tm` (trusty-mpm) binary itself: register and verify **MCP
servers**, manage the **session lifecycle**, and run **health diagnostics**.

This is distinct from `tm-tool-usage-guide`, which covers the PM's in-session
`mcp__trusty-mpm__* / mcp__trusty-memory__* / mcp__trusty-search__*` **tool**
families. This skill is about the `tm` **command-line binary** and the config it
owns.

**PM-delegation framing.** A PM under circuit-breaker discipline does not shell
out to run process-spawning or health commands (`tm mcp test`, `tm services`,
`tm doctor`) — it delegates execution to **local-ops** and reads back the
result. This skill tells you *what* to run and how to read it; local-ops (or you
at your own shell, outside a managed session) actually runs it. One-time config
edits (`tm mcp add/remove`, `tm auth set-token`) are setup, run once at your
shell.

---

## 1. MCP Server Management (headline)

### Where the config lives — the tm-owned config dir

Managed sessions launch `claude` under a **relocated** `CLAUDE_CONFIG_DIR`, not
your real `~/.claude`. `tm mcp` edits the top-level `mcpServers` map of that
dir's `.claude.json`:

- Default (daemon-managed): `~/.trusty-tools/trusty-mpm/claude-config/.claude.json`
- Standalone root: `<root>/claude-config/.claude.json` when you pass `--root`
  (same precedence chain as `tm register`/`tm ls`:
  `--root` > `TRUSTY_MPM_ROOT` env > `[standalone] root` in config > `~/.trusty-mpm`).

This map is **user scope** — "available in all your projects." Every tm-managed
session sees these servers, with no per-project approval dialog. There is
deliberately **no `--scope` flag**: `tm mcp` is inherently user scope. (Stock
`claude mcp add` cannot target this relocated dir, which is why `tm mcp` exists.)

### The 3 auto-provisioned framework built-ins

Every managed session always gets these three, whether or not they appear in the
`mcpServers` map:

| Server | Launch command |
|---|---|
| `trusty-memory` | `trusty-memory serve --stdio` |
| `trusty-review` | `trusty-review serve --stdio` |
| `trusty-search` | `trusty-search serve` |

You never register these — they are built in. `tm mcp add` is for **additional**
servers on top of them.

### Adding a server — `tm mcp add`

stdio (default transport — a local subprocess speaking MCP over stdio):

```bash
# Options (-t/-e/--root) MUST precede the command token.
# A leading `--` stops flag parsing so hyphen-led args pass through.
tm mcp add my-server -e API_KEY=xxx -- npx -y @scope/some-mcp-server
tm mcp add pyserver -e LOG=debug -- python -m my_mcp_pkg --flag
```

- `-e KEY=VALUE` (repeatable) — environment for the subprocess. Splits on the
  **first** `=` (so `-e B=x=y` → `B` = `x=y`).
- The first positional token after the options is the **command**; the rest are
  subprocess args. `add` only **warns** (never blocks) when the command is not
  on `PATH` — it may resolve later at session launch.

http / sse (a remote endpoint):

```bash
tm mcp add remote-api -t http -H "Authorization: Bearer $TOKEN" https://host/mcp
tm mcp add events -t sse -H "X-Api-Key: $KEY" https://host/sse
```

- `-H "Name: Value"` (repeatable) — HTTP headers. Splits on the **first** `:`,
  value trimmed. Only valid for http/sse.
- `-e` and subprocess args are rejected for http/sse; `-H` is rejected for stdio.

### How added servers flow into the trust-seed

You do **not** manage trust separately. On the next managed-session start, tm
computes the trust list as `BUILTIN_MANAGED_MCP_SERVERS ∪ (your mcpServers keys)`
and pre-seeds each workspace's `enabledMcpjsonServers` (via
`preseed_managed_trust`). Result: a `tm mcp add`-ed server is **auto-trusted** —
the session never shows the "New MCP servers found" approval dialog. No extra
bookkeeping.

### Inspecting — `tm mcp list` / `tm mcp get`

```bash
tm mcp list                 # table: NAME  TYPE  TARGET
tm mcp list --json          # raw mcpServers map (automation)
tm mcp get my-server        # one server: Type / Target / Scope
tm mcp get my-server --json # the raw entry
```

### Verifying — `tm mcp test` (the real handshake)

Registration only proves the config is well-formed. `tm mcp test` **spawns each
server and runs a real MCP handshake** so you (and CI) get a definitive
pass/fail. This is the MCP-layer check.

```bash
tm mcp test                 # bare sweep: every configured server ∪ the 3 built-ins
tm mcp test my-server       # just one (config entry, else built-in fallback, else error)
tm mcp test --json          # machine-readable results array (automation)
```

- **stdio** servers: full `initialize` → `tools/list` handshake; reports the
  tool count on success. Bounded by a 5s probe timeout.
- **http/sse** servers: an HTTP reachability check.
- **Exit code: non-zero (1) if ANY tested server fails** — slots straight into
  a CI gate, matching `tm services`' convention. A named server that isn't found
  is a hard error.

**Interpreting failures** (the RESULT/DETAIL column, or `error` in `--json`):

| Failure | Meaning | Typical fix |
|---|---|---|
| `spawn error: …` | The command couldn't be launched | Command not on `PATH`; fix the `-- <command>` or install it |
| `timeout` | No handshake within 5s | Server hung on startup, or waits on missing env/creds |
| `protocol error: …` | Launched but spoke bad/no MCP | Wrong command, non-MCP binary, or a version mismatch |
| `unreachable: …` | http/sse endpoint not reachable | Bad URL, server down, or auth header rejected |

### Migrating servers from a stock claude-code config

To bring servers you already use in stock Claude Code into the managed fleet,
read them from your existing config and re-register with `tm mcp add`:

- User-level source: `~/.claude.json` → its `mcpServers` map.
- Project-level source: a repo's `.mcp.json` → its `mcpServers` map.

For each entry, translate to a `tm mcp add`: `command` + `args` → the stdio form
(`-- <command> <args...>`), `env` → repeated `-e KEY=VALUE`, a `url` → the
`-t http|sse` form with `-H` headers. **Never echo secret env/header values into
logs or transcripts** — reference them via `$ENV_VAR`, and confirm the config
dir before writing. Verify the migration with `tm mcp test <name>`.

### Removing — `tm mcp remove`

```bash
tm mcp remove my-server     # drops it from the map; no-op (not an error) if absent
```

---

## 2. Session Lifecycle

`tm session <verb>` has **two families** under one command group. A verb from one
family is not a valid target for the other's IDs.

**Local project sessions** (bound to the cwd, daemon-backed):

```bash
tm session start [--dir <path>]        # new session in the project
tm session list [--dir <path>]         # sessions for this project
tm session info <id|name>              # detailed info
tm session pause <id|name> [--note …]  # save state for later resume
tm session run <id|name> "<cmd>"       # send a command to the pane
tm session output <id|name> [--lines N]# capture recent pane output
tm session clean [--dir <path>]        # reap dead sessions
```

**Managed fleet sessions** (provisioned in isolated worktrees):

```bash
tm session new <repo> --task "<desc>" [--git-ref main] [--name-hint …] [--runtime claude-code|tcode]
tm session ls [--current | --source-id owner/repo] [--all] [--json]
tm session activity <id>               # what a session is doing (no attach)
tm session send <id> "<text>"          # inject text into the pane
tm session answer <id> "<answer>"      # resolve a pending decision
tm session attach <id>                 # print the tmux attach command
```

**Shared verbs** (managed-aware — auto-detect managed vs local by id/name):

```bash
tm session stop <id|name>              # non-destructive; workspace preserved, resumable
                                       #   (alias: `tm session kill`)
tm session resume <id|name>            # re-spawn runtime / restore paused session
```

### The `tm ls` / bare `tm` picker (incl. `d<N>` delete)

On a real terminal, bare `tm ls` (and bare `tm`) opens the interactive
session picker — a numbered menu of live managed sessions. Actions:

- `<N>` — connect to session N.
- **`d<N>`** (or `d <N>`) — **delete** the session at slot N (#2304). A
  confirm prompt guards it, and the delete is routed correctly for managed vs
  local sessions.
- Piped / `--json` / non-TTY invocations degrade to a static, pipeable list
  (never block on stdin). `tm ls --projects` (`-p`) shows the repo-alias /
  project registry instead of sessions.

### Delete / teardown semantics — pick the right verb

| Verb | Runtime | Workspace on disk | Record | Resumable? |
|---|---|---|---|---|
| `stop` / `kill` | stopped | **preserved** | kept | **yes** |
| `decommission` | stopped | **removed** | tombstone kept | no (terminal) |
| `delete` | (guarded) | **never touched** | **hard-deleted** | no |

- `tm session decommission <id>` — the only op that **removes the workspace
  directory**. Terminal. Keeps a tombstone for `ls` history.
- `tm session delete <id> [--force]` — hard-deletes the store **record** only
  (never the workspace). **Fail-closed**: refuses a RUNNING session unless
  `--force`. Use `decommission` first if you also want the disk reclaimed.

### Bulk cleanup

```bash
tm session prune-idle [--dry-run] [--json]      # idle→stop, done→decommission (locked policy)
tm session prune --state ephemeral|stopped|decommissioned|all [--dry-run] [--include-active]
tm session decommission-ephemeral               # tear down all test/throwaway sessions
tm session prune-worktrees [--force]            # remove orphaned .worktrees/ dirs (default dry-run)
```

`prune-worktrees` defaults to a safe preview (pass `--force` to act);
`prune`/`prune-idle` act immediately unless you pass `--dry-run` yourself.
`prune --include-active` is the only explicit override to also touch RUNNING
sessions; `prune-idle`'s idle/done verdict policy inherently leaves
working/blocked sessions alone, and `prune-worktrees` only ever removes
directories that have no active session at all.

---

## 3. Health & Diagnostics

Three complementary layers — use the right one:

| Command | Layer it checks |
|---|---|
| `tm doctor` | Full trusty-mpm stack (instructions, agents, skills, memory, search) — needs a reachable daemon |
| `tm validate [--path <dir>] [--repair]` | A workspace's deployed `.claude/{agents,skills}` + `settings.json` vs the bundled roster — **standalone, no daemon** |
| `tm services status <name> [--json]` | A service daemon's **HTTP health** (is trusty-search up, on what port, healthy) |
| `tm mcp test [<name>]` | The **MCP protocol layer** — does each server actually spawn and speak MCP |

Key distinction: **`tm services` checks HTTP liveness; `tm mcp test` checks the
MCP handshake.** A daemon can be HTTP-healthy yet fail its MCP handshake (or vice
versa) — test the layer you actually care about.

```bash
tm doctor                          # full diagnostic report
tm validate --repair               # re-run the deploy pipeline to close gaps; non-zero exit if gaps remain
tm services list [--json]          # every declared service + status (exit 0)
tm services status trusty-search   # 0=running, 1=down, 2=unknown service
tm services health trusty-search   # probes /health: prints OK / FAIL
tm services port|url|log <name>    # scriptable single-value lookups
tm mcp test                        # MCP-layer sweep (see §1)
```

`tm services` exit codes are stable and scriptable: `0` ok/running, `1`
down/unhealthy, `2` unknown service.

---

## 4. Managed Auth (`CLAUDE_CODE_OAUTH_TOKEN`)

Because managed sessions run under a relocated `CLAUDE_CONFIG_DIR`, a `/login`
inside a managed session can diverge from your default-dir login (the macOS
Keychain entry is keyed by a hash of the config dir) — producing a "login
successful" → "not logged in" loop (#2246). `tm auth` manages a tm-owned token
that bypasses the Keychain entirely.

```bash
claude setup-token | tm auth set-token   # store from stdin (safer — no shell history)
tm auth set-token --token <val>          # explicit (lands in history — avoid in shared shells)
tm auth status                           # presence of stored token + env vars + on-disk creds — NEVER the value
tm auth clear-token                       # remove it (roll back to ambient auth)
```

The token is written 0600 to `~/.trusty-tools/trusty-mpm/claude-code-oauth.token`
and injected by the daemon into managed sessions. `tm auth status` never prints
the secret — safe to include in a `tm doctor`-style report.

---

## 5. Other Operator Commands

```bash
tm install [--force]               # deploy bundled framework artifacts to ~/.trusty-mpm/framework/
tm register <alias> <url> [--force]# map a repo alias → URL (no clone)
tm load <alias>                    # clone/refresh + deploy managed config
tm run <alias> [--task …]          # launch an interactive claude session for an alias
tm path <alias>                    # print the stable repo path (IDE-attach / cd)
tm update [<alias>]                # git pull --ff-only + re-deploy config (all loaded aliases if omitted)
tm rm <alias>                      # deregister + delete the alias's project dir (shared config untouched)
tm start | stop | restart | status # daemon lifecycle
tm serve --stdio                   # the MCP stdio bridge wired into .mcp.json
```

`--root <path>` on the alias commands (and `tm mcp`) switches to a standalone
managed root via the precedence chain in §1.

---

## When to load this skill

Load `/tm-cli-operations` when the task is to **set up or verify an MCP server**
for the fleet, **drive session lifecycle** beyond the MCP session tools, or
**diagnose the tm stack** at the CLI. For the PM's in-session MCP *tools*, see
`tm-tool-usage-guide`; for the delegation model, `tm-delegation-patterns`.
