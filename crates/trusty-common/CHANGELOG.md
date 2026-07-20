# Changelog

All notable changes are documented in this file.

Format follows [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).

---
## [Unreleased]

### Added

- `BM25Index::score_query_all_with_filter` — same ranking as `score_query_all`,
  but evaluates a caller-supplied `Fn(&str) -> bool` predicate on each
  candidate `doc_id` BEFORE the internal `top_k` truncation, not after
  (trusty-search issue #3401: a scope filter applied only on the already-
  truncated result set can silently drop a genuinely matching, lexically
  relevant document). Purely additive — `score_query_all`'s signature and
  behaviour are unchanged, so existing callers (`trusty-bm25-daemon`) are
  unaffected.

## [0.23.4] — 2026-07-19

### Added

- new `banner` module — `TRUSTY_SPLASH_ART` (the compact block-robot "TRUSTY" wordmark art) and `shade_bucket` (glyph → amber/rust RGB triple), extracted from `tm`'s previously binary-local splash renderer so both `tm` and `trusty-agents`' REPL can render the identical trusty branding without drifting apart again (closes [#3326](https://github.com/bobmatnyc/trusty-tools/issues/3326)). Zero extra dependencies — pure `&str` + `match`.
- register a `local`/OpenAI-compatible inference provider (Ollama by default, `http://localhost:11434/v1`) in the unified inference registry — no external credentials required (`credential_env: None`, Bedrock precedent); base URL overridable via `OLLAMA_HOST`, optional bearer credential via `TRUSTY_LOCAL_API_KEY`; slug prefixes `local/` and `ollama/` both resolve to it (closes [#3247](https://github.com/bobmatnyc/trusty-tools/issues/3247))
- add a `claude-code` → `CLAUDE_CODE_OAUTH_TOKEN` mapping to `inference::credentials::env_var_for`, so trusty-agents' `claude` CLI OAuth routing can resolve through the shared 3-tier credential resolver (part of [#3248](https://github.com/bobmatnyc/trusty-tools/issues/3248))
- **Security:** shared same-origin (CSRF) write guard behind the `axum-server`
  feature — `server::origin_guard` (`SelfOrigins`, `guard_write_origin`,
  `origin_is_loopback`, `origin_matches_self`) lifted verbatim from
  trusty-console's proven implementation (#3268/#3269/#3280), plus
  `server::with_guarded_middleware` which composes it router-wide into the
  standard middleware stack. Lets every trusty-* daemon adopt the guard with a
  one-line change instead of re-implementing it (architecture review tranche 1,
  [#3304](https://github.com/bobmatnyc/trusty-tools/issues/3304)).
- surface index readiness + working-context budget as events (UI Phase-1) ([#2861](https://github.com/bobmatnyc/trusty-tools/pull/2861)) ([`c5d75fc`](https://github.com/bobmatnyc/trusty-tools/commit/c5d75fc86259ac370a07504efd34671c70db9de7))
- adopt DOC-38 policy + sld-lint gate (closes #2853, #2854) ([#2863](https://github.com/bobmatnyc/trusty-tools/pull/2863)) ([`580c9a7`](https://github.com/bobmatnyc/trusty-tools/commit/580c9a7d08e873d9706c6b05cfe83eafb2befbfa))

### Fixed

- EmbedderSupervisor shutdown reachable + no respawn on intentional shutdown ([#3023](https://github.com/bobmatnyc/trusty-tools/pull/3023)) ([`dd5f212`](https://github.com/bobmatnyc/trusty-tools/commit/dd5f212900abff69573121e826028e941188b79a))
- embedder reader-death detection + wedged-sidecar restart ([#2978](https://github.com/bobmatnyc/trusty-tools/pull/2978)) ([`25c56d0`](https://github.com/bobmatnyc/trusty-tools/commit/25c56d0564a281e42719cdd0ea18f03099c47749))
- validate consolidation model at startup, fail loud once instead of per-cycle retry ([#2977](https://github.com/bobmatnyc/trusty-tools/pull/2977)) ([`afcbfce`](https://github.com/bobmatnyc/trusty-tools/commit/afcbfce4638e68d660c2ce21b926051da645b2ff))

### Changed

- single shared tmux library; route trusty-mpm + trusty-agents through it ([#3017](https://github.com/bobmatnyc/trusty-tools/pull/3017)) ([`383b9f4`](https://github.com/bobmatnyc/trusty-tools/commit/383b9f475e781ef6049900f1630875e8ebf68264))
- `sld` module (behind the new lightweight `sld` feature — `regex` + `serde_yaml` + `thiserror`, deliberately NOT the heavy `intent-source`/`tickets` stack): the language-agnostic **Spec-Linked Documentation (DOC-38)** reference grammar — the `SPEC-{SUBSYSTEM}-{NN}~{rev}` id grammar + §2.2 reference regex, the per-extension comment-syntax table, fenced-code-aware inline `# Spec References` block parsing, `spec_refs:` YAML-frontmatter parsing, and `{#SPEC-…}` heading-anchor scanning/resolution. Consolidates `revision_of`/`base_id` here as the single source both this grammar and `intent_source::spec_resolve` share (the `intent-source` feature now enables `sld`), so the new `trusty-sld-lint` gate and the ISR parse ONE grammar (DOC-38 §10 F1) ([#2854](https://github.com/bobmatnyc/trusty-tools/issues/2854))
### Changed

- re-cut to escape crates.io collision with PR #2209's source-deficient 0.22.0; carries PR #2221's chat-session/consolidation hardening (#1712/#1713/#1714)

### Fixed

- auto-fall back to CPU when CoreML embedder init hangs; stop leaking blocked ORT threads ([#2127](https://github.com/bobmatnyc/trusty-tools/pull/2127)) ([`f7dc2dd`](https://github.com/bobmatnyc/trusty-tools/commit/f7dc2dd20524ee9d1a9c6146245aaacc5d1e7b2b))
- reach trusty-memory over discovered JSON-RPC, never a hardcoded port ([#2040](https://github.com/bobmatnyc/trusty-tools/pull/2040)) ([`e0f41c5`](https://github.com/bobmatnyc/trusty-tools/commit/e0f41c51f1baa7ddf0e427cb5c7e86cbe9bba5fa))
- verify_installed_binary checks ~/.local/bin and $CARGO_HOME ([#2042](https://github.com/bobmatnyc/trusty-tools/pull/2042)) ([`e0d2c7b`](https://github.com/bobmatnyc/trusty-tools/commit/e0d2c7bc8dc2c06cd6a004b777454dd129dc7b5b))
## [0.19.0] — 2026-07-03

### Added

- unify session start with protected-path routing, rename sessions->session, bare tm shortcut (closes #1916) ([#1920](https://github.com/bobmatnyc/trusty-tools/pull/1920)) ([`0f40c01`](https://github.com/bobmatnyc/trusty-tools/commit/0f40c01085d15d6ec5f7f2424593640ad11da23e))
- wire trusty-mpm into console reverse proxy ([#1850](https://github.com/bobmatnyc/trusty-tools/pull/1850)) ([`970d297`](https://github.com/bobmatnyc/trusty-tools/commit/970d297bf9448cf74b3117445401524bd17b20e4))
- detach returns to tm picker + daemon/clone cwd hardening ([#1795](https://github.com/bobmatnyc/trusty-tools/pull/1795)) ([`3b0e723`](https://github.com/bobmatnyc/trusty-tools/commit/3b0e7231e85ca8fbc53dbd55bb4968d4d96e811c))

### Fixed

- decouple recall/remember from embedder warm-up (closes #1970) ([#1972](https://github.com/bobmatnyc/trusty-tools/pull/1972)) ([`bb322d4`](https://github.com/bobmatnyc/trusty-tools/commit/bb322d4678f8e167691688e77190b44d9c08627a))
- palace-level alias resolution for claude-mpm parity (owner-repo -> bare palace) ([#1945](https://github.com/bobmatnyc/trusty-tools/pull/1945)) ([`af7f904`](https://github.com/bobmatnyc/trusty-tools/commit/af7f90499402971ac65aed5b104cde251e182599))
- warn on skipped malformed claude-mpm session in catchup ([#1762](https://github.com/bobmatnyc/trusty-tools/pull/1762)) ([#1769](https://github.com/bobmatnyc/trusty-tools/pull/1769)) ([`e0b2e7c`](https://github.com/bobmatnyc/trusty-tools/commit/e0b2e7c47cc426d5dd19df37c08d54b53bd436e3))

### Changed

- extract DOC-28 catch-up engine behind catchup feature (PR1, #1762) ([`addfdbb`](https://github.com/bobmatnyc/trusty-tools/commit/addfdbb04ed78028887a0e782afe7cfe83c10b46))
## [0.18.0] — 2026-06-25

### Added

- `DrawerType::Task` variant (index 5) — privileged drawer type that is exempt from
  dream-cycle eviction and semantic consolidation while `completed_at` is `None` (closes #1722)
- `Drawer::completed_at: Option<DateTime<Utc>>` field — setting this re-enables cleanup
  for Task drawers after work is finished (closes #1722)
- Serialization-safety guarantee for `DrawerType` postcard indices; backward-compat test
  asserts every variant encodes to its expected byte index (closes #1722)
- Task drawer protection end-to-end: `DrawerType::Task.is_protected()` is exercised by
  `task_drawer_survives_dream_cycle` integration test via the `task_add`/`task_list`/
  `task_complete` MCP tools (refs #1722)
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
