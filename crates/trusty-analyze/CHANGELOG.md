# Changelog

All notable changes to trusty-analyze are documented in this file.

Format follows [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).
Versions correspond to `Cargo.toml` patch releases.

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

[Unreleased]: https://github.com/bobmatnyc/trusty-analyze/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/bobmatnyc/trusty-analyze/releases/tag/v0.1.0
