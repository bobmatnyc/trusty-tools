# Changelog — trusty-memory

## [0.15.2] — 2026-06-09

### Fixed

- **Lock TOCTOU hardening (#797)** — palace and store operations now acquire
  the advisory lock before any stat/open sequence, eliminating the window in
  which a concurrent writer could observe a partially-written file between the
  existence check and the open.

- **`libc::kill` replaces unsafe `set_var` in tests (#797)** — test helpers
  that previously used `std::env::set_var` (unsound in multi-threaded tests)
  now signal the daemon via `libc::kill`, making the test suite safe to run
  with `--test-threads > 1`. Test isolation improved.

- **Module documentation corrected (#797)** — doc comments that referenced
  internal implementation details now reflect the current architecture.

---

## [0.15.1] — 2026-06-05

### Fixed

- Minor stability fixes after the redb 4.x migration; no user-visible API changes.

---

## [0.15.0] — 2026-06-03

### Added

- **redb 4.x + graceful recovery for activity/store** (#702) — all embedded redb
  stores upgraded to redb 4.x. Existing redb 2.x activity and memory stores are
  detected as incompatible, backed up to `*.v2-incompatible`, and recreated on
  first start.

- **Dashboard auto-start** (#687) — the web UI dashboard auto-starts on first
  daemon launch without requiring a manual invocation.

- **add_alias/discover_aliases optional palace param** (#664) — the
  `add_alias` and `discover_aliases` MCP tools now accept an optional `palace`
  parameter to scope alias operations to a specific palace.

- Bundled `trusty-bm25-daemon` as a second binary target. One
  `cargo install trusty-memory` now produces three binaries:
  `trusty-memory`, `trusty-memory-mcp-bridge`, and `trusty-bm25-daemon`.
  Users who set `TRUSTY_BM25_DAEMON=1` no longer need a separate
  `cargo install trusty-bm25-daemon` step.

- `locate_bm25_daemon_binary()` in `trusty-common::bm25_client` (behind
  the `bm25-client` feature flag). Discovery order: `TRUSTY_BM25_DAEMON_BIN`
  env var, sibling of `current_exe()` (bundled-install path), then PATH.
  The `current_exe().parent()` fallback ensures the bundled-install case
  works without `~/.cargo/bin` on PATH globally.

> **OPERATOR NOTE:** Existing redb stores are backed up to `*.v2-incompatible`
> and recreated empty on first start after upgrade.
