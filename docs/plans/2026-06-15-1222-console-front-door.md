# Implementation Plan: #1222 — trusty-console Sessions tab (P2) + single HTTP front door (P3)

**Date:** 2026-06-15
**Branch:** `feat/1222-console-front-door`
**RFC:** `docs/trusty-mpm/design/RFC-session-manager-mcp-console.md` (§4 P2/P3, §6 Q3/Q6/Q7)

## Scope decision

- **P2 (commit 1):** Sessions tab rendered natively from MCP. Requires NEW
  trusty-mpm MCP tools the console can poll: `console_metrics`,
  `supervisor_status`, `auto_resume_set`. Console gains a trusty-mpm MCP handle,
  metrics cache + poller leg, detect connector, and session REST routes. Svelte
  Sessions tab with fleet list, controls, activity pane, supervisor widget,
  auto-resume toggle, configurable poll interval (default 5s).
- **P3 (commit 2):** The P2 routes ARE the single HTTP front door. `/proxy/mpm`
  was never in the proxy allowlist, so nothing to remove. P3 here = documentation
  (front-door operator doc + time-bounded Telegram exception note). Live SSE is
  deferred per RFC Q3 (poll-based refresh).

## Poll interval decision

Default **5s** for the Sessions tab (review flagged 15s too coarse for watching
actively-failing/auto-resuming sessions). Configurable in the UI (3s/5s/10s/30s).
The console-side background metrics poller for mpm stays on the global
`--poll-interval` (default 15s) for the supervisor/health cache; the *Sessions
tab* fetches the live session list directly at the faster 5s cadence.

## Auto-resume controls (RFC Q6)

`TRUSTY_MPM_AUTO_RESUME` is a supervisor process env var. The supervisor runs as
a separate launchd process, so the daemon cannot mutate its live env. The
console exposes a persisted **desired state** written to
`~/.trusty-mpm/auto_resume` via the `auto_resume_set` MCP tool; `supervisor_status`
reports both the persisted desired flag and (best-effort) the env-derived flag.
The supervisor reads the persisted flag on each sweep (follow-up wiring noted).
This gives the console a real, non-CLI control surface today.

## Tasks

### trusty-mpm
1. `mcp/tools/console.rs` — descriptors for `console_metrics`, `supervisor_status`,
   `auto_resume_set`. Wire into `tools/mod.rs` catalog (18 tools).
2. `mcp/mod.rs` — add 3 `OrchestratorBackend` methods + dispatch arms.
3. `daemon/mcp_console.rs` — backend impls: build `ConsoleMetricsReport` +
   `FleetMetrics` from `SessionManager::list()`, read/write persisted auto-resume.
4. `daemon/mcp_backend.rs` — delegate the 3 methods to `mcp_console`.
5. `mcp/session_dispatch.rs` or `mcp/mod.rs` — dispatch the 3 new tools.
6. `core/auto_resume.rs` — persisted desired-state helper (read/write file).
7. Tests: catalog count, dispatch arms, fleet derivation, auto-resume round-trip.

### trusty-console
8. `server.rs` — add mpm handle + `mpm_metrics_cache` to AppState; add routes:
   `GET /api/console/metrics/mpm`, `GET /api/console/sessions`,
   `GET /api/console/sessions/{id}`, `GET /api/console/sessions/{id}/activity`,
   `POST /api/console/sessions`, `POST /api/console/sessions/{id}/stop`,
   `POST /api/console/sessions/{id}/resume`, `DELETE /api/console/sessions/{id}`,
   `GET /api/console/sessions/supervisor`,
   `POST /api/console/sessions/supervisor/auto-resume`.
   Extract session route handlers into `src/routes/sessions.rs` to keep
   `server.rs` under the 500-SLOC cap.
9. `detect/mpm.rs` + `detect/mod.rs` — trusty-mpm connector.
10. `lib.rs` — start mpm metrics poller leg.
11. Tests: cold-cache 503, absent-binary 503, route wiring.

### UI
12. `ui/src/SessionsTab.svelte` — fleet list grouped by state, controls,
    activity pane, supervisor widget, auto-resume toggle, poll-interval selector.
13. `ui/src/App.svelte` — register Sessions tab + SERVICE_TAB_MAP entry.
14. `pnpm build` to refresh `ui/dist`.

### docs
15. `docs/trusty-mpm/console-front-door.md` — operator doc; single HTTP front
    door; time-bounded Telegram exception note.

## Quality bar
`cargo fmt`; `cargo clippy -p trusty-console -p trusty-mpm --all-targets -- -D warnings`;
`cargo test -p trusty-console -p trusty-mpm`; `SKIP_UI_BUILD=1 cargo check`;
`bash scripts/check_line_cap.sh`.
