# Changelog — trusty-embedder-client

## [0.1.0] — 2026-05-26

Initial release — issue #110 Phase 1 (RPC + ship service with opt-in).

### Added

- `EmbedderClient` trait with `async fn embed_batch(Vec<String>) -> Result<Vec<Vec<f32>>, EmbedderError>`.
- `InProcessEmbedderClient` — wraps `trusty_common::embedder::FastEmbedder` for backward-compatible in-process embedding.
- `RemoteEmbedderClient` — HTTP client (JSON over HTTP) for the `trusty-embedderd` standalone daemon.
- `EmbedRequest` / `EmbedResponse` — JSON wire types shared between the daemon and all consumers.
- `EmbedderError` — structured `thiserror` error enum covering model errors, transport failures, dimension mismatches, and remote error responses.
