# trusty-common — Specification Set

> **Status:** Canonical · Living Document
> **Last reviewed:** 2026-05-29
> **Derived from:** code/docs/tickets audit

This directory holds the canonical product and engineering specification for the
`trusty-common` crate (`crates/trusty-common/`). It is the single authoritative
reference for *what trusty-common is meant to be*, *what it is today*, and *what
gaps remain*.

## What is trusty-common?

`trusty-common` is the **foundational shared library of the trusty-\* workspace**:
the one internal crate every other trusty-* tool links. It started as a thin bag
of utilities (port-walking, data-dir resolution, tracing init, an OpenRouter chat
helper) and, under the workspace-consolidation effort (issue
[#5](https://github.com/bobmatnyc/trusty-tools/issues/5)), **absorbed seven
formerly standalone micro-crates** — `trusty-mcp-core`, `trusty-rpc`,
`trusty-embedder`, `trusty-symgraph`, `trusty-memory-core`, `trusty-tickets`, and
`trusty-monitor-tui` — into one heavily **feature-gated** crate. The headline
guarantee: **the default build is dependency-light** (`tokio`, `serde`,
`reqwest`, `tracing`, `sysinfo`, `dirs`), and every heavy subsystem (ONNX
embedder, tree-sitter symbol graph, redb/HNSW memory engine, axum HTTP stack,
ratatui TUI) is gated behind an opt-in feature so a consumer pays only for what it
imports.

Everything here is a **library**: pure free functions and small helper structs,
**no global state** except the idempotent tracing subscriber, `thiserror` error
enums (not `anyhow`) for the library-facing surfaces, and logs always to
**stderr** so stdout stays clean for MCP JSON-RPC framing. It is edition 2021,
licensed Elastic-2.0, and currently at version **0.8.0**.

## Documents in this set

| Document | Read it when you want to know… |
|---|---|
| **[PRD.md](./PRD.md)** | The product: vision & mission (the shared foundation + the consolidation of seven micro-crates), goals/non-goals (no unconditional axum dep, pure functions, no global state, API keys passed in not read from env), the personas (the *other* crates are the users), the full functional-requirement catalog grouped by subsystem (core utilities, tracing/logging, chat/OpenRouter, MCP, RPC, embedder, embedder-client, BM25, memory-core, symgraph, migrations, tickets, help, monitor, setup/launchd) and tagged by status, success criteria, and open questions. Start here for *why* and *what*. |
| **[ARCHITECTURE.md](./ARCHITECTURE.md)** | The system shape: the **feature-flag model** (the full dependency-gating matrix), how `memory-core` is consumed by `trusty-memory`, the chat-helper design (API key passed in, never read from env in lib), tracing-to-stderr, the `thiserror`-vs-`anyhow` error convention, and the single tree-sitter `links =` slot constraint. Includes the source-module map with `src/` citations. Start here for *how it fits together*. |
| **[COMPONENTS.md](./COMPONENTS.md)** | Per-module specs: every subsystem (utilities, tracing/log-buffer, sys-metrics, chat, MCP, RPC, embedder, embedder-client, BM25, BM25-client, memory-core, symgraph, migrations, tickets, help, monitor, setup helpers). Each states responsibility, key types (with `src/` paths), current state, and known gaps. Start here for *the detail of one subsystem*. |

## Reading order

1. **New to trusty-common?** PRD → ARCHITECTURE → COMPONENTS.
2. **Adding a feature to one subsystem?** Jump to the relevant COMPONENTS
   section, then cross-check the feature-flag matrix in ARCHITECTURE.
3. **Deciding whether to add a dependency?** ARCHITECTURE feature-flag model +
   the Non-Goals in PRD §2.

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

- **[../decisions/](../decisions/)** — Architecture Decision Records (Nygard
  format). [`0001-consolidate-library-micro-crates.md`](../decisions/0001-consolidate-library-micro-crates.md)
  captures the issue-#5 absorption of seven standalone crates behind feature
  flags.
- **[../research/](../research/)** — dated investigations and decision documents.
- **[../regression-testing/](../regression-testing/)** — versioned performance
  snapshots.
- **[../sessions/](../sessions/)** — engineering-session narratives.
- **[crates/trusty-common/README.md](../../../crates/trusty-common/README.md)** —
  in-crate quick-start, feature-flag table, and usage snippets.

## Provenance & maintenance

These documents are derived from an audit of the `crates/trusty-common/src/`
tree (93 source files across 17 feature-gated subsystems as of v0.8.0), the crate
`README.md` and `Cargo.toml` `[features]` table, the root `CLAUDE.md`
conventions, and the open/closed issue backlog — notably the consolidation
tracker [#5](https://github.com/bobmatnyc/trusty-tools/issues/5), the migration
kernel [#179](https://github.com/bobmatnyc/trusty-tools/issues/179), the CLI-help
system [#216](https://github.com/bobmatnyc/trusty-tools/issues/216), the
embedder-process split [#110](https://github.com/bobmatnyc/trusty-tools/issues/110),
and the monitor-TUI consolidation [#31](https://github.com/bobmatnyc/trusty-tools/issues/31)
/ [#34](https://github.com/bobmatnyc/trusty-tools/issues/34). When the code
changes materially, update the relevant document and bump the *Last reviewed*
date. Source-path citations reflect the layout at the time of review.

> **Note on in-crate `README.md` drift:** the crate-local
> `crates/trusty-common/README.md` predates several absorptions — it documents the
> crate at version `0.3` and omits the `memory-core`, `monitor-tui`,
> `embedder-client`, `tickets`, and `cli-help` features that the audited
> `Cargo.toml` (v0.8.0) actually ships. This spec set reflects the audited tree,
> not the stale README header.
