# Changelog

All notable changes are documented in this file.

Format follows [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).

---

## [0.4.0] — 2026-09-19

Includes the changes published as 0.3.11, which was released without a
changelog section.

### Breaking

- **`--http` is gone, and with it the last TCP listener this daemon could open.** It defaulted to `127.0.0.1:7890`, and it defaulted to *on* — a bare `trusty-embedderd` with no flags took the port. ADR-0032 makes `trusty-console` the only HTTP surface in the workspace, and nothing in the repo dialled this one: the sole consumer was the env-gated `TRUSTY_EMBEDDER=http://…` path in trusty-search, with no plist, install script or workflow setting it. `--stdio` (the auto-spawn transport) and `--socket` (a `0600` socket in a `0700` directory, with a peer-uid check on every accept) are the two transports now, exactly one is required, and passing `--http` is refused with a message naming ADR-0032 rather than silently ignored. `axum` and `tower-http` leave the dependency graph; the `http-server` feature is renamed `daemon` and kept as a deprecated alias ([#6289](https://github.com/bobmatnyc/trusty-tools/issues/6289))
  - **What to do instead.** Replace `--http <addr>` with `--socket <path>` and
    point the client at `unix:<path>`; a client that let trusty-search spawn
    the sidecar needs no change, because that path is `--stdio`. A Cargo
    consumer naming the `http-server` feature keeps building — it forwards to
    `daemon` — but it is no longer in the default set, so a
    `default-features = false` build must name `daemon` explicitly.

### Fixed

- **The UDS listener bound a world-readable socket**
  ([#5099](https://github.com/bobmatnyc/trusty-tools/issues/5099)).
  `bind_uds_listener` called `UnixListener::bind` bare, so the socket was
  created at the process umask (`0755` under the common `022`) in a directory
  that was not narrowed either — despite ADR-0031 and ADR-0032 both resting
  their access-control argument on a `0600` socket. It now binds through
  `trusty_common::uds::bind_hardened` (`0700` directory, `0600` socket before
  the first accept), and `run_uds_accept_loop` drops any connection whose peer
  uid is not this process's own.
- Version bumped to 0.3.11: 0.3.10 is already published on crates.io and this
  PR changes `src/**`, so leaving it would turn main's version-parity workflow
  red (#4421, #3366).
- `run_uds_accept_loop` now accepts through `trusty_common::uds::accept_sized`
  instead of `UnixListener::accept`
  ([#6940](https://github.com/bobmatnyc/trusty-tools/issues/6940)). Linux builds
  the server-side AF_UNIX socket from scratch and does not copy the listener's
  `SO_SNDBUF`/`SO_RCVBUF` onto it, so the daemon was serving its multi-KiB
  embedding frames over a socket at `net.core.wmem_default` however
  `bind_uds_listener` had sized the listener. `accept_sized` shipped in
  [#6942](https://github.com/bobmatnyc/trusty-tools/pull/6942) but converted
  only trusty-common's own call sites. Throughput only; no wire or API change.

### Documentation

- Repaired every broken rustdoc intra-doc link in this crate and added
  `#![deny(rustdoc::broken_intra_doc_links)]` to its crate root(s), so a new
  one fails the build instead of shipping as dead text on docs.rs (#5744).

## [0.3.10] — 2026-07-21

### Changed

- **Report the real loaded model on startup logs + `/health` (issue #3530 —
  the `(Q)` observability bug, epic #3524 slice 6, PR 2/5)** — the
  `"loading AllMiniLML6V2Q model..."` startup log and the `/health` JSON's
  `"model": "AllMiniLML6V2Q"` field were hardcoded and went stale the moment
  `trusty-common`'s default flipped to the non-quantized fp32 model (#3486 /
  #3493 P0). Both now read the RESOLVED model name via the new
  `FastEmbedder::model_name()` (trusty-common), threaded through `AppState`
  as a new `model_name` field so `health_handler` (now
  `State`-extractor-based) can report it.

## [0.3.9] — 2026-07-20

### Changed

- Rebuild against `trusty-common` 0.23.6 to pick up the two embedding-performance
  fixes ([#3500](https://github.com/bobmatnyc/trusty-tools/pull/3500),
  [#3511](https://github.com/bobmatnyc/trusty-tools/pull/3511); refs #3486 / #3493):
  platform-conditional ORT intra-op thread default (no longer pinned to `1` off
  the CUDA path) and a non-quantized (fp32) default embedding model. This crate
  is the sidecar that actually runs inference, so the fixes are inert until the
  installed `trusty-embedderd` binary is rebuilt — no source change here beyond
  the dependency bump. `TRUSTY_ORT_INTRA_THREADS` and `TRUSTY_EMBEDDER_MODEL=int8`
  remain available to restore prior behaviour.

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
