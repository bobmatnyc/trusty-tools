# trusty-search vs mcp-vector-search — Comparison

**Date:** 2026-05-12
**trusty-search version:** 0.3.27
**mcp-vector-search version:** 4.1.13

---

## Executive summary

`trusty-search` and `mcp-vector-search` are both hybrid code search tools that
expose results to AI coding assistants via the Model Context Protocol. They
solve overlapping problems but make very different architectural choices.

- **`trusty-search`** is a single Rust binary that runs as a machine-wide,
  always-on daemon. It optimises aggressively for warm-query latency
  (~13–16 ms hybrid query on a 1 282-chunk Rust repo) and shared resource
  use (one loaded embedding model across every Claude Code session on the
  box). Its query pipeline adds a sub-ms regex **intent classifier** that
  retunes α / β weights and gates KG expansion per query.
- **`mcp-vector-search`** is a Python service that ships per-project state
  (`<project>/.mcp-vector-search/`) and a much broader **analysis tool
  surface** — 28 MCP tools covering complexity, code smells, circular
  dependencies, code review (SARIF), wiki / story generation, and a
  temporal KuzuDB-backed knowledge graph. It uses LanceDB for persistent
  IVF-PQ vector search.

If you primarily want fast hybrid retrieval that disappears into the
background and serves many projects from one process, prefer
`trusty-search`. If you want a richer in-IDE analysis suite with code-review
output, history-aware KG traversal, and configurable embedding models,
prefer `mcp-vector-search`. The two are not mutually exclusive — for code
*analysis* specifically, `trusty-search` v0.2.0+ deliberately defers to
[trusty-analyzer](https://github.com/bobmatnyc/trusty-analyzer).

---

## When to use which

| You want…                                                         | Pick                  |
|-------------------------------------------------------------------|-----------------------|
| Sub-20 ms warm hybrid queries shared across all your projects     | trusty-search         |
| One install, one process, unlimited named indexes                 | trusty-search         |
| Automatic α / β tuning per query (Definition vs Usage vs …)       | trusty-search         |
| Drop-in single binary, no Python / venv / pip                     | trusty-search         |
| Per-project portable index (`.mcp-vector-search/`)                | mcp-vector-search     |
| Complexity hotspots, code smells, circular-dep detection          | mcp-vector-search or  |
|                                                                   | trusty-analyzer       |
| MCP-driven code review with SARIF output                          | mcp-vector-search     |
| Temporal KG queries (point-in-time, `kg_callers_at_commit`)       | mcp-vector-search     |
| Wiki / story / narrative generation                               | mcp-vector-search     |
| Configurable embedding models (CodeBERT, CodeT5+, etc.)           | mcp-vector-search     |
| Embedded admin UI for hands-on operators                          | trusty-search (Svelte)|

---

## Search capabilities

| Capability                | trusty-search                                                            | mcp-vector-search                                  |
|---------------------------|--------------------------------------------------------------------------|----------------------------------------------------|
| Lexical                   | BM25, zero-dep port, camelCase + snake_case splitting                    | `rank-bm25` `BM25Okapi`                            |
| Vector                    | usearch 2.25 HNSW, all-MiniLM-L6-v2 INT8 Q (384-dim), CoreML auto on AS  | LanceDB IVF-PQ, configurable models (MiniLM / CodeBERT / CodeT5+), MPS on AS |
| Fusion                    | RRF k = 60, **always-on**                                                | RRF k = 60 in `search_hybrid` tool **only**; default `search_code` is vector-only |
| Knowledge graph           | petgraph `SymbolGraph`, 1–2 hop callers_of / callees_of, `EdgeKind` multipliers, **gated to Usage intent** | KuzuDB, **persistent + temporal**, point-in-time traversal |
| Query intent routing      | **Yes** — sub-ms regex classifier, 5 intents, per-intent α / β + KG gate | No                                                 |
| Reranking                 | None (RRF only)                                                          | Cross-encoder second-pass available                |
| Query embedding cache     | LRU (256–20 000 entries, tier-tuned)                                     | Implicit (LanceDB caches)                          |

---

## Architecture

| Dimension              | trusty-search                                                | mcp-vector-search                                |
|------------------------|--------------------------------------------------------------|--------------------------------------------------|
| Deployment unit        | **Machine-wide daemon**, single binary                       | Per-project MCP server                           |
| Indexes per process    | Unlimited, via `DashMap<IndexId, IndexHandle>`               | One per server                                   |
| Vector storage         | In-memory HNSW (`Duration::MAX` cool-after, never paged out) | LanceDB on disk (IVF-PQ ANN file)                |
| KV store               | redb 2.6 (chunk metadata, file → chunk map)                  | LanceDB tables                                   |
| KG store               | petgraph in-memory                                           | KuzuDB persistent (temporal)                     |
| Concurrency model      | `Arc<RwLock<CodeIndexer>>` reader-priority, HTTP/2 multiplex | asyncio                                          |
| File watching          | `notify-debouncer-mini` 500 ms                               | `watchdog`                                       |
| Multi-project daemon   | **Shipping** (this is the default mode)                      | **Design doc only** as of 2026-05-09             |
| RAM floor              | 16 GB hard check at daemon start                             | Not enforced                                     |
| Memory autotuning      | 5 tiers (Tiny → XLarge) at startup                           | None                                             |
| Admin UI               | Embedded Svelte 5 (compiled into binary)                     | D3.js visualization CLI                          |

---

## Performance

### Observed (this repository, 1 282-chunk Rust crate, M-series Mac)

| Metric                         | trusty-search                                |
|--------------------------------|----------------------------------------------|
| Warm hybrid query (p50)        | 13–16 ms                                     |
| Cold query (embedding miss)    | ~95 ms                                       |
| Cache-hit cold-start           | ~15 ms                                       |
| Daemon RSS at rest             | 573 MB                                       |
| Reindex throughput (14k files) | ~2–3 min (INT8 model + batch 512 + split lock)|

### Published / claimed (mcp-vector-search)

| Metric                         | mcp-vector-search                            |
|--------------------------------|----------------------------------------------|
| IVF-PQ vs flat scan            | 4.9× faster (3.4 ms vs 16.7 ms median)       |
| `search_hybrid` end-to-end     | < 0.2 s                                      |
| CLI cold start                 | 4 000–9 000 ms                               |
| MCP server (warm)              | 40–85 ms                                     |
| M4 Max MPS speedup             | 2–4× over CPU                                |

**Cold-start asymmetry is the headline difference.** `trusty-search`
amortises model load and HNSW residency across the lifetime of one
machine-wide daemon; every Claude Code session pays only the warm-query
cost. `mcp-vector-search` pays a 4–9 s cold start per CLI invocation;
its MCP server is competitive once warm (40–85 ms) but a separate
process per project.

---

## MCP tool surface

| Category         | trusty-search (11)                                    | mcp-vector-search (28)                                                                                       |
|------------------|-------------------------------------------------------|--------------------------------------------------------------------------------------------------------------|
| Retrieval        | `search_code`, `search_similar`                       | `search_code`, `search_context`, `search_similar`, `search_hybrid`, `embed_chunks`                           |
| Index lifecycle  | `index_file`, `remove_file`, `create_index`, `delete_index`, `reindex`, `index_status`, `list_indexes`, `list_chunks` | `index_project`, `get_project_status`, `analyze_file`                                                        |
| Health / chat    | `search_health`, `chat`                               | —                                                                                                            |
| Static analysis  | — (see [trusty-analyzer](https://github.com/bobmatnyc/trusty-analyzer)) | `find_smells`, `get_complexity_hotspots`, `analyze_project`, `analyze_tests`, `check_circular_dependencies`, `interpret_analysis` |
| Knowledge graph  | (used internally; no MCP surface)                     | `kg_build`, `kg_stats`, `kg_query`, `kg_ontology`, `kg_ia`, `kg_history`, `kg_callers_at_commit`, `trace_execution_flow` |
| Code review      | —                                                     | `code_review`, `review_repository`, `review_pull_request`                                                    |
| Narrative / docs | —                                                     | `wiki_generate`, `story_generate`                                                                            |
| Reporting        | —                                                     | `save_report`                                                                                                |

---

## Analysis tools: the trusty-analyzer separation

As of `trusty-search` v0.2.0 (issue #71), the `complexity_hotspots`,
`smells`, and `quality` HTTP endpoints were removed and `CodeChunk`
no longer carries `complexity_score`, `complexity`, or `blame` fields.
Those concerns moved to a sibling service,
[**trusty-analyzer**](https://github.com/bobmatnyc/trusty-analyzer), which
owns the canonical cyclomatic / Halstead / cognitive metrics and git-blame
integration.

This is a deliberate divergence from `mcp-vector-search`, which retains
analysis as part of the same MCP service. If you want analysis tools
**inside** Claude Code via MCP today, `mcp-vector-search` is the more
complete option; if you prefer composing two narrow services with sharper
contracts (search vs analysis), `trusty-search` + `trusty-analyzer` is the
intended pairing.

---

## Migration: `trusty-search convert`

`trusty-search` ships a first-class migration command for users coming
from `mcp-vector-search`:

```bash
trusty-search convert project          # git-style upward discovery from CWD
trusty-search convert all              # scan ~ depth 6, skip noise dirs
trusty-search convert all --dry-run    # preview
```

It reads each project's `.mcp-vector-search/config.json` and registers
the index against the running daemon. The command is idempotent (existing
indexes are detected via `{created: false}`) and bounds parallel migrations
with a `tokio::Semaphore` via `--concurrency`.

---

## Key advantages — trusty-search

1. **Zero cold-start** — HNSW pinned in RAM, model loaded once per machine
2. **Machine-wide** — one binary, one daemon, unlimited named indexes,
   one shared model across every Claude Code session on the box
3. **Query intent routing** — automatic α / β adjustment per query class;
   KG expansion gated to Usage intent only
4. **Single binary** — no Python runtime, no venv, `cargo install` standalone
5. **HTTP/2 reader-priority concurrency** — many concurrent queries, no
   lock contention
6. **Incremental reindex** — sha2 fingerprints skip unchanged files across
   daemon restarts
7. **Migration path** — `trusty-search convert` reads `mcp-vector-search`
   configs

## Key advantages — mcp-vector-search

1. **Rich analysis tools** — 28 MCP tools vs 11; complexity, smells,
   circular deps, test analysis
2. **Code review MCP tools** with SARIF output
3. **Temporal KG (KuzuDB)** — point-in-time traversal, `kg_callers_at_commit`
4. **Development narrative generation** — `story_generate`, `wiki_generate`
5. **Configurable embedding models** — CodeBERT, CodeT5+, MiniLM
6. **Cross-encoder reranking** (second-pass)
7. **Privacy policy auditor**
8. **Interactive D3.js visualization**
9. **Per-project index portability** — `.mcp-vector-search/` directory
   travels with the repo

---

## Stack summary

| Attribute       | trusty-search                                | mcp-vector-search                               |
|-----------------|----------------------------------------------|-------------------------------------------------|
| Language        | Rust 2021                                    | Python 3.11+                                    |
| Version         | 0.3.27                                       | 4.1.13                                          |
| Vector store    | usearch 2.25 (HNSW, in-memory)               | LanceDB (IVF-PQ, file-based)                    |
| Embedding       | fastembed + ONNX (all-MiniLM-L6-v2 INT8)     | sentence-transformers (configurable)            |
| BM25            | Custom zero-dep port                         | `rank-bm25` (`BM25Okapi`)                       |
| KG store        | petgraph (in-memory)                         | KuzuDB (persistent, temporal)                   |
| Code parsing    | tree-sitter (14 grammars)                    | tree-sitter-language-pack (13)                  |
| HTTP            | axum 0.7                                     | FastAPI + uvicorn                               |
| Fusion          | RRF k = 60 always-on                         | RRF k = 60 in `search_hybrid` only              |
| MCP tools       | 11                                           | 28                                              |
| Install         | `cargo install trusty-search`                | `pip install mcp-vector-search`                 |
| Daemon          | Yes — machine-wide, always-on                | Design doc only (not yet shipped)               |
| Cold-start      | ~95 ms (miss) / ~15 ms (hit)                 | 4–9 s CLI; 40–85 ms MCP server                  |
| Admin UI        | Embedded Svelte 5                            | D3.js visualization CLI                         |
| License         | MIT                                          | Elastic-2.0                                     |
