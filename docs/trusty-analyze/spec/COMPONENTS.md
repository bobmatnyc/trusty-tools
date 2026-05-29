# trusty-analyze — Components

> **Status:** Canonical · Living Document
> **Last reviewed:** 2026-05-29
> **Derived from:** code/docs/tickets audit

Per-subsystem specs. Each states **responsibility**, **key types/modules** (with
`src/` paths), **current state**, and **gaps**, tagged ✅ / 🟡 / 🔵 / ⚪ (see
[README](./README.md#status-legend-used-throughout-this-set)).

---

## 1. trusty-search HTTP Client — `core/client.rs`

- **Responsibility:** the analyzer's only window onto the corpus — fetch chunks
  and index summaries from trusty-search.
- **Key types:** `TrustySearchClient` (clone-cheap `reqwest::Client` wrapper),
  `IndexSummary`. Constants `CHUNK_PAGE_LIMIT = 1000`, `CHUNK_PAGE_CONCURRENCY = 4`.
- **Current (✅):** HTTP/2 prior-knowledge (loopback, no TLS) for low-latency
  multiplexed paging; `get_chunks` pages with `FuturesOrdered` at concurrency 4;
  `health()` powers the hard-dependency probe.
- **Gaps:** no retry/backoff; a mid-paging failure aborts the whole fetch.

---

## 2. Complexity — `core/complexity.rs`, `core/complexity_ts.rs`

- **Responsibility:** cyclomatic + cognitive complexity, A–F grade, smell list
  per chunk.
- **Key types/fns:** `compute_complexity_for(content, language)` dispatcher;
  `compute_complexity` (text heuristic); `compute_complexity_rust` /
  `compute_complexity_ts` (AST); thresholds `LONG_FUNCTION_THRESHOLD = 50`,
  `DEEP_NESTING_THRESHOLD = 4`, `TOO_MANY_PARAMS_THRESHOLD = 5`.
- **Current (✅/🟡):** Rust/TS/JS use a tree-sitter walk (accurate, line-anchored,
  no false `if ` matches inside strings); everything else falls back to the
  language-agnostic text heuristic, also used on any parse failure as a safety
  net.
- **Gaps:** AST-accurate complexity is Rust/TS/JS-only despite 14 structural
  adapters existing. Thresholds are compile-time constants (FR-3.3 / OQ-1).

---

## 3. Quality Aggregation — `core/quality.rs`

- **Responsibility:** one-shot corpus summaries for the `quality` endpoint/tool
  and the `analyze` CLI.
- **Key types/fns:** `QualityReport { avg_cyclomatic, pct_grade_a, smell_count,
  chunk_count }`; `complexity_hotspots(chunks, n)`; `smelly_chunks(chunks)`.
- **Current (✅):** pure functions; re-score chunks on demand from `content` when
  a pre-computed `complexity` field is absent, so trusty-search need not populate
  it.
- **Gaps:** grade-rollup weights are fixed; no per-file (vs per-index) report at
  the HTTP layer despite the README mentioning per-file grades.

---

## 4. Code Smells — `types/complexity.rs`

- **Responsibility:** define the smell taxonomy and grade enum (wire types live
  in this crate's `types/`, mirroring the old `trusty-common` types).
- **Key types:** `CodeSmell::{LongFunction{lines}, DeepNesting{max_depth},
  TooManyParams{count}, MissingDocstring}`; `ComplexityGrade::{A,B,C,D,F}`;
  `ComplexityMetrics`.
- **Current (✅):** detection runs in both the text and AST paths; the AST path
  produces line-accurate smell positions.
- **Gaps:** four smell categories only; no duplication/dead-code/coupling smells.

---

## 5. Language Adapters & Registry — `lang/`, `core/registry.rs`

- **Responsibility:** parse chunk content per language into a `KgGraph`; route
  chunks to the right adapter; detect languages/build-systems/frameworks.
- **Key types:** `LanguageAnalyzer` trait (`analyze_chunks`,
  `supported_extensions`) + `StaticAnalysisResult`; `LanguageDetector` /
  `DetectionResult` / `detect_frameworks`; `AnalyzerRegistry::default_registry`.
- **Current (✅):** 14 adapters registered — Rust, TypeScript, JavaScript,
  Python, Java, Go, C, C++, C#, Kotlin, PHP, Ruby, Scala, Swift — each emitting
  File/Function/Method/Class/Interface/Module/Import nodes plus Contains/Imports/
  Implements/Calls edges (and TestCase where the grammar allows).
- **Gaps:** `lang/mod.rs` module doc falsely claims Python/Java/Go are "stubbed
  for Phase 2b" — a doc gap (FR-4.4 / OQ-2). The trait exposes only the static
  path; the `enrich_semantics`/`prepare_runtime`/`run_runtime` lifecycle from
  `CLAUDE.md` is design-only (🔵).

---

## 6. Knowledge Graph, Linker & SCIP — `types/graph.rs`, `core/linker.rs`, `core/scip.rs`

- **Responsibility:** a language-neutral symbol graph, deduplicated across
  overlapping chunk windows, optionally enriched by precise SCIP indexes.
- **Key types:** `KgGraph`, `KgNode`, `KgEdge`, `KgNodeKind` (14 kinds incl.
  Repository/Package/Module/File/Class/Interface/Function/Method/Field/Import/
  Export/CallExpression/TestCase/Dependency), `KgEdgeKind` (11 kinds incl.
  GeneratedFrom/RuntimeObservationFor); `linker::link`; `scip::extract_kg_from_scip`
  + `ScipIngestSummary`.
- **Current (✅):** `link` collapses `(language, kind, qualified_name)` duplicates,
  keeps the widest range, rewires edges, drops self-loops. SCIP ingest decodes
  the protobuf `Index`, emitting definitions as nodes, `is_implementation` as
  Implements edges, occurrences as References edges.
- **Gaps:** `GeneratedFrom` / `RuntimeObservationFor` have no producer (await
  Phase 3/4, 🔵). No graph persistence — graphs are recomputed per request.

---

## 7. Concept Clustering & Embedders — `core/concept_cluster.rs`, `embedder/`

- **Responsibility:** group semantically related chunks into labelled concept
  clusters.
- **Key types:** `cluster()` (Lloyd k-means + k-means++ seeding),
  `ConceptCluster { id, label, members, cohesion }`, `ClusterResult`;
  `bow_embedding`; `Embedder` trait with `BowEmbedder` + `NeuralEmbedder`
  (`EmbedderKind::{Bow,Neural}`).
- **Current (✅):** BoW is deterministic and dependency-free; neural is fastembed
  all-MiniLM-L6-v2 (384-dim). Neural load failure at `serve` startup degrades
  gracefully to BoW (`main.rs`).
- **Gaps:** `k` is caller-supplied (no auto-k); neural requires a pre-cached
  model (shares trusty-search's cache).

---

## 8. Refactor, Review, Deep Analysis & GitHub — `core/refactor.rs`, `core/review.rs`, `core/explain.rs`, `core/github.rs`

- **Responsibility:** turn metrics into actionable guidance and PR-time review.
- **Key types:** `RefactorSuggestion` / `RefactorType` / `Severity`;
  `DiffParser` / `FileDiff` / `ReviewReport` / `analyze_diff_with_client` /
  `render_review_text`; `DeepAnalysisReport { narrative, frameworks,
  recommendations, model_used }` / `explain_report` / `deep_analysis`;
  `fetch_pr_diff` / `post_pr_comment` / `format_review_as_markdown` /
  `verify_webhook_signature`.
- **Current (✅/🟡):** refactor + review are deterministic pure pipelines —
  review cross-references the index corpus (stored complexity for indexed files,
  local tree-sitter for new files). Deep analysis layers an LLM narrative via a
  `trusty_common::chat::ChatProvider`, kept *out* of the deterministic
  `ReviewReport` so reproducibility is preserved; needs `OPENROUTER_API_KEY`
  (returns 400 otherwise). GitHub helpers fetch a PR diff, run review, post a
  comment, and verify webhook HMAC (SHA-256).
- **Gaps:** deep analysis is non-deterministic and model-dependent (🟡); GitHub
  integration relies on `GITHUB_TOKEN` being present in the daemon env.

---

## 9. Facts Store — `core/facts.rs`, `types/facts.rs`

- **Responsibility:** the only persistent state — a canonical
  `(subject, predicate, object)` triple store with provenance.
- **Key types:** `FactStore` (redb `facts` table), `FactRecord`, `new_fact`,
  `fact_hash` (length-prefixed `xxh3`).
- **Current (✅):** the triple *is* the identity; re-asserting merges provenance.
  `xxh3` hashing is stable across toolchains (replaced `DefaultHasher`, #64).
  #67 removed a read-lock contention p99 spike in `list_facts`.
- **Gaps:** `#64` notes pre-switch redb rows are invalidated with no migration
  (facts are re-derivable on next run, so accepted).

---

## 10. External Static Tools — `core/tools.rs`, `core/tool_registry.rs`, `core/tool_impls/`

- **Responsibility:** complement the tree-sitter baseline with real linters.
- **Key types:** `StaticTool` trait (`name`/`language`/`is_available`/`run`),
  `ToolDiagnostic`, `Severity::{Error,Warning,Info,Hint}`; `ToolRegistry`
  (`discover`, `tools_for`, `run_all`) + process-wide `global_registry()`;
  `TOOL_TIMEOUT = 30s`.
- **Current (✅):** 10 adapters — `ClippyTool`, `RuffTool`, `BiomeTool`,
  `RubocopTool`, `PhpstanTool`, `DetektTool`, `PmdTool`, `StaticcheckTool`,
  `ClangtidyTool`, `SwiftlintTool` — discovered lazily via `which`, indexed by
  language, each parsing native output into `ToolDiagnostic` under a hard timeout.
  Surfaced at `/indexes/:id/diagnostics` + MCP `run_diagnostics`.
- **Gaps:** availability depends on the host having each binary installed;
  tool-specific config (rulesets) is not yet plumbed.

---

## 11. NER — `core/ner.rs`

- **Responsibility:** extract natural-language phrases from doc comments as
  `RawEntity`s for downstream BM25/KG lookup.
- **Key types:** `NerExtractor` (`try_load`, `extract`), `extract_doc_comments`.
- **Current (🟡):** double-gated — requires `--features ner` (pulls `ort` +
  `tokenizers`) **and** a model at `~/.trusty-analyzer/models/ner.onnx`.
  Always constructible; `extract()` is a no-op when disabled, so the single
  binary ships without ONNX yet can light up if a user drops the model in.
  Surfaced at `/indexes/:id/ner` + MCP `extract_ner`.
- **Gaps:** off by default; no bundled model; structural extractor remains the
  primary entity source.

---

## 12. MCP Server — `mcp/mod.rs`, `mcp/stdio.rs`, `mcp/sse.rs`

- **Responsibility:** expose the full HTTP surface as MCP tools (strict parity).
- **Key types:** `AnalyzerMcpServer::dispatch` (JSON-RPC→HTTP translator),
  `Request`/`Response`/`JsonRpcError`, `error_codes`; `stdio::run`;
  `sse::router`.
- **Current (✅):** 18 tools — `complexity_hotspots`, `find_smells`,
  `analyze_quality`, `run_diagnostics`, `list_facts`, `upsert_fact`,
  `delete_fact`, `extract_graph`, `list_entities`, `cluster_concepts`,
  `analyzer_health`, `ingest_scip`, `extract_ner`, `suggest_refactors`,
  `review_diff`, `review_github_pr`, `deep_analysis` (plus the resource/index
  listing in `tools/list`). Two transports share one dispatcher; the dispatcher
  owns only a `reqwest::Client` + base URL.
- **Gaps:** the README/CLAUDE.md still list 9 tools (OQ-2). The dispatcher is
  stateless — it cannot serve when the analyzer daemon it points at is down.

---

## 13. HTTP Daemon & Embedded UI — `service/mod.rs`, `service/ui.rs`

- **Responsibility:** the axum REST surface (port 7879) + dashboard.
- **Key types:** `AnalyzerAppState` (`new`, `with_embedder`), `serve`,
  `DEFAULT_PORT`; route handlers; `WebAssets` (rust-embed) + `ui_index_handler` /
  `ui_asset_handler`.
- **Current (✅):** ~20 routes — health, index proxy, complexity/smells/quality,
  diagnostics/graph/entities/clusters/ner, scip ingest, review/github-pr/deep,
  github webhook, facts CRUD, `/ui` SPA (with index.html fallback for client-side
  routing). Gated behind the `http-server` feature (#249); `serve` can also fork
  an MCP stdio loop (`--mcp`) and/or an MCP HTTP/SSE server (`--mcp-port`).
- **Gaps:** UI assets come from `ui/dist/` built by `build.rs` via pnpm; the
  build warns (non-fatal) when pnpm is unavailable, shipping an empty dashboard.

---

## 14. CLI & Daemon Lifecycle — `main.rs`, `commands/`

- **Responsibility:** the user-facing command surface and process management.
- **Key modules:** `commands/daemon.rs` (`start`/`stop`/`status`/`doctor` +
  PID file under `~/.trusty-analyze/`); `commands/service.rs` (macOS launchd
  install/uninstall/status/logs); `commands/setup.rs` (`SetupTarget`:
  ClaudeCode/Cursor/ClaudeMpm/Daemon/All).
- **Current (✅):** full lifecycle parity with trusty-search/trusty-memory;
  shell completions via `clap_complete`; shared "did you mean?" help (#216);
  `dashboard`/`dash` opens the UI after a TCP probe.
- **Gaps:** launchd `service` is macOS-only (exits 1 on Linux/Windows with a
  clear message).

---

## 15. Wire Types — `types/`

- **Responsibility:** serde wire-format types crossing the HTTP/MCP boundary,
  forward-compatible with trusty-search additions.
- **Key types:** `CodeChunk` (id, file, line range, content, function_name,
  score, snippet, match_reason — no complexity/blame carrier, removed in #71),
  `ComplexityMetrics`/`CodeSmell`/`ComplexityGrade`, `ChunkBlame`, `RawEntity`/
  `EntityType`/`EdgeKind`, `FactRecord`, `KgGraph`/`KgNode`/`KgEdge`.
- **Current (✅):** every struct uses `#[serde(default)]` on optional fields and
  tolerant unknown-field handling so trusty-search can evolve independently.
- **Gaps:** these duplicate the old `trusty-common` types (the crate now carries
  its own `types/` rather than depending on `trusty-common` for chunk types);
  drift between the two is a maintenance risk.
