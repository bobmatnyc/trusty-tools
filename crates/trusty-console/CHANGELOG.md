# Changelog — trusty-console

All notable changes to trusty-console are documented in this file.

Format follows [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).

---

## [Unreleased]

### Added

- **Known-sibling port guard extended to `trusty-mpm`'s supervisor metrics
  listener (7881) and `trusty-code`'s new default (7882) (#3364).**
  `default_port_does_not_collide_with_known_siblings` now also rejects a
  future `DEFAULT_PORT` edit that collides with either — the supervisor
  entry was previously missing from every sibling's guard table, which is
  how it silently collided with `trusty-code`'s old default.
- **trusty-agents proxy route (#3331):** `agents` is now in the reverse-proxy
  allowlist, so the trusty-agents API surface is reachable via `/api/agents/*`.
  Under the loopback-only doctrine (#3328) the agents daemon binds `127.0.0.1`
  by default, making this console proxy the intended remote path to it. A new
  `AgentsConnector` resolves the daemon's live base URL from the standard
  `http_addr` discovery file — the same mechanism the other proxied siblings use
  (`resolve_data_dir("trusty-agents")/http_addr`, gated on the `tagent` binary).
  `all_connectors()` now returns six connectors.

### Changed

- **Security (internal):** the write-origin (CSRF) guard implementation moved
  to `trusty-common` (`server::origin_guard`); `routes::origin_guard` is now a
  thin re-export so there is exactly one guard implementation shared with the
  sibling daemons. No behavioural change — the existing guard regression suite
  passes unchanged (architecture review tranche 1,
  [#3304](https://github.com/bobmatnyc/trusty-tools/issues/3304)).

### Fixed

- **Security (P1):** the write-origin (CSRF) guard is now applied
  router-wide via `Router::layer` instead of a route-scoped `route_layer`, so
  it also covers the reverse-proxied upstream daemon routes
  (`/api/{service}/{*path}`, `/proxy/{daemon}/{*path}`) — previously a
  cross-origin page could reach destructive daemon endpoints (index deletion,
  daemon shutdown) through the proxy unguarded (closes #3268).
- the same guard is now bind-aware: in Tailscale bind mode the console's own
  resolved non-loopback bind address is trusted as an additional self-origin
  (narrowly, not the whole CGNAT range), fixing 403s on the console's own
  write UI when bound on a Tailscale address (closes #3269).
- the cross-crate `default_port_does_not_collide_with_known_siblings`
  port-contract table now also tracks trusty-embedderd's `--http` mode
  default (7890) and trusty-review's corrected default (7891), closing the
  gap that let trusty-review's 7890 collide with trusty-embedderd silently
  (closes #2573).

## [0.4.0] — 2026-07-09

### Changed

- Version reconcile to match already-published crates.io state; no functional change.

## [0.3.0] — 2026-06-16

### Changed (closes part of #1318)

- **Sole binary owner.** The standalone `trusty-console` crate is now the ONLY
  producer of the `trusty-console` binary. The bundled `[[bin]]` shims were
  removed from all five host crates (`trusty-search`, `trusty-memory`,
  `trusty-analyze`, `trusty-mpm`, `trusty-review`) to fix the cargo
  `.crates2.json` binary-ownership collisions that forced `--force` on
  `cargo install` / self-`upgrade` (#1262). Install with
  `cargo install trusty-console`.
- **`run()` decoupled from global argv.** Added `run_from(argv: Vec<String>)`
  as the canonical library entry point; `run()` is now a thin wrapper that
  forwards `std::env::args().collect()`. This lets callers (and tests) drive
  the console deterministically without mutating `std::env`.

### Added (closes part of #1318)

- **`trusty-console port [--json]` verb.** Reports the console's bound (live,
  from the discovery file) or default (`7788`) HTTP port. `--json` emits the
  `{"addr":"<host>","port":<u16>}` envelope consumed by `tctl` console
  discovery (`trusty-controller`), fixing the latent bug where `tctl` spawned
  a `port --json` verb that did not exist.
