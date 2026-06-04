# Changelog — trusty-review

All notable changes to trusty-review are documented in this file.

Format follows [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).

---

## [0.3.2] — 2026-06-03

### Added

- **Model + token-usage footer on posted reviews** (#728) — every posted PR review
  comment now includes a metadata footer with the reviewer model slug, input/output
  token counts, and cost estimate (e.g., `🤖 Reviewed by us.anthropic.claude-sonnet-4-6 · tokens ↑1234 ↓567 · est. $0.01`).
  Single-source-of-truth with the `ReviewResult` struct fields; appears identically in dry-run output.

---

## [0.3.1] — 2026-06-03

### Fixed

- **Required-dependency health gating** (#722) — `review_health` now reports
  `status: degraded` only when a *required* dependency (trusty-search) is unreachable.
  Non-required dependencies (trusty-analyze) being unavailable does NOT degrade status.
  Centralized `compute_status` logic shared by HTTP + MCP paths.

---

## [0.3.0] — 2026-06-03

### Added

- **Inference reachability probe in health endpoint** (#719) — `review_health` now
  includes an `inference` field (`ok` | `unreachable` | `auth_error` | `unknown`),
  always-on with 10s TTL and 3s timeout. Covers Bedrock + OpenRouter availability.
  When inference != ok, service status is degraded.

---

## [0.2.0] — 2026-06-03

### Added

- **Map-reduce review Phase 1: config + selector** (#690) — structured
  map-reduce review pipeline with per-file configuration and selector logic
  to route files to the appropriate review strategy.

- **Map-reduce review Phase 2: per-file diff splitter** (#698) — per-file
  diff splitter that partitions large diffs into reviewable chunks, enabling
  reviews on PRs that exceed the single-pass diff cap.

- **Auto-derive search index from repo root** (#661) — the reviewer now
  auto-detects the trusty-search index ID from the repository root path,
  eliminating manual index configuration for standard installations.

- **Daemon http_addr discovery + auto-detect** (#665/#676) — the review
  daemon discovers the trusty-search HTTP address automatically via the
  port-lock file; manual `--search-url` override still supported.

- **list_indexes envelope parse + resolve_index wiring** (#672/#670) —
  MCP tool `list_indexes` correctly parses the response envelope and
  `resolve_index` is wired into the review pipeline.

- **GitHub Issues query cap 256 chars** (#675) — GitHub Issues search
  queries are now capped at 256 characters to stay within API limits.

- **BrokenPipe on stdin treated non-fatal** (#702) — a broken pipe on
  the MCP stdio transport is treated as a clean client disconnect rather
  than a crash, improving robustness in CI and pipe-based invocations.

- **redb 4.x dedup store recovery** (#702) — the dedup claim store is
  upgraded to redb 4.x with graceful incompatible-file recovery: existing
  redb 2.x `dedup.redb` is backed up and recreated automatically.

> **OPERATOR NOTE:** Existing `dedup.redb` is backed up to
> `dedup.redb.v2-incompatible` and recreated on first start after upgrade.
> The dedup store only tracks posted reviews (SHA-keyed claim locks), not
> review history or results — no review content is lost.

---

## [0.1.0] — 2026-05-28

### Added

- Initial release: LLM-backed PR-review service with GitHub App auth,
  MCP stdio JSON-RPC service, diff analysis with noise filtering,
  context orchestration, and dedup claim store.
