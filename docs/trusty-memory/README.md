# trusty-memory — documentation

Memory palace storage daemon + MCP server frontend (`crates/trusty-memory-core` + `crates/trusty-memory`).

## Canonical specification

The authoritative product + engineering specification lives in
[`spec/`](spec/) — start here for *what trusty-memory is, what it does today, and
what gaps remain*:

| Document | Covers |
|---|---|
| [`spec/README.md`](spec/README.md) | Index, one-paragraph summary, status legend, reading order. |
| [`spec/PRD.md`](spec/PRD.md) | Vision/mission, goals/non-goals, personas, the full status-tagged functional-requirement catalog, success criteria, roadmap. |
| [`spec/ARCHITECTURE.md`](spec/ARCHITECTURE.md) | The frontend/core split, multi-transport model, BM25 sidecar bundling, fire-and-forget remember path, storage model, L0–L3 retrieval, source-module map. |
| [`spec/COMPONENTS.md`](spec/COMPONENTS.md) | Per-subsystem specs (MCP server, HTTP API, palace store, BM25 sidecar, retrieval, KG/dream, UI, `note` CLI, migration) with `src/` citations. |

Architectural decision records live in [`decisions/`](decisions/):

- [ADR-0001 — Frontend/core split: trusty-memory ⇄ trusty-common `memory_core`](decisions/0001-frontend-core-split.md)

## Layout

This directory follows the standard three-subdir layout used across all
published trusty-* crates:

| Subdir | Contents |
|--------|----------|
| [`spec/`](spec/) | Canonical product + engineering specification (PRD / ARCHITECTURE / COMPONENTS). |
| [`decisions/`](decisions/) | Architectural decision records (Nygard format). |
| [`regression-testing/`](regression-testing/) | Versioned performance/quality snapshots, baseline measurements, alternate-corpus baselines. |
| [`research/`](research/) | Investigation docs, audits, decision documents. |
| [`sessions/`](sessions/) | Engineering-session summaries — narrative + reasoning. |

## Status

The canonical [`spec/`](spec/) set has been authored from a code/docs/tickets
audit (2026-05-29). As work on trusty-memory produces benchmarks or session
summaries, add files under the appropriate subdir and update its README index.

See [`docs/trusty-search/`](../trusty-search/) for a worked example of the
populated layout.
