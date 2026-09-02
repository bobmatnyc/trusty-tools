# trusty-memory — documentation

Memory palace storage daemon + MCP server frontend. The storage engine lives in
`crates/trusty-common`'s `memory_core` module (behind the `memory-core` feature
flag, absorbing the former `trusty-memory-core` crate); the frontend is
`crates/trusty-memory`.

## Historical product baseline

The [`spec/`](spec/) set records an earlier product and engineering model. Use
the [crate README](../../crates/trusty-memory/README.md), current behavior
contracts, and source for implemented behavior:

| Document | Covers |
|---|---|
| [`spec/README.md`](spec/README.md) | Index, one-paragraph summary, status legend, reading order. |
| [`spec/PRD.md`](spec/PRD.md) | Vision/mission, goals/non-goals, personas, the full status-tagged functional-requirement catalog, success criteria, roadmap. |
| [`spec/ARCHITECTURE.md`](spec/ARCHITECTURE.md) | The frontend/core split, multi-transport model, BM25 sidecar bundling, fire-and-forget remember path, storage model, L0–L3 retrieval, source-module map. |
| [`spec/COMPONENTS.md`](spec/COMPONENTS.md) | Per-subsystem specs (MCP server, HTTP API, palace store, BM25 sidecar, retrieval, KG/dream, UI, `note` CLI, migration) with `src/` citations. |

Architectural decision records live in [`decisions/`](decisions/):

- [ADR-0001 — Frontend/core split: trusty-memory ⇄ trusty-common `memory_core`](decisions/0001-frontend-core-split.md)

## Layout

This product has the following extended documentation:

| Subdir | Contents |
|--------|----------|
| [`spec/`](spec/) | Historical PRD / architecture / component baseline. |
| [`decisions/`](decisions/) | Architectural decision records (Nygard format). |
| [`regression-testing/`](regression-testing/) | Versioned performance/quality snapshots, baseline measurements, alternate-corpus baselines. |
| [`research/`](research/) | Investigation docs, audits, decision documents. |
| [`sessions/`](sessions/) | Engineering-session summaries — narrative + reasoning. |

## Status

The historical [`spec/`](spec/) set came from a 2026-05-29 audit. New current
requirements belong in the workspace behavior-contract catalog; dated
benchmarks and investigations stay in their evidence directories.
