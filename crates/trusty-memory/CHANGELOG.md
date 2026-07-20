# Changelog

All notable changes are documented in this file.

Format follows [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).

---

## [0.19.2] — 2026-07-09

### Changed

- Add crates.io package metadata (keywords/categories/homepage/readme).

## [Unreleased]

### Changed

- **Migrated the admin UI to Foundry v2 design tokens** ([#3487](https://github.com/bobmatnyc/trusty-tools/issues/3487)):
  `ui/src/lib/styles/tokens.css` now sources its palette, fonts, radii, and
  shadows from the canonical `docs/design/UI/design-system/tokens.css`
  (rust-on-paper light theme) and ships a full `[data-theme='dark']` block
  ("Night Shift") — this UI previously had no dark theme at all. Existing
  `--trusty-*` custom-property names are unchanged; several components that
  referenced tokens the old palette never actually defined
  (`--trusty-bg-subtle`, `--trusty-border-light`, `--trusty-font-mono`, a bare
  `--trusty-text`) now resolve to real values instead of silently falling
  through to their inline fallback. Dark-mode activation follows OS
  `prefers-color-scheme` via a new `lib/theme-bootstrap.js`, wired from
  `main.js` before the shell mounts.

### Security

- **Router-wide same-origin (CSRF) write guard** ([#3304](https://github.com/bobmatnyc/trusty-tools/issues/3304)):
  destructive write routes — `DELETE /api/v1/palaces/{id}` (palace deletion),
  `DELETE …/drawers/{drawer_id}` (drawer deletion), `POST /api/v1/admin/stop`,
  `POST /rpc` (the full JSON-RPC tool surface), `POST /api/v1/dream/run`, KG
  asserts/deletes — are now guarded against cross-origin browser requests via
  the shared `trusty_common::server::with_guarded_middleware`. Method-gated (GET
  reads and `/sse` unaffected) and fail-open on a missing `Origin` (the console
  proxy, the `serve --stdio` bridge, and `curl` keep working).

### Fixed

- idle-to-disk palace eviction + unpin dream scheduler + configurable max-open ([#2276](https://github.com/bobmatnyc/trusty-tools/pull/2276)) ([`0e8e504`](https://github.com/bobmatnyc/trusty-tools/commit/0e8e50440cea09a8f5eedf2c7bba9613f96cd8a8))

### Changed

- release trusty-common 0.22.2 + trusty-mpm 0.19.1 ([#2241](https://github.com/bobmatnyc/trusty-tools/pull/2241)) ([`f7ab5f4`](https://github.com/bobmatnyc/trusty-tools/commit/f7ab5f43c8a5cc41ed4d821e2a53800974e74207))
## [Unreleased]

### Fixed

- slim build (`--no-default-features`) now compiles: `tools::dream_ops` reached
  the user-config loader through the `axum-server`-gated `crate::web::` re-export,
  breaking any dependent that opts out of `axum-server` (e.g. `trusty-agents`,
  which uses `default-features = false`). Now routes through the axum-free
  `crate::service::load_user_config` like `chat_provider()` already does
  (closes #2049).
- decouple recall/remember from embedder warm-up (closes #1970) ([#1972](https://github.com/bobmatnyc/trusty-tools/pull/1972)) ([`bb322d4`](https://github.com/bobmatnyc/trusty-tools/commit/bb322d4678f8e167691688e77190b44d9c08627a))
- palace-level alias resolution for claude-mpm parity (owner-repo -> bare palace) ([#1945](https://github.com/bobmatnyc/trusty-tools/pull/1945)) ([`af7f904`](https://github.com/bobmatnyc/trusty-tools/commit/af7f90499402971ac65aed5b104cde251e182599))
- stop console_metrics force-opening every palace on poll (closes #1924) ([#1926](https://github.com/bobmatnyc/trusty-tools/pull/1926)) ([`74e9e54`](https://github.com/bobmatnyc/trusty-tools/commit/74e9e54243efc6de3778d7c43d938add2ab7b676))
## [0.17.0] — 2026-06-25

### Added

- `task_add` MCP tool — creates a `DrawerType::Task` drawer that is never evicted or
  consolidated by the dream cycle (`is_protected() = true`); bypasses content filters
  via `force=true` (spec-001 issue #1722)
- `task_list` MCP tool — returns all Task drawers in a palace; open tasks only by
  default, `include_completed=true` includes tasks with a `completed_at` timestamp
  (spec-001 issue #1722)
- `task_complete` MCP tool — sets `completed_at` on a Task drawer and persists via
  `kg.upsert_drawer`; errors if drawer does not exist or is not a Task drawer
  (spec-001 issue #1722)
- `palace_create` `force=true` flag — bypasses project-slug gate for arbitrary-slug
  palace creation (e.g. per-app/per-tenant chat session stores); slug format validation
  (`[a-z0-9][a-z0-9-]{0,62}`) still runs unconditionally (closes #1719)
- `chat_turn_append` MCP tool — appends a prompt+response pair as two messages (user
  then assistant) to an existing chat session in one call (closes #1720)
- `chat_session_recall` MCP tool — alias for `chat_session_get`; returns ordered turn
  history for a session (closes #1720)
- `chat_session_delete` MCP tool — removes a chat session by ID; idempotent for unknown
  IDs (closes #1720)
- `palace_dream` MCP tool — on-demand, room-filtered LLM compaction; gracefully returns
  a no-op result when `OPENROUTER_API_KEY` is absent (closes #1721)
- chat session manager MVP — force palaces, chat-session MCP tools, room-scoped consolidation, Task drawers (closes #1700, #1701, #1702, #1703) ([#1710](https://github.com/bobmatnyc/trusty-tools/pull/1710)) ([`dcb31f7`](https://github.com/bobmatnyc/trusty-tools/commit/dcb31f7e6743dda227e79cb8d8a7116440868d10))
- pin trusty-memory palace slug in managed-session MCP injection (closes #1605) ([#1652](https://github.com/bobmatnyc/trusty-tools/pull/1652)) ([`d15c96d`](https://github.com/bobmatnyc/trusty-tools/commit/d15c96dc846e805f2ddf6549d157d2719afd4e9a))

### Fixed

- serialize env-mutating cwd_palace_slug_at tests to stop CI flake ([#1624](https://github.com/bobmatnyc/trusty-tools/pull/1624)) ([`3660bcd`](https://github.com/bobmatnyc/trusty-tools/commit/3660bcd20ca0ff4b726fffce80b846eaa08f2afc))

### Documentation

- correct stale SQLite references to redb in comments and README ([#1704](https://github.com/bobmatnyc/trusty-tools/pull/1704)) ([`63645b3`](https://github.com/bobmatnyc/trusty-tools/commit/63645b3d3028940299dd6f9a4b09310ac5ee5f00))
# Changelog — trusty-memory

## [0.15.5] — 2026-06-16

### Changed (closes part of #1318)

- **De-bundled `trusty-console`.** Removed the bundled `trusty-console`
  `[[bin]]` shim and dependency. `cargo install trusty-memory` now produces
  `trusty-memory` and `trusty-bm25-daemon` only. The console is its own
  single-owner crate — install it with `cargo install trusty-console`. This
  resolves the cargo binary-ownership collision that forced `--force` on
  install / self-`upgrade` (#1262). `trusty-bm25-daemon` is still bundled here
  (single-owner: memory is its sole producer).

## [0.15.2] — 2026-06-09

### Fixed

- **Lock TOCTOU hardening (#797)** — palace and store operations now acquire
  the advisory lock before any stat/open sequence, eliminating the window in
  which a concurrent writer could observe a partially-written file between the
  existence check and the open.

- **`libc::kill` replaces unsafe `set_var` in tests (#797)** — test helpers
  that previously used `std::env::set_var` (unsound in multi-threaded tests)
  now signal the daemon via `libc::kill`, making the test suite safe to run
  with `--test-threads > 1`. Test isolation improved.

- **Module documentation corrected (#797)** — doc comments that referenced
  internal implementation details now reflect the current architecture.

---

## [0.15.1] — 2026-06-05

### Fixed

- Minor stability fixes after the redb 4.x migration; no user-visible API changes.

---

## [0.15.0] — 2026-06-03

### Added

- **redb 4.x + graceful recovery for activity/store** (#702) — all embedded redb
  stores upgraded to redb 4.x. Existing redb 2.x activity and memory stores are
  detected as incompatible, backed up to `*.v2-incompatible`, and recreated on
  first start.

- **Dashboard auto-start** (#687) — the web UI dashboard auto-starts on first
  daemon launch without requiring a manual invocation.

- **add_alias/discover_aliases optional palace param** (#664) — the
  `add_alias` and `discover_aliases` MCP tools now accept an optional `palace`
  parameter to scope alias operations to a specific palace.

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

> **OPERATOR NOTE:** Existing redb stores are backed up to `*.v2-incompatible`
> and recreated empty on first start after upgrade.
