# Changelog — trusty-memory

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
