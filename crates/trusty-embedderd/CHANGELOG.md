# Changelog

All notable changes are documented in this file.

Format follows [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).

---
## [0.3.8] — 2026-07-13

### Fixed

- AL2023 close-out — CI gate + startup glibc probe + docs (refs #2222) ([#2525](https://github.com/bobmatnyc/trusty-tools/pull/2525)) ([`db59ebe`](https://github.com/bobmatnyc/trusty-tools/commit/db59ebeb4a4a5148f57ac7a47243247c3bd8c337))

  `bundled-ort` was hardcoded unconditionally in `[dependencies]`, so Cargo
  feature unification pulled the glibc-2.38-bound static ORT libs into any
  build regardless of `--features load-dynamic` passed to `trusty-search`.
  It is now gated behind this crate's own `bundled-ort` Cargo feature
  (still on by default — no runtime behavior change for existing installs).
  Also adds a fast startup glibc-version probe on Linux/glibc builds that
  fails loudly before the 180s ORT-init timeout when the host is below the
  glibc 2.38 floor.

### Documentation (carried from prior Unreleased entry)

- add missing package metadata to 7 crates ([#2293](https://github.com/bobmatnyc/trusty-tools/pull/2293)) ([`ee58b6a`](https://github.com/bobmatnyc/trusty-tools/commit/ee58b6a4ae01e1338e4761aaa5c27053c49f192b))

# Changelog — trusty-embedderd

## [0.3.7] — 2026-07-09

### Changed

- Add crates.io package metadata (keywords/categories/homepage/readme).

## [0.3.6] — 2026-07-08

### Changed

- re-cut to escape collision with PR #2209's 0.3.5; carries PR #2218's fail-loud ORT-init watchdog (#1633)

## [0.3.5] — 2026-07-07

### Fixed (mitigation for #1633; toolchain root cause deferred to infra decision)

- **Bounded model-init — fail loud instead of hanging forever.** The published
  binary's `FastEmbedder::new()` model-load call had no timeout at all. On
  Amazon Linux 2023 / glibc 2.34 hosts, ONNX Runtime CPU(no-arena)
  execution-provider init deadlocks in `futex_wait_queue` indefinitely (0% CPU,
  no error, no further log output) — the daemon would sit hung for hours with
  no HTTP/stdio/UDS listener ever bound, silently degrading semantic search to
  lexical-only with no signal to the operator. `run_with_args` now races the
  model load against a bounded timeout (`readiness::run_bounded`, default
  180 s, overridable via `TRUSTY_EMBEDDER_INIT_TIMEOUT_SECS`); on expiry the
  process exits nonzero with a stderr message naming issue #1633 and the
  remediation (raise the timeout, or reinstall with
  `--features embedder-load-dynamic` + `ORT_DYLIB_PATH` on AL2023/older-glibc
  hosts). Because every transport listener is only bound after model load
  succeeds, the daemon already could not report readiness while init was
  outstanding — this change makes the failure observable (bounded, loud exit)
  instead of an unbounded silent hang.
- Suspected toolchain root cause (see issue #1633 for full writeup, deferred to
  an infra/release-workflow decision): the crates.io-published default feature
  set (`embedder-bundled-ort`) links a statically-bundled ONNX Runtime built
  assuming glibc >= 2.38; AL2023 ships glibc 2.34. `cargo install` always
  builds with default features regardless of host glibc, so AL2023 users who
  `cargo install` never get the already-existing `embedder-load-dynamic` /
  AL2023 release-asset variant that the GitHub Releases build matrix produces.

## [0.3.4] — 2026-06-16

### Changed (BREAKING for the binary; closes part of #1318)

- **Library-only.** Removed the `[[bin]]` target / `src/main.rs` shim. The
  `trusty-embedderd` **binary** is now produced solely by `trusty-search` (the
  host that bundles and supervises it), eliminating the cargo `.crates2.json`
  binary-ownership collision that made `cargo install trusty-search` fail
  without `--force` (#1262). This crate is still published to crates.io as a
  **library** (dependent published crates require it; `publish = true` retained).
  Install the binary via `cargo install trusty-search`.

## [0.3.2] — 2026-06-04

### Changed (closes #753)

- **`DEFAULT_BATCH_SIZE` raised 32 → 64** — empirical sweep on M4 Max showed
  batch=64 gives the best throughput (~83 cps vs ~77 at 32) at modest extra
  RSS (369 MB vs 285 MB — safely under the CoreML tripwire ceiling). Matches
  the `DEFAULT_COREML_BATCH_SIZE` change in `trusty-search` 0.23.5.

## [Unreleased]

### Changed

- **#110 Phase 2 — `trusty-embedderd` is now a core `trusty-search` subprocess.**
  `trusty-search start` auto-spawns `trusty-embedderd --stdio` as a supervised
  child process when `TRUSTY_EMBEDDER` is unset.

  **`trusty-embedderd` is now distributed via `cargo install trusty-search`.**
  A second `[[bin]]` in `trusty-search/Cargo.toml` shims into this crate's
  library entry point, so one install command produces both binaries:
  ```bash
  cargo install trusty-search --locked
  ```
  The standalone `cargo install trusty-embedderd --locked` remains available
  for advanced users who want only the embedding daemon (e.g. trusty-memory
  consumers that do not install trusty-search).

- **Library crate** — this crate now exposes a `[lib]` target
  (`trusty_embedderd::run()`). The previous binary-only surface is still
  available; the new library surface enables zero-duplication bundling.

---

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
