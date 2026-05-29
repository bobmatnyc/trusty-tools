# trusty-analyze — documentation

Code-analysis daemon + MCP server (complexity, smells, quality metrics).
Sidecar to trusty-search; listens on port 7879. Crate lives in
`crates/trusty-analyze/`.

This directory is the single source of truth for trusty-analyze design and
research documentation. The crate `README.md` and rustdoc stay in-crate
(see [ADR-0001](../adr/0001-docs-live-top-level.md)).

## Documentation map

This directory follows the standard three-subdir layout used across all
published trusty-* crates:

| Subdir | Contents |
|--------|----------|
| [`spec/`](spec/) | **Canonical specification set** — the single source of truth for *what trusty-analyze is meant to be, is today, and is missing*: [README](spec/README.md) (index + status legend), [PRD](spec/PRD.md), [ARCHITECTURE](spec/ARCHITECTURE.md), [COMPONENTS](spec/COMPONENTS.md). |
| [`decisions/`](decisions/) | Evidenced design-decision records (ADR-style). |
| [`research/`](research/) | Investigation docs and audits: [trustee/search code-analysis summary](research/trustee_search_code_analysis_summary.md), plus the source `code_search_analysis.docx`. |
| [`regression-testing/`](regression-testing/) | Versioned performance/quality snapshots, baseline measurements. (None authored yet.) |
| [`sessions/`](sessions/) | Engineering-session summaries — narrative + reasoning. (None authored yet.) |

## Conventions

Subdirs follow the workspace documentation conventions described in the root
[`CLAUDE.md`](../../CLAUDE.md). See [`docs/trusty-search/`](../trusty-search/)
for a worked example of the fully populated layout.
