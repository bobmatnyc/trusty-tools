# Changelog — trusty-embedderd

## [0.2.0] — 2026-05-26

### Changed

- **Dependency change**: replaced `trusty-embedder-client = { workspace = true }`
  with `trusty-common = { workspace = true, features = ["embedder-client"] }`.
  Wire types and client trait are now consumed from
  `trusty_common::embedder_client` instead of the former `trusty_embedder_client`
  crate. The `tests/bit_identical.rs` integration test updated accordingly.
  No functional change — binary behaviour and HTTP API are identical.

- **License change**: MIT → **Elastic License 2.0**, matching the rest of the
  trusty-* ecosystem. The `LICENSE` file is now the canonical Elastic-2.0 text;
  `Cargo.toml` uses `license-file = "LICENSE"`.

  Note: the `trusty-embedder-client` crate that this daemon previously depended
  on was shipped as MIT in PR #163 as a temporary state. This release completes
  the license alignment described in the PR #163 follow-up.

## [0.1.0] — 2026-05-26

Initial release — issue #110 Phase 1 (RPC + ship service with opt-in).

### Added

- Standalone HTTP daemon that loads `AllMiniLML6V2Q` once at startup.
- `GET /health` endpoint returning `{"status":"ok","model":"AllMiniLML6V2Q","dim":384}`.
- `POST /embed` endpoint accepting `EmbedRequest` JSON, returning `EmbedResponse` JSON.
- `--http <addr>` CLI flag (default `127.0.0.1:7890`); also configurable via `TRUSTY_EMBEDDERD_ADDR`.
- All logs to stderr (MCP policy — stdout is never written to).
- `tests/bit_identical.rs` integration test (marked `#[ignore]`): asserts that remote and in-process embedding produce bit-identical vectors for 10 fixed probe strings.
