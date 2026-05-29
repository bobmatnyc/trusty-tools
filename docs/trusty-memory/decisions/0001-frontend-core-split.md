# 0001. Frontend/core split: trusty-memory ⇄ trusty-common `memory_core`

- **Status:** Accepted
- **Date:** 2026-05-29
- **Scope:** Crates `trusty-memory` and `trusty-common`
- **Supersedes / Superseded by:** Absorbs the former `trusty-memory-core` crate (issue #5)

## Context

The Memory Palace has two distinct concerns: (1) the **storage and retrieval
engine** — the palace data model, the redb/HNSW vector store, the temporal
knowledge graph, the dream/consolidation loop, and the embedder — and (2) the
**frontend** that exposes that engine to the world over MCP, HTTP/SSE, a Unix
domain socket, an embedded Svelte dashboard, and a CLI.

Originally the engine lived in a separate published crate, `trusty-memory-core`,
imported by `trusty-memory`. That created a published-crate boundary and an extra
member to version and release. Meanwhile, two very different consumers needed the
engine:

- The **standalone daemon** (`trusty-memory serve`) needs the full HTTP/axum
  surface, the UI, and the BM25 sidecar.
- **open-mpm** links the engine **in-process** as an rlib only to register
  `MemoryMcpService` for a merged `rpc.discover` document — it never binds an HTTP
  socket, so dragging axum + tower-http into its dependency tree was pure cost
  (issue #226).

The engine also pulls heavy storage dependencies (`redb`, an HNSW implementation,
`tiktoken-rs`, `git2`) that not every `trusty-common` consumer wants.

## Decision

1. **Absorb `trusty-memory-core` into `trusty-common`** as the `memory_core`
   module, gated behind the **`memory-core` feature** (issue #5, phase 2d). The
   trusty-* toolchain then links one internal library and ships one fewer
   published crate. The former crate's submodules (`palace`, `registry`,
   `retrieval`, `store/*`, `dream`, `semantic_consolidation`, `filter`, `decay`,
   `community`, `analytics`, `git`, `embed`) move under
   `trusty-common/src/memory_core/` unchanged, keeping their tests.

2. **Keep `trusty-memory` as a thin frontend** over that core: the MCP tool
   surface (`tools.rs`), the HTTP/SSE + REST + `/rpc` server (`web.rs`,
   `transport/`), the embedded UI, the CLI (`commands/*`), and the BM25 sidecar
   supervisor. The frontend depends on
   `trusty-common = { features = ["mcp", "memory-core", …] }`.

3. **Gate the HTTP surface behind `axum-server`** (default on) so open-mpm can
   link `trusty-memory` with `default-features = false` and get `MemoryMcpService`
   *without* axum/tower-http (issue #226). The standalone binaries keep the default.

## Consequences

- **Positive — one engine, two consumption modes:** the same storage/retrieval
  code is either a standalone daemon *or* an in-process library, selected purely by
  Cargo features, with no code fork.
- **Positive — fewer published crates:** `trusty-memory-core` is gone; only
  `trusty-common` (engine) and `trusty-memory` (frontend) are versioned/released.
- **Positive — lean embedding:** open-mpm's dependency tree drops axum + tower-http
  and (via `memory-core` gating) only pulls the storage deps it actually links.
- **Positive — clean transport boundary:** because the engine knows nothing about
  HTTP/MCP, the frontend can add transports (UDS, `/rpc`, future gRPC) without
  touching the core.
- **Known doc drift:** the workspace-root `CLAUDE.md` still lists
  `crates/trusty-memory-core/` as a shim crate, but the directory no longer exists
  on disk — the engine lives entirely in `trusty-common::memory_core`. Treat
  `memory_core/mod.rs` and this ADR as authoritative.
- **Neutral — feature discipline required:** consumers must remember to enable the
  `memory-core` feature (and `axum-server` for the daemon path); a missing feature
  surfaces as link errors rather than a clear message.
