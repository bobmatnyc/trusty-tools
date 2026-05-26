# Changelog — trusty-embedderd

## [0.1.0] — 2026-05-26

Initial release — issue #110 Phase 1 (RPC + ship service with opt-in).

### Added

- Standalone HTTP daemon that loads `AllMiniLML6V2Q` once at startup.
- `GET /health` endpoint returning `{"status":"ok","model":"AllMiniLML6V2Q","dim":384}`.
- `POST /embed` endpoint accepting `EmbedRequest` JSON, returning `EmbedResponse` JSON.
- `--http <addr>` CLI flag (default `127.0.0.1:7890`); also configurable via `TRUSTY_EMBEDDERD_ADDR`.
- All logs to stderr (MCP policy — stdout is never written to).
- `tests/bit_identical.rs` integration test (marked `#[ignore]`): asserts that remote and in-process embedding produce bit-identical vectors for 10 fixed probe strings.
