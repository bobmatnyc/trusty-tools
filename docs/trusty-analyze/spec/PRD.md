# trusty-analyze — Product Requirements Document (PRD)

> **Status:** Canonical · Living Document
> **Last reviewed:** 2026-05-29
> **Derived from:** code/docs/tickets audit

This PRD defines *what trusty-analyze is meant to be*, *what it is today*, and
*what gaps remain*. Every requirement is framed **Vision / Current / Gap** and
tagged with one of:

| Tag | Meaning |
|---|---|
| ✅ **Implemented** | Built and working today. |
| 🟡 **Partial** | Partly built; usable but incomplete or with known caveats. |
| 🔵 **Designed-not-built** | Design exists (types, scaffolding, RFC, or plan) but no working path. |
| ⚪ **Aspirational** | North-star intent; no design committed yet. |

---

## 1. Vision & Mission

### Vision

A **local code-intelligence layer** that turns a searchable code corpus into
actionable quality signal — complexity, smells, structural graph, refactor
guidance, and reviews — surfaced uniformly to LLM agents (via MCP), humans (via
CLI/dashboard), and CI (via HTTP), with no cloud round-trip and no per-project
install.

### Mission

Be the **analysis sidecar** to `trusty-search`: reuse the search daemon's
already-chunked, already-indexed corpus rather than re-walking the filesystem,
and compute every quality metric in-process so a single `cargo install
trusty-analyze` adds a complete static-analysis surface to any machine that
already runs trusty-search.

### Product north star (⚪/🔵)

The crate's `CLAUDE.md` lays out a five-phase roadmap that grows the tool from a
"complexity reporter" into a full **code-intelligence engine**: Phase 1 static
analysis (now), Phase 2 language-specific semantic enrichment (largely now via
tree-sitter adapters + SCIP), Phase 3 Dockerized sandboxed runtime execution
(🔵), Phase 4 runtime-to-graph mapping (🔵), and Phase 5 unified
text+embed+graph+runtime scoring (⚪). Phases 3–5 are documented design, not
shipped code.

---

## 2. Goals & Non-Goals

### Goals

- **G1 (✅)** Compute complexity, smells, and quality grades over a fetched chunk
  corpus, with no direct filesystem or database access.
- **G2 (✅)** Build a language-neutral knowledge graph from tree-sitter parses
  across many languages, deduplicated across overlapping chunk windows.
- **G3 (✅)** Maintain strict **HTTP↔MCP parity** — every endpoint has a tool and
  vice versa.
- **G4 (✅)** Treat trusty-search as a hard runtime dependency; fail fast and
  loudly when it is unreachable.
- **G5 (✅)** Ship a single self-contained binary (CLI + daemon + MCP + embedded
  UI), installable with `cargo install trusty-analyze`.
- **G6 (✅)** Keep stdout clean for MCP JSON-RPC framing; all logs to stderr.
- **G7 (🟡)** Provide LLM-augmented deep analysis on top of deterministic
  metrics, opt-in and isolated from the reproducible path.
- **G8 (🔵)** Add sandboxed runtime execution + runtime-to-graph mapping
  (Phases 3–4).

### Non-Goals

- **NG1** Not a search engine — it never re-implements BM25/vector/KG *search*;
  it consumes trusty-search's corpus.
- **NG2** Not a corpus owner — it does **not** read trusty-search's redb files
  directly; the only state it owns is its own facts store.
- **NG3** Not an offline tool — there is deliberately no standalone mode.
- **NG4** Not a name resolver by itself — precise cross-file symbol resolution
  is delegated to SCIP indexers; tree-sitter stays structural/heuristic.
- **NG5** `trusty-common` (the shared type crate) must never depend on
  trusty-search or trusty-analyze.

---

## 3. Personas

| Persona | Needs | Primary surface |
|---|---|---|
| **LLM coding agent** (Claude Code, Cursor) | "Where is the gnarly code? What should I refactor? Review this diff." Structured, machine-readable answers. | MCP tools (stdio) |
| **Developer at the CLI** | One-shot complexity report, diff review before pushing, a dashboard to browse hotspots. | `trusty-analyze` CLI + `/ui` dashboard |
| **CI / automation** | Quality gate on a PR diff; fetch a GitHub PR diff and post a review comment. | HTTP API + `review-pr` |
| **Platform integrator** | Wire the analyzer into an MCP host or a remote service. | MCP HTTP/SSE transport, `setup` |

---

## 4. Functional Requirements

### 4.1 Analysis Engine & Corpus Access

- **FR-1.1 (✅)** Fetch a named index's full chunk corpus from trusty-search via
  paged `GET /indexes/:id/chunks` (HTTP/2 prior-knowledge, 1000/page, 4
  concurrent pages). *(`core/client.rs`)*
- **FR-1.2 (✅)** Operate purely on `CodeChunk` wire-format types with
  forward-compatible serde (`#[serde(default)]`), so trusty-search can add fields
  without breaking the analyzer. *(`types/`)*
- **FR-1.3 (✅)** Hard startup health check against trusty-search `/health`;
  exit code 1 with a clear message if unreachable. Applies to `serve`, `review`,
  and `review-pr`. *(`main.rs`)*
- **FR-1.4 (✅)** `analyze` one-shot CLI report: quality summary + per-language
  node/edge rollup + top-N complexity hotspots. *(`main.rs::Cmd::Analyze`)*

### 4.2 Complexity Metrics

- **FR-2.1 (✅)** Cyclomatic + cognitive complexity per chunk; A–F letter grade.
  *(`types/complexity.rs::ComplexityMetrics`, `ComplexityGrade`)*
- **FR-2.2 (✅)** Language-aware dispatch: tree-sitter AST walk for Rust /
  TypeScript / JavaScript (accurate, line-anchored), with a graceful fallback to
  a language-agnostic text heuristic for everything else or on parse failure.
  *(`core/complexity.rs::compute_complexity_for`, `core/complexity_ts.rs`)*
- **FR-2.3 (✅)** Aggregate quality report over a corpus: avg cyclomatic,
  %grade-A, smell count, chunk count. *(`core/quality.rs::QualityReport`)*
- **FR-2.4 (✅)** Complexity hotspots: top-N chunks by descending cyclomatic
  complexity. *(`core/quality.rs::complexity_hotspots`)*

### 4.3 Code Smell Detection

- **FR-3.1 (✅)** Threshold-based smells: `LongFunction`, `DeepNesting`,
  `TooManyParams`, `MissingDocstring`. *(`types/complexity.rs::CodeSmell`)*
- **FR-3.2 (✅)** Smell listing endpoint/tool, optionally filtered by category.
  *(`service/mod.rs` `/indexes/:id/smells`, MCP `find_smells`)*
- **FR-3.3 (🟡)** Thresholds are compile-time constants (`LONG_FUNCTION_THRESHOLD`
  = 50, `DEEP_NESTING_THRESHOLD` = 4, `TOO_MANY_PARAMS_THRESHOLD` = 5). The
  README advertises "configurable thresholds"; runtime configuration is **not**
  yet wired. *Gap.*

### 4.4 Language & AST Support

- **FR-4.1 (✅)** `LanguageAnalyzer` plugin trait + extension-based detection.
  *(`lang/lang.rs`, `lang/detection.rs`)*
- **FR-4.2 (✅)** 14 registered tree-sitter adapters — Rust, TypeScript,
  JavaScript, Python, Java, Go, C, C++, C#, Kotlin, PHP, Ruby, Scala, Swift —
  each emitting File/Function/Method/Class/Interface/Module/Import nodes,
  `Contains`/`Imports`/`Implements`/`Calls` edges, and TestCase nodes where
  applicable. *(`lang/adapters/`, `core/registry.rs::default_registry`)*
- **FR-4.3 (✅)** Build-system + framework detection from manifest files (Cargo,
  npm, Maven/Gradle, pip, go-mod; Next.js, Django, Rails, …).
  *(`lang/detection.rs::detect_frameworks`)*
- **FR-4.4 (🟡)** The `lang/mod.rs` module doc still claims Python/Java/Go are
  "stubbed for Phase 2b". In fact all 14 adapters are fully implemented and
  registered. *Doc gap, not a code gap.*

### 4.5 Knowledge Graph

- **FR-5.1 (✅)** Language-neutral `KgGraph` of `KgNode`/`KgEdge` with node kinds
  (Repository, Package, Module, File, Class, Interface, Function, Method, Field,
  Import, Export, CallExpression, TestCase, Dependency) and edge kinds (Contains,
  Imports, Exports, Calls, Implements, Extends, References, Tests, DependsOn,
  GeneratedFrom, RuntimeObservationFor). *(`types/graph.rs`)*
- **FR-5.2 (✅)** Cross-chunk linker: merges duplicate nodes (same
  language+kind+qualified_name) emitted by overlapping chunk windows, rewires
  edges, drops self-loops. *(`core/linker.rs`)*
- **FR-5.3 (✅)** SCIP protobuf ingest → KgGraph (definitions → nodes,
  `is_implementation` → Implements edges, occurrences → References edges).
  *(`core/scip.rs`)*
- **FR-5.4 (✅)** Graph + entity endpoints/tools. *(`/indexes/:id/graph`,
  `/indexes/:id/entities`, MCP `extract_graph`, `list_entities`)*
- **FR-5.5 (🔵)** `GeneratedFrom` / `RuntimeObservationFor` edge kinds exist in
  the schema but have no producer — they await Phase 3/4 runtime execution.

### 4.6 Concept Clustering & Embeddings

- **FR-6.1 (✅)** k-means (Lloyd + k-means++) concept clustering over chunk
  embeddings; returns labelled clusters with cohesion. *(`core/concept_cluster.rs`)*
- **FR-6.2 (✅)** Two embedder backends: BoW hashed (always available,
  deterministic) and neural (fastembed all-MiniLM-L6-v2, 384-dim). Neural load
  failure degrades gracefully to BoW. *(`embedder/`, `main.rs` serve path)*
- **FR-6.3 (✅)** Clusters endpoint/tool with `k` and `method=bow|neural`.
  *(`/indexes/:id/clusters`, MCP `cluster_concepts`)*

### 4.7 Refactor, Review & LLM Deep Analysis

- **FR-7.1 (✅)** Rule-engine refactor suggestions derived from complexity +
  smells (ExtractMethod, ReduceNesting, …), severity mirroring the grade with a
  bump at 3+ smells. *(`core/refactor.rs`, MCP `suggest_refactors`)*
- **FR-7.2 (✅)** Unified-diff review: parse a git diff, cross-reference the
  index corpus, report per-file grades + recommendations; indexed files use
  stored complexity, new files fall back to local tree-sitter analysis.
  *(`core/review.rs`, CLI `review`, `/review`, MCP `review_diff`)*
- **FR-7.3 (✅)** GitHub PR review: fetch a PR diff via the REST API, run the
  review pipeline, optionally post the report back as a comment; HMAC webhook
  signature verification. *(`core/github.rs`, CLI `review-pr`, `/review/github-pr`,
  `/webhooks/github`, MCP `review_github_pr`)*
- **FR-7.4 (🟡)** LLM-augmented deep analysis: a `DeepAnalysisReport`
  (narrative + frameworks + recommendations + model) layered on top of the
  deterministic `ReviewReport` via a `ChatProvider`. Requires an
  `OPENROUTER_API_KEY` on the daemon side; returns 400 without one. Working but
  non-deterministic and opt-in. *(`core/explain.rs`, CLI `deep`, `/analyze/deep`,
  MCP `deep_analysis`)*

### 4.8 External Static-Tool Integration

- **FR-8.1 (✅)** `StaticTool` plugin trait + lazily-discovered `ToolRegistry`
  that probes installed linters once and indexes them by language.
  *(`core/tools.rs`, `core/tool_registry.rs`)*
- **FR-8.2 (✅)** 10 tool adapters: clippy (Rust), ruff (Python), biome (TS/JS),
  rubocop (Ruby), phpstan (PHP), detekt (Kotlin), pmd (Java), staticcheck (Go),
  clang-tidy (C/C++), swiftlint (Swift). Each has a 30s hard timeout and parses
  native output into `ToolDiagnostic`. *(`core/tool_impls/`)*
- **FR-8.3 (✅)** Diagnostics endpoint/tool runs available tools for an index's
  languages. *(`/indexes/:id/diagnostics`, MCP `run_diagnostics`)*

### 4.9 Facts Store

- **FR-9.1 (✅)** redb-backed `(subject, predicate, object)` triple store; the
  triple *is* the identity (re-asserting merges provenance). *(`core/facts.rs`)*
- **FR-9.2 (✅)** Stable `xxh3` fact hash (replaced the non-stable
  `DefaultHasher` to survive toolchain bumps). *(`core/facts.rs::fact_hash`,
  issue #64)*
- **FR-9.3 (✅)** List/upsert/delete via CLI, HTTP, and MCP. *(`/facts`,
  `/facts/:id`, MCP `list_facts`/`upsert_fact`/`delete_fact`)*

### 4.10 NER (optional)

- **FR-10.1 (🟡)** Optional ONNX NER over doc comments, double-gated: requires
  both `--features ner` *and* a model present at `~/.trusty-analyzer/models/ner.onnx`.
  Disabled by default; `extract()` is a no-op when off. *(`core/ner.rs`,
  `/indexes/:id/ner`, MCP `extract_ner`)*

### 4.11 MCP Surface

- **FR-11.1 (✅)** 18 MCP tools (a superset of the 9 in the README): the
  9 originals plus `run_diagnostics`, `extract_graph`, `list_entities`,
  `extract_ner`, `suggest_refactors`, `review_diff`, `review_github_pr`,
  `deep_analysis`. *(`mcp/mod.rs` dispatcher)*
- **FR-11.2 (✅)** Two transports sharing one dispatcher: stdio (subprocess) and
  HTTP/SSE (`POST /mcp`, `GET /mcp/sse`, 15s keep-alive). *(`mcp/stdio.rs`,
  `mcp/sse.rs`)*
- **FR-11.3 (✅)** MCP dispatcher is a pure JSON-RPC→HTTP translator owning only a
  `reqwest::Client` + base URL — no analysis state. *(`mcp/mod.rs`)*

### 4.12 HTTP / Daemon Surface

- **FR-12.1 (✅)** axum HTTP daemon on port 7879 (auto-detect upward if busy);
  ~20 routes including `/health`, `/indexes`, complexity/smells/quality/
  diagnostics/graph/entities/clusters/ner, scip ingest, review endpoints, facts,
  and the embedded UI. *(`service/mod.rs`)*
- **FR-12.2 (✅)** `http-server` cargo feature gates axum + tower-http + the
  `service` and `mcp::sse` modules; library consumers can drop the HTTP stack
  with `--no-default-features`. *(`Cargo.toml`, issue #249)*
- **FR-12.3 (✅)** Daemon lifecycle: `serve`, `start` (detached + PID file under
  `~/.trusty-analyze/`), `stop` (SIGTERM + port poll), `status`, `doctor`,
  plus macOS launchd `service install/uninstall/status/logs`.
  *(`commands/daemon.rs`, `commands/service.rs`)*
- **FR-12.4 (✅)** `setup` integrations for Claude Code, Cursor, claude-mpm, and
  the daemon, via `trusty_common::claude_config`. *(`commands/setup.rs`)*
- **FR-12.5 (✅)** Embedded Svelte dashboard served from `/ui` (rust-embed),
  `dashboard`/`dash` opens it; `completions` emits shell scripts.
  *(`service/ui.rs`, `main.rs`)*

### 4.13 Output & Reporting

- **FR-13.1 (✅)** All HTTP responses are JSON; reports serialize cleanly for
  machine consumption.
- **FR-13.2 (✅)** Review and deep-analysis reports render as both JSON and
  human-readable text. *(`render_review_text`, `render_deep_analysis_text`)*
- **FR-13.3 (✅)** Review-as-markdown rendering for GitHub PR comments.
  *(`core/github.rs::format_review_as_markdown`)*

---

## 5. Success Criteria

| # | Criterion | Status |
|---|---|---|
| SC-1 | `cargo install trusty-analyze` yields a working CLI + daemon + MCP server + dashboard. | ✅ |
| SC-2 | Every HTTP endpoint has an MCP tool and vice versa (parity rule). | ✅ (now 18 tools / ~20 routes) |
| SC-3 | Daemon refuses to start without trusty-search, with a clear error. | ✅ |
| SC-4 | No MCP framing corruption — stdout carries only JSON-RPC. | ✅ (fixed in #66) |
| SC-5 | Complexity numbers for Rust/TS/JS match a real AST walk, not substring counting. | ✅ |
| SC-6 | Analysis spans ≥10 languages structurally. | ✅ (14 adapters) |
| SC-7 | Diff/PR review usable as a CI quality gate. | ✅ |
| SC-8 | Workspace test suite green (`cargo test -p trusty-analyze`). | ✅ |
| SC-9 | Runtime execution (Phase 3) maps profiler data onto graph nodes. | 🔵 |

---

## 6. Open Questions & Roadmap

- **OQ-1 (🟡)** Make smell thresholds runtime-configurable (README already
  advertises this). Today they are compile-time constants.
- **OQ-2 (🟡)** Reconcile stale docs: `lang/mod.rs` adapter-status comment, the
  crate `CLAUDE.md` nested-workspace layout, and the README's 9-tool / 8-endpoint
  tables vs. the actual 18-tool / ~20-route surface (issue #430).
- **OQ-3 (🔵)** Phase 3 — Dockerized sandboxed runtime execution (per-language
  profiler images, non-root, network-isolated, resource-capped).
- **OQ-4 (🔵)** Phase 4 — normalize profiler output to the runtime-result schema
  and attach to graph nodes via `RuntimeObservationFor` / `GeneratedFrom` edges.
- **OQ-5 (⚪)** Phase 5 — unified scoring blending text + embedding + graph
  centrality + static complexity + runtime cost + error/coverage/dependency-risk.
- **OQ-6 (⚪)** Performance/quality regression baselines (the
  `regression-testing/` subdir is empty).
