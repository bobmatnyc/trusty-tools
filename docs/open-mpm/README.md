# open-mpm — documentation

`open-mpm` (`crates/open-mpm/`) is a Rust-native AI agent orchestration harness:
a long-running **CTRL** controller coordinates per-project **PM** actors, each of
which delegates work to specialized **sub-agents** that run either in-process
(fast, read-only) or as isolated OS subprocesses communicating over NDJSON IPC.
Its defining differentiator is **model-agnostic dispatch** — any agent role can
be backed by OpenRouter, the direct Anthropic API, AWS Bedrock, or the `claude`
CLI OAuth path, assignable per-agent via a two-line TOML change. open-mpm
consumes the shared trusty-* libraries (trusty-search, trusty-memory-core,
trusty-symgraph).

This directory is the **single source of truth** for open-mpm design, research,
specification, and user/developer documentation. (Rustdoc and the crate
`README.md` stay in-crate; see [ADR-0001](../adr/0001-docs-live-top-level.md).)

## Documentation map

| Subdir | What's here |
|--------|-------------|
| [`spec/`](spec/) | Canonical specification set: **PRD**, **ARCHITECTURE**, **COMPONENTS**. Start here for *what open-mpm is, how it fits together, and the per-subsystem detail*. |
| [`research/`](research/) | ~70 investigation, audit, and design docs that shaped open-mpm — frameworks, IPC patterns, dispatch, token compression, UI surfaces, bug analyses. Indexed in [`research/README.md`](research/README.md). |
| [`design/`](design/) | Focused design notes: workflow engine, CTRL REPL, design goals. Visual assets (icon, treatment PDF) live in [`design/visual/`](design/visual/). |
| [`developer/`](developer/) | Contributor docs: architecture overview, building, contributing, testing. |
| [`user/`](user/) | End-user docs: quickstart, CLI reference, configuration, agents & skills. |
| [`architecture/`](architecture/) | Cross-cutting architecture notes: agent/skill design, drift detection. |
| [`regression-testing/`](regression-testing/) | Performance baselines, bake-off comparisons, and the per-run telemetry tooling (`analyze.py`, `runs.log`). See [`PERFORMANCE.md`](regression-testing/PERFORMANCE.md) for the run-file schema. |
| [`sessions/`](sessions/) | Engineering-session narratives and end-to-end user-story walkthroughs. |
| [`decisions/`](decisions/) | **Crate-specific ADRs** (Nygard format). Workspace-wide ADRs live in [`docs/adr/`](../adr/). |

## Where to start

- **New to open-mpm?** [`spec/PRD.md`](spec/PRD.md) → [`spec/ARCHITECTURE.md`](spec/ARCHITECTURE.md) → [`spec/COMPONENTS.md`](spec/COMPONENTS.md).
- **Installing / using it?** [`user/quickstart.md`](user/quickstart.md).
- **Contributing?** [`developer/contributing.md`](developer/contributing.md) and [`developer/building.md`](developer/building.md).
- **Understanding a past decision?** [`decisions/`](decisions/) (crate-specific) or [`docs/adr/`](../adr/) (workspace-wide).

## Conventions

Subdirs follow the workspace documentation conventions described in the root
[`CLAUDE.md`](../../CLAUDE.md). The `spec/` set is a living document; `research/`
files are dated point-in-time investigations preserved as-is.
