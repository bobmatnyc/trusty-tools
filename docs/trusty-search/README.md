# trusty-search — documentation

Machine-wide hybrid code-search service: BM25 + vector + knowledge-graph search
behind one always-on daemon and an MCP server. Crate lives in
`crates/trusty-search/`. This is the **authoritative worked example** of the
per-crate documentation layout — other crates mirror its structure.

This directory is the single source of truth for trusty-search research,
regression testing, and engineering-session documentation. The crate
`README.md` and rustdoc stay in-crate
(see [ADR-0001](../adr/0001-docs-live-top-level.md)).

## Documentation map

| Subdir | What's here |
|--------|-------------|
| [`research/`](research/) | Investigation, audit, and decision documents — BM25 memory, Candle/Metal validation, the nested-index fan-out RFC, NLP/ER/KG indexing, the staged-pipeline (stage-1 minimal, stage-3 KG, phase-3 async symbol-graph) decisions, and the trusty-search vs. mcp-vector-search comparison. Indexed in [`research/README.md`](research/README.md). |
| [`regression-testing/`](regression-testing/) | Versioned performance snapshots (`v{VERSION}-{DATE}.md`) plus alternate-corpus baselines (synthetic, open-mpm) and certification runs. [`current.md`](regression-testing/current.md) symlinks the latest snapshot. Methodology in [`regression-testing/README.md`](regression-testing/README.md). |
| [`sessions/`](sessions/) | Engineering-session narratives (`SESSION-{DATE}-{topic}.md`). Indexed in [`sessions/README.md`](sessions/README.md). |
| [`examples/`](examples/) | Reference configurations: [`trusty-search.yaml`](examples/trusty-search.yaml) — multi-index per-repo config consumed by `trusty-search index`. |

## Where to start

- **Performance / benchmarks?** [`regression-testing/README.md`](regression-testing/README.md) → [`regression-testing/current.md`](regression-testing/current.md).
- **Why a feature works the way it does?** [`research/README.md`](research/README.md).
- **Configuring multi-index repos?** [`examples/trusty-search.yaml`](examples/trusty-search.yaml).

## Conventions

Subdirs follow the workspace documentation conventions described in the root
[`CLAUDE.md`](../../CLAUDE.md). `research/` files are dated point-in-time
investigations preserved as-is; `regression-testing/` snapshots are tied to
released versions.
