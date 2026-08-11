# trusty-mpm — documentation

Unified MPM platform — one crate (`crates/trusty-mpm`) with two identically-
behaving `[[bin]]` targets, `tm` and `trusty-mpm`. There is no separate
daemon binary: long-running daemon mode is the `tm daemon` / `trusty-mpm
daemon` subcommand of that same binary, and the TUI (`tm tui`) and Telegram
bot (`tm telegram`) are subcommands too, not standalone executables. The
Tauri GUI lives in the sibling `trusty-mpm-gui` crate and is wrapped via the
optional `gui` feature. Docs covering any surface live here.

## Canonical specification

The authoritative product + engineering spec for trusty-mpm lives in
[**`spec/`**](spec/):

- [spec/README.md](spec/README.md) — index, status legend, reading order, and the
  trusty-agents relationship.
- [spec/PRD.md](spec/PRD.md) — vision, personas, status-tagged functional
  requirements grouped by surface.
- [spec/ARCHITECTURE.md](spec/ARCHITECTURE.md) — multi-binary topology,
  single-daemon coordination, MCP framing, HTTP API, filesystem layout.
- [spec/COMPONENTS.md](spec/COMPONENTS.md) — per-binary and per-subsystem specs.

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
