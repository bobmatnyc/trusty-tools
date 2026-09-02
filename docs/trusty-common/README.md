# trusty-common — documentation

The foundational shared library of the trusty-* workspace: tracing, daemon and
configuration helpers, model clients, memory/search support, ticketing, and
other cross-package utilities behind opt-in features. Some former micro-crates
were consolidated here; MCP protocol primitives were later extracted into
`trusty-mcp` under ADR-0040.

## Historical product baseline

The [**`spec/`**](spec/) set records the library's earlier consolidation model.
Use the [crate README](../../crates/trusty-common/README.md), accepted ADRs, and
source for the current feature and module surface:

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

This library has the following extended documentation:

| Subdir | Contents |
|--------|----------|
| [`spec/`](spec/) | Historical PRD / architecture / component baseline. |
| [`decisions/`](decisions/) | Architecture Decision Records (Nygard format). |
| [`regression-testing/`](regression-testing/) | Versioned performance/quality snapshots, baseline measurements, alternate-corpus baselines. |
| [`research/`](research/) | Investigation docs, audits, decision documents. |
| [`sessions/`](sessions/) | Engineering-session summaries — narrative + reasoning. |

See [ADR-0040](../adr/0040-trusty-mcp-services-absorbs-trusty-gworkspace.md)
for the later MCP extraction boundary.
