# trusty-analyze — documentation

Code-analysis daemon + MCP server (complexity, smells, quality metrics).
Sidecar to trusty-search; listens on port 7879. Crate lives in
`crates/trusty-analyze/`.

This directory is the single source of truth for trusty-analyze design and
research documentation. The crate `README.md` and rustdoc stay in-crate
(see [ADR-0001](../adr/0001-docs-live-top-level.md)).

## Role in Technical DD Report Generation

trusty-analyze is the **standalone analysis engine** for the trusty-review report-generation 
feature (announced 2026-07-10). When `trusty-review report --repo <path>` is invoked, 
trusty-analyze performs all static code analysis (complexity, smells, quality metrics, size/LoC 
breakdowns, violations, ISO-5055 compliance, etc.) and exposes results via HTTP API on port 7879. 
trusty-review consumes those metrics and fills templated DD report skeletons (generic or 
CAST-specific), optionally synthesizing prose via LLM.

**Current architecture (2026-07-10):** trusty-analyze has a **runtime HTTP dependency** on 
trusty-search (fetches chunk corpus via `GET /indexes/:id/chunks`), but NO compile-time crate 
dependency in `Cargo.toml`. Future work may add trusty-search as a guide-analysis dependency 
to focus deep analysis on hot/coupled areas and identify refactoring opportunities.

See [`docs/trusty-review/spec/report-generation.md`](../trusty-review/spec/report-generation.md) 
for the full report-generation specification and roadmap.

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
