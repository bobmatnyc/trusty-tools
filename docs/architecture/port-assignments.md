# Workspace Port Assignments — trusty-tools

**Status:** Accepted
**Version:** v1
**Subsystem:** ALL (every daemon that binds a loopback HTTP port)
**Owner:** Engineering / Architecture
**Last-updated:** 2026-07-19
**Related:** [ADR-0018 — Loopback-only doctrine](../adr/0018-loopback-only-doctrine.md),
[Three-Harness Architecture](harnesses.md),
[trusty-console `service.rs`](../../crates/trusty-console/src/service.rs),
[trusty-review `service/mod.rs`](../../crates/trusty-review/src/service/mod.rs)

---

## Purpose & Scope

Every `trusty-*` daemon in this workspace binds a fixed default loopback port
(`127.0.0.1:<port>`) so operators, CLIs, and the trusty-console gateway can
find it without a discovery file. Those defaults have been picked
independently, crate by crate, over time — and have collided in production
at least three times (#2566, #2573, #3364). This table is the single
cross-cutting inventory: **before adding a new default port anywhere in the
workspace, check this table and pick an unclaimed value.**

This doc is the source the per-crate collision-guard unit tests
(`default_port_does_not_collide_with_known_siblings` in `trusty-console` and
`trusty-review`, `default_http_port_does_not_collide_with_known_siblings` in
`trusty-code`) are meant to stay in sync with. Those tests are the
enforcement mechanism; this table is the human-readable index. If you add a
new default port, update BOTH: this table and every existing
`known_siblings` list (so the new binary's default is rejected by everyone
else's guard test too).

## Table of Contents

| Section | Topic |
|---------|-------|
| [Port Table](#port-table) | Every claimed default port, owner, and source constant |
| [Next Free Port](#next-free-port) | Guidance for picking a new default |
| [Incident History](#incident-history) | Why this table exists |
| [Non-Bind Defaults](#non-bind-defaults) | Ports that are claimed but are not workspace daemon binds |

---

## Port Table

| Port | Binary / Daemon | Constant (source of truth) | Managed by launchd? |
|------|------------------|------------------------------|----------------------|
| 7070 | `trusty-memory` | `trusty-memory/src/http_server.rs::DEFAULT_HTTP_PORT` | Yes |
| 7700 | `trusty-common` symgraph HTTP server (optional library feature, not a standalone daemon) | `trusty-common/src/symgraph/server.rs::DEFAULT_PORT` | No |
| 7788 | `trusty-console` | `trusty-console/src/lib.rs::DEFAULT_PORT` | Yes |
| 7878 | `trusty-search` | `trusty-search/src/service/constants.rs::DEFAULT_PORT` | Yes |
| 7879 | `trusty-analyze` | `trusty-analyze/src/service/events.rs::DEFAULT_PORT` | Yes |
| 7880 | `trusty-mpm` daemon (`trusty-mpmd` / `tm`) | `trusty-mpm/src/core/discovery.rs::DEFAULT_DAEMON_ADDR` | Yes |
| 7881 | `trusty-mpm` **supervisor** metrics/health listener — a distinct process from the 7880 daemon above | `trusty-mpm/src/supervisor/config.rs::DEFAULT_METRICS_ADDR` | Yes |
| 7882 | `trusty-code` (`tcode serve --http`) | `trusty-code/src/serve/mod.rs::DEFAULT_HTTP_PORT` (mirrored by `trusty-code-gui/src/state.rs::DEFAULT_DAEMON_URL`) | No (#3364 follow-up) |
| 7890 | `trusty-embedderd` `--http` mode (manual/dev-run only; auto-spawn always uses `--stdio`/UDS) | `trusty-embedderd/src/lib.rs::Args::http_addr` | No |
| 7891 | `trusty-review` | `trusty-review/src/service/mod.rs::DEFAULT_PORT` | Yes |
| 8080 | `trusty-agents` (`open-mpm`) API server | `trusty-agents/src/runtime/mode_dispatch.rs` / `trusty-agents/src/service/mod.rs::DEFAULT_SERVICE_PORT` | No |

## Next Free Port

The next unclaimed value in the `78xx`/`79xx` block used by this workspace
is **7892** (immediately after `trusty-review`'s `7891`). Prefer sequential
allocation in that block over reusing a gap, so this table stays easy to
scan. Whatever you pick:

1. Check this table for the exact value.
2. `grep -rn "78[0-9][0-9]\|79[0-9][0-9]" --include="*.rs"` across the
   workspace as a second check — this table can drift; the grep cannot lie.
3. Add your new default to every existing `known_siblings` guard test
   (`trusty-console/src/service.rs`, `trusty-review/src/service/mod.rs`,
   `trusty-code/src/serve/mod.rs`) so siblings reject it if it ever
   collides with them in the future, and add your own equivalent guard test
   pointing back at all of the above.
4. Add a row to the table above.

## Incident History

- **#2566** — `trusty-review`'s original default (7880) collided with
  `trusty-mpm`'s `DEFAULT_DAEMON_ADDR`, crash-looping trusty-review's
  launchd agent on every install (`KeepAlive::Always`, 10s throttle).
  Fixed by moving to 7890.
- **#2573** — trusty-review's follow-up default (7890) itself collided with
  `trusty-embedderd`'s `--http` mode default, which the original
  known-siblings table omitted because embedderd's HTTP listener is a
  manual/dev-run opt-in rather than a `tctl`-managed daemon. Fixed by moving
  to 7891 and extending the known-siblings table to cover manual listeners
  too, not just launchd-managed ones.
- **#3364** — `trusty-code`'s default HTTP port (7881) collided with
  `trusty-mpm`'s supervisor metrics listener (`DEFAULT_METRICS_ADDR`, also
  7881) — two defaults picked independently, on the same port, with no
  cross-crate guard catching it because neither `trusty-code` nor the
  supervisor had a `known_siblings`-style test at the time. The collision
  was masked rather than surfaced: the supervisor answers `/health` with a
  generic `{"status":"ok"}`, so both `tcode`'s GUI client and ops health
  probes got a false-healthy signal while every real `tcode` route 404'd.
  Fixed by moving `trusty-code::serve::DEFAULT_HTTP_PORT` to 7882 (in
  lockstep with `trusty-code-gui`'s hardcoded default) and adding this
  table plus a collision-guard test to `trusty-code` itself.

**Pattern across all three incidents:** a new default port was picked without
consulting a single cross-cutting inventory, and the guard tests that did
exist did not know about the new binary. This table plus the requirement to
update every `known_siblings` list together (not just add a test to the new
crate) is the fix for the pattern, not just the individual collision.

## Non-Bind Defaults

Some `127.0.0.1:<port>` constants in the workspace are **not** workspace
daemon binds and are excluded from the table above — they are compiled-in
*client* defaults pointing at services outside this repo's bind space, or
test-only sentinel addresses:

- `trusty-agents/src/memory/trusty_client/mod.rs::DEFAULT_TRUSTY_URL`
  (`127.0.0.1:7775`) — a client default for an external "trusty" memory
  service, not one of this workspace's own daemons.
- Test sentinel addresses such as `127.0.0.1:9999`, `127.0.0.1:19999`,
  `127.0.0.1:65534`/`65535` scattered across unit tests — deliberately
  unreachable/reserved addresses used to exercise error paths, not real
  defaults.

---

## References

- [ADR-0018 — Loopback-only doctrine](../adr/0018-loopback-only-doctrine.md)
- [Three-Harness Architecture](harnesses.md)
- `crates/trusty-console/src/service.rs` — `known_siblings` guard (console)
- `crates/trusty-review/src/service/mod.rs` — `known_siblings` guard (review)
- `crates/trusty-code/src/serve/mod.rs` — `known_siblings` guard (tcode, #3364)
- Issue #3364 — trusty-code default HTTP port collision (this table's origin)
