# trusty-analyze — Specification Set

> **Status:** Canonical · Living Document
> **Last reviewed:** 2026-06-01
> **Derived from:** code/docs/tickets audit (drift audit v0.4.1)

This directory holds the canonical product and engineering specification for the
`trusty-analyze` crate (`crates/trusty-analyze/`). It is the single authoritative
reference for *what trusty-analyze is meant to be*, *what it is today*, and *what
gaps remain*.

## What is trusty-analyze?

`trusty-analyze` is a **local code-quality analysis sidecar**: a daemon + MCP
server that fetches a project's chunk corpus from the [`trusty-search`](../../trusty-search/)
daemon over HTTP and computes **complexity, code smells, quality grades, a
language-neutral knowledge graph, concept clusters, refactor suggestions, and
diff/PR reviews** — every result reachable both as an HTTP endpoint and as an
MCP tool (strict parity). Structural analysis is built on **tree-sitter** (14
language adapters) with optional **external-linter** integration (clippy, ruff,
biome, …, 10 tools) and **SCIP** ingest for LSP-grade symbol resolution.
Persistence is a single embedded **redb** facts store; everything is local,
service-free, and MIT-licensed. trusty-search is a **hard runtime dependency** —
there is no offline mode; the daemon refuses to start if the search daemon is
unreachable.

## Documents in this set

| Document | Read it when you want to know… |
|---|---|
| **[PRD.md](./PRD.md)** | The product: vision & mission, goals/non-goals, personas (LLM agents, devs, CI), the full functional-requirement catalog grouped by capability (analysis engine, complexity metrics, smell detection, language/AST support, knowledge graph, clustering, review/LLM, MCP, HTTP/daemon, output/reporting) and tagged by implementation status, success criteria, and the open-question roadmap. Start here for *why* and *what*. |
| **[ARCHITECTURE.md](./ARCHITECTURE.md)** | The system shape: the sidecar topology and hard runtime dependency on trusty-search, the chunk-fetch → analyze → serve pipeline, the tree-sitter AST substrate and text-heuristic fallback, the knowledge-graph schema and cross-chunk linker, MCP stdio + HTTP/SSE framing (stdout reserved for JSON-RPC, logs to stderr), the `http-server` feature gate, and the daemon lifecycle. Includes the source-module map with `src/` citations. Start here for *how it fits together*. |
| **[COMPONENTS.md](./COMPONENTS.md)** | Per-subsystem specs: HTTP client, complexity (text + tree-sitter), smells/quality, the language-adapter registry, the knowledge graph + linker + SCIP, concept clustering + embedders, refactor/review/deep-analysis/GitHub, the facts store, the external-tool registry, NER, the MCP server, the HTTP daemon + embedded UI, and the CLI/daemon lifecycle. Each states responsibility, key types/modules (with `src/` paths), current state, and known gaps. Start here for *the detail of one subsystem*. |

## Reading order

1. **New to trusty-analyze?** PRD → ARCHITECTURE → COMPONENTS.
2. **Implementing a feature?** Jump to the relevant COMPONENTS section, then
   cross-check the analysis pipeline in ARCHITECTURE.
3. **Evaluating product direction?** PRD vision + success criteria, then the gap
   callouts throughout COMPONENTS.

## Status legend (used throughout this set)

Every requirement and component is framed as **Vision / Current / Gap** and
tagged inline with one of:

| Tag | Meaning |
|---|---|
| ✅ **Implemented** | Built and working today. |
| 🟡 **Partial** | Partly built; usable but incomplete or with known caveats. |
| 🔵 **Designed-not-built** | Design exists (types, scaffolding, RFC, or plan) but no working path. |
| ⚪ **Aspirational** | North-star intent; no design committed yet. |

## Related documentation

This `spec/` set is the *what/why/gap* layer. The point-in-time and operational
docs live alongside it:

- **[../research/](../research/)** — dated investigations and audits (the
  trustee/search code-analysis summary and source `.docx`).
- **[../regression-testing/](../regression-testing/)** — versioned
  performance/quality snapshots (none authored yet).
- **[../sessions/](../sessions/)** — engineering-session narratives (none
  authored yet).
- **[../decisions/](../decisions/)** — evidenced design-decision records.
- **[crates/trusty-analyze/README.md](../../../crates/trusty-analyze/README.md)**
  and **[crates/trusty-analyze/CLAUDE.md](../../../crates/trusty-analyze/CLAUDE.md)**
  — in-crate quick-start, HTTP/MCP catalogue, and project history.

## Provenance & maintenance

These documents are derived from an audit of the `crates/trusty-analyze/src/`
tree (the single-crate `commands` / `core` / `embedder` / `lang` / `mcp` /
`service` / `types` module layout as of v0.4.1), the crate `README.md` /
`CLAUDE.md`, and the open/closed issue backlog (notably the axum feature-gate
[#249](https://github.com/bobmatnyc/trusty-tools/issues/249), the MCP
stdout-framing fix [#66](https://github.com/bobmatnyc/trusty-tools/issues/66),
the `list_facts` read-lock contention fix
[#67](https://github.com/bobmatnyc/trusty-tools/issues/67), the LLM-narrative /
framework analysis feature [#4](https://github.com/bobmatnyc/trusty-tools/issues/4),
the micro-crate consolidation [#5](https://github.com/bobmatnyc/trusty-tools/issues/5),
the crate-inventory reconciliation
[#430](https://github.com/bobmatnyc/trusty-tools/issues/430), the reqwest
timeouts + spawn_blocking fix [#521](https://github.com/bobmatnyc/trusty-tools/issues/521),
the MCP deep_analysis timeout raise
[#528](https://github.com/bobmatnyc/trusty-tools/issues/528)/[#529](https://github.com/bobmatnyc/trusty-tools/issues/529),
the AWS Bedrock deep-pass
[#530](https://github.com/bobmatnyc/trusty-tools/issues/530)/[#531](https://github.com/bobmatnyc/trusty-tools/issues/531),
the connection-safe graceful-shutdown upgrade
[#534](https://github.com/bobmatnyc/trusty-tools/issues/534)/[#535](https://github.com/bobmatnyc/trusty-tools/issues/535),
and the ORT backend feature-select
[#536](https://github.com/bobmatnyc/trusty-tools/issues/536)/[#538](https://github.com/bobmatnyc/trusty-tools/issues/538)).
When the code changes materially, update the relevant document and bump the
*Last reviewed* date. Source-path citations reflect the layout at the time of review.

> **Note on in-crate `CLAUDE.md` drift:** the crate-local
> `crates/trusty-analyze/CLAUDE.md` predates the workspace consolidation and
> still describes a *nested* multi-crate workspace
> (`crates/trusty-analyze/crates/trusty-analyze-{types,lang,core,mcp,service,embedder}`),
> a 9-tool MCP surface, and an 8-endpoint HTTP surface. The authoritative layout
> is the **single `trusty-analyze` crate** with `core` / `lang` / `mcp` /
> `service` / `embedder` / `types` modules, an **18-tool MCP surface**, and a
> ~20-route HTTP surface. This spec set reflects the audited tree, not the stale
> CLAUDE.md; the reconciliation is tracked in
> [#430](https://github.com/bobmatnyc/trusty-tools/issues/430).
