# Research & Decision Documents

This folder contains investigation docs, decision documents, audits, and performance research that inform the engineering trajectory of trusty-search and related components.

## Purpose

Research documents capture:
- **Investigation artifacts**: Deep-dive analysis, validation runs, audits
- **Decision rationale**: Why we chose one approach over another
- **Trade-off analysis**: Performance vs. maintainability, accuracy vs. latency
- **Experimental findings**: Results that shaped architectural decisions

These are distinct from [regression-testing/](../regression-testing/) (which tracks NUMBER snapshots per release) and [sessions/](../sessions/) (which capture NARRATIVE trajectory of engineering work).

## Naming Convention

- Investigation / audit docs: `<topic>-<YYYY-MM-DD>.md`
  - Example: `candle-metal-validation-2026-05-22.md`
- Decision documents: `<topic>-decision-<YYYY-MM-DD>.md`
  - Example: `stage-3-kg-decision-2026-05-25.md`

## Documents

### Validation / Audits

- [`candle-metal-validation-2026-05-22.md`](candle-metal-validation-2026-05-22.md) — Candle vs ORT-CoreML embedder validation on Apple Silicon.
- [`bm25-memory-2026-05-28.md`](bm25-memory-2026-05-28.md) — BM25 index memory-footprint investigation.
- [`fjall-evaluation-2026-06-03.md`](fjall-evaluation-2026-06-03.md) — fjall LSM-tree vs. redb CoW B-tree: deeper evaluation and benchmark spike plan (refs #692, #694).

### Decision Documents

- [`stage-1-minimal-2026-05-27.md`](stage-1-minimal-2026-05-27.md) — Stage-1 minimal pipeline decision.
- [`stage-3-kg-decision-2026-05-25.md`](stage-3-kg-decision-2026-05-25.md) — Stage-3 knowledge-graph indexing decision.
- [`phase3-async-symbol-graph-decision-2026-05-27.md`](phase3-async-symbol-graph-decision-2026-05-27.md) — Phase-3 async symbol-graph decision.
- [`nested-index-fanout-rfc-2026-05-29.md`](nested-index-fanout-rfc-2026-05-29.md) — RFC: nested-index graph + fan-out prioritization.

### Comparisons & Integration (migrated from in-crate `docs/`)

- [`trusty-search-vs-mcp-vector-search-2026-05-12.md`](trusty-search-vs-mcp-vector-search-2026-05-12.md) — Feature/performance comparison against mcp-vector-search.
- [`mcp-vector-search-integration.md`](mcp-vector-search-integration.md) — Integration notes for mcp-vector-search.
- [`nlp-er-kg-indexing.md`](nlp-er-kg-indexing.md) — NLP / entity-resolution / knowledge-graph indexing investigation.

---

**Related**: [Regression Testing Index](../regression-testing/README.md) | [Session Summaries Index](../sessions/README.md)
