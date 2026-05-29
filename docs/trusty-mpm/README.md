# trusty-mpm — documentation

MPM platform — 8 binary crates: `trusty-mpm-core`, `-mcp`, `-daemon`, `-client`, `-cli`, `-tui`, `-telegram`, `-gui`. Docs covering any of the eight live here.

## Layout

This directory follows the standard three-subdir layout used across all
published trusty-* crates:

| Subdir | Contents |
|--------|----------|
| [`regression-testing/`](regression-testing/) | Versioned performance/quality snapshots, baseline measurements, alternate-corpus baselines. |
| [`research/`](research/) | Investigation docs, audits, decision documents. |
| [`sessions/`](sessions/) | Engineering-session summaries — narrative + reasoning. |

## Status

Foundational design docs live under [`research/`](research/):

- [Product Requirements Document (reconstructed)](research/prd-2026-05-29.md)
- [Architecture & technical specification (reconstructed)](research/architecture-spec-2026-05-29.md)
- [`tm services` discovery spec](research/tm-services-discovery-spec-2026-05-28.md)

As work on trusty-mpm produces benchmarks, decisions, or session summaries, add files
under the appropriate subdir and update its README index.

See [`docs/trusty-search/`](../trusty-search/) for a worked example of the
populated layout.
