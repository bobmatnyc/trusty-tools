Fixed

- `tga audit` now starts the `trusty-search` daemon before its sweep, ahead of the
  `trusty-analyze` preflight. `trusty-analyze serve` exits at its own trusty-search
  check, so on a machine with no search daemon the audit refused outright and the
  operator had to run `trusty-search start` by hand.
- A stale `trusty-analyze` that had been up for days on top of a dead
  trusty-search also refused every run: it answers `503 degraded` while the search
  daemon is unreachable, which the readiness probe reads as no daemon at all. The
  search guard running first recovers it.
- The daemon address comes from `trusty_common`'s shared `DaemonAddrLayout`
  resolver rather than a hard-coded `127.0.0.1:7878`, so an auto-ported or
  `TRUSTY_DATA_DIR`-isolated instance is found. The binary honours
  `TRUSTY_SEARCH_BIN`, else PATH.
