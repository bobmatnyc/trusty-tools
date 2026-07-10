# trusty-review Report Generation — Feature Specification

> **Status:** Design Phase  
> **Last updated:** 2026-07-10  
> **Scope:** Technical DD report generation by repository inspection (vs. external document ingestion)  
> **Validation:** Tested against a March 2025 CAST technical DD deck (deal details excluded from repo)

## Motivation

`trusty-review` currently focuses on GitHub pull-request code review. This spec expands its scope to generate **CAST-style technical due-diligence reports** about a repository by **direct inspection** — positioning trusty-review as a vendor-alternative to commercial tools (CAST Highlight, BlackDuck, Crosslake) while reusing their proven report anatomy.

**Use case:** An acquirer or internal technical team analyzes a codebase under consideration and receives a structured, templated markdown report with executive summary, health-factor scorecards, findings by severity, risk registers, and graph-ready datasets — all derived from static and dynamic analysis.

**Why this approach:**
- Consolidates analysis logic (complexity, smells, violations, metrics) into a dedicated analysis engine (`trusty-analyze`) rather than scattering it.
- Positions trusty-review as a **rendering and synthesis layer** over trusty-analyze output, not a standalone analysis tool.
- Allows trusty-search to guide analysis context (architecture mapping, hot/coupled areas, call-chains) without trusty-review owning or parsing codebases.
- Provides a reusable template structure that adapts to multiple vendor methodologies (CAST, BlackDuck, homegrown scoring rules).

## Architecture

### Component Responsibilities

**`trusty-analyze` — Standalone Analysis Engine** (the primary change)

- **Ownership:** ALL repo inspection logic (static analysis, complexity, smells, quality metrics, size/LoC/tech mix, structural violations, etc.)
- **Dependency:** Takes an OPTIONAL dependency on `trusty-search` to **GUIDE ANALYSIS CONTEXT** — uses trusty-search for architecture mapping, symbol/KG lookup, hybrid retrieval to focus deep analysis on hot/coupled areas and identify refactoring opportunities.
- **Status as of 2026-07-10:** trusty-analyze has a **runtime HTTP dependency** on trusty-search (fetches chunk corpus via `GET /indexes/:id/chunks`) but NO compile-time crate dependency in `Cargo.toml` (verified: crates/trusty-analyze/Cargo.toml line 44–115 shows `trusty-common`, `tokio`, `axum`, etc., but NO `trusty-search`). Adding trusty-search as a guide-analysis dependency would be a new feature (marked as **future** in this spec).
- **Output:** Structured metrics (JSON or binary format TBD) containing: complexity metrics per file/function, code smells with categories, quality grades (A–F), size/LoC breakdown by technology, violations by rule and category, risk scores (security, reliability, efficiency, maintainability), and any runtime data if available.
- **No changes to current trusty-analyze scope** — this spec does NOT expand trusty-analyze's own capabilities; it merely positions it as the upstream analysis producer for report generation.

**`trusty-review` — Report Rendering & Synthesis Layer** (new module)

- **New module:** `src/report/` (mirrors `src/profile/`'s shape: types → template loader → synthesizer → reporter)
- **Responsibilities:**
  1. Load templated report skeletons from disk (template override via XDG, fallback to `include_str!()` bundled defaults)
  2. Fill placeholders (`{{field}}`) and repeatable blocks (`<!-- BEGIN … / END … -->`) from trusty-analyze metrics
  3. Synthesize prose (executive summary, findings descriptions, business-impact narratives) — initially via LLM, later deterministic for fully-templated sections
  4. Render markdown + JSON dual output (markdown for human reading, JSON for downstream tooling)
- **Template Loader Pattern:** Clone `VoiceLoader` (`src/voice/loader.rs`): XDG config directory (`~/.trusty-review/templates/`) is checked first; if not found, use bundled default via `include_str!()`.
- **Feature Gate:** New Cargo feature `report` (off by default). The standard `cargo install trusty-review` does not include report generation (keeps binary size and dependency surface minimal); users opt in via `--features report`.
- **CLI sketch:** `trusty-review report --repo <path> --template report-technical-dd [--out <dir>]`

### Section → Data Source Mapping

| Template Section | Data source | Notes |
|---|---|---|
| Report metadata (doc name, vendor, date, target, apps assessed, analyst) | Repo inspection + user input | Analyst name from env var or CLI flag; date = current date |
| Scoring model normalization | Template metadata | Maps vendor's native scale to 0-100 bands; CAST example: 1-4 → 0-100 with RED/AMBER/GREEN thresholds |
| Per-application scorecard (health factors, size, benchmark position) | `trusty-analyze` metrics + benchmarking module | Health factors: computed from violations per rule-category (or vendor-specific model); size: LoC/file/class counts; benchmarks: percentile ranks against internal corpus (future M3 feature) |
| Findings by severity (RED/AMBER/GREEN) | `trusty-analyze` metrics + LLM synthesis | RED/AMBER: violations + smells categorized by severity + LLM-generated prose (finding, evidence, affected component, business impact, remediation cost/effort); GREEN: one-line topic list only (no elaboration) |
| Risk registers (security violations, CVE exposure, license, obsolescence, cloud readiness, remediation economics) | `trusty-analyze` metrics + external tooling | Security violations: ISO-5055 domains from trusty-analyze; CVEs: cargo-audit output; Licenses: cargo-license + redb license-scan results; Obsolescence: cargo-outdated; Cloud readiness: trusty-analyze cloud-maturity rules; Remediation: cost/effort from violation rule metadata or LLM estimation |
| Graph-ready datasets (with `<!-- dataset: -->` comments) | All sources above, pivoted for visualization | E.g., `health_factors_by_app` (radar chart), `violations_by_domain` (stacked bar), `cve_by_component` (heatmap). Mandatory appendix section. |
| Gaps & caveats | Manual or inferred | Analyst notes things the source report does not cover; approximate/inferred readings are flagged with source markers |

### Scoring & Normalization

- **Native scales supported:** CAST 1.00–4.00 (TQI + 5 health factors); user-defined models via scoring-rule config.
- **Normalized 0-100 scale:** All vendor reports normalized to `(native - min) / (max - min) * 100` so reports become comparable.
- **Band mapping:** RED < 33, AMBER 33–66, GREEN > 66 (configurable per template).
- **Health-factor model example (CAST):**
  - Robustness: risk of outages / data-integrity issues (derived from reliability violations)
  - Efficiency: performance/scalability risk (derived from performance-efficiency violations)
  - Security: breach risk (derived from security violations)
  - Changeability: time-to-market / regulatory-change risk (derived from modularity/maintainability violations)
  - Transferability: team ramp-up difficulty (derived from documentation/naming violations)
  - TQI: aggregate mean of the five factors

### Conventions Enforced

1. **No-green-analysis rule:** GREEN findings are one-line topic lists only; no elaboration, root-cause narrative, or recommendations. Keeps analysis focused on actionable issues.
2. **Graph-ready appendix is mandatory:** Every instance MUST include a data-export section with pipe-table datasets preceded by `<!-- dataset: <slug> | chart: <type> | x: … | y: … -->` HTML comments so downstream charting tools (D3, Recharts, Plotly, etc.) can extract datasets mechanically.
3. **Honesty markers:** Any value estimated from an unlabeled chart is marked `(approx, from chart)`. Silent unknowns are labeled `not stated in source report`, never guessed.
4. **Per-app repeatability:** Sections that repeat per application (scorecard, findings, risk registers) use `<!-- BEGIN per_application --> / <!-- END per_application -->` blocks; fill each once and duplicate as needed.

### Phasing

| Phase | Deliverable | Effort | Status |
|---|---|---|---|
| **M1** | Deterministic fill (metrics → tables) from trusty-analyze output; no LLM synthesis | Define output schema for trusty-analyze; build template loader; implement placeholder fill engine | Not started |
| **M2** | LLM synthesis of exec summary + findings prose (RED/AMBER descriptive narratives) | Plug LLM backend (OpenRouter or Bedrock); implement synthesizer; validate output quality | Not started |
| **M3** | Cross-repo benchmarking (percentile ranks, quartile placement like CAST Appmarq) | Maintain corpus of analyzed repos; compute percentile functions; wire into scorecard section | Not started |

### Explicit Non-Goals

- **PDF/deck/document ingestion:** Intentionally rejected by stakeholder 2026-07-10. trusty-review will never parse external documents (PDFs, PowerPoint exports, etc.). All analysis is derived from repository inspection.
- **Commercial tool equivalence:** trusty-review aims to *position* as a vendor alternative, not achieve feature parity with CAST or BlackDuck. A focused 80% solution is preferred to feature bloat.

## Template Structure

Two templates will ship in `crates/trusty-review/templates/`:

1. **`report-technical-dd.md`** (generic, vendor-neutral): Structure shared by all CAST/BlackDuck/homegrown reports. Sections: metadata, exec summary + top risks, scoring model, per-app scorecard, findings by severity, risk registers, graph datasets, gaps/caveats.
2. **`report-technical-dd-cast.md`** (CAST-specific): Native 1.00–4.00 health-factor scales (Robustness/Efficiency/Security/Changeability/Transferability + TQI), ISO-5055 domains, Highlight-style scans (OSS/CVE, license, cloud maturity boosters/blockers, green impact), Appmarq peer benchmarks (quartiles/ranks), age-adjusted acceptability thresholds, remediation horizon tiers (fix-before-close/near/mid/long) with cost/effort.

Both are **placeholder-only** — absolutely zero deal-specific data (component names, schema names, figures) appears in them.

## CLI & Configuration

```bash
trusty-review report --repo <path> --template <name> [--out <dir>]
  --repo <path>       Repository to analyze (required)
  --template <name>   Template name, e.g. report-technical-dd or report-technical-dd-cast (default: report-technical-dd)
  --out <dir>         Output directory for generated report (default: ./trusty-review-reports/)
  --format <fmt>      Output format: markdown, json, both (default: markdown)
  --analyzer-url <u>  trusty-analyze HTTP endpoint (default: http://127.0.0.1:7879)
```

Example:
```bash
trusty-review report --repo /path/to/acme-web-app --template report-technical-dd-cast --out /tmp/dd-reports
# Produces: /tmp/dd-reports/2026-07-10-report-technical-dd-cast.md + .json
```

## References & Related Docs

- **[crates/trusty-review/templates/](../../../crates/trusty-review/templates/)** — Template instances (generic + CAST-specific)
- **[docs/trusty-review/reports/](../reports/)** — Generated report examples
- **[crates/trusty-analyze/CLAUDE.md](../../../crates/trusty-analyze/CLAUDE.md)** — trusty-analyze architecture and metrics schema (will be updated per task D)
- **[trusty-review README](../../../crates/trusty-review/README.md)** — Usage and quick-start
