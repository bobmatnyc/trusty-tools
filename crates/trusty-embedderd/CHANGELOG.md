# Changelog — trusty-embedderd

## [0.3.0] — 2026-05-26

Issue #164 consolidation — absorbs `trusty-embed-daemon` (PR #157), completing
the three-step plan started by PR #163 (HTTP daemon) and PR #166 (moved client
into trusty-common). This release supersedes `trusty-embed-daemon` entirely;
that crate is deleted from the workspace.

### Added

- **`BatchQueue`** — ported verbatim from `trusty-embed-daemon::batch_queue`
  (issue #157). A Tokio-based coalescing queue that batches concurrent embed
  requests into single ONNX calls. Configurable via `--batch-size` (default 32)
  and `--batch-window-ms` (default 10).

- **UDS transport** — `POST /embed` HTTP requests AND JSON-RPC 2.0 UDS requests
  now both flow through the SAME `BatchQueue`. One ONNX session serves all
  transports.

- **`--socket <path>`** CLI flag — optional Unix Domain Socket listener. When
  set, `trusty-embedderd` also accepts newline-framed JSON-RPC 2.0 connections
  on that path. The wire protocol is identical to the retired
  `trusty-embed-daemon`.

- **`--batch-size <N>`** and **`--batch-window-ms <N>`** CLI flags — configure
  the `BatchQueue` coalescing window.

- **`uds_server.rs`** module — UDS accept loop, per-connection handler,
  JSON-RPC 2.0 dispatch. Unit tests for all dispatch paths.

- **`tests/concurrent_embed.rs`** — four new integration tests:
  1. `concurrent_http_requests_all_succeed` — 50 concurrent HTTP callers
  2. `concurrent_uds_requests_all_succeed` — 50 concurrent UDS callers
  3. `mixed_http_uds_concurrent_all_succeed` — 25 HTTP + 25 UDS through one queue
  4. `batch_queue_unit_collapses_concurrent_requests` — unit test for the queue

### Changed

- **HTTP `POST /embed` handler** now routes through `BatchQueue::embed_many`
  instead of calling `FastEmbedder` synchronously. Semantics are identical for
  callers; under concurrent load, requests are coalesced into batches for better
  ONNX throughput.

- **Validation**: at least one of `--http` and `--socket` must be specified;
  binary exits with an error if neither is provided.

### Notes

- The `trusty-embed-daemon` binary is deleted. Consumers that depended on that
  binary should use `trusty-embedderd --socket <path>` instead.
- The `embed-client` feature and `embed_client` module in `trusty-common` are
  deleted. Use `trusty_common::embedder_client::UdsEmbedderClient` instead.

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
