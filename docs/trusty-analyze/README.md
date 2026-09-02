# trusty-analyze — documentation

Code-analysis daemon + MCP server for complexity, smells, quality metrics,
graph extraction, diagnostics, and review support. It consumes indexed source
from trusty-search, serves JSON-RPC over a derived Unix-domain socket, and also
supports MCP over stdio. The crate lives in `crates/trusty-analyze/`.

This directory is the extended index for trusty-analyze design and research.
Implemented behavior remains code-owned; the crate README is the current
operator entry point.

## Role in Technical DD Report Generation

trusty-analyze is the static-analysis engine used by trusty-review's report
pipeline. The primary review path starts it on demand as a subprocess; report
clients may also use the daemon through its derived socket. Analyzer results
populate the deterministic report sections before any optional LLM narrative
is synthesized.

**Current architecture (verified 2026-09-02):** trusty-analyze has a runtime
HTTP dependency on the trusty-search daemon for indexed chunks. Its own daemon
does not expose an HTTP API: JSON-RPC uses
`<data-dir>/trusty-analyze/trusty-analyze.sock`, with the path derived by the
shared daemon-path helper. MCP clients use the stdio transport. The analyzer
starts on demand and normally exits after ten idle minutes.

See [`docs/trusty-review/spec/report-generation.md`](../trusty-review/spec/report-generation.md) 
for the full report-generation specification and roadmap.

## Documentation map

This directory contains product-specific design and research material. The
workspace does not require every package to have the same set of subdirectories:

| Subdir | Contents |
|--------|----------|
| [`spec/`](spec/) | Historical product baseline: [README](spec/README.md), [PRD](spec/PRD.md), [ARCHITECTURE](spec/ARCHITECTURE.md), and [COMPONENTS](spec/COMPONENTS.md). |
| [`decisions/`](decisions/) | Evidenced design-decision records (ADR-style). |
| [`research/`](research/) | Investigation docs and audits: [trustee/search code-analysis summary](research/trustee_search_code_analysis_summary.md), plus the source `code_search_analysis.docx`. |
| [`regression-testing/`](regression-testing/) | Versioned performance/quality snapshots, baseline measurements. (None authored yet.) |
| [`sessions/`](sessions/) | Engineering-session summaries — narrative + reasoning. (None authored yet.) |

## Conventions

Subdirs follow the workspace documentation conventions described in the root
[`CLAUDE.md`](../../CLAUDE.md). See [`docs/trusty-search/`](../trusty-search/)
for a worked example of the fully populated layout.
