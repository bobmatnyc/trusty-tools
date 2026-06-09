# Changelog

All notable changes to trusty-analyze are documented in this file.

Format follows [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).
Versions correspond to `Cargo.toml` patch releases.

---

## [0.6.0] — 2026-06-09

### Added

- **`limit` / `offset` / `omit_content` query parameters on smells + diagnostics
  endpoints (#917/#918)** — `GET /indexes/:id/smells` and
  `GET /indexes/:id/diagnostics` now accept:
  - `limit` (default 500, max enforced server-side)
  - `offset` (default 0, for cursor-style pagination)
  - `omit_content` (default **`true`** — see Changed below)
  The response body gains a pagination envelope:
  `{ total, returned, truncated, items: [...] }`.
  `find_smells` and `run_diagnostics` MCP tool input schemas are extended
  with the same three parameters and wired through `build_query()`.

- **MCP stdio size guard (#917)** — `guard_response_size()` in `mcp/stdio.rs`
  checks the serialised response length against
  `TRUSTY_MCP_MAX_RESPONSE_BYTES` (default **2 MB**, read at startup) before
  `write_all`. Oversized payloads are replaced with a well-formed
  `isError: true` truncation notice — the JSON-RPC `id` is preserved in the
  notice so the caller can correlate it — so an over-limit response can never
  kill the MCP session with `-32000`.

- **`build_query()` numeric/bool value support** — the MCP dispatch helper now
  parses JSON `Number` (u64) and `Boolean` values, not just strings, so
  `limit`, `offset`, and `omit_content` fields from MCP tool calls are
  forwarded correctly to the HTTP layer.

### Changed

- **`omit_content` defaults to `true` (behavior-affecting default change)** —
  `SmellItem` serialisation omits the raw chunk `content` field by default.
  This reduces typical smells payloads from multi-megabyte to tens of
  kilobytes. Pass `omit_content=false` (HTTP) or `"omit_content": false` (MCP)
  to restore the full text. **Callers that previously relied on `content`
  being present in smells responses must add `omit_content=false`.**

- **`omit_content` removed from `run_diagnostics` / `DiagnosticsParams`** —
  `ToolDiagnostic` carries no raw source body, so the field was a no-op.
  Removing it prevents callers from being misled into believing it affected
  output.

### Fixed

- **#917 — over-limit smells/diagnostics responses crashed the MCP session** —
  replaced unbounded `GET /smells` serialisation with the paginated,
  omit-content-by-default path described above; the stdio size guard provides
  an additional safety net at the transport layer.

- **JSON-RPC `id` echoed in stdio size-guard truncation notice** — previously
  the notice hardcoded `"id": null`, violating JSON-RPC 2.0 §5. The guard
  now parses the id from the oversized bytes and echoes it.

---

## [0.5.1] — 2026-06-07

### Added

- **Prebuilt binary distribution via GitHub Releases** — the `trusty-analyze`
  binary is now published to GitHub Releases on every tagged version for
  `aarch64-apple-darwin`, `x86_64-unknown-linux-gnu`, and
  `x86_64-unknown-linux-gnu` (Amazon Linux 2023 `load-dynamic` variant).
  Install without Rust toolchain:
  ```
  curl -L https://github.com/bobmatnyc/trusty-tools/releases/download/trusty-analyze-v0.5.1/trusty-analyze-aarch64-apple-darwin.tar.gz | tar xz
  ```
  or via `cargo install --git`:
  ```
  cargo install --git https://github.com/bobmatnyc/trusty-tools trusty-analyze --locked
  ```
- **Cargo.toml packaging metadata** — added `exclude`, `keywords`, `categories`,
  and `[package.metadata.docs.rs]` so docs.rs renders the full API surface
  (including the `http-server` feature) and the crates.io page is correctly
  categorised.
- **Expanded `lib.rs` module-level docs** — top-level rustdoc now covers the
  analysis pipeline, transport options (HTTP API + MCP stdio/SSE), feature
  flags (`http-server`, `bundled-ort`, `load-dynamic`, `cuda`, `ner`, `review`),
  and quickstart examples.
- **CHANGELOG backfill** — all patch releases since 0.1.0 documented with
  accurate dates and descriptions.

### Changed

- **Workspace MIT relicense** — the workspace `license` field was changed from
  `Elastic-2.0` to `MIT`; `trusty-analyze` inherits `license.workspace = true`
  and is now MIT-licensed.

---

## [0.5.0] — 2026-06-03

### Added

- **redb 4.x + facts store recovery** (#702) — facts store upgraded to redb 4.x
  with graceful incompatible-file recovery: existing redb 2.x `facts.redb` is
  backed up to `facts.redb.v2-incompatible` and recreated on first start.

- **Optional `review` feature exposing trusty-review MCP tools** (#630/#631) —
  a new `review` Cargo feature wires trusty-review's MCP tool surface into the
  trusty-analyze daemon, enabling PR-review tools without a separate process.

- **On-demand SubprocessAnalyzeClient + facts store** (#632) — the analysis
  client now supports on-demand subprocess invocation for environments where a
  persistent daemon is not running.

- **Dashboard auto-start** (#684) — the web UI dashboard auto-starts on first
  daemon launch without requiring a manual invocation.

> **OPERATOR NOTE:** Existing `facts.redb` is backed up to
> `facts.redb.v2-incompatible` and recreated empty on first start after upgrade.
> No analysis history is stored in the facts store; only user-authored facts are
> affected.

---

## [0.4.2] — 2026-06-02

### Fixed
- **Amazon Linux 2023 / glibc < 2.38 build failure** (closes #605): the
  prebuilt ONNX Runtime bundled via `fastembed/ort-download-binaries` (ORT
  1.24.2, compiled against glibc 2.38) caused a link-time `__isoc23_strtol`
  unresolved-symbol error on AL2023 (glibc 2.34). The `load-dynamic` Cargo
  feature — introduced in #536 — bypasses the static bundle entirely and lets
  `ort` dlopen a system-installed `libonnxruntime.so` at runtime. README now
  documents the full three-step AL2023 installation procedure including the
  exact ORT version to download and how to set `ORT_DYLIB_PATH`.

---

## [0.4.1] — 2026-06-01

### Added
- **Load-dynamic ORT feature for glibc < 2.38** (closes #536) — a new `load-dynamic`
  Cargo feature lets `ort` dlopen a system-installed `libonnxruntime.so` instead of
  linking the bundled static library, enabling installation on Amazon Linux 2023
  (glibc 2.34) and other older-glibc hosts.

### Added
- **AWS Bedrock LLM provider for deep-analysis pass** (closes #530) — the
  `POST /analyze/deep` endpoint and `deep_analysis` MCP tool now route LLM calls
  through AWS Bedrock when `TRUSTY_LLM_MODEL` starts with `bedrock/`. Auth uses
  the standard AWS credential chain; no OpenRouter key is needed.

### Fixed
- **MCP `deep_analysis` timeout** (closes #528) — raised the timeout above
  OpenRouter's 120 s limit; improved error messaging when the API key is absent.

### Changed
- Excluded `ui/node_modules` from cargo package; fixed `.gitignore` for the
  embedded UI source tree.

---

## [0.3.0] — 2026-06-01

### Added
- **Connection-safe daemon upgrades** (closes #534) — graceful shutdown drains
  in-flight requests before exit; the `mcp_bridge` binary reconnects with
  exponential backoff after a restart. Use `launchctl bootout` (SIGTERM), not
  `kickstart -k` (SIGKILL), when upgrading.

---

## [0.2.1] — 2026-05-31

### Fixed
- Repaired `LaunchdConfig` build break introduced in 0.2.0.
- Added `reqwest` timeouts to all outbound HTTP calls to trusty-search.
- `spawn_blocking` used for neural-embedding calls to avoid blocking the async runtime.

---

## [0.2.0] — 2026-05-29

### Added
- **Update-check helper** (closes #455) — CLI notifies the user when a newer
  version of `trusty-analyze` is available on crates.io.
- **Declarative CLI help system** (closes #216) — structured `help.yaml` with
  `suggest` completing unknown subcommands; wired into all user-facing CLIs.
- **`axum` behind feature flag** (closes #249) — `axum` and `tower-http` are now
  optional behind the `http-server` feature flag, matching the convention
  established in `trusty-common`. Library consumers can drop the HTTP stack with
  `default-features = false`.
- Documentation migrated from in-crate to top-level `docs/trusty-analyze/`.

---

## [0.1.10] — 2026-05-22

### Fixed
- Routed all daemon output to stderr (MCP stdio framing requires a clean stdout).
- Resolved `list_facts` read-lock contention under concurrent MCP requests (#66, #67).

### Changed
- Included `ui/dist` and the MCP stdio harness in the release binary tarball.

---

## [0.1.6] — 2026-05-20

### Changed
- Adopted `trusty-common` `LaunchdConfig` and `claude_config` helpers in the
  service/setup module (closes #3), eliminating duplicate macOS service-install
  logic.

---

## [0.1.5] — 2026-05-20

### Changed
- Renamed crate from `trusty-analyzer` to `trusty-analyze` for consistency with
  the rest of the `trusty-*` ecosystem.

---

## [0.1.2] — 2026-05-11

### Added
- Light / dark / system theme support with Catppuccin Latte + Mocha palettes
- Svelte 5 dashboard with D3 visualizations and SSE live updates
- launchd service install/uninstall/status/logs subcommands (macOS)

### Fixed
- Dashboard now validates selected index against trusty-search index list; stale localStorage selections are cleared on refresh
- Empty-state guidance when no indexes are registered: "run trusty-search index <path>"

---

## [0.1.0] — full Phase 1 + Phase 2 static analysis engine

### Added — Phase 1 (static analysis engine, HTTP API, MCP server)

- **trusty-analyzer-core**: full analysis pipeline wired end-to-end
  - `client.rs` — reqwest HTTP client fetching `GET /indexes/:id/chunks` from trusty-search
  - `complexity.rs` — cyclomatic and cognitive complexity via tree-sitter AST walk
  - `blame.rs` — `git log --follow` parser + temporal decay scoring
  - `quality.rs` — grade aggregation (A–F) over ComplexityMetrics per file and index
  - `facts.rs` — `FactStore` backed by redb with upsert / query / delete
- **trusty-analyzer-service**: axum HTTP sidecar on port 7879
  - `GET /health` — liveness + trusty-search reachability check
  - `GET /indexes` — proxied from trusty-search
  - `GET /indexes/:id/complexity_hotspots[?top_k=N]`
  - `GET /indexes/:id/smells[?category=<name>]`
  - `GET /indexes/:id/quality`
  - `GET /facts[?subject=<s>&predicate=<p>]`
  - `POST /facts`
  - `DELETE /facts/:id`
- **trusty-analyzer-mcp**: MCP stdio server with 7 tools
  (`analyzer_health`, `complexity_hotspots`, `find_smells`, `analyze_quality`,
  `list_facts`, `upsert_fact`, `delete_fact`)
- **CLI subcommands**: `serve`, `analyze`, `facts list`, `facts upsert`, `health`
- Daemon PID lockfile (fs4), graceful shutdown, `--search-url` flag
- Integration test suite: self-analysis suite validating the static pipeline on
  own source tree

---

### Added — Phase 2 (language-specific static enrichment)

- **`LanguageAnalyzer` trait**: `detect` / `parse_static` / `enrich_semantics` lifecycle
  interface; concrete adapters plugged in without touching the orchestration layer
- **Tree-sitter adapters**: complete implementations for Python, Java, Go (complexity,
  smells, quality grade); Rust / TypeScript / C / C++ scaffolded
- **Knowledge Graph Phase 2**: CALLS edges extracted from Rust adapter via tree-sitter
  function-call pattern matching; cross-chunk entity linker resolves symbol references
  across file boundaries
- **k-means concept clustering** (bag-of-words): `linfa` k-means over TF-IDF vectors;
  `GET /indexes/:id/clusters?k=N&method=bow` endpoint
- **Neural clustering**: fastembed-backed embedding backend for `method=neural`
  clustering; uses model cached by trusty-search
- **SCIP protobuf ingest** (`#47`): `POST /indexes/:id/scip` accepts a serialized SCIP
  index protobuf; ingests occurrence → definition mappings into the knowledge graph for
  IDE-grade symbol resolution

#### New HTTP endpoints (Phase 2)

```
POST /indexes/:id/scip
     body: SCIP protobuf (application/octet-stream)
     → { symbols_ingested: N }

GET  /indexes/:id/clusters?k=N&method=bow|neural
     → Vec<ConceptCluster> (label, chunk_ids, centroid_terms)
```

#### New MCP tools (Phase 2)

| Tool | Equivalent endpoint |
|------|---------------------|
| `ingest_scip` | `POST /indexes/:id/scip` |
| `cluster_concepts` | `GET /indexes/:id/clusters` |

---

### Testing

- 107 tests passing across workspace (`cargo test --workspace`)
- Integration self-analysis suite covers HTTP API, MCP tools, SCIP ingest, clustering

---

[Unreleased]: https://github.com/bobmatnyc/trusty-tools/compare/trusty-analyze-v0.5.1...HEAD
[0.5.1]: https://github.com/bobmatnyc/trusty-tools/releases/tag/trusty-analyze-v0.5.1
[0.5.0]: https://github.com/bobmatnyc/trusty-tools/releases/tag/trusty-analyze-v0.5.0
[0.4.2]: https://github.com/bobmatnyc/trusty-tools/releases/tag/trusty-analyze-v0.4.2
[0.4.1]: https://github.com/bobmatnyc/trusty-tools/releases/tag/trusty-analyze-v0.4.1
[0.3.0]: https://github.com/bobmatnyc/trusty-tools/releases/tag/trusty-analyze-v0.3.0
[0.2.1]: https://github.com/bobmatnyc/trusty-tools/releases/tag/trusty-analyze-v0.2.1
[0.2.0]: https://github.com/bobmatnyc/trusty-tools/releases/tag/trusty-analyze-v0.2.0
[0.1.10]: https://github.com/bobmatnyc/trusty-tools/releases/tag/trusty-analyze-v0.1.10
[0.1.6]: https://github.com/bobmatnyc/trusty-tools/releases/tag/trusty-analyze-v0.1.6
[0.1.5]: https://github.com/bobmatnyc/trusty-tools/releases/tag/trusty-analyze-v0.1.5
[0.1.2]: https://github.com/bobmatnyc/trusty-tools/releases/tag/trusty-analyze-v0.1.2
[0.1.0]: https://github.com/bobmatnyc/trusty-tools/releases/tag/trusty-analyze-v0.1.0
