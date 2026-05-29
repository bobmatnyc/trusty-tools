# trusty-common — documentation

The **foundational shared library** of the trusty-* workspace — the one internal
crate every other trusty-* tool links. Started as a bag of utilities (tracing,
OpenRouter chat, port-walking, daemon address resolution) and absorbed seven
formerly standalone micro-crates (MCP/RPC primitives, the embedder, the symbol
graph, the memory-palace engine, ticketing, monitor TUIs) behind opt-in feature
flags (issue #5).

## Canonical specification

The authoritative *what / why / gap* reference lives in
[**`spec/`**](spec/) — start there:

| Document | Purpose |
|--------|---------|
| [`spec/README.md`](spec/README.md) | Index, summary, status legend, reading order. |
| [`spec/PRD.md`](spec/PRD.md) | Vision, goals/non-goals, personas (the other crates), functional requirements by subsystem. |
| [`spec/ARCHITECTURE.md`](spec/ARCHITECTURE.md) | The feature-flag model, cross-crate consumption, design conventions, module map. |
| [`spec/COMPONENTS.md`](spec/COMPONENTS.md) | Per-subsystem specs with `src/` citations. |

Architecture Decision Records (Nygard format) live in
[**`decisions/`**](decisions/) — see
[`0001-consolidate-library-micro-crates.md`](decisions/0001-consolidate-library-micro-crates.md).

## Layout

This directory follows the standard layout used across all published trusty-*
crates:

| Subdir | Contents |
|--------|----------|
| [`spec/`](spec/) | Canonical PRD / Architecture / Components specification set. |
| [`decisions/`](decisions/) | Architecture Decision Records (Nygard format). |
| [`regression-testing/`](regression-testing/) | Versioned performance/quality snapshots, baseline measurements, alternate-corpus baselines. |
| [`research/`](research/) | Investigation docs, audits, decision documents. |
| [`sessions/`](sessions/) | Engineering-session summaries — narrative + reasoning. |

See [`docs/trusty-search/`](../trusty-search/) for a worked example of the
populated layout.
