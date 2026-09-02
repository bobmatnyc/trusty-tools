# trusty-search — documentation

Machine-wide hybrid code-search service: BM25 + vector + knowledge-graph search
behind one always-on daemon and an MCP server. Crate lives in
`crates/trusty-search/`.

This directory is the extended index for trusty-search research, regression
testing, historical design, and engineering-session documentation. The crate
README and rustdoc are the current package entry points.

## Documentation map

| Subdir | What's here |
|--------|-------------|
| [`spec/`](spec/) | Historical PRD, architecture, and component baseline, indexed in [`spec/README.md`](spec/README.md). |
| [`research/`](research/) | Investigation, audit, and decision documents — BM25 memory, Candle/Metal validation, the nested-index fan-out RFC, NLP/ER/KG indexing, the staged-pipeline (stage-1 minimal, stage-3 KG, phase-3 async symbol-graph) decisions, and the trusty-search vs. mcp-vector-search comparison. Indexed in [`research/README.md`](research/README.md). |
| [`regression-testing/`](regression-testing/) | Versioned performance snapshots (`v{VERSION}-{DATE}.md`) plus alternate-corpus baselines (synthetic, open-mpm) and certification runs. [`current.md`](regression-testing/current.md) symlinks the latest snapshot. Methodology in [`regression-testing/README.md`](regression-testing/README.md). |
| [`decisions/`](decisions/) | Crate-specific Architecture Decision Records (Nygard format). Indexed in [`decisions/README.md`](decisions/README.md). |
| [`sessions/`](sessions/) | Engineering-session narratives (`SESSION-{DATE}-{topic}.md`). Indexed in [`sessions/README.md`](sessions/README.md). |
| [`examples/`](examples/) | Reference configurations: [`trusty-search.yaml`](examples/trusty-search.yaml) — multi-index per-repo config consumed by `trusty-search index`. |

## Where to start

- **What is trusty-search?** Start with the [crate README](../../crates/trusty-search/README.md); use the [behavior-contract catalog](../specs/README.md) for current target-state specs.
- **Performance / benchmarks?** [`regression-testing/README.md`](regression-testing/README.md) → [`regression-testing/current.md`](regression-testing/current.md).
- **Why a feature works the way it does?** [`research/README.md`](research/README.md).
- **Configuring multi-index repos?** [`examples/trusty-search.yaml`](examples/trusty-search.yaml).

## Conventions

Subdirs follow the workspace documentation conventions described in the root
[`CLAUDE.md`](../../CLAUDE.md). `research/` files are dated point-in-time
investigations preserved as-is; `regression-testing/` snapshots are tied to
released versions.
