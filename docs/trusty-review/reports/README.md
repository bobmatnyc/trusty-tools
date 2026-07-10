# Technical DD Report Analyses

This directory holds `trusty-review`'s generated technical due-diligence (DD) report instances.
Reports are generated via **repository inspection** using `trusty-analyze` (static analysis,
complexity, quality metrics) with `trusty-search` as a context guide for architecture understanding.

## How instances are produced

Reports are produced via the `trusty-review report` subcommand:

```bash
trusty-review report --repo <path> --template <name> [--out <dir>]
```

The pipeline:

1. **trusty-analyze** inspects the repository, producing structured metrics (complexity, smells,
   quality grades, LoC breakdowns, violations, risk scores).
2. **trusty-review** loads a template skeleton (generic or CAST-specific) and fills placeholders
   from trusty-analyze output, optionally synthesizing prose (executive summary, findings narratives) via LLM.
3. The result is a standalone markdown report (plus optional JSON) with scorecard, findings by
   severity, risk registers, and graph-ready datasets.

## Templates

Two templates are available in [`crates/trusty-review/templates/`](../../../crates/trusty-review/templates/):

1. **`report-technical-dd.md`** — Generic, vendor-neutral template. Sections: metadata, exec summary,
   scoring model, per-app scorecard, findings, risk registers, datasets, gaps.
2. **`report-technical-dd-cast.md`** — CAST Highlight specific. Includes: 1.00–4.00 health-factor
   scales (TQI, Robustness, Efficiency, Security, Changeability, Transferability), ISO-5055
   compliance, Highlight-style scans (OSS, CVEs, licenses, cloud maturity, green impact), Appmarq-style
   benchmarks (quartiles, percentile ranks), age-adjusted acceptability thresholds, remediation tiers.

See [`docs/trusty-review/spec/report-generation.md`](../spec/report-generation.md) for the full
feature specification, data sources, and roadmap.

## The no-green-analysis convention

RED (critical) and AMBER (medium-risk) findings are always reproduced in full analytical detail —
finding, evidence, affected component, business impact, and remediation. GREEN (positive/healthy)
findings are deliberately given **no analysis**: they appear as a single one-line-per-topic list and
nothing more. This keeps reports focused on actionable issues.

## Contents

(Ad hoc reports generated via `trusty-review report` will appear here.)
