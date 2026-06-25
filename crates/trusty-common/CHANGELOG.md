# Changelog

All notable changes are documented in this file.

Format follows [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).

---
## [0.18.0] — 2026-06-25

### Added

- `DrawerType::Task` variant (index 5) — privileged drawer type that is exempt from
  dream-cycle eviction and semantic consolidation while `completed_at` is `None` (closes #1722)
- `Drawer::completed_at: Option<DateTime<Utc>>` field — setting this re-enables cleanup
  for Task drawers after work is finished (closes #1722)
- Serialization-safety guarantee for `DrawerType` postcard indices; backward-compat test
  asserts every variant encodes to its expected byte index (closes #1722)
- `DrawerType::Task serialization safety — fix index order and add backward-compat test (closes #1722) ([`4646c3e`](https://github.com/bobmatnyc/trusty-tools/commit/4646c3e535a1e1b67aae33ec429f7f0c860e3aca))
- chat session manager MVP — force palaces, chat-session MCP tools, room-scoped consolidation, Task drawers (closes #1700, #1701, #1702, #1703) ([#1710](https://github.com/bobmatnyc/trusty-tools/pull/1710)) ([`dcb31f7`](https://github.com/bobmatnyc/trusty-tools/commit/dcb31f7e6743dda227e79cb8d8a7116440868d10))
- pin trusty-memory palace slug in managed-session MCP injection (closes #1605) ([#1652](https://github.com/bobmatnyc/trusty-tools/pull/1652)) ([`d15c96d`](https://github.com/bobmatnyc/trusty-tools/commit/d15c96dc846e805f2ddf6549d157d2719afd4e9a))

### Fixed

- accept key=value secret-filter tokens with slash-path values (closes #1676) ([#1678](https://github.com/bobmatnyc/trusty-tools/pull/1678)) ([`b236744`](https://github.com/bobmatnyc/trusty-tools/commit/b236744ad5e4ca931f777815fb4ff41e3a6d7b7b))
- stop secret filter false-flagging path/slug technical tokens (closes #1667) ([#1669](https://github.com/bobmatnyc/trusty-tools/pull/1669)) ([`16b5eee`](https://github.com/bobmatnyc/trusty-tools/commit/16b5eeea015e143ccbeb05f9f0c9fe4224d625c6))
- pin ORT intra-op to 1 + disable spinning to break CUDA deferred-embed deadlock ([#1668](https://github.com/bobmatnyc/trusty-tools/pull/1668)) ([`1b65d16`](https://github.com/bobmatnyc/trusty-tools/commit/1b65d16e94f4a4e6020af194c87a9a4a8d45428b))

### Documentation

- correct stale SQLite references to redb in comments and README ([#1704](https://github.com/bobmatnyc/trusty-tools/pull/1704)) ([`63645b3`](https://github.com/bobmatnyc/trusty-tools/commit/63645b3d3028940299dd6f9a4b09310ac5ee5f00))
# Changelog — trusty-common

## [0.17.0] — 2026-06-17

### Added (refs #1373)

- **`index_id` module: `derive_index_id` + `resolve_project_root`.** The single
  source of truth for deriving a trusty-search index id from a project path
  (the path basename, preserved verbatim for backward-compatibility) and for
  walking up to the git root. Both trusty-search (`detect_project`, serve pin)
  and trusty-mpm (register-and-pin at session launch) call these so they cannot
  drift. Re-exported at the crate root as `trusty_common::derive_index_id` and
  `trusty_common::resolve_project_root`.

## [0.16.0] — 2026-06-16

### Changed (refs #1361, PR #1371)

- **SLD spec-resolver hardening (C4).** Strengthened the Spec-Linked
  Documentation spec resolver so traceability survives realistic doc drift:
  - **Block-scoped `# Spec References` parsing** — references are now collected
    from a delimited block rather than scanned line-by-line across the whole
    file, so stray matches outside the references block no longer pollute the
    resolved set.
  - **Revision-tolerant section matching** — section lookups tolerate revision
    suffixes / minor heading reformatting, so a spec section still resolves when
    its heading has been lightly edited.
  - **Drift flagging** — references that no longer resolve to a live spec section
    are surfaced as drift rather than silently dropped, giving CI a signal to act
    on instead of a false pass.

## [0.15.3] — 2026-06-16

### Changed (closes #1326)

- **`StdioEmbedderClient` reader: down-level benign `timed_out_id=None` timeout to `debug!`** — when the timeout fires with no in-flight request (empty pending map, a periodic idle re-arm while the embedder is healthy), the log is now `debug!` instead of `warn!`. The `warn!` path is preserved for `timed_out_id=Some(id)`, where a real in-flight request actually stalled. Eliminates ~2,800 spurious WARN lines/day during normal operation.

## [0.14.0] — 2026-06-04

### Changed (closes #753)

- **`StdioEmbedderClient` rewritten as multi-flight pipelined client** — the
  previous implementation held a single `Mutex` for the entire
  write→wait→read round-trip, allowing only one batch in flight at a time.
  The new implementation splits into: (1) a write-only stdin `Mutex` held
  only for `write_all + flush`, (2) a dedicated reader task that owns stdout
  and dispatches responses via a FIFO `VecDeque<oneshot::Sender>` (no id
  lookup needed — the sidecar never re-orders responses), and (3) an
  `inflight` semaphore capping concurrent requests at `TRUSTY_EMBED_INFLIGHT`
  (default 2, max 4). Crash/restart: EOF or IO error drains all pending
  oneshots with an error so callers return immediately.
- New env var: `TRUSTY_EMBED_INFLIGHT` — controls the semaphore depth.

## [0.13.0] — 2026-06-04

### Added (closes #747 Fix C)

- **`sidecar_batch_size` helper + `SupervisorConfig::sidecar_batch_size` field** —
  `EmbedderSupervisor` now accepts an optional resolved ONNX batch size in its
  config and forwards it as `TRUSTY_EMBED_BATCH_SIZE` to the `trusty-embedderd`
  child process at spawn time (and on crash-restart). Previously the sidecar
  always defaulted to 32 chunks per ONNX call regardless of what the parent
  daemon had computed via memory-tier autosizing, leaving significant throughput
  on the table (e.g. a Medium-tier host with CoreML computed 256 while the sidecar
  ran at 32). A CoreML memory-safety cap (`min(resolved, coreml_cap)`) is applied
  to prevent oversized unified-memory tensor allocations from triggering macOS
  jetsam SIGKILL.

## [0.12.0] — 2026-06-03

### Changed

- **redb 2.6 → 4.1 upgrade** (#702) — all stores upgraded to redb 4.x API.
  Graceful old-format recovery at every store open: existing `.redb` files
  written by redb 2.x are detected as incompatible, backed up to
  `*.v2-incompatible`, and recreated automatically. No manual intervention
  required.

- **Memory recall ranked by similarity score** (#633) — recall results are
  now sorted by embedding similarity score (descending) rather than insertion
  order, surfacing the most relevant memories first.

> **OPERATOR NOTE:** Existing palace `.redb` files are detected as incompatible
> on first open, backed up to `*.v2-incompatible`, and recreated empty.
> Re-populating palace data requires re-importing or re-creating memories.

## [0.11.1] — 2026-06-02

### Fixed

- **CUDA arena VRAM OOM prevention (issue #600)** — `embedder-cuda` builds now
  configure ORT's BFCArena with `arena_extend_strategy = kSameAsRequested` and an
  explicit `gpu_mem_limit` (default 12 GiB, tunable via `TRUSTY_GPU_MEM_LIMIT_BYTES`
  / `TRUSTY_GPU_MEM_LIMIT_MB`) so the arena no longer grows by `kNextPowerOfTwo`
  and over-reserves device VRAM. Eliminates the OOM failure on 16 GB Tesla T4 GPUs
  without requiring the `TRUSTY_MAX_BATCH_SIZE=32` workaround.

- **Accurate `/health` provider reporting (issue #604)** — the `provider` field in
  `/health` responses now reflects the actual ORT execution provider in use (e.g.
  `CUDA`, `CoreML`, `CPU`) rather than always reporting `CPU`.

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
