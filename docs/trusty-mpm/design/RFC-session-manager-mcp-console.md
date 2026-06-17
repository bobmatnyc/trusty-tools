# RFC: Session-Manager MCP Service + trusty-console HTTP Front Door

**Status:** Accepted  
**Date:** 2026-06-15  
**Accepted:** 2026-06-15 (all open questions resolved by owner sign-off — see §6)  
**Issues:** [#1221] (MCP service), [#1222] (console front door), [#1220] (config convention)  
**Related:** [#1104] (console architecture), [#1206] (24/7 supervisor), [#1218], [#1219]  
**Author:** Bob Matsuoka

---

## 1. Problem Statement

The trusty-mpm session manager is currently a two-surface system:

| Surface | Implementation | Location |
|---|---|---|
| HTTP REST API | axum daemon, binds `:7880` (or auto-port) | `crates/trusty-mpm/src/daemon/api.rs` |
| CLI | `tm sessions …` subcommands shelling out to HTTP | `crates/trusty-mpm/src/bin/tm/commands/session.rs` |

This creates three concrete pain points:

1. **No MCP surface.** The claude-mpm driver skill (#842) scrapes `tm session` CLI text
   output. The `--json` flag referenced in the docs does not exist, causing a defect.
   Every other trusty-* tool (`trusty-search`, `trusty-memory`, `trusty-analyze`,
   `trusty-review`) exposes JSON-RPC/stdio MCP. trusty-mpm is the odd one out.

2. **HTTP violation of #1104 principle.** trusty-console's architecture directive
   (`#1104`) is: _"HTTP is implemented exactly once — in trusty-console."_ The
   session-manager daemon speaks its own HTTP on its own port, which operators
   must know about, route through, and secure independently.

3. **No standard config location.** Sessions land under `~/.trusty-mpm/workspaces/`,
   workspace roots are not derived from GitHub path, and each crate invents its own
   config convention. There is no `~/.trusty-tools/<crate>/config.yaml` standard.

---

## 2. Current Architecture (Source Evidence)

### 2.1 trusty-mpm daemon HTTP surface

`crates/trusty-mpm/src/daemon/mod.rs` — `run_http` / `serve_http` bind an axum
listener and serve `api::router(state)`. The router registers 30+ routes:

```
GET /health                          GET/POST /sessions
POST /sessions/{id}/pause            POST /sessions/{id}/resume
POST /sessions/{id}/command          GET /sessions/{id}/output
POST /hooks                          GET /events (SSE)
GET /breakers                        GET /tmux/sessions
…and 20+ more                        GET /api-docs (SwaggerUI)
```

The daemon is a durable 24/7 process: it runs the dead-session reaper loop
(`reap_loop` in `daemon/mod.rs`), the file watcher (`watcher::FileWatcher`),
and Telegram alerts. It cannot be a short-lived stdio child.

### 2.2 Existing MCP layer in trusty-mpm

`crates/trusty-mpm/src/mcp/mod.rs` — `dispatch()` and `OrchestratorBackend` trait
already exist! Nine tools are already defined in `mcp/tools.rs`:
`session_list`, `session_status`, `agent_delegate`, `memory_protect`,
`circuit_breaker_status`, `hook_event`, plus 3 bug-reporting tools.

`crates/trusty-mpm/src/daemon/mod.rs` already has `run_mcp()`:

```rust
pub async fn run_mcp(state: Arc<DaemonState>) -> anyhow::Result<()> {
    let backend = mcp_backend::StateBackend::new(state);
    trusty_common::mcp::run_stdio_loop(move |req| {
        let backend = backend.clone();
        async move { crate::mcp::dispatch(&backend, req).await }
    }).await
}
```

The MCP layer exists. What is missing is:
- A `trusty-mpm serve --stdio` CLI subcommand that acts as a **bridge** (like
  `trusty-memory serve --stdio`): connects to the running daemon over its internal
  channel and proxies JSON-RPC calls through it.
- The **session-manager tools** (lifecycle ops: `session_new`, `session_stop`,
  `session_resume`, `session_decommission`, `activity`, etc.) are not yet in the
  MCP tool catalog.
- Wiring in `.mcp.json`.

### 2.3 Reference architecture: trusty-memory `serve --stdio` bridge

`crates/trusty-memory/src/main.rs` (`run_serve_stdio`) — demonstrates the canonical
pattern:

```
[Claude Code]
    |--- JSON-RPC/stdio ---> [trusty-memory serve --stdio] (lightweight proxy)
                                     |--- HTTP POST /rpc ---> [trusty-memory HTTP daemon]
                                                                   (durable, holds redb locks)
```

Key: the stdio process is a pure **proxy**. It never touches the durable store.
It auto-starts the HTTP daemon if not running (`commands::serve_stdio_bridge`).
Every JSON-RPC request is forwarded to `POST /rpc` on the HTTP daemon.

trusty-search follows the same pattern: `trusty-search serve` (bare) runs the
MCP server, which wraps the daemon's tool set.

trusty-analyze: `trusty-analyze mcp` is the stdio MCP entry point.

### 2.4 trusty-console architecture (#1104)

`crates/trusty-console/src/server.rs` — `build_router` shows the current console
surface: it is an axum HTTP server that:
- Serves the Svelte 5 SPA at `/` and `/ui/*`.
- Exposes `GET /api/console/services` (poller-backed service card data).
- Exposes `GET /api/console/metrics/{analyze,memory,search,review}` (per-service
  metrics fetched from each service's stdio MCP via `McpServiceHandle`).
- Provides `/proxy/{daemon}/{*path}` reverse-proxy to live daemon HTTP.

`crates/trusty-console/src/mcp_handle/mod.rs` — `McpServiceHandle` is the
console's supervised stdio-MCP client, implementing:
- Lazy spawn + exponential backoff (`SpawnBackoff`).
- `poll_metrics()` → calls `console_metrics` tool on each service.
- `call_tool_raw()` / `call_tool_checked()` → capability-gated tool calls.
- Lock discipline: outer state lock released before the long `call_tool` I/O
  (fixed issue #1164).

Currently wired services: `trusty-analyze` (`mcp`), `trusty-memory`
(`serve --stdio`), `trusty-search` (`serve`), `trusty-review` (`serve --stdio`).
trusty-mpm is **absent**.

### 2.5 Workspace-root and config paths (current state)

`crates/trusty-mpm/src/core/paths.rs` — `FrameworkPaths` resolves everything under
`~/.trusty-mpm`. Session workspaces land at `~/.trusty-mpm/workspaces/<project>/<id>/`
(inferred from daemon state; no canonical constant in the code).

There is no `~/.trusty-tools/<crate>/config.yaml` convention anywhere in the
workspace today. `trusty-memory` uses its own `~/.trusty-tools/trusty-memory.yaml`
pin file convention (`crates/trusty-memory/src/project_root.rs`), but it is
bespoke to that crate.

### 2.6 In-flight: palace-ID from GitHub path (#1220 dependency)

Branch `feat/memory-palace-id-from-path` adds a helper to derive a palace slug
from a directory's GitHub remote URL (`owner/repo` → `owner-repo` or similar).
Issue #1220's workspace-root default (`~/trusty-mpm-projects/<owner>/<repo>`)
needs the same derivation. The shared helper should live in `trusty-common` so
both crates can consume it without circular deps.

---

## 3. Target Architecture

### 3.1 ASCII Diagram

```
  CLAUDE CODE SESSION (stdin/stdout MCP)
       |
       | JSON-RPC / stdio
       v
  [trusty-mpm serve --stdio]        <- lightweight stdio proxy (short-lived per session)
       |
       | internal channel (HTTP POST /rpc  OR  Unix socket)
       v
  [trusty-mpm daemon]               <- DURABLE 24/7 process
       |-- DaemonState (sessions, circuit breakers, overseer)
       |-- reap_loop (dead session GC)
       |-- FileWatcher
       |-- Telegram bot
       |-- Supervisor (issue #1206, auto-resume fleet)
       |
       | (managed sessions)
       v
  [tmux fleet / claude processes]

  BROWSER / OPERATOR
       |
       | HTTP (single front door)
       v
  [trusty-console]                  <- ONLY HTTP surface (per #1104)
       |-- Svelte 5 SPA (/ui/*)
       |-- /api/console/services
       |-- /api/console/metrics/*
       |-- /api/console/sessions/*  <- NEW (P2/P3)
       |-- /api/console/config/*    <- NEW (P4)
       |
       | McpServiceHandle (stdio) -- one per service, supervised
       |
       +-- [trusty-mpm serve --stdio]   <- spawns child, keeps pipe open
       |        (same process the CLI uses)
       |
       +-- [trusty-analyze mcp]
       +-- [trusty-memory serve --stdio]
       +-- [trusty-search serve]
       +-- [trusty-review serve --stdio]
```

### 3.2 Resolution of the Daemon-Persistence vs. HTTP-Only Tension

**The tension:** trusty-console #1104 says "HTTP lives only in the console." But
the session-manager daemon must be a durable 24/7 process (it runs the
supervisor, the reaper, Telegram alerts, the file watcher). A stdio MCP server
dies when its parent process exits, so the daemon cannot be replaced by a
simple stdio process.

**Resolution:** The tension is resolved by treating the daemon's HTTP as
**internal plumbing**, not an external operator surface:

| Layer | Protocol | Visibility |
|---|---|---|
| trusty-mpm daemon | HTTP (loopback only, auto-port) | Internal — operators never call it directly |
| trusty-mpm serve --stdio | stdio JSON-RPC | Claude Code MCP client |
| trusty-console | HTTP (user-facing) | Operators + browser |

Concretely:
1. The trusty-mpm daemon retains its loopback HTTP API. It is the only process
   that holds durable state. This is identical to how `trusty-memory` works:
   the HTTP daemon holds the redb write lock; stdio proxies forward to it.
2. `trusty-mpm serve --stdio` is a thin proxy (mirrors `trusty-memory serve --stdio`).
   It auto-starts the daemon if not running, then forwards every JSON-RPC call
   to `POST /rpc` on the daemon's loopback HTTP. It exits when the MCP client
   (Claude Code) disconnects; the daemon keeps running.
3. trusty-console's `McpServiceHandle` spawns `trusty-mpm serve --stdio` and
   keeps the pipe open. For the console's session-view HTTP routes
   (`/api/console/sessions/*`), it calls through that stdio MCP pipe — never
   directly to the daemon's HTTP port. This satisfies #1104.

**What changes for operators:** They only need to know the trusty-console URL.
The daemon's port is an implementation detail discovered by the stdio bridge at
startup (same as `trusty-memory port` / port-lock file pattern).

**The supervisor wrinkle (#1206):** The 24/7 unattended supervisor that
auto-resumes sessions must remain a daemon process. It is not affected by this
architecture because the supervisor runs inside the daemon and is managed by
launchd/systemd — not by the stdio bridge.

---

## 4. Phased Implementation Plan

### Phase Breakdown Table

| Phase | Issues | Description | Size | Backward Compat |
|---|---|---|---|---|
| P1 | #1221 | Session-manager MCP tools + `serve --stdio` bridge | M | Full — existing HTTP + CLI untouched |
| P2 | #1222 (part) | trusty-console session-manager tab (native MCP rendering) | M | Full — console additions only |
| P3 | #1222 (part) | console as single HTTP front door for session REST API | L | Additive — daemon HTTP stays for backward compat |
| P4 | #1220 | Config convention + workspace-root default + console config UI | M | Additive |

---

### P1 — MCP Tool Surface + `serve --stdio` Bridge (Issue #1221)

**Goal:** `trusty-mpm serve --stdio` exposes session-manager MCP tools wired into
`.mcp.json`. The claude-mpm driver skill calls MCP tools instead of scraping CLI output.

**Scope:**

1. **Expand the MCP tool catalog** (`crates/trusty-mpm/src/mcp/tools.rs`).
   Add the missing session-lifecycle tools to `TOOL_CATALOG` and `tool_catalog()`:

   | Tool | Description |
   |---|---|
   | `session_new` | Spawn a new managed session in a working directory |
   | `session_stop` | Stop a session (SIGTERM + pane cleanup) |
   | `session_resume` | Resume a paused session |
   | `session_decommission` | Fully remove a session and its workspace |
   | `session_activity` | Get recent tmux pane output for a session |
   | `session_send` | Send a command to a session's pane |
   | `session_answer` | Send a Y/N answer to a blocked session prompt |
   | `supervisor_status` | Return supervisor fleet state and metrics |
   | `console_metrics` | Standard `ConsoleMetricsReport` for the console poller |

   The existing 9 tools (`session_list`, `session_status`, `agent_delegate`,
   `memory_protect`, `circuit_breaker_status`, `hook_event`, plus 3 bug tools)
   are retained unchanged.

2. **Implement `OrchestratorBackend` methods** (`crates/trusty-mpm/src/daemon/mcp_backend.rs`)
   for the new tools, delegating to `SessionService` / `TmuxService` / `DaemonState`.

3. **Add `serve --stdio` subcommand** (`crates/trusty-mpm/src/bin/tm/commands/`).
   Mirror `trusty-memory serve --stdio` pattern:
   - Check if daemon is running (port-lock file / `GET /health`).
   - If not, auto-start the daemon (detached, launchd-managed).
   - Enter `run_stdio_loop` forwarding each JSON-RPC request to `POST /rpc` on
     the daemon's loopback HTTP address.
   - Map `POST /rpc` on the daemon side: add a unified RPC dispatch endpoint that
     accepts a raw JSON-RPC `Request` body and routes it through `dispatch()`.

   The daemon's `POST /rpc` endpoint is a two-liner using the existing
   `dispatch()` function and the `StateBackend` impl already in `mcp_backend.rs`.

4. **Add `console_metrics` tool implementation.** This is the standard
   `ConsoleMetricsReport` shape (`trusty_common::console_metrics`) needed by the
   console poller. Return: service id, display name, version, health status, and
   a session-fleet payload (active count, paused count, supervisor state).

5. **Wire `.mcp.json`** to add `trusty-mpm` entry:
   ```json
   "trusty-mpm": {
     "command": "trusty-mpm",
     "args": ["serve", "--stdio"]
   }
   ```

6. **Update the claude-mpm driver skill** to call `mcp__trusty-mpm__session_list`,
   `mcp__trusty-mpm__session_new`, etc. instead of shelling out to `tm session`.

**Acceptance criteria:**
- `trusty-mpm serve --stdio` starts, sends `initialize` response, lists tools via
  `tools/list`, and correctly proxies `session_list` to the running daemon.
- `cargo test -p trusty-mpm` passes; tool catalog count matches expected.
- `.mcp.json` wired; Claude Code can call `mcp__trusty-mpm__session_list`.
- Driver skill updated; no more CLI text scraping.

**Key files touched:**
- `crates/trusty-mpm/src/mcp/tools.rs` (expand catalog)
- `crates/trusty-mpm/src/mcp/mod.rs` (expand `OrchestratorBackend` + dispatch)
- `crates/trusty-mpm/src/daemon/mcp_backend.rs` (new tool impls)
- `crates/trusty-mpm/src/daemon/api.rs` (add `POST /rpc` endpoint)
- `crates/trusty-mpm/src/bin/tm/commands/` (new `serve.rs` or `serve_stdio.rs`)
- `crates/trusty-mpm/src/bin/tm/main.rs` (wire subcommand)
- `.mcp.json` (add trusty-mpm entry)

**Risks:**
- `POST /rpc` endpoint must be carefully scoped: it accepts raw JSON-RPC and
  must NOT be accessible outside loopback. The daemon already binds loopback-only
  but this should be documented.
- Session tools that involve tmux interaction (`session_activity`, `session_send`)
  may have latency; consider a configurable timeout in the MCP tool schema.
- SLOC cap: `mcp/tools.rs` will grow. Split into `mcp/tools/core.rs` and
  `mcp/tools/session.rs` if needed to stay under 500 SLOC.

**Size:** M (3–4 days)

---

### P2 — trusty-console Session-Manager Tab (Issue #1222, part 1)

**Goal:** trusty-console gains a native "Sessions" tab rendering fleet status,
lifecycle controls, activity pane, and supervisor metrics via the trusty-mpm
`McpServiceHandle` — no separate operator HTTP port needed.

**Scope:**

1. **Add `McpServiceHandle` for trusty-mpm** in `crates/trusty-console/src/server.rs`.
   Mirror the `memory_handle` / `search_handle` pattern:
   ```rust
   let mpm_handle = Arc::new(McpServiceHandle::new(
       "trusty-mpm",
       vec!["serve".to_string(), "--stdio".to_string()],
   ));
   ```
   Wire into `mcp_handles` map and `AppState`.

2. **Add a trusty-mpm metrics cache** (`mpm_metrics_cache: MetricsCache`) in
   `AppState`. Wire the background metrics poller (`metrics_poller.rs`) to call
   `mpm_handle.poll_metrics()` every 15 s.

3. **Add console route `GET /api/console/metrics/mpm`** in `server.rs` — returns
   the cached `ConsoleMetricsReport` (session counts, supervisor state, version).

4. **Add session-management routes** in `server.rs`:
   - `GET /api/console/sessions` → calls `session_list` via MCP handle.
   - `GET /api/console/sessions/{id}` → calls `session_status`.
   - `GET /api/console/sessions/{id}/activity` → calls `session_activity`.
   - `POST /api/console/sessions` → calls `session_new` (spawn).
   - `POST /api/console/sessions/{id}/stop` → calls `session_stop`.
   - `POST /api/console/sessions/{id}/resume` → calls `session_resume`.
   - `DELETE /api/console/sessions/{id}` → calls `session_decommission`.

   All of these use `mpm_handle.call_tool_checked()` — capability-gated, returns
   503+hint if the tool is absent (stale binary).

5. **Add "trusty-mpm" connector** in `crates/trusty-console/src/detect/` (new
   `mpm.rs`), mirroring `detect/analyze.rs`. Detects via `which("trusty-mpm")`.

6. **Svelte 5 SPA: Sessions tab** in `crates/trusty-console/ui/`.
   - Fleet list: session cards grouped by lifecycle state (Active / Paused /
     Stopped / Decommissioned).
   - Per-session controls: Stop / Resume / Decommission buttons (POST to console routes).
   - Activity panel: last 50 pane lines from `session_activity`.
   - Supervisor metrics widget: fleet counts + auto-resume state.
   - Poll interval: 15 s (same as other service tabs, via existing poller).

**Acceptance criteria:**
- `GET /api/console/metrics/mpm` returns live data when trusty-mpm daemon is running.
- Sessions tab renders correctly in browser.
- Stop/resume/spawn controls work end-to-end through the console → MCP bridge →
  daemon.
- `cargo test -p trusty-console` passes; new routes covered.

**Key files touched:**
- `crates/trusty-console/src/server.rs` (new routes, new handle + cache in AppState)
- `crates/trusty-console/src/metrics_poller.rs` (add mpm poll leg)
- `crates/trusty-console/src/detect/mpm.rs` (new connector)
- `crates/trusty-console/src/detect/mod.rs` (register new connector)
- `crates/trusty-console/ui/src/` (Sessions tab component)

**Risks:**
- `server.rs` is currently approaching 500 SLOC. Before adding new routes,
  extract existing metrics handlers into `src/routes/metrics.rs` to keep the
  file cap clean.
- Session activity pane content can be large; the console route should accept a
  `?lines=N` param and pass it to the MCP tool.

**Size:** M (3–5 days)

---

### P3 — Console as Single HTTP Front Door for Session REST API (Issue #1222, part 2)

**Goal:** Operators and external tools that previously called the daemon's HTTP API
(`http://127.0.0.1:<port>/sessions`) now call the console's HTTP API
(`http://127.0.0.1:<console-port>/api/console/sessions`). The console routes
added in P2 are the canonical operator surface; the daemon's own HTTP remains
as an internal detail.

**Scope:**

1. **Document deprecation of direct daemon HTTP access.** Update daemon docs and
   the README to note that `http://127.0.0.1:<daemon-port>/*` is internal
   plumbing; operators should use the console. No routes are removed yet.

2. **Add `GET /api/console/sessions/supervisor`** route (if not already in P2)
   exposing supervisor-level metrics (fleet count by state, auto-resume enabled,
   next-check time) via `supervisor_status` MCP tool.

3. **Extend console route surface** to cover less-common operations currently
   only in the daemon HTTP API:
   - `GET /api/console/sessions/events` → SSE stream of session hook events
     (proxy to daemon SSE, or re-emit from the MCP hook_event tool stream).
   - `POST /api/console/sessions/{id}/command` → `session_send`.
   - `GET /api/console/sessions/{id}/output` → `session_activity` with `?lines=N`.

4. **Evaluate SwaggerUI surface.** The daemon exposes SwaggerUI at `/api-docs`.
   Consider whether the console should expose a merged API doc covering all
   console-surfaced routes. (Out of scope for this PR but noted for the epic.)

5. **Remove `/proxy/{daemon}/*` route for `mpm`** (the reverse-proxy escape hatch
   in trusty-console currently routes `/proxy/mpm/*` directly to the daemon HTTP).
   After P3, operators use `/api/console/sessions/*` instead. Remove the proxy
   route for mpm to enforce the architecture.

**Acceptance criteria:**
- All session operations available via `http://localhost:<console-port>/api/console/sessions/*`.
- No operator workflow requires knowing the daemon's HTTP port.
- `/proxy/mpm/*` removed from console router.
- Documentation updated.

**Key files touched:**
- `crates/trusty-console/src/server.rs` (extend routes, remove mpm proxy)
- `crates/trusty-console/src/proxy/routes.rs` (remove mpm from allowed daemons)
- `docs/trusty-mpm/` (operator docs update)

**Risks:**
- SSE forwarding from the daemon to the console browser client is non-trivial
  (the console is a stdio MCP client, not an HTTP proxy for SSE). Consider whether
  the console instead maintains its own event ring from the `hook_event` MCP tool
  and exposes a console-native SSE endpoint.
- Existing integrations (Telegram bot, external scripts) that hit the daemon HTTP
  directly will need to migrate. Provide a migration guide; do not remove the
  daemon routes until a deprecation period has elapsed.

**Size:** L (5–7 days, including SSE design decision)

---

### P4 — Config Convention + Workspace-Root Default + Console Config UI (Issue #1220)

**Goal:** New sessions default to `~/trusty-mpm-projects/<owner>/<repo>/`.
`~/.trusty-tools/<crate>/config.yaml` becomes the cross-crate standard.
Console provides a configuration UI for trusty-mpm.

**Scope:**

1. **Shared GitHub-path derivation helper in `trusty-common`.**
   Reuse the logic from branch `feat/memory-palace-id-from-path`. Extract
   a `trusty_common::github_path::derive_github_path(dir: &Path) -> Option<GithubPath>`
   function that:
   - Runs `git remote get-url origin` inside `dir`.
   - Parses the URL to extract `owner` and `repo`.
   - Returns `GithubPath { owner, repo }`.
   Both trusty-memory (palace-ID) and trusty-mpm (workspace-root) consume this.

2. **Default workspace root change** in `crates/trusty-mpm/src/core/paths.rs`
   and/or `crates/trusty-mpm/src/core/session_launch/`.
   - `FrameworkPaths` gains a `workspace_root(github_path: Option<&GithubPath>) -> PathBuf`
     method: returns `~/trusty-mpm-projects/<owner>/<repo>` when a github path is
     available, falls back to `~/.trusty-mpm/workspaces/<project>` for backward compat.
   - `tm sessions new` (and the MCP `session_new` tool) derive the default workspace
     root from the target repo's GitHub remote URL.
   - CLI flag `--workspace-root` and MCP tool param `workspace_root` override the
     default.

3. **`~/.trusty-tools/<crate>/config.yaml` convention.**
   - Add `trusty_common::config::load_crate_config(crate_name: &str) -> Result<serde_yaml::Value>`
     (behind a `crate-config` feature flag). Resolves `~/.trusty-tools/<crate>/config.yaml`.
   - trusty-mpm reads `~/.trusty-tools/trusty-mpm/config.yaml` at startup for:
     `workspace_root_template`, `default_model`, `auto_resume`.
   - Document the convention for other crates (trusty-search, trusty-memory, etc.)
     so they can adopt it independently in follow-up tickets.

4. **Console config UI** (`crates/trusty-console/ui/`).
   - New "Config" tab in the SPA: renders per-service configuration.
   - trusty-mpm config section: `workspace_root_template` field (text input),
     `auto_resume` toggle, `default_model` selector.
   - Console route `GET /api/console/config/mpm` returns current config (reads via
     `config_read` MCP tool or reads the file directly).
   - Console route `POST /api/console/config/mpm` writes updated config (via
     `config_write` MCP tool or file write with validation).
   - Add `config_read` and `config_write` MCP tools to the trusty-mpm tool catalog
     (added in P1 / follow-up to P1).

**Acceptance criteria:**
- `tm sessions new` in a git repo defaults to `~/trusty-mpm-projects/bobmatnyc/trusty-tools/<id>/`.
- `~/.trusty-tools/trusty-mpm/config.yaml` is read at daemon startup;
  `workspace_root_template` overrides the default.
- Console config UI renders and saves trusty-mpm config.
- `trusty_common::github_path` is consumed by both trusty-memory (palace-ID from
  task #12) and trusty-mpm (this task).

**Key files touched:**
- `crates/trusty-common/src/` (new `github_path` module + `config` module)
- `crates/trusty-common/Cargo.toml` (new feature flags)
- `crates/trusty-mpm/src/core/paths.rs` (`workspace_root` method)
- `crates/trusty-mpm/src/core/session_launch/` (use new workspace root)
- `crates/trusty-mpm/src/mcp/tools.rs` (`config_read`, `config_write` tools)
- `crates/trusty-console/src/server.rs` (config routes)
- `crates/trusty-console/ui/src/` (Config tab)

**Risks:**
- Path migration: existing sessions under `~/.trusty-mpm/workspaces/` must still
  be discoverable. The daemon should scan both the old and new root on startup for
  a transition period.
- `serde_yaml` is not currently a workspace dependency; prefer `serde_json` with
  a `.json` config file to avoid adding a new dep, or gate the YAML support behind
  a feature flag.
- The `~/.trusty-tools/<crate>/config.yaml` convention is a cross-crate standard.
  File a child ticket per crate (trusty-search, trusty-memory, etc.) rather than
  doing all migrations in this PR.

**Size:** M (3–4 days)

---

## 5. Backward Compatibility

All phases are strictly additive with respect to the existing HTTP REST API and
CLI:

| Existing surface | Impact in P1 | Impact in P2–P4 |
|---|---|---|
| `tm sessions list/new/stop/resume` | No change | No change |
| `GET http://...:7880/sessions` | No change | Documented as internal; not removed |
| `trusty-mpm daemon` binary | `POST /rpc` endpoint added | No change |
| `.mcp.json` trusty-mpm entry | Added (new) | Extended |
| claude-mpm driver skill | Updated to use MCP tools | No change |

The daemon's HTTP API is not removed in any phase of this RFC. Removal is a
possible follow-up once the console has proven itself as the sole operator
surface and all known callers have migrated.

---

## 6. Open Questions for Bob (ALL RESOLVED 2026-06-15)

> All seven open questions were resolved by owner decision (Bob Matsuoka) on
> 2026-06-15. The resolutions below are baked into the phased plan in §4; where a
> resolution refines scope, treat §6 as authoritative. With these resolved, the
> RFC status is flipped to **Accepted** (see header).

1. **`POST /rpc` endpoint shape.** Should the daemon accept a raw JSON-RPC
   `Request` body at `POST /rpc` (mirroring `trusty-memory`'s `POST /rpc`), or
   should the stdio bridge use a different internal channel (Unix socket, named
   pipe)? HTTP `POST /rpc` is the simplest and most consistent with `trusty-memory`.

   **RESOLVED 2026-06-15 (owner decision)** — Use a raw JSON-RPC `Request` body at
   `POST /rpc`, mirroring `trusty-memory`. Simplest and most consistent with the
   existing reference pattern; no Unix-socket/named-pipe channel.

2. **Session-manager tool scope in P1.** The 9 existing tools cover orchestration
   (delegation, circuit breakers, memory protection). The new session-lifecycle
   tools (`session_new`, `session_stop`, `session_resume`, `session_decommission`,
   `session_activity`, `session_send`) more than double the catalog. Should they
   ship as one PR (P1) or as a follow-up P1b? The SLOC cap on `mcp/tools.rs`
   (currently ~254 SLOC) and `mcp/mod.rs` will need splits either way.

   **RESOLVED 2026-06-15 (owner decision)** — Ship the 6 new session-lifecycle
   tools in P1 as a single PR (not a P1b follow-up). Perform the required SLOC
   splits of `mcp/tools.rs` and `mcp/mod.rs` as part of P1.

3. **SSE streaming in P3.** The daemon exposes `GET /events` (SSE) for live hook
   events. The console (a stdio MCP client) cannot forward that SSE stream to the
   browser without an internal channel change. Options:
   - (a) Console maintains its own event ring from MCP `hook_event` calls and
     exposes a console-native SSE endpoint.
   - (b) Console's reverse-proxy route for `/proxy/mpm/events` is retained
     specifically for SSE (selective exception to the "no proxy for mpm" rule in P3).
   - (c) Defer live SSE to a later phase; poll-based refresh is good enough for P2/P3.
   Which option do you prefer?

   **RESOLVED 2026-06-15 (owner decision)** — Option (c): defer live SSE to a later
   phase; poll-based refresh is sufficient for P2/P3. Lowest risk — no proxy
   exception and no console-native event ring yet.

4. **Config file format.** YAML (`~/.trusty-tools/trusty-mpm/config.yaml` as
   specified in #1220) requires `serde_yaml`. JSON is already a workspace dep.
   Is YAML required by the spec, or is JSON + `.yaml` extension unacceptable?

   **RESOLVED 2026-06-15 (owner decision)** — YAML
   (`~/.trusty-tools/trusty-mpm/config.yaml`, per #1220). Add the `serde_yaml`
   dependency.

5. **`feat/memory-palace-id-from-path` branch timing.** The `GithubPath` helper
   for P4 should be extracted from that branch. Is task #12
   (trusty-memory palace-ID) expected to land before P4, or should we extract the
   helper in P4 independently and then reconcile?

   **RESOLVED 2026-06-15 (owner decision)** — Extract the `GithubPath` helper in P4
   independently; do NOT serialize on task #12 (trusty-memory palace-ID).
   Reconcile after.

6. **Supervisor persistence.** The 24/7 unattended supervisor (#1206) lives in the
   daemon and is managed by launchd. After P2, the console's session tab will show
   supervisor state. Should the console also provide controls to enable/disable
   auto-resume (the `TRUSTY_MPM_AUTO_RESUME` env var), or is that a CLI-only
   operator setting?

   **RESOLVED 2026-06-15 (owner decision)** — The console SHALL provide controls to
   enable/disable auto-resume (`TRUSTY_MPM_AUTO_RESUME`), consistent with the
   "console always-on" decision. Not CLI-only.

7. **Telegram bot coupling.** The Telegram bot currently calls the daemon's HTTP
   API directly (`POST /hooks`, `GET /sessions`, etc.). After P3, should the bot
   be updated to call via the console, or is direct-daemon access a justified
   exception for the bot's 24/7 alert path?

   **RESOLVED 2026-06-15 (owner decision)** — Keep the Telegram bot's direct-daemon
   HTTP access as a justified exception for its 24/7 alert path; it is NOT required
   to route via the console after P3.

---

## 7. Reference: Key Files

| File | Relevance |
|---|---|
| `crates/trusty-mpm/src/mcp/mod.rs` | Existing `dispatch()`, `OrchestratorBackend` trait |
| `crates/trusty-mpm/src/mcp/tools.rs` | Current 9-tool catalog |
| `crates/trusty-mpm/src/daemon/mod.rs` | `run_mcp()`, `run_http()`, `serve_http()` |
| `crates/trusty-mpm/src/daemon/api.rs` | HTTP router with 30+ routes |
| `crates/trusty-mpm/src/daemon/mcp_backend.rs` | `StateBackend` impl |
| `crates/trusty-mpm/src/daemon/services/session_service.rs` | Session lifecycle ops |
| `crates/trusty-mpm/src/core/paths.rs` | `FrameworkPaths`, `~/.trusty-mpm` roots |
| `crates/trusty-memory/src/main.rs` | `run_serve_stdio()` — reference for stdio bridge |
| `crates/trusty-memory/src/mcp_service.rs` | `ServiceDescriptor` impl pattern |
| `crates/trusty-console/src/server.rs` | Console router, `AppState`, metrics routes |
| `crates/trusty-console/src/mcp_handle/mod.rs` | `McpServiceHandle` — console's MCP client |
| `crates/trusty-console/src/metrics_poller.rs` | Background poll loop |
| `crates/trusty-console/src/detect/` | Per-service connector detection |
| `crates/trusty-search/src/mcp/stdio.rs` | Search stdio MCP run loop reference |
| Branch `feat/memory-palace-id-from-path` | GitHub path derivation (P4 dependency) |
