# Changelog — trusty-common

## [0.5.0] — 2026-05-26

### Added

- **`UdsEmbedderClient`** in `trusty_common::embedder_client` — a new third impl
  of the `EmbedderClient` trait that communicates with `trusty-embedderd` over a
  Unix Domain Socket using newline-framed JSON-RPC 2.0 (issue #164, Step A).
  Provides sub-millisecond in-host embedding without TCP overhead. Re-exported
  as `pub use uds::UdsEmbedderClient` from the module root.

- **`EmbedderError::Uds(String)`** variant — added to cover UDS transport
  failures (connect refused, broken pipe, decode error) distinctly from the
  existing `Transport(reqwest::Error)` HTTP variant.

### Breaking changes

- **`embed-client` feature removed** — the `embed-client` feature flag (and
  the underlying `trusty_common::embed_client` module) that provided the old
  `EmbedClient` UDS-only struct have been deleted (issue #164, Step C). The
  retired `trusty-embed-daemon` binary (PR #157) is also deleted. **Migration**:
  replace `trusty_common::embed_client::EmbedClient` with
  `trusty_common::embedder_client::UdsEmbedderClient`. The wire protocol is
  identical; the main difference is that `UdsEmbedderClient::embed_batch` now
  implements the `EmbedderClient` trait and returns `EmbedderError` instead of
  `anyhow::Error`.

### Changed

- Updated `embedder_client` module doc-comment to reflect the three-impl unified
  surface (InProcess, HTTP, UDS). Removed the "Issue #164 will reconcile" note.

## [0.4.23] — 2026-05-26

### Added

- **`embedder-client` feature** — moves the former `trusty-embedder-client` crate
  (issue #110 Phase 1) into `trusty-common` as a feature-gated module
  `trusty_common::embedder_client`. Reduces workspace crate count by one and aligns
  the client library under Elastic-2.0 licensing to match the rest of the
  trusty-* ecosystem (the originating PR #163 shipped it as MIT temporarily).

  The new module exposes:
  - `EmbedderClient` trait (async `embed_batch`)
  - `InProcessEmbedderClient` (wraps `FastEmbedder` for zero-config backwards compat)
  - `RemoteEmbedderClient` (HTTP JSON client for a running `trusty-embedderd`)
  - `EmbedRequest` / `EmbedResponse` wire types
  - `EmbedderError` (`thiserror`-derived)

  The module name is `embedder_client` (with `er`) to distinguish from the
  existing `embed_client` (UDS, PR #157). Issue #164 will reconcile the two
  embed-client modules into a unified interface.

  Enable with:
  ```toml
  trusty-common = { version = "0.4.23", features = ["embedder-client"] }
  ```
  Note: `embedder-client` implies `embedder` (and `embedder-bundled-ort` by
  extension of the embedder feature chain) because `InProcessEmbedderClient`
  wraps `FastEmbedder`. Callers that only need the remote HTTP path and wish
  to skip fastembed/ORT compilation are served by `embed-client` (UDS, #157).
  Issue #164 will provide a unified single-feature entry point.

### Changed

- No existing APIs modified. All changes are additive behind the new feature flag.
