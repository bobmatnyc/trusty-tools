# trusty-search — Specification Set

> **Status:** Canonical · Living Document
> **Last reviewed:** 2026-05-29
> **Derived from:** code/docs/tickets audit

This directory holds the canonical product and engineering specification for the
`trusty-search` crate (`crates/trusty-search/`). It is the single authoritative
reference for *what trusty-search is meant to be*, *what it is today*, and *what
gaps remain*.

## What is trusty-search?

`trusty-search` is a **machine-wide hybrid code-search service**: one install per
machine (`cargo install trusty-search`), one always-on HTTP daemon, and an
unlimited number of named per-project indexes managed concurrently from a single
`DashMap<IndexId, Arc<IndexHandle>>`. Each query is classified by intent (a
sub-millisecond regex classifier) and then run across **three search lanes —
BM25 lexical, HNSW vector-semantic, and a tree-sitter-derived knowledge graph
(KG)** — whose ranked outputs are fused, parameter-free, via Reciprocal Rank
Fusion (RRF, k = 60). The daemon exposes the same pipeline over **three
surfaces**: a REST HTTP API (axum, loopback-only), an MCP server (JSON-RPC 2.0
over stdio + HTTP/SSE, for Claude Code), and a `clap`-based CLI. Embedding runs
in a **bundled sidecar process** (`trusty-embedderd`) that `cargo install
trusty-search` installs alongside the main binary, keeping the ONNX/CoreML arena
out of the search daemon's address space. Everything is local, embedded, and
service-free: redb for the durable chunk corpus, usearch for the in-memory HNSW
graph, fastembed/ONNX (all-MiniLM-L6-v2 INT8, 384-dim) for embeddings, petgraph
for the symbol graph.

## Documents in this set

| Document | Read it when you want to know… |
|---|---|
| **[PRD.md](./PRD.md)** | The product: vision & mission, goals/non-goals, personas, the full functional-requirement catalog grouped by capability (indexing, lexical, semantic, KG, hybrid ranking, MCP, HTTP, CLI, daemon lifecycle, memory/auto-tuning, UI) and tagged by implementation status, success criteria & differentiators, and the open-question roadmap. Start here for *why* and *what*. |
| **[ARCHITECTURE.md](./ARCHITECTURE.md)** | The system shape: daemon/registry topology, the staged indexing pipeline, the `trusty-embedderd` sidecar bundling + single-install relationship, storage (redb corpus + usearch HNSW + petgraph KG), the query-dispatch pipeline (classify → route → search → fuse → expand), MCP/HTTP framing (stdout reserved for JSON-RPC, logs to stderr), the memory auto-tuning `TRUSTY_*` env knobs, and the CoreML/ONNX/CUDA execution-provider paths. Includes the source-module map with `src/` citations. Start here for *how it fits together*. |
| **[COMPONENTS.md](./COMPONENTS.md)** | Per-subsystem specs: indexer/pipeline, lexical (BM25), vector (HNSW + embedder), KG (symbol graph), ranker (RRF + MMR + intent routing), MCP server, HTTP API, CLI, daemon lifecycle, embedder-sidecar integration, and the embedded Svelte UI. Each states responsibility, key types/modules (with `src/` paths), current state, and known gaps. Start here for *the detail of one subsystem*. |

## Reading order

1. **New to trusty-search?** PRD → ARCHITECTURE → COMPONENTS.
2. **Implementing a feature?** Jump to the relevant COMPONENTS section, then
   cross-check the dispatch pipeline in ARCHITECTURE.
3. **Evaluating product direction?** PRD vision + success criteria, then the gap
   callouts throughout COMPONENTS.

## Status legend (used throughout this set)

Every requirement and component is framed as **Vision / Current / Gap** and
tagged inline with one of:

| Tag | Meaning |
|---|---|
| ✅ **Implemented** | Built and working today. |
| 🟡 **Partial** | Partly built; usable but incomplete or with known caveats. |
| 🔵 **Designed-not-built** | Design exists (types, scaffolding, RFC, or plan) but no working path. |
| ⚪ **Aspirational** | North-star intent; no design committed yet. |

## Related documentation

This `spec/` set is the *what/why/gap* layer. The point-in-time and operational
docs live alongside it:

- **[../research/](../research/)** — dated investigations and decision documents
  (BM25 memory, Candle/Metal validation, the nested-index fan-out RFC #404, the
  staged-pipeline decisions, the trusty-search vs. mcp-vector-search comparison).
- **[../regression-testing/](../regression-testing/)** — versioned performance
  snapshots and alternate-corpus baselines; [`current.md`](../regression-testing/current.md)
  symlinks the latest.
- **[../sessions/](../sessions/)** — engineering-session narratives.
- **[../examples/trusty-search.yaml](../examples/trusty-search.yaml)** —
  multi-index per-repo config.
- **[crates/trusty-search/README.md](../../../crates/trusty-search/README.md)**
  and **[crates/trusty-search/CLAUDE.md](../../../crates/trusty-search/CLAUDE.md)**
  — in-crate quick-start, HTTP endpoint catalogue, and release process.

## Provenance & maintenance

These documents are derived from an audit of the `crates/trusty-search/src/`
tree (the single-crate `core` / `service` / `mcp` module layout as of v0.18.0),
the crate `README.md` / `CLAUDE.md`, and the open/closed issue backlog (notably
the cross-release performance tracker [#129](https://github.com/bobmatnyc/trusty-tools/issues/129),
the `.trusty-search/` co-located-storage work [#403](https://github.com/bobmatnyc/trusty-tools/issues/403),
and the nested-index fan-out RFC [#404](https://github.com/bobmatnyc/trusty-tools/issues/404)).
When the code changes materially, update the relevant document and bump the
*Last reviewed* date. Source-path citations reflect the layout at the time of
review.

> **Note on in-crate `CLAUDE.md` drift:** the crate-local
> `crates/trusty-search/CLAUDE.md` predates the workspace consolidation and
> still references the pre-monorepo crate names (`trusty-search-core`,
> `trusty-embedder`, `trusty-mcp-core`) and per-crate test paths. The authoritative
> layout is the single `trusty-search` crate with `core` / `service` / `mcp`
> modules plus the bundled `trusty-embedderd` sidecar — this spec set reflects
> the audited tree, not the stale CLAUDE.md.
