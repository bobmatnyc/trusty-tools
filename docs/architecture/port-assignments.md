# Workspace Port Assignments — trusty-tools

**Status:** Accepted
**Version:** v1
**Subsystem:** ALL (every daemon that binds a loopback HTTP port)
**Owner:** Engineering / Architecture
**Last-updated:** 2026-08-28
**Related:** [ADR-0018 — Loopback-only doctrine](../adr/0018-loopback-only-doctrine.md),
[Three-Harness Architecture](harnesses.md),
[trusty-console `service.rs`](../../crates/trusty-console/src/service.rs),
[trusty-installer `probe_http.rs`](../../crates/trusty-installer/src/commands/probe_http.rs)

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
| 7700 | `trusty-common` symgraph HTTP server (optional library feature, not a standalone daemon) | `trusty-common/src/symgraph/server.rs::DEFAULT_PORT` | No |
| 7788 | `trusty-console` | `trusty-console/src/lib.rs::DEFAULT_PORT` | Yes |
| 7878 | `trusty-search` (still bound; also serves `<data dir>/trusty-search.sock` since #6285 slice 1) | `trusty-search/src/service/constants.rs::DEFAULT_PORT` | Yes |
| 7880 | `trusty-mpm` daemon (`trusty-mpmd` / `tm`) | `trusty-mpm/src/core/discovery.rs::DEFAULT_DAEMON_ADDR` | No |
| 7882 | `trusty-code` (`tcode serve --http`) | `trusty-code/src/serve/mod.rs::DEFAULT_HTTP_PORT` (mirrored by `trusty-code-gui/src/state.rs::DEFAULT_DAEMON_URL`) | No (#3364 follow-up) |
| 7890 | `trusty-embedderd` `--http` mode (manual/dev-run only; auto-spawn always uses `--stdio`/UDS) | `trusty-embedderd/src/lib.rs::Args::http_addr` | No |
| 8080 | `trusty-agents` API server | `trusty-agents/src/runtime/mode_dispatch.rs` / `trusty-agents/src/service/mod.rs::DEFAULT_SERVICE_PORT` | No |

## Next Free Port

The next unclaimed value in the `78xx`/`79xx` block used by this workspace
is **7892**. `7891`, `7881`, `7879` and `7070` are also free again — #6277 moved
`trusty-review` off TCP onto a Unix socket, #6287 did the same for
`trusty-analyze`, #6286 for `trusty-memory`, and #6288 retired the `trusty-mpm`
supervisor's listener outright — but prefer sequential allocation over reusing a
released value, so a stale reference to any of them in an old log or script
cannot resolve to a different daemon.

`7070` is the one to be most careful about: it was the workspace's
longest-standing default, it is still named as a taken port by the
`known_siblings` guard tests in `trusty-console/src/service.rs` and
`trusty-code/src/serve/mod.rs`, and `trusty_memory::DEFAULT_HTTP_PORT` still
exists as a compile-time stub for `trusty-agents`' unmigrated REST client. None
of those bind it; all three go when that client migrates. Whatever you pick:

1. Check this table for the exact value.
2. `grep -rn "78[0-9][0-9]\|79[0-9][0-9]" --include="*.rs"` across the
   workspace as a second check — this table can drift; the grep cannot lie.
3. Add your new default to every existing `known_siblings` guard test
   (`trusty-console/src/service.rs`, `trusty-code/src/serve/mod.rs`) so
   siblings reject it if it ever collides with them in the future, and add
   your own equivalent guard test pointing back at all of the above.
   Neither `trusty-review` nor `trusty-analyze` has one — #6277 and #6287
   removed their `DEFAULT_PORT`s, and #6287 dropped `trusty-analyze`'s 7879
   row from the console's and `trusty-code`'s guard tables in the same change.
   #6288 dropped the `trusty-mpm-supervisor` 7881 row from both for the same
   reason.
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
- **#6277** — `trusty-review` left this table entirely. ADR-0032 makes UDS the
  inter-service transport and `trusty-console` the only HTTP surface, so the
  review daemon binds `<data dir>/trusty-review.sock`
  (`trusty_common::daemon_socket_path`) and has no `DEFAULT_PORT` to collide
  with anything. Its consumers — the console's `ReviewConnector` and `tctl`'s
  health probe — dial that socket, and moved in the same change: a probe left
  on 7891 would read `Refused` for a healthy daemon and kickstart it, which is
  the #4246 class.
- **#6290** — `trusty-review` stopped having a transport at all. ADR-0032's
  review lane retired the daemon: reviews run per invocation
  (`trusty-review run --json`), so there is no port, no socket and no launchd
  unit, and `com.trusty.review` is evicted by `tctl install` rather than
  installed.
  Its consumers moved with it in the same change, for the reason #6277 records
  — `tctl`'s probe and the console's `ReviewConnector` both ask presence
  (binary on PATH plus `--version`) instead of dialling. A probe left dialling
  would read `Refused`, which `is_confirmed_down` accepts, and kickstart a
  launchd label that no longer exists.
- **#6350** — `trusty-analyze` went further and stopped being resident at all.
  It has no launchd unit and no port; a client starts it through
  `trusty_common::uds::OnDemandAnalyze` and it exits after an idle window. An
  operator has nothing to run and nothing to install: neither
  `trusty-analyze serve` nor `trusty-analyze service install` (which no longer
  exists) is part of bringing the stack up. `com.trusty.analyze` is evicted by
  `tctl install` and `tctl upgrade`, through the same `RETIRED_SERVICES`
  mechanism that clears `com.trusty.review` — one eviction path, two rows.
- **#6285** — `trusty-search` has NOT left this table. It binds
  `<data dir>/trusty-search.sock` as of slice 1 and keeps 7878 bound alongside
  it, because its HTTP surface is ~35 routes with eleven consumer crates
  dialling the port. A single-PR cutover is not viable, so it migrates the way
  #6288 migrated `trusty-mpm`: the socket is added first, route families move
  onto it one slice at a time, and the retire slice deletes the axum surface,
  moves the consumers, and removes this row. Until then 7878 is still bound and
  `tctl`'s port guard is still correct to reserve it.
- **#6287** — `trusty-analyze` left this table the same way, on the same
  reasoning: it binds `<data dir>/trusty-analyze/trusty-analyze.sock` and has
  no `DEFAULT_PORT`. Four consumers dialled 7879 rather than two, so all four
  moved in the same change — the console's `AnalyzeConnector`, `tctl`'s health
  probe, `tga`'s audit guard, and `trusty-audit`'s grounding guard — and
  `trusty-analyze/tests/uds_consumer_contract.rs` stands the daemon up and asks
  each of them what it sees. The 7879 rows in `trusty-console`'s and
  `trusty-code`'s `known_siblings` guards went with it: a guard naming a port
  nothing binds refuses a value that is free.
- **#6288** — the `trusty-mpm` **supervisor** left this table. It served
  `/metrics` + `/health` on 7881 for fleet observability and nothing in the
  workspace read it: the daemon's `console_metrics` / `supervisor_status`
  rebuilt `FleetMetrics` from the session store and left `run_stats` at its
  default, so both reported zero sweeps and zero auto-resumes however long the
  supervisor had been running. It now publishes each sweep's snapshot to
  `~/.trusty-mpm/supervisor-metrics.json` and the daemon merges the real
  counters from there, with an absent, corrupt, or stale file reported as such
  rather than as a zero. The installer's #4470 foreign-port guard for this
  bootstrap went with it — a process that binds nothing cannot collide.
- **#3364** — `trusty-code`'s default HTTP port (7881) collided with
  `trusty-mpm`'s supervisor metrics listener (then `DEFAULT_METRICS_ADDR`,
  also 7881; both removed by #6288) — two defaults picked independently, on
  the same port, with no cross-crate guard catching it because neither
  `trusty-code` nor the supervisor had a `known_siblings`-style test at the
  time. The collision was masked rather than surfaced: the supervisor
  answered `/health` with a generic `{"status":"ok"}`, so both `tcode`'s GUI
  client and ops health probes got a false-healthy signal while every real
  `tcode` route 404'd.
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
- `crates/trusty-installer/src/commands/probe_http.rs` — `fixed_port_for` / `uds_socket_for` / `presence_only`: which members are dialled at all
- `crates/trusty-code/src/serve/mod.rs` — `known_siblings` guard (tcode, #3364)
- Issue #3364 — trusty-code default HTTP port collision (this table's origin)
