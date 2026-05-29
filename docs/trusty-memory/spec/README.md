# trusty-memory — Specification Set

> **Status:** Canonical · Living Document
> **Last reviewed:** 2026-05-29
> **Derived from:** code/docs/tickets audit

This directory holds the canonical product and engineering specification for the
`trusty-memory` crate (`crates/trusty-memory/`) and the storage core it sits on
top of (`trusty-common`'s `memory_core` feature). It is the single authoritative
reference for *what trusty-memory is meant to be*, *what it is today*, and *what
gaps remain*.

## What is trusty-memory?

`trusty-memory` is a **local, embedded "memory palace" for AI agents and humans**:
a long-running daemon that lets an MCP-aware client (Claude Code, open-mpm, or a
plain `curl`) **remember** natural-language facts and **recall** them later via
hybrid BM25 + vector retrieval, organized into per-project namespaces called
**palaces**. It bundles an optional knowledge-graph layer for structured triples,
an embedded Svelte admin dashboard, and a background "dream" consolidation cycle.
It is licensed **MIT** (most of the workspace is Elastic-2.0) and runs with **no
external services** — storage is pure-Rust redb + an in-process HNSW vector index
+ `fastembed` ONNX embeddings.

The defining structural fact is a **frontend/core split**: this crate is the
*MCP-server + HTTP + UI frontend*; all the storage, retrieval, knowledge-graph,
and dream logic lives in **`trusty-common`'s `memory_core` module** (gated behind
the `memory-core` feature). The historical `trusty-memory-core` crate has been
**fully absorbed** into `trusty-common` (issue #5); it no longer exists as a
separate workspace member. See [ARCHITECTURE.md §1](./ARCHITECTURE.md) and
[ADR-0001](../decisions/0001-frontend-core-split.md).

## Documents in this set

| Document | Read it when you want to know… |
|---|---|
| **[PRD.md](./PRD.md)** | The product: vision & mission, goals/non-goals, personas (agents + humans across projects), the full functional-requirement catalog (tagged by implementation status), success criteria, and the open-question roadmap. Start here for *why* and *what*. |
| **[ARCHITECTURE.md](./ARCHITECTURE.md)** | The system shape: the **frontend/core split**, the multi-transport model (MCP-stdio bridge ⇄ UDS ⇄ HTTP/SSE), the bundled `trusty-bm25-daemon` sidecar (one-install convention), the fire-and-forget `POST /api/v1/remember` path for sub-agents, the storage model (redb + HNSW + SQLite-legacy KG), the 4-layer (L0–L3) progressive retrieval, and the embedded-UI build. Includes the source-module map with `src/` citations. Start here for *how it fits together*. |
| **[COMPONENTS.md](./COMPONENTS.md)** | Per-subsystem specs: MCP server / tool surface, HTTP API, palace store, BM25 sidecar integration, retrieval & ranking, knowledge-graph + dream, the embedded UI, the `note` CLI, and import/export/migration. Each component states responsibility, key types/modules (with `src/` paths), current state, and known gaps. Start here for *the detail of one subsystem*. |

## Reading order

1. **New to trusty-memory?** PRD → ARCHITECTURE → COMPONENTS.
2. **Implementing a feature?** Jump to the relevant COMPONENTS section, then
   cross-check the transport/storage model in ARCHITECTURE.
3. **Evaluating product direction?** PRD vision + success criteria, then the
   gap callouts throughout COMPONENTS.

## Status legend (used throughout this set)

Every requirement and component is framed as **Vision / Current / Gap** and
tagged inline with one of:

| Tag | Meaning |
|---|---|
| ✅ **Implemented** | Built and working today. |
| 🟡 **Partial** | Partly built; usable but incomplete or with known caveats. |
| 🔵 **Designed-not-built** | Design exists (types, scaffolding, or plan) but no working path. |
| ⚪ **Aspirational** | North-star intent; no design committed yet. |

## Provenance & maintenance

These documents are derived from a direct audit of the `crates/trusty-memory/src/`
tree, the `trusty-common/src/memory_core/` storage core, the crate `README.md`,
the existing `docs/trusty-memory/` research/sessions, and the closed-issue backlog
(notably the redb migration sweep #43–#56, the BM25/embed sidecar work #155/#156/
#193, the multi-transport refactor #149/#150, inter-project messaging #99, and the
palace-as-project enforcement #88). When the code changes materially, update the
relevant document and bump the *Last reviewed* date. Source-path citations reflect
the layout at the time of review.

## Related decisions

- [ADR-0001 — Frontend/core split: trusty-memory ⇄ trusty-common `memory_core`](../decisions/0001-frontend-core-split.md)
