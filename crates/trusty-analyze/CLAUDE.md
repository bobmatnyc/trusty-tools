# trusty-analyze

Sidecar code-analysis daemon for trusty-search. Reads chunk corpora from
trusty-search via HTTP, runs static analysis, and exposes results on port 7879
via HTTP API and MCP stdio server.

> **Coordination:** Shared library patterns, consistent conventions, and CI/CD configuration for this project are managed by [trusty-common](../trusty-common). See that repo's CLAUDE.md for cross-project guidelines.

## Project History

trusty-analyze is the third generation of code analysis tooling in this lineage.
Understanding the lineage helps clarify what to preserve, what to discard, and why
certain design decisions were made.

### Generation 1: mcp-vector-search (Python, per-project)

- Located at `../mcp-vector-search`
- Python 3.11+, LanceDB vector store, KuzuDB knowledge graph, sentence-transformers
- Per-project deployment: `mcp-vector-search setup` run inside each project directory
- Rich analysis: cyclomatic complexity, code smells, git blame, D3.js visualizations,
  narrative generation, privacy auditing
- Config artifact: `.mcp-vector-search/config.json` per project
- MCP stdio server exposing 17 tools for Claude Code integration
- **Value**: Proved hybrid BM25+vector+KG search, defined the analysis feature set
  worth preserving, and validated MCP tool ergonomics

### Generation 2: trusty-search (Rust, machine-wide daemon)

- Located at `../trusty-search`
- Ground-up Rust 2021 rewrite, scaffolded 2026-05-09
- Solves mcp-vector-search's per-project limitation: one daemon serves all projects
  on the machine
- Sub-10ms p50 warm query latency; ships a `convert project|all` command for
  zero-touch migration from mcp-vector-search
- Analysis features (complexity, smells, git blame, facts) were initially absorbed
  into trusty-search as part of its search layer
- **v0.1.37**: analysis layer extracted into this project (trusty-analyze)

### Generation 3: trusty-analyze (Rust, analysis sidecar daemon) — this project

- Sidecar to trusty-search: fetches chunk corpus via `GET /indexes/:id/chunks`,
  runs analysis, serves results on port 7879
- Shared types come from the `trusty-common` sibling crate in the `trusty-tools`
  workspace (path dep), which is also consumed by trusty-search
- Planned Phase 2: dynamic analysis (runtime call graphs, test coverage,
  mutation testing scores)

**GitHub issues tracking this extraction (in `bobmatnyc/trusty-search`):**
- `#40` feat: extract code analysis into sibling project trusty-analyze
- `#38` refactor: extract trusty-mcp-core (shared JSON-RPC transport)
- `#39` refactor: extract trusty-embedder (shared FastEmbedder crate)
- `#41` refactor: extract trusty-common utilities (shared port binding, registry)

---

## Hard Runtime Dependency

> **trusty-search is a hard runtime dependency. The analyzer will not start if
> trusty-search is unreachable.**
>
> There is no standalone or offline mode. Every `serve` invocation performs a
> startup health check against `GET <search-url>/health` before binding its own
> port. If the check fails the process prints a clear error and exits with code 1.

---

## Project Goals

### Phase 1 — Static Analysis (current)

- **Complexity analysis**: cyclomatic and cognitive complexity per chunk, file, index
- **Code smell detection**: configurable thresholds, named smell categories
- **Quality grade aggregation**: A–F per file and per index
- **Git blame / temporal decay**: score stale high-complexity code by last-modified age
- **Concept clustering**: k-means over doc embeddings, grouping related chunks
- **Facts store**: `(subject, predicate, object)` knowledge triples with provenance,
  persisted in redb
- **SCIP protobuf ingest**: LSP-quality symbol data import
- **NER extraction**: named entities from doc comments (optional ONNX, feature-gated)
- **Full HTTP API + MCP stdio server**: all endpoints have MCP tool equivalents
  (parity rule — no endpoint without a tool, no tool without an endpoint)

### Phase 2 — Language-Specific Static Enrichment (planned)

Tree-sitter provides a fast, uniform structural baseline across all supported
languages. Phase 2 adds per-language semantic analyzers that go deeper where
tree-sitter heuristics are insufficient.

**Adapter implementation order** (easier → harder):

1. **TypeScript / JavaScript** — TypeScript Compiler API, Babel/SWC parser for
   full type resolution, module graph, and export/import semantics
2. **Java** — JavaParser or Spoon for class/method/type-level analysis;
   Maven/Gradle introspection for dependency graphs
3. **Go** — `go/packages` and `go/types` for full type-checked symbol resolution
4. **Rust** — `rust-analyzer` for IDE-grade semantic metadata, module graph,
   type info, and diagnostics
5. **Python** — `ast` module and LibCST for accurate scope/import analysis
6. **C / C++** — Clang/libclang for semantic depth; `clangd` for
   IDE-style symbol resolution

Each adapter plugs into the `LanguageAnalyzer` trait (see **Plugin Architecture**
below). Tree-sitter remains the fallback for all languages at every phase.

### Phase 3 — Dockerized Runtime Execution (planned)

A job runner that orchestrates per-repo sandboxed execution:

```
clone / receive repo path
  → detect language(s) + build system
  → select Docker image
  → install dependencies (network-on)
  → build / compile
  → inject instrumentation
  → run tests / benchmarks / entrypoints (network-off)
  → collect profiler output
  → normalize to runtime result schema
  → map observations back to graph nodes
```

**Language difficulty / implementation order:**

| Difficulty | Languages | Notes |
|------------|-----------|-------|
| Easy | Python, JavaScript, TypeScript, Java | Decorator/AST injection; JVM bytecode weaving |
| Moderate | Go, Rust | Build/test orchestration + profiler output parsing |
| Hard | C, C++ | Variable build systems; binary instrumentation complexity |

**Planned Docker images:**

- `trustee-java-analyzer` — JDK + Maven/Gradle + async-profiler/JFR + AspectJ
- `trustee-node-analyzer` — Node.js + npm/pnpm + V8 profiler / OpenTelemetry
- `trustee-python-analyzer` — Python 3.x + pip + cProfile / py-spy / wrapt
- `trustee-go-analyzer` — Go toolchain + pprof
- `trustee-rust-analyzer` — Rust toolchain + cargo-flamegraph / tarpaulin
- `trustee-cpp-analyzer` — Clang/LLVM + Valgrind / perf / sanitizers

**Security requirements for every runtime job:**

- Non-root container user
- Read-only source mount where possible; separate writable workspace volume
- CPU, memory, process, and disk limits enforced
- Network isolated after dependency installation
- No host secrets mounted
- Timeout enforcement + audit log of commands executed
- Optional Firecracker / gVisor for higher-assurance workloads

### Phase 4 — Runtime-to-Graph Mapping (planned)

Normalize profiler output from every language adapter into a common schema and
attach observations to static graph nodes. Matching keys: file path, function
name, class name, method signature, source range, symbol ID, language-qualified
name.

**Runtime result schema** (per function / method / symbol):

| Field | Type | Description |
|-------|------|-------------|
| `symbol_id` | `String` | Stable ID from static graph |
| `language` | `String` | `"rust"`, `"java"`, … |
| `file` | `String` | Repo-relative file path |
| `function` | `String` | Qualified function / method name |
| `source_range` | `(u32, u32)` | Start / end line |
| `invocation_count` | `u64` | Total calls during run |
| `total_time_ns` | `u64` | Aggregate wall time |
| `avg_time_ns` | `u64` | Mean wall time per call |
| `p95_time_ns` | `u64` | P95 latency |
| `p99_time_ns` | `u64` | P99 latency |
| `error_count` | `u64` | Exceptions / panics observed |
| `memory_bytes` | `Option<u64>` | Peak memory if available |
| `profiler_source` | `String` | Tool name (`"cProfile"`, `"jfr"`, …) |
| `run_id` | `Uuid` | Links all records from one execution |

### Phase 5 — Advanced Search and Ranking (planned)

Unified scoring that combines every evidence layer:

```
score = w_text   × text_relevance
      + w_embed  × embedding_similarity
      + w_graph  × graph_centrality
      + w_cyclo  × static_complexity_score
      + w_rt     × runtime_cost_score
      + w_err    × error_frequency_score
      + w_cov    × test_coverage_score
      + w_dep    × dependency_risk_score
```

This transforms trusty-analyze from a complexity reporter into a full
**code intelligence** layer: "find the slowest functions in checkout that call
external services" becomes a single query.

---

## Plugin Architecture

Every language-specific analysis adapter implements a single trait. Concrete
implementations may call external binaries, run Docker jobs, parse JSON output,
or embed native libraries — the orchestration layer only sees the trait.

```rust
/// Why: Decouples orchestration from language-specific tooling so new languages
/// can be added without touching the analysis pipeline.
/// What: Lifecycle interface for detect → static → semantic → runtime.
/// Test: Implement a NoopAnalyzer; assert detect() returns false for foreign repos.
trait LanguageAnalyzer {
    fn detect(&self, repo: &Repo) -> DetectionResult;
    fn parse_static(&self, files: &[SourceFile]) -> StaticAnalysisResult;
    fn enrich_semantics(&self, repo: &Repo) -> SemanticAnalysisResult;
    fn prepare_runtime(&self, repo: &Repo) -> RuntimePlan;
    fn run_runtime(&self, plan: RuntimePlan) -> RuntimeAnalysisResult;
}
```

**Planned language adapters:** Rust, Java, TypeScript, JavaScript, Python, Go,
C, C++

---

## Knowledge Graph

### Node Types

| Node | Description |
|------|-------------|
| `Repository` | Root node; one per indexed repo |
| `Package` / `Module` | Cargo crate, npm package, Maven artifact, Go module, Python package |
| `File` | Source file; contains functions/classes |
| `Class` | OOP class or struct |
| `Interface` | Trait, Java interface, TypeScript interface |
| `Function` | Free function or closure |
| `Method` | Class/struct method |
| `Field` | Struct field or class property |
| `Import` / `Export` | Module boundary crossing |
| `Call` | Observed or inferred call expression |
| `TestCase` | Unit / integration test function |
| `Dependency` | External package / crate / library |

### Edge Types

| Edge | Semantics |
|------|-----------|
| `CONTAINS` | Parent contains child (repo → file, file → function) |
| `IMPORTS` | File or module imports another |
| `EXPORTS` | Symbol exported from a module |
| `CALLS` | Function A calls function B (static or runtime) |
| `IMPLEMENTS` | Class implements interface / struct implements trait |
| `EXTENDS` | Class inherits from another class |
| `REFERENCES` | Symbol references another symbol |
| `TESTS` | Test case exercises a production symbol |
| `DEPENDS_ON` | Package depends on external package |
| `GENERATED_FROM` | Runtime observation derived from static node |
| `RUNTIME_OBSERVATION_FOR` | Profiler measurement attached to static symbol |

### Scale Target

15,000 files / ~1 M lines of Java fully indexed in under 10 minutes.

---

## Architecture

```
trusty-search daemon (port 7878)          trusty-analyze daemon (port 7879)
  GET /indexes/:id/chunks  ─────────────► src/core/  (analysis engines)
  (bulk corpus export)                      complexity.rs   — cyclomatic/cognitive
                                            blame.rs        — git temporal decay
                                            quality.rs      — grade aggregation
                                            facts.rs        — FactStore (redb)
                                            client.rs       — HTTP client to trusty-search
                                          src/service/  (axum HTTP API)
                                          src/mcp/      (MCP stdio + SSE)
```

### trusty-common — Shared Type Crate

Lives at `crates/trusty-common` (a sibling crate in the same `trusty-tools`
workspace). Referenced as a path dep (`trusty-common = { workspace = true }`)
by both trusty-analyze and trusty-search.

Key types:

```rust
// crates/trusty-common/src/chunk.rs
pub struct CodeChunk { ... }          // canonical search result. Carries id, file,
                                      // line range, content, function_name, score,
                                      // compact_snippet, match_reason. Does NOT
                                      // carry complexity or blame — trusty-analyze
                                      // computes those independently via
                                      // `compute_complexity_for()` and the blame
                                      // module. The carrier fields were removed in
                                      // #71 because trusty-search never populated
                                      // them in practice.

// crates/trusty-common/src/complexity.rs
pub struct ComplexityMetrics { ... }
pub enum ComplexityGrade { A, B, C, D, F }
pub struct CodeSmell { ... }

// crates/trusty-common/src/blame.rs
pub struct ChunkBlame { ... }

// crates/trusty-common/src/entity.rs
pub enum EntityType { ... }
pub enum EdgeKind { ... }
pub struct RawEntity { ... }

// crates/trusty-common/src/facts.rs
pub struct FactRecord { subject, predicate, object, provenance, ... }
```

### Analysis Pipeline

```
1. Fetch corpus   GET /indexes/:id/chunks  →  Vec<CodeChunk>
2. Complexity     tree-sitter AST walk     →  ComplexityMetrics per chunk
3. Smells         threshold rules          →  Vec<CodeSmell> per chunk
4. Blame          git log --follow         →  ChunkBlame (age, author)
5. Grade          weighted formula         →  ComplexityGrade A–F per file
6. Cluster        k-means (linfa)          →  concept groups
7. Facts          upsert to redb           →  FactRecord store
8. Serve          axum HTTP + MCP stdio    →  query results
```

---

## Crate Layout

`trusty-analyze` is a **single crate** (one `Cargo.toml`, lib + bin targets)
within the `trusty-tools` workspace. There is no nested
`crates/trusty-analyze/crates/*` workspace — the analysis engines, language
adapters, embedder, MCP server, and service layer are all sibling modules under
`src/`. Shared types come from the in-workspace `trusty-common` path dep.

```
crates/trusty-analyze/
├── Cargo.toml                          single-crate manifest (lib + bin)
├── CLAUDE.md                           this file
├── README.md
├── src/
│   ├── lib.rs                          re-publishes modules below
│   ├── main.rs                         CLI: serve / analyze / facts / health
│   ├── types/                          CodeChunk, ComplexityMetrics, CodeSmell,
│   │                                   ComplexityGrade, ChunkBlame, EntityType,
│   │                                   EdgeKind, FactRecord, graph types
│   ├── core/                           analysis engines — complexity(.rs/_ts.rs),
│   │                                   blame, quality, facts (redb FactStore),
│   │                                   client (HTTP → trusty-search), concept_cluster,
│   │                                   explain, github, linker
│   ├── lang/                           LanguageAnalyzer trait, detection, and
│   │   └── adapters/                   tree-sitter adapters (15: rust, python, java,
│   │                                   go, typescript, javascript, c, cpp, csharp,
│   │                                   kotlin, php, ruby, scala, swift)
│   ├── embedder/                       BoW + neural concept-clustering embedders
│   ├── service/                        axum HTTP API (port 7879) + embedded UI
│   ├── mcp/                            MCP server: stdio + HTTP/SSE transports
│   └── commands/                       per-subcommand handlers (daemon/service/setup)
```

Documentation lives at the workspace top level under
`docs/trusty-analyze/`, not in-crate.

---

## HTTP API

Port 7879. All responses JSON. trusty-search must be running on port 7878.

```
GET  /health
     → { status: "ok", search_reachable: bool }

GET  /indexes
     → proxied from trusty-search GET /indexes

GET  /indexes/:id/complexity_hotspots[?top_k=N]
     → top-N chunks ranked by cyclomatic complexity

GET  /indexes/:id/smells[?category=<name>]
     → chunks with detected code smells, optionally filtered by category

GET  /indexes/:id/quality
     → { avg_cyclomatic, pct_grade_a, smell_count, grade: "A"|"B"|"C"|"D"|"F" }

GET  /facts[?subject=<s>&predicate=<p>]
     → Vec<FactRecord>

POST /facts
     body: { subject, predicate, object, provenance? }
     → { id: <uuid> }

DELETE /facts/:id
     → 204 No Content

POST /indexes/:id/scip
     body: SCIP protobuf (application/octet-stream)
     → { symbols_ingested: N }

GET  /indexes/:id/clusters?k=N&method=bow|neural
     → Vec<ConceptCluster> (label, chunk_ids, centroid_terms)
```

---

## MCP Tools

Parity rule: every HTTP endpoint has an MCP tool equivalent. The MCP server
registers **17 tools** (authoritative source: `src/mcp/mod.rs`
`tool_definitions`):

`complexity_hotspots`, `find_smells`, `analyze_quality`, `run_diagnostics`,
`list_facts`, `upsert_fact`, `delete_fact`, `analyzer_health`, `extract_graph`,
`cluster_concepts`, `ingest_scip`, `extract_ner`, `suggest_refactors`,
`review_diff`, `deep_analysis`, `review_github_pr`, `list_entities`.

### Transports

The MCP server supports two transports:

- **stdio**: `trusty-analyze serve --mcp` — JSON-RPC 2.0 over stdin/stdout,
  used by Claude Code and other clients that spawn the server as a subprocess.
- **HTTP/SSE**: `trusty-analyze serve --mcp-port 7880` — exposes
  `POST /mcp` for synchronous JSON-RPC and `GET /mcp/sse` for a long-lived
  Server-Sent Events stream with 15s keep-alive pings. Useful for remote
  integrations and browser-based clients.

Both transports share the same dispatcher (`AnalyzerMcpServer::dispatch`) and
expose identical tool surfaces.

### Claude Code Integration

The repo ships a `.mcp.json` at the workspace root registering the
analyzer's stdio transport with Claude Code:

```json
{
  "mcpServers": {
    "trusty-analyze": {
      "command": "trusty-analyze",
      "args": ["serve", "--mcp"],
      "env": {}
    }
  }
}
```

Claude Code auto-discovers this file on project open. The `trusty-analyze`
binary must be on `PATH` (e.g. via `cargo install --path .`).

---

## Stack

Matches trusty-search conventions where applicable for consistency.

| Concern | Crate |
|---------|-------|
| Language | Rust 2021 |
| Async runtime | tokio (full features) |
| HTTP server | axum 0.7 + tower-http 0.5 (CORS, trace, gzip) |
| HTTP client | reqwest 0.12 (rustls-tls, no native-tls) |
| Persistence | redb 2.6 (FactStore) |
| Concurrency | dashmap 5, tokio::sync::RwLock |
| Concept clustering | linfa 0.7 + ndarray (k-means) |
| Embeddings | fastembed 5.x (optional; uses cached model from trusty-search) |
| Code parsing | tree-sitter 0.24 (multi-language AST parsing; baseline for all phases) |
| Container runtime | Docker (sandboxed runtime execution; Phase 3+) |
| Temporal decay | chrono 0.4 |
| Serde | serde + serde_json |
| Errors | anyhow (app), thiserror (lib) |
| Tracing | tracing + tracing-subscriber (env-filter) |
| CLI | clap 4 (derive + env) |

---

## Relationship to Other Projects

| Project | Relationship |
|---------|-------------|
| `../mcp-vector-search` | Ancestor — Python prototype that defined the analysis feature set |
| `../trusty-search` | Sibling daemon — provides chunk corpus via `GET /indexes/:id/chunks`; consumes trusty-common types |
| `crates/trusty-common` | Shared type crate within this workspace; path dep for both projects |

### Dependency Direction

```
trusty-search  ──path dep──►  trusty-common  (types only)
trusty-analyze──path dep──►  trusty-common  (types only)
trusty-analyze──HTTP──────►  trusty-search  (chunk corpus at runtime)
```

trusty-common must never depend on trusty-search or trusty-analyze.

---

## Development Workflow

> **trusty-search MUST always be running before the analyzer starts.**
> `trusty-analyze serve` performs a startup health check and will exit with
> code 1 if the search daemon is unreachable.

```bash
# Step 1 — start trusty-search first (REQUIRED; analyzer will not start without it)
trusty-search daemon   # port 7878

# Step 2 — build everything
cargo build

# Step 3 — run the analyzer sidecar (development)
RUST_LOG=debug cargo run -- serve --search-url http://127.0.0.1:7878

# Analyze a named index
cargo run -- analyze <index-id> --top-k 20

# List / upsert facts
cargo run -- facts list
cargo run -- facts upsert '{"subject":"fn auth","predicate":"uses","object":"JWT"}'

# Liveness check
cargo run -- health

# Tests
cargo test --workspace

# Lint (zero warnings enforced)
cargo clippy --all-targets --all-features -- -D warnings

# Check only (faster during development)
cargo check --workspace
```

### Environment Variables

```bash
TRUSTY_SEARCH_URL=http://127.0.0.1:7878   # default; override for non-standard port
TRUSTY_ANALYZER_PORT=7879                  # default listen port
RUST_LOG=debug                             # enable debug tracing
```

---

## Publishing

`trusty-analyze` is a single crate published independently with the workspace's
per-crate tag convention `trusty-analyze-v<version>` (the version comes from
this crate's `Cargo.toml`). There is no nested multi-crate workspace and no
separate `trusty-analyze-types` / `-lang` / `-core` / `-mcp` / `-service` /
`-embedder` packages — those were sibling modules under `src/`, not crates.

```bash
# Dry-run before tagging (no upload)
cargo publish -p trusty-analyze --dry-run

# Then follow the workspace release workflow (see the root CLAUDE.md):
#   bump version → cargo test -p trusty-analyze → tag trusty-analyze-v<version>
#   → push tag → cargo publish -p trusty-analyze → cargo install --path .
```

Dependency bumps follow the workspace `[workspace.dependencies]` table; do not
pin tree-sitter or other shared crates locally if they already live there.

---

## Project Status

**Phase**: Phase 1 + Phase 2 complete. Full static analysis pipeline, HTTP API,
MCP server, SCIP ingest, neural/BoW concept clustering, and language-specific
tree-sitter adapters are all functional.

**Working:**
- Crate builds and tests pass (`cargo test -p trusty-analyze`)
- `trusty-common` type definitions (chunk, complexity, blame, entity, facts)
- `src/core/` fully wired: `client.rs`, `complexity.rs`, `complexity_ts.rs`,
  `blame.rs`, `quality.rs`, `facts.rs`, `concept_cluster.rs`, `linker.rs`,
  `explain.rs`, `github.rs`
- axum HTTP sidecar (`src/service/`) on port 7879
- MCP stdio + HTTP/SSE server (`src/mcp/`) — 17 tools (authoritative source:
  `src/mcp/mod.rs` `tool_definitions`)
- CLI subcommands: `serve`, `analyze`, `facts list/upsert`, `health`
- Daemon PID lockfile (fs4), graceful shutdown, `--search-url` flag
- `LanguageAnalyzer` trait + 15 tree-sitter adapters, all implemented:
  rust, python, java, go, typescript, javascript, c, cpp, csharp, kotlin,
  php, ruby, scala, swift (see `src/lang/adapters/`)
- CALLS edges + cross-chunk entity linker (`#47` complete)
- k-means concept clustering (BoW / neural) + `/indexes/:id/clusters` endpoint
- SCIP protobuf ingest → knowledge graph (`#47` complete)
- Integration self-analysis suite

**Remaining / next steps:**
- Phase 2 semantic enrichment: deepen adapters beyond the tree-sitter baseline
- Phase 3: Dockerized runtime execution (sandboxed profiler jobs)
- Phase 4: Runtime-to-graph mapping (normalize profiler output → graph nodes)
- Phase 5: Advanced unified scoring (text + embed + graph + runtime layers)
- CI workflow + integration test gate (requires trusty-search running)
- `cargo install` smoke test
