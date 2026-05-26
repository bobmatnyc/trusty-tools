# Changelog — trusty-memory

## [Unreleased]

### Added
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
