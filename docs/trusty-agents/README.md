# trusty-agents — documentation

`trusty-agents` (`crates/trusty-agents/`, binary `tagent`) is a Rust-native AI
agent orchestration harness: a long-running **CTRL** controller coordinates
per-project **PM** actors, each of which delegates work to specialized
**sub-agents** that run either in-process (fast, read-only) or as isolated OS
subprocesses communicating over NDJSON IPC. Its defining differentiator is
**model-agnostic dispatch** — any agent role can be backed by OpenRouter, the
direct Anthropic API, AWS Bedrock, or the `claude` CLI OAuth path, assignable
per-agent via configuration. trusty-agents integrates with trusty-search and
trusty-memory and reuses the workspace's common MCP and agent libraries.

This directory is the extended index for trusty-agents design, research, and
user/developer documentation. Implemented behavior remains code-owned; start
with the crate README and current workspace behavior contracts.

## Documentation map

| Subdir | What's here |
|--------|-------------|
| [`spec/`](spec/) | Historical pre-rename PRD, architecture, and component baseline. |
| [`research/`](research/) | ~70 investigation, audit, and design docs that shaped trusty-agents — frameworks, IPC patterns, dispatch, token compression, UI surfaces, bug analyses. Indexed in [`research/README.md`](research/README.md). |
| [`design/`](design/) | Focused design notes: workflow engine, CTRL REPL, design goals. Visual assets (icon, treatment PDF) live in [`design/visual/`](design/visual/). |
| [`developer/`](developer/) | Contributor docs: architecture overview, building, contributing, testing. |
| [`user/`](user/) | End-user docs: quickstart, CLI reference, configuration, agents & skills. |
| [`architecture/`](architecture/) | Cross-cutting architecture notes: agent/skill design, drift detection. |
| [`regression-testing/`](regression-testing/) | Performance baselines, bake-off comparisons, and the per-run telemetry tooling (`analyze.py`, `runs.log`). See [`PERFORMANCE.md`](regression-testing/PERFORMANCE.md) for the run-file schema. |
| [`sessions/`](sessions/) | Engineering-session narratives and end-to-end user-story walkthroughs. |
| [`decisions/`](decisions/) | **Crate-specific ADRs** (Nygard format). Workspace-wide ADRs live in [`docs/adr/`](../adr/). |

## Where to start

- **New to trusty-agents?** Start with the [crate README](../../crates/trusty-agents/README.md) and [current product spec](../specs/trusty-agents-product-spec.md).
- **Installing / using it?** [`user/quickstart.md`](user/quickstart.md).
- **Contributing?** [`developer/contributing.md`](developer/contributing.md) and [`developer/building.md`](developer/building.md).
- **Understanding a past decision?** [`decisions/`](decisions/) (crate-specific) or [`docs/adr/`](../adr/) (workspace-wide).

## Conventions

Subdirs follow the workspace documentation conventions described in the root
[`CLAUDE.md`](../../CLAUDE.md). The legacy `spec/` set and `research/` files are
point-in-time evidence; current behavior is established by source, current
READMEs, ADRs, and workspace behavior contracts.
