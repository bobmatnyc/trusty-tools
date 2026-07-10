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
- **Feature Gate:** Cargo feature `report`, **default-on** (mirrors the `profile` gate). It pulls in no new external dependencies (toml/serde/serde_json/regex/chrono/tempfile/dirs are already present), so the gate is purely for module/subcommand opt-out, not dependency weight; disable with `--no-default-features`.
- **CLI sketch:** `trusty-review report --manifest <file> [--template <name>] [--out <dir>]` (see [CLI & Configuration](#cli--configuration)).

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
| **M1** | Deterministic, manifest-driven fill (metrics → tables) from trusty-analyze output; no LLM synthesis | Manifest loader + validation; git enrichment; v0 metrics schema; template loader; placeholder fill engine; `report` subcommand | **Landed (#2313)** — `src/report/`, `report` Cargo feature (default-on), `trusty-review report --manifest` |
| **M2** | LLM synthesis of exec summary + findings prose (RED/AMBER descriptive narratives) | Plug LLM backend (OpenRouter or Bedrock); implement synthesizer; validate output quality | **Landed (#2314)** — `src/report/synthesize*.rs`, opt-in `--synthesize` flag, fail-closed + numeric guardrail (see [Synthesis (M2)](#synthesis-m2)) |
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

As of M1 (#2313) report generation is **manifest-driven** — a single TOML file
names the target repositories and their sources; the earlier `--repo` flag is
replaced by `--manifest`.

```bash
trusty-review report --manifest <file> [--template <name>] [--out <dir>] [--synthesize]
  --manifest <file>   Path to the report manifest TOML (required)
  --template <name>   Template override, e.g. report-technical-dd or
                      report-technical-dd-cast. Precedence:
                      --template flag > manifest [report].template > default
                      (report-technical-dd)
  --out <dir>         Output directory for the generated report pair
                      (default: ./reports)
  --synthesize        Opt in to M2 LLM synthesis of the narrative sections
                      (default OFF — deterministic M1 output). Spends LLM
                      tokens; fails closed to deterministic output on any
                      provider/parse/guardrail failure. See "Synthesis (M2)".
```

Output is always the dual `{date}-{title-slug}.md` + `.json` pair written
atomically (temp file + rename). The markdown is the human report; the JSON is
the full [`ReportModel`] (report metadata + per-repository git provenance +
metrics), the machine-readable twin for downstream tooling. STDERR carries
progress; STDOUT emits the written paths (one per line) so
`$(trusty-review report …)` is scriptable.

Example:
```bash
trusty-review report --manifest dd/acme.toml --template report-technical-dd-cast --out /tmp/dd-reports
# Produces: /tmp/dd-reports/2026-07-10-acme-technical-dd.md + .json
```

### Report Manifest

The manifest is a typed TOML document with one `[report]` section and one or
more `[[repositories]]` entries. It is parsed and validated by
`src/report/manifest.rs` (`load_manifest`), which returns a `thiserror`
`ManifestError` on any failure.

```toml
[report]
title    = "Acme Technical DD"          # required — report title, codename, and slug seed
template = "report-technical-dd"        # optional — default template if omitted
analyst  = "bobmatnyc"                  # optional — recorded in report metadata

[[repositories]]
name    = "Acme Web"                    # required — application name
slug    = "acme-web"                    # optional — derived from name when absent
path    = "/path/to/local/checkout"     # EITHER a local path …
# remote = "owner/repo"                 # … OR a remote (owner/repo or full git URL)
# username = "bobmatnyc"                # for remote access/attribution only
ref     = "main"                        # optional — branch, tag, or commit
metrics = "acme-metrics.json"           # optional — trusty-analyze v0 metrics JSON (relative to the manifest dir)
```

**Validation rules (enforced by the loader):**

1. At least one `[[repositories]]` entry, else `ManifestError::NoRepositories`.
2. Each entry declares **exactly one** of `path` or `remote`:
   - both set → `ManifestError::ConflictingSources { name }`
   - neither set → `ManifestError::MissingSource { name }`
3. `username` is meaningful only with `remote`. An **orphaned username** on a
   local-path entry is **tolerated** (kept in the model) but logs a `warn!` —
   it is ignored because local checkouts need no access/attribution in M1.
4. A missing `slug` is derived from `name` (lowercase; non-alphanumeric runs →
   `-`; trimmed; empty → `report`).
5. TOML parse failures surface as `ManifestError::Parse` with line numbers.

**Multi-repo → per-application mapping.** Each `[[repositories]]` entry maps to
exactly one `<!-- BEGIN per_application --> … <!-- END per_application -->`
repetition in the template, filled in manifest order. Report-level fields
(`applications_list`, etc.) aggregate across all entries. Finding and
risk-register blocks that have no deterministic M1 data render once with honesty
markers (never invented, never duplicated per app).

**Git enrichment (deterministic, local entries only).** For a `path` source the
loader shells out to the local `git` binary (via `std::process::Command`, no
`git2`/heavy dependency) to record current branch, short HEAD SHA, origin remote
URL, and a dirty flag. A path that is not a git work tree yields no git info
(the report still generates). **Remote entries perform no network fetch in M1** —
their declared `remote`/`ref`/`username` are recorded as-is. All git provenance
lands in the report metadata (the JSON `ReportModel`); the bundled templates
carry it in JSON, and a custom template may surface `{{git_branch}}`,
`{{git_head_sha}}`, `{{git_origin_url}}`, `{{git_dirty}}` in markdown.

### v0 trusty-analyze Metrics JSON Schema

M1 consumes a **pre-produced** trusty-analyze metrics JSON per repository (the
`metrics = …` manifest field); live invocation of the analyzer is deferred to a
later milestone. The v0 schema (`src/report/metrics.rs`) is intentionally small
and every field defaults, so partial analyzer output still parses and any field
the template needs but the metrics omit falls through to the honesty marker.

```jsonc
{
  "schema_version": "v0",              // informational
  "repository": "acme-web",            // informational
  "loc": {
    "total": 8200,                     // total lines of code
    "by_language": [                   // per-language breakdown
      { "language": "TypeScript", "loc": 6000 },
      { "language": "CSS",        "loc": 2200 }
    ]
  },
  "counts": { "files": 120, "functions": 640 },
  "complexity": {                      // cyclomatic-complexity distribution buckets
    "buckets": [
      { "label": "low (1-5)",  "count": 250 },
      { "label": "high (>20)", "count": 12 }
    ]
  },
  "findings": [                        // top findings with severity (no LLM prose in M1)
    { "title": "SQL injection", "severity": "red",
      "category": "security", "component": "db.rs" }
  ]
}
```

**Field → template mapping (M1 deterministic subset):**

| Metrics field | Per-application placeholder |
|---|---|
| `loc.by_language` (top 4, by LoC desc) | `{{app_tech_stack}}` |
| `loc.total` | `{{app_loc}}` |
| `counts.files` / `counts.functions` | `{{app_file_counts}}` |

`severity` is one of `red` / `amber` / `green` (unknown/absent → `green`).
Scoring, health-factor, and benchmark placeholders have no deterministic M1
source and therefore render as `not stated in source data` until M2/M3 land.

## Synthesis (M2)

M2 layers **opt-in** LLM prose on top of the M1 deterministic fill. It is OFF by
default: without `--synthesize` the report is byte-for-byte the deterministic M1
output. Synthesis is opt-in because it spends LLM tokens. It reuses the crate's
existing provider layer (`src/llm/` — OpenRouter or Bedrock, selected from the
**reviewer** role config exactly as the review pipeline does); it adds no new LLM
client or dependency.

### What the LLM writes — and does not

The synthesizer produces **only** the narrative fields M1 leaves as honesty
markers:

- `{{executive_summary_paragraph}}` — one deal-analytic paragraph.
- The **Top Risks** table rows (`{{risk_N_description}}` / `_severity` / `_cost`
  / `_apps`).
- Per-**RED/AMBER** finding elaboration (`per_application_red` / `_amber` blocks:
  finding, evidence, affected component, business impact, remediation, cost/effort).

**GREEN findings are never synthesized.** The no-green-analysis rule is enforced
**structurally**, not by prompt text: the prompt digest (`synthesize_prompt.rs`)
filters `severity == Green` out before the data ever reaches the model, so the
LLM cannot elaborate a green even if asked. Greens remain the M1 one-line topic list.

### JSON contract

The provider is forced into structured output via `ResponseSchema`
(`report_synthesis`): a top-level object with `executive_summary` (string),
`top_risks` (array of `{description, severity, cost, apps}`), and `findings`
(array of `{app_slug, title, severity, description, evidence, component,
business_impact, remediation, cost_effort}`). Each finding carries its `app_slug`
and `severity` so the reporter routes prose back to the correct application
section and band. The system prompt mandates: use ONLY provided values, never
invent numbers, preserve any `(approx)` marker verbatim, RED/AMBER only.

### Fail-closed posture (mirrors `Verdict::Unknown`)

Any failure keeps the deterministic honesty-marker output for the affected fields
and records a **visible** `synthesis: unavailable (<reason>)` note in the rendered
report's *Synthesis Status* section (and on the JSON twin's `synthesis` object). A
malformed or incomplete response is never partial-trusted. Fail-closed reasons:
`provider timeout` (120 s ceiling), `provider error: …`, `truncated response`
(`finish_reason = length`), `unparseable response`, and `no verifiable content`.

### Numeric guardrail

After the model returns, a deterministic post-check (`synthesize_guard.rs`)
verifies that **every number** (digit sequence) appearing in each synthesized
field exists in the source `ReportModel` data. The allowed set is computed from
the serialized deterministic model, so it captures every metric, count, LoC
total, and date the report is derived from. Matching tolerates formatting
variants — thousands separators (`8,200` = `8200`), `$`, `%`, and trailing-zero
decimals (`8200.0` = `8200`, `3.50` = `3.5`). Any field citing an unverifiable
figure is **rejected**: it falls back to the deterministic output and a
`synthesis: rejected (unverified figure)` note is recorded. This is the
report-side analogue of the crate's verdict-integrity posture — the guardrail,
not the prompt, is the last line of defence against a fabricated figure.

## References & Related Docs

- **[crates/trusty-review/templates/](../../../crates/trusty-review/templates/)** — Template instances (generic + CAST-specific)
- **[docs/trusty-review/reports/](../reports/)** — Generated report examples
- **[crates/trusty-analyze/CLAUDE.md](../../../crates/trusty-analyze/CLAUDE.md)** — trusty-analyze architecture and metrics schema (will be updated per task D)
- **[trusty-review README](../../../crates/trusty-review/README.md)** — Usage and quick-start
