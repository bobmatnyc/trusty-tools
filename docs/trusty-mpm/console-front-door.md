# trusty-console: the single HTTP front door for the session manager

**Status:** Active (P2 + P3 of [RFC-session-manager-mcp-console](design/RFC-session-manager-mcp-console.md))
**Issues:** #1222 (P2 Sessions tab + P3 front door), #1221 (MCP service), #1104 (console architecture)

## Principle (#1104)

HTTP is implemented exactly once — in **trusty-console**. Backing services speak
stdio MCP. The console is a stdio MCP client that renders natively (no iframes,
no service-rendered HTML, no daemon-HTTP calls from the browser).

For the session manager this means:

| Layer | Protocol | Visibility |
|---|---|---|
| trusty-mpm daemon | HTTP (loopback only, auto-port) | **Internal plumbing** — operators never call it directly |
| `trusty-mpm serve --stdio` | stdio JSON-RPC | Claude Code / console MCP client |
| trusty-console | HTTP (user-facing) | **The single operator/browser surface** |

The trusty-mpm daemon remains a durable 24/7 process (it runs the supervisor,
the reaper, the file watcher, and the Telegram bot). It keeps a loopback HTTP API
as an internal detail; the `serve --stdio` bridge forwards JSON-RPC to it. The
console spawns that bridge and renders the Sessions tab from it — it never opens
the daemon's HTTP port.

## Operator surface — `/api/console/sessions/*`

All session operations are available through the console's HTTP API. Operators
only need to know the **console** URL; the daemon's port is discovered by the
stdio bridge from `~/.trusty-mpm/daemon.lock`.

| Method | Console route | MCP tool | Purpose |
|---|---|---|---|
| GET | `/api/console/sessions` | `session_list` | Fleet list |
| GET | `/api/console/sessions/{id}` | `session_status` | Session detail |
| GET | `/api/console/sessions/{id}/activity?lines=N` | `session_activity` | Recent pane (default 60) |
| GET | `/api/console/sessions/supervisor` | `supervisor_status` | Fleet counts + auto-resume state |
| POST | `/api/console/sessions` | `session_new` | Spawn (body: `repo_url`, `ref`, `task`, opt `name_hint`/`runtime`) |
| POST | `/api/console/sessions/{id}/stop` | `session_stop` | Stop |
| POST | `/api/console/sessions/{id}/resume` | `session_resume` | Resume |
| DELETE | `/api/console/sessions/{id}` | `session_decommission` | Terminal teardown |
| POST | `/api/console/sessions/supervisor/auto-resume` | `auto_resume_set` | Toggle auto-resume (body: `enabled`) |
| GET | `/api/console/metrics/mpm` | `console_metrics` | Coarse health/fleet cache (background poll) |

Each route is **capability-gated**: when the trusty-mpm binary is absent, in
backoff, or a tool is missing from a stale daemon, the route returns `503` with
an actionable hint rather than leaking a raw JSON-RPC error or a `502`.

## Sessions tab (P2)

The console's **Sessions** tab renders the fleet grouped by lifecycle state
(active / provisioning / stopped / errored / decommissioned) with per-session
Stop / Resume / Decommission / Activity controls, a spawn form, and a supervisor
widget.

### Poll-based refresh (RFC Q3)

Live SSE is deferred (RFC §6 Q3); the tab uses **poll-based refresh**. The RFC's
implied 15 s default was flagged as too coarse for watching an actively-failing
or auto-resuming session, so the Sessions tab defaults to a **5-second** poll of
the live `/api/console/sessions` list and is **configurable in the UI**
(3 s / 5 s / 10 s / 30 s). The coarse `/api/console/metrics/mpm` health cache
continues to refresh on the console's global `--poll-interval` (default 15 s).

### Auto-resume controls (RFC Q6)

The console SHALL provide controls to enable/disable supervisor auto-resume — it
is **not** CLI-only. Because the supervisor is a separate launchd-managed process
that reads `TRUSTY_MPM_AUTO_RESUME` at boot, the console toggle persists the
operator's **desired** flag to `~/.trusty-mpm/auto_resume` via the
`auto_resume_set` MCP tool. `supervisor_status` reports both the persisted desired
flag and the supervisor's boot-time env flag; when they differ the widget shows
"restart pending" so the operator knows a supervisor restart is required for the
change to take full effect.

## Telegram bot — time-bounded direct-daemon exception (RFC Q7)

The Telegram bot currently calls the daemon's HTTP API directly
(`POST /hooks`, `GET /sessions`, …) for its 24/7 alert path. This is a
**justified, TIME-BOUNDED exception** to the HTTP-only principle:

- The exception exists only for the bot's existing alert path.
- It is **reviewed at the end of the P3 deprecation period**; the bot is expected
  to migrate to the console surface (or an internal MCP path) at that review.
- **New integrations MUST use the console surface** (`/api/console/sessions/*`),
  not the daemon's HTTP port — so this exception does not silently become the norm.

## Backward compatibility

P2/P3 are strictly additive. The daemon's HTTP REST API and the `tm session …`
CLI are unchanged; the daemon HTTP is documented as internal plumbing but **not
removed**. Removal is a possible follow-up once all known callers have migrated.
