# trusty-analyze

Sidecar code-analysis daemon for trusty-search. Reads chunk corpora from
trusty-search via HTTP, runs static analysis, and serves results as JSON-RPC
over `<data dir>/trusty-analyze/trusty-analyze.sock` and via an MCP stdio
server.

🔴 **#6287 (ADR-0032) retired this daemon's HTTP surface.** It bound
`127.0.0.1:7879` and served an axum router, an `/sse` broadcast, and a second
`--mcp-port` HTTP/SSE listener. All three are gone: the router became
`service::rpc`'s JSON-RPC dispatcher over a Unix socket, `/sse`'s only
subscriber was the daemon's own SPA, and `--mcp-port` had no in-repo consumer
at all. Four crates dialled 7879 and all four moved in the same change;
`tests/uds_consumer_contract.rs` is what keeps them in step. Sections below
that still describe HTTP routes are historical — `service::rpc::METHODS` is
the live surface.

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
  runs analysis, serves results over its own Unix socket (port 7879 until
  #6287)
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
- **Full JSON-RPC surface over UDS + MCP stdio server**: every method has an MCP
  tool equivalent (parity rule — no method without a tool, no tool without a
  method)

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
trusty-search daemon (port 7878)          trusty-analyze (trusty-analyze.sock)
  GET /indexes/:id/chunks  ─────────────► src/core/  (analysis engines)
  (bulk corpus export)                      complexity.rs   — cyclomatic/cognitive
                                            blame.rs        — git temporal decay
                                            quality.rs      — grade aggregation
                                            facts.rs        — FactStore (redb)
                                            client.rs       — HTTP client to trusty-search
                                          src/service/  (JSON-RPC over UDS)
                                          src/mcp/      (MCP stdio)
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
8. Serve          JSON-RPC/UDS + MCP stdio →  query results
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
│   ├── embedder/                       BoW concept-clustering embedder
│   ├── service/                        JSON-RPC over UDS (rpc.rs + handlers/)
│   ├── mcp/                            MCP server: stdio only since #6287
│   └── commands/                       per-subcommand handlers (daemon/service/setup)
```

Documentation lives at the workspace top level under
`docs/trusty-analyze/`, not in-crate.

---

## RPC Surface

JSON-RPC 2.0 over `<data dir>/trusty-analyze/trusty-analyze.sock`, one
newline-terminated frame per request, 32 MiB request budget. trusty-search must
be running on port 7878. `service::rpc::METHODS` is the authoritative list and
`rpc_router_registers_every_documented_method` keeps it equal to what
`build_router` registers.

```
analyze.health                  → { status: "ok"|"degraded", version, search_reachable }
analyze.list_indexes            → proxied from trusty-search GET /indexes
analyze.complexity_hotspots     { index_id, top_n? }
analyze.complexity_distribution { index_id }  → full A–F histogram + counted total
analyze.smells                  { index_id, category? }
analyze.refactor_suggestions    { index_id, file?, min_severity?, top_k? }
analyze.quality                 { index_id }  → { avg_cyclomatic, pct_grade_a, … }
analyze.diagnostics             { index_id, language?, tools?, limit?, offset? }
analyze.graph                   { index_id, language? }
analyze.entities                { index_id, kind?, language? }
analyze.clusters                { index_id, k?, method? }
analyze.ner                     { index_id, top_k? }
analyze.scip_ingest             { index_id, scip_base64 }
analyze.scip_status             { index_id }  → -32004 when never ingested (#5049)
analyze.review                  { index_id, diff }
analyze.review_github_pr        { owner, repo, pr, index_id, post_comment? }
analyze.deep_analysis           { index_id, model? }
analyze.facts_list              { subject?, predicate?, object? }
analyze.facts_upsert            { subject, predicate, object, index_id, … }
analyze.facts_delete            { id }
```

Two error codes carry meaning beyond the JSON-RPC standard set, both in the
implementation-defined `-32000..=-32099` band:

| Code | Constant | Meaning |
|---|---|---|
| `-32004` | `service::events::CODE_NOT_FOUND` | The request is well formed and names something absent — `analyze.scip_status` for an index nobody ingested (#5049). |
| `-32005` | `service::events::CODE_DEADLINE_EXCEEDED` | The handler exhausted its own deadline. `trusty-review` reads this to print "ran out of time" rather than "could not be reached". |

---

## MCP Tools

Parity rule: every RPC method has an MCP tool equivalent.

<!-- BEGIN GENERATED: mcp-tools -->
The MCP server registers **21 tools** with default features, **25 tools** with `--features review`. Authoritative source: `trusty_analyze::mcp::tool_descriptors + trusty_analyze::mcp::descriptors::review_tool_descriptors` —
this table is generated from it, not maintained by hand.

| Tool | Available | Arguments | Summary |
|---|---|---|---|
| `analyze_quality` | always | `index?`, `index_id?` | Aggregate quality stats: avg cyclomatic, %A, smell count |
| `analyzer_health` | always | — | Probe analyzer daemon liveness and version |
| `cluster_concepts` | always | `index?`, `index_id?`, `k?`, `method?` | Group chunks into concept clusters using k-means over hashed bag-of-words embeddings |
| `complexity_distribution` | always | `index?`, `index_id?` | Full A-F cyclomatic-complexity histogram over the whole index corpus, with the counted total. |
| `complexity_hotspots` | always | `index?`, `index_id?`, `top_n?` | Top-N chunks ranked by cyclomatic complexity |
| `console_metrics` | always | — | Return health and operational metrics for trusty-console polling. |
| `deep_analysis` | always | `index_id`, `model?` | Run an LLM-augmented deep analysis pass over an index: synthesises a deterministic review report from the indexed corpus, looks up detected… |
| `delete_fact` | always | `id` | Delete a fact by its u64 id |
| `extract_graph` | always | `index?`, `index_id?`, `language?` | Build the multi-language knowledge graph (nodes + edges) for an index |
| `extract_ner` | always | `index?`, `index_id?`, `top_k?` | Extract named entities from doc comments for a code index using NER |
| `find_smells` | always | `index?`, `index_id?`, `limit?`, `offset?`, `omit_content?` | Chunks with at least one detected code smell. |
| `ingest_scip` | always | `scip_base64`, `index?`, `index_id?` | Ingest a SCIP (Scalable and Precise Index for Code) protobuf index for a given index_id, enriching the knowledge graph with fully-resolved… |
| `list_analyze_indexes` | always | — | List all indexes known to the trusty-analyze daemon. |
| `list_entities` | always | `index?`, `index_id?`, `kind?`, `language?` | List symbol-level entities (functions, classes, ...) for an index |
| `list_facts` | always | `object?`, `predicate?`, `subject?` | List canonical facts, optionally filtered by subject/predicate/object |
| `review_diff` | always | `diff`, `index_id` | Review a unified git diff and return a structured quality report (per-file complexity, code smells, grade A-F, recommendations). |
| `review_github_pr` | always | `owner`, `repo`, `pr`, `index_id`, `post_comment?` | Fetch a GitHub pull request's unified diff and run a structured quality review against a trusty-search index. |
| `run_diagnostics` | always | `index?`, `index_id?`, `language?`, `limit?`, `offset?`, `tools?` | Run available external static-analysis tools (clippy, ruff, biome, staticcheck, pmd, rubocop, phpstan, swiftlint, detekt, clang-tidy,… |
| `scip_status` | always | `index?`, `index_id?` | Report whether a SCIP overlay has been ingested for an index. |
| `suggest_refactors` | always | `file?`, `index?`, `index_id?`, `min_severity?`, `top_k?` | Suggest concrete refactoring actions (extract method, reduce nesting, ...) ranked by severity, derived from complexity metrics and code… |
| `tr_report` | `--features review` | `manifest_path`, `analyze?`, `code_only?`, `instructions?`, `out?`, `template?` | Generate a technical due-diligence report from a report manifest, via the embedded trusty-review report pipeline. |
| `tr_review_diff` | `--features review` | `diff`, `context?`, `reviewer_model?` | LLM-backed review of a raw unified diff string via the embedded trusty-review pipeline. |
| `tr_review_health` | `--features review` | — | Probe the embedded trusty-review pipeline's liveness and configuration (dry_run mode, reviewer model, dependency URLs). |
| `tr_review_pr` | `--features review` | `owner`, `repo`, `pr`, `reviewer_model?` | LLM-backed review of a GitHub pull request via the embedded trusty-review pipeline. |
| `upsert_fact` | always | `subject`, `predicate`, `object`, `index_id`, `confidence?`, `provenance?` | Insert or update a canonical fact triple |
<!-- END GENERATED: mcp-tools -->

### Transport

**stdio only**: `trusty-analyze serve --mcp` — JSON-RPC 2.0 over stdin/stdout,
used by Claude Code and other clients that spawn the server as a subprocess.

#6287 deleted the `--mcp-port` HTTP/SSE transport (`POST /mcp` plus a
`GET /mcp/sse` stream). Nothing in this repository consumed it, and ADR-0032
forbids a second HTTP surface; the original scoping missed that it was one.

The dispatcher itself is an RPC CLIENT of this daemon's own socket
(`mcp/rpc_client.rs`), which is why it names each `analyze.*` method by literal
— `service` sits behind the `http-server` feature and the dispatcher does not.
`mcp_names_the_methods_the_router_registers` keeps those literals equal to what
`build_router` registers.

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
| Embeddings | hashed bag-of-words (`core::bow_embedding`); no model, no ONNX Runtime since #5067 |
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
trusty-search start   # port 7878

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
TRUSTY_ANALYZE_SOCKET=/path/to.sock        # trusty-audit's guard override; the
                                           # default is derived from the data dir
RUST_LOG=debug                             # enable debug tracing

# TRUSTY_ANALYZER_PORT was removed with the listener (#6287).
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

**Phase**: Phase 1 + Phase 2 complete. Full static analysis pipeline, RPC
surface, MCP server, SCIP ingest, BoW concept clustering, and language-specific
tree-sitter adapters are all functional.

**Working:**
- Crate builds and tests pass (`cargo test -p trusty-analyze`)
- `trusty-common` type definitions (chunk, complexity, blame, entity, facts)
- `src/core/` fully wired: `client.rs`, `complexity.rs`, `complexity_ts.rs`,
  `blame.rs`, `quality.rs`, `facts.rs`, `concept_cluster.rs`, `linker.rs`,
  `explain.rs`, `github.rs`
- JSON-RPC sidecar (`src/service/`) on `trusty-analyze.sock` (#6287)
- MCP stdio server (`src/mcp/`) — see [MCP Tools](#mcp-tools)
- CLI subcommands: `serve`, `analyze`, `facts list/upsert`, `health`
- Daemon PID lockfile (fs4), graceful shutdown, `--search-url` flag
- `LanguageAnalyzer` trait + 15 tree-sitter adapters, all implemented:
  rust, python, java, go, typescript, javascript, c, cpp, csharp, kotlin,
  php, ruby, scala, swift (see `src/lang/adapters/`)
- CALLS edges + cross-chunk entity linker (`#47` complete)
- k-means concept clustering (BoW) + the `analyze.clusters` method
- SCIP protobuf ingest → knowledge graph (`#47` complete)
- Integration self-analysis suite

**Remaining / next steps:**
- Phase 2 semantic enrichment: deepen adapters beyond the tree-sitter baseline
- Phase 3: Dockerized runtime execution (sandboxed profiler jobs)
- Phase 4: Runtime-to-graph mapping (normalize profiler output → graph nodes)
- Phase 5: Advanced unified scoring (text + embed + graph + runtime layers)
- CI workflow + integration test gate (requires trusty-search running)
- `cargo install` smoke test
