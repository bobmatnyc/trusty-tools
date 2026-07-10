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
| **M3** | Cross-repo benchmarking (percentile ranks, quartile placement like CAST Appmarq) | Maintain corpus of analyzed repos; compute percentile functions; wire into scorecard section | **Landed (#2314)** — `src/report/benchmark.rs`, opt-in `--corpus` / `--corpus-add` / `--benchmark` flags, deterministic percentile/quartile placement + small-n honesty gate (see [Benchmarking (M3)](#benchmarking-m3)) |
| **Wave 2** | Inference-first output: analyst instructions doc, self-derived metadata, built-in repo scanning, provenance labels + omit-empty | Instructions loader + prompt injection; repo scanner; provenance model; post-render polish (comment strip + omit-empty + gaps) | **Landed (#2340 / #2342)** — `src/report/{instructions,scan,provenance,polish,reporter_fill}.rs`, `--instructions` flag (see [Inference-first output](#inference-first-output--analyst-instructions-wave-2-2340--2342)) |
| **Wave 3** | Repo-evidence findings investigation: inspect the code and produce the findings, gated by a verifiable-evidence guardrail; deterministic dependency inventory; coverage honesty | Deterministic file selection + budgets; reviewer-role investigation LLM call; verbatim-evidence guardrail; manifest/lockfile dependency parsing; coverage section + prompt injection | **Landed (#2357)** — `src/report/investigate/`, runs under `--synthesize` for local checkouts, `--investigate-max-files` / `--investigate-max-bytes` (see [Repo-evidence investigation](#repo-evidence-investigation-wave-3-2357)) |
| **Wave 3.1** | Follow-up hardening after live-QA acceptance: batch the investigation's own LLM calls so per-batch output truncation cannot collapse a whole repository's findings; bound the top-level synthesis call's input/output so a large verified-finding count cannot blank the executive summary; layer synthesized-section instructions (generic → template override → analyst overlay) | `investigate/batch.rs` (size-bounded batching + retry-once + merge/dedupe); `synthesize_digest.rs` (compact digest + capped elaboration targets); `section_instructions.rs` + `template::parse_section_instructions` | **Landed (#2357 follow-up)** — see [Repo-evidence investigation](#repo-evidence-investigation-wave-3-2357) (batching), [Compact findings digest](#compact-findings-digest--truncation-retry-2357-follow-up), and [Section instruction layering](#section-instruction-layering-2357-layered-instructions) |

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
trusty-review report --manifest <file> [--template <name>] [--out <dir>] [--instructions <md>] \
                     [--synthesize] [--investigate-max-files <n>] [--investigate-max-bytes <b>] \
                     [--corpus <dir>] [--corpus-add] [--benchmark]
  --manifest <file>   Path to the report manifest TOML (required)
  --instructions <md> Free-form analyst instructions markdown (#2340). Recorded
                      verbatim as an "Analyst Instructions" section and, under
                      --synthesize, injected as focus directives. Precedence:
                      this flag > manifest [report].instructions. A missing file
                      is an error; an empty file warns and proceeds as absent.
  --template <name>   Template override, e.g. report-technical-dd or
                      report-technical-dd-cast. Precedence:
                      --template flag > manifest [report].template > default
                      (report-technical-dd)
  --out <dir>         Output directory for the generated report pair
                      (default: ./reports)
  --synthesize        Opt in to M2 LLM synthesis of the narrative sections AND
                      the wave-3 repo-evidence investigation (default OFF —
                      deterministic M1 output). Spends LLM tokens; fails closed to
                      deterministic output on any provider/parse/guardrail failure.
                      See "Synthesis (M2)" and "Repo-evidence investigation".
  --investigate-max-files <n>
                      Wave-3 cap on files sent per repository (#2357). Precedence:
                      this flag > manifest [report].investigate_max_files > 40.
  --investigate-max-bytes <b>
                      Wave-3 cap on total content bytes sent per repository.
                      Precedence: this flag > [report].investigate_max_bytes >
                      409600 (400 KiB).
  --corpus <dir>      Benchmark corpus directory (M3). Precedence:
                      --corpus flag > manifest [report].corpus > per-user XDG
                      default (~/.local/share/trusty-review/benchmark/ or the
                      platform equivalent). A no-op without --corpus-add/--benchmark.
  --corpus-add        After a successful run, append each analyzed repository's
                      metrics snapshot to the corpus. See "Benchmarking (M3)".
  --benchmark         Compute cross-repo percentile/quartile placement against the
                      corpus and fill the benchmark tables. Deterministic; a corpus
                      with < 5 peers is disclosed, never silently ranked.
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
title        = "Acme Technical DD"      # required — report title, codename, and slug seed
template     = "report-technical-dd"    # optional — default template if omitted
analyst      = "bobmatnyc"              # optional — recorded in report metadata (declared)
client       = "Acme Corp"             # optional — deal-side client (declared); omitted → Gaps
instructions = "focus.md"              # optional — analyst brief (#2340); --instructions flag wins

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
`top_risks` (array of `{description, severity, cost, apps}`, capped at 5 items —
3 on the concise retry), and `findings` (array of `{app_slug, title, severity,
description, evidence, component, business_impact, remediation, cost_effort}`,
capped at 10 items). Each finding carries its `app_slug` and `severity` so the
reporter routes prose back to the correct application section and band. The
system prompt mandates: use ONLY provided values, never invent numbers,
preserve any `(approx)` marker verbatim, RED/AMBER only.

### Compact findings digest + truncation retry (#2357 follow-up)

**Incident.** Once wave-3 investigation began verifying dozens of real findings
per repository, a live-QA acceptance run found that this SAME top-level
synthesis call — which historically asked for a full prose elaboration of
EVERY RED/AMBER finding alongside the executive summary — hit its own
output-token ceiling with 26 verified findings, failed closed (correctly, per
the fail-closed posture above), and left the Executive Summary and Top Risks
blank atop an otherwise-strong findings section. Honest, but a blank exec
summary atop verified findings is a delivery defect, not a feature.

**Fix (`synthesize_digest.rs`, structural — mirrors the wave-3 batch
investigation's own output-cap fix):**

- **Compact CONTEXT digest.** Every RED/AMBER finding across all repositories
  is reduced to `title / severity / dimension / file / a ≤140-char truncated
  one-line description` — never the full evidence quote, business impact, or
  remediation prose, which already lives in the report body once verified.
  Capped at the top 40 findings by severity (RED first); an overflow renders an
  honest tail note ("… and N additional lower-severity finding(s) exist").
  This is the ONLY findings data used to ground the executive summary and top
  risks.
- **Elaboration targets, separately bounded and verified-exclusive.** A finding
  already wave-3-verified has authoritative, evidence-grounded prose that is
  merged onto the report regardless of what this call produces (investigation
  prose always wins — see `investigate::merge_investigation_prose`), so asking
  the model to re-elaborate it would only spend output budget on discarded
  text. The `findings` elaboration array therefore lists ONLY unverified
  findings (capped separately at 10, RED first, with its own overflow note);
  when investigation already verified everything the digest explicitly says
  "none — every RED/AMBER finding already has verified evidence-grounded prose"
  and the model must return `findings: []`.
- **Structural output bounds.** `top_risks` and `findings` both carry a JSON
  Schema `maxItems` (5/10 respectively, 3 for `top_risks` on the retry) —
  bounding the SHAPE of the ask, not merely describing a preference in prose,
  exactly as wave-3's `analyze.rs` bounds `maxItems`/`maxLength` on the
  investigation schema.
- **One-shot truncation retry.** On `finish_reason = length`/`max_tokens`,
  `Synthesizer::synthesize` retries ONCE with `retry_concise = true` (shrinking
  `top_risks_cap` to 3 and appending an explicit "be maximally concise"
  directive) before falling back to the existing fail-closed
  `Unavailable("truncated response")`.

This keeps prompt INPUT size roughly linear (~200 bytes/finding × 40 ≈ 8 KB) and
prompt OUTPUT size bounded regardless of how many findings a large repository's
investigation verifies — the whole point being that "26 verified findings, hand-
checked, no dupes" (the wave-3 investigation's own success) must also mean "and
the exec summary is populated," not a second, unrelated failure mode.

### Section instruction layering (#2357 layered instructions)

The three synthesized sections — `executive_summary`, `top_risks`,
`finding_elaboration` — are each governed by a **layered instruction**, the
same generic → template → analyst-overlay shape the crate already uses for
reviewer voice (stock rules → `principles` addendum → an optional custom
`voice` package, see `src/voice/`), applied here to what each synthesized
section is told to write:

1. **Generic built-in default** (`section_instructions.rs`) — the tool's own
   shipped instruction per section id, used when nothing overrides it. Structured
   as string constants (`EXECUTIVE_SUMMARY`, `TOP_RISKS`, `FINDING_ELABORATION`)
   plus a `default_instruction(id)` lookup and an `ALL_SECTION_IDS` validity list.
2. **Template override** — a template may embed
   `<!-- instruct:<section_id> ... -->` anywhere in its file (single- or
   multi-line body), where `<section_id>` is one of the three ids above.
   `template::parse_section_instructions` parses these from the RAW template
   text (before the render pipeline ever runs) into a `section_id →
   instruction` map, recorded on `ReportModel::section_instructions` for JSON
   auditability. An unrecognised section id logs a `tracing::warn!` and is
   ignored — a template typo must never abort the run. The override REPLACES
   the generic default for that section only; other sections keep their
   generic default (`section_instructions::resolve` performs this merge).
   `instruct:` blocks are ordinary HTML comments to the render pipeline — like
   every other non-`dataset:` comment, `polish::strip_template_comments` strips
   them, so they NEVER reach rendered output regardless of where in the
   template they're placed. Both bundled templates document this syntax in
   their header comments; `report-technical-dd-cast.md` ships a worked
   demonstration overriding `executive_summary` with CAST health-factor voice
   ("lead with the TQI … name which of the five health factors … drive the
   RED/AMBER findings").
3. **Analyst `--instructions` overlay** — unchanged semantics from wave 2
   (#2340): an ADDITIVE per-run focus brief injected into the digest
   separately from the resolved section instructions. Precedence: analyst
   overlay + (template override, else generic default) — the analyst brief
   steers EMPHASIS on top of whichever section instruction is active; it never
   replaces or relaxes it (and never relaxes the numeric guardrail or the
   no-green exclusion either).

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

## Benchmarking (M3)

M3 layers **opt-in, fully deterministic** (no LLM anywhere) cross-repo placement
on top of the M1/M2 fill. Without `--benchmark` the report is byte-for-byte the
M2 output. It ranks a target against an accumulated **corpus** of per-repository
metrics snapshots — the trusty-review analogue of CAST's Appmarq peer benchmark —
and preserves the honesty rule: every ranking carries its population size, and a
corpus with fewer than five peers is never ranked against.

### Corpus format & location

The corpus is a directory of accumulated snapshot JSON files, one per repository
per run, keyed `<slug>-<sha-or-date>.json` (the git short-SHA when known, else the
run date), so a re-run overwrites the same key rather than accumulating duplicates.
Each snapshot (`src/report/benchmark.rs`, schema tag `corpus-v0`) records:

```jsonc
{
  "schema_version": "corpus-v0",
  "slug": "acme-web",
  "name": "Acme Web",
  "source_basename": "acme-web",   // source path/URL redacted to its basename (privacy)
  "git_sha": "abc1234",            // short HEAD SHA for local checkouts, if any
  "timestamp": "2026-07-10",       // run date, injected by the CLI (std/chrono at the edge)
  "metrics": { /* the v0 AnalyzeMetrics document */ }
}
```

The directory is resolved by precedence: `--corpus <dir>` flag > manifest
`[report].corpus` key (relative to the manifest dir) > the per-user XDG data dir
(`~/.local/share/trusty-review/benchmark/` or the platform equivalent, via the
same `dirs` conventions the template loader uses). `--benchmark`/`--corpus-add`
require a resolvable directory; the only hard error is a platform with no data dir
**and** no explicit source.

`--corpus-add` writes one snapshot per analyzed repository **that has metrics**
after a successful report write (metric-less repos contribute nothing rankable).
The loader reads every `*.json`, **skipping** — with a collected, surfaced warning
list, never a hard error — any file that is unreadable, unparseable, or whose
`schema_version` differs. A missing corpus directory yields an empty corpus plus a
warning (a normal first-run condition).

### Comparable metrics & percentile method

For each target the engine ranks a fixed, documented set of comparable scalars,
omitting any whose inputs are absent (so it is excluded from that metric's
population rather than ranked as a spurious zero):

- **total LoC**, **file count**, **function count** (raw size);
- **findings / 1k LoC** — total, RED, and AMBER density;
- **high-complexity function share (%)** — the share of functions in the highest
  complexity bucket (the last bucket in the distribution, by convention).

The **percentile** is the cumulative-share method: `100 × (count of population
values ≤ target) / population_size`. The population **always includes the target's
own value** (the documented choice — placement reflects where the target sits
within the full set); a stale corpus copy of the target (same slug) is excluded so
it is never double-counted. Consequences: a unique maximum → 100th percentile; the
sole member of an `n=1` metric population → 100th; tied values share a percentile.
The **quartile** maps the percentile as `≤25 → Q1`, `≤50 → Q2`, `≤75 → Q3`, else
`Q4` (Q1 = bottom quartile by that metric, Q4 = top). The **rank** is the 1-based
ascending position (`1 + count of strictly-smaller values`; ties share a rank),
reported as `r of n` with `n` the per-metric population size.

### Small-n honesty gate

If the corpus holds fewer than **5 peers** (excluding the target), the target is
**not ranked**: its Benchmark Position table renders a single explicit
`benchmark: corpus too small (n=<peers>)` row instead of placements, and the same
marker appears in the appended **Benchmark Status** section. Placement is never
computed silently against an empty or tiny corpus.

### Template wiring

For each ranked application the reporter fills the per-application **Benchmark
Position** table (one row per comparable metric: criterion, percentile
compliance, quartile, `rank of n`, and the population as the peer set) and the
graph-appendix `tqi_benchmark_position` dataset (one headline row per app, keyed
on the Total LoC placement; the full per-metric breakdown lives in the
per-application table). The bundled templates carry the benchmark row blocks with
a **leading-newline block idiom** so that with benchmarking OFF they render exactly
once as honesty markers — byte-identical to the M2 output. The CAST template's
health-factor benchmark tables (TQI/Robustness/Security/…) map to quality scores
trusty-review does not compute deterministically and therefore remain
honesty-marked; only the generic placement dataset is filled. A `## Benchmark
Status` section (appended only when `--benchmark` is active) discloses the corpus
size, each app's peer count or small-n marker, and any corpus load warnings. The
placement is also recorded on the JSON twin's `benchmark` object.

## Inference-first output & analyst instructions (wave 2, #2340 / #2342)

Owner feedback on the first live-generated reports was that they read as empty
templates: inline instruction comments leaked into output and "not stated in
source data" was everywhere. The directive: **inference is the tool's job** —
honesty markers are the last resort, not the default. Wave 2 implements this.

### Analyst instructions document (#2340)

An analyst hands the generator a free-form markdown brief (focus areas, deal
concerns, questions). It is resolved by precedence **`--instructions <file>` flag
> manifest `[report].instructions` key** (a relative manifest path resolves
against the manifest directory), loaded by `src/report/instructions.rs`:

- a **missing** file is a hard error (`ReportError::InstructionsNotFound`) — a
  mistyped path must fail loudly, never silently drop the recorded focus;
- an **empty** file (after trimming) logs a `warn!` and proceeds as if absent.

Two consumers use the brief. **Deterministically** it is recorded verbatim as an
`## Analyst Instructions` section (provenance: declared) so every report documents
what it was asked to focus on. Under **`--synthesize`** it is injected into the
synthesis prompt digest as *focus directives* that steer the emphasis of the
executive summary and RED/AMBER prose. **All guardrails are unchanged** — the
numeric guardrail, the structural green exclusion, and the fail-closed posture
still bind. Instructions steer emphasis; they never authorize invention.

### Self-derived metadata (#2342.2)

The tool never honesty-marks what it knows about itself:

- **vendor / methodology** = `trusty-review report (repository inspection) v<crate version>`;
- **report date / generated date** = the generation date;
- **analyst / client** = manifest fields when present (declared), else omitted → Gaps;
- **Section 3 Scoring Model** self-describes trusty-review's own normalized 0–100
  RED/AMBER/GREEN model (`RED < 33`, `AMBER 33–66`, `GREEN > 66`). The tool
  defines this scale, so it is `measured`, never "not stated".

### Built-in repository scanning (#2342.3)

For a local-path repository `src/report/scan.rs` computes a **measured** baseline
directly, so a bare run (no external metrics JSON) still produces a substantive
report:

- **file list** via `git ls-files` (honours `.gitignore`), falling back to a
  filtered directory walk (skips `.git`, `node_modules`, `target`, `dist`, …) for
  a non-git path;
- **total LoC + per-language breakdown** by file extension. **Heuristic:** a line
  counts when it contains any non-whitespace character (blank lines excluded);
  comments are *not* stripped (that would need per-language lexers). Only
  recognised source extensions contribute LoC; data/config files (`.json`,
  `.toml`, `.yaml`) count toward the file total but not LoC. Files over 4 MiB are
  counted but not line-scanned.
- **file count** = all tracked files;
- **frameworks / dependencies** from the manifests present in the root
  (`package.json`, `Cargo.toml`, `pyproject.toml`, `go.mod`) — project name +
  the first several declared dependency names.

No new heavy dependencies — std + the already-present `serde_json`/`toml`.

**Enrichment precedence.** An external trusty-analyze metrics JSON becomes
enrichment layered on the scan. Where **both** provide a figure the **declared
metrics win** over the **measured** scan (documented precedence); where only the
scan has it, the measured figure fills the field. Scanned values carry `measured`
provenance; metrics-derived values carry `declared`.

### Provenance labels + omit-empty (#2342.4)

Every substantive value carries one of four **provenance** kinds
(`src/report/provenance.rs`):

- **measured** — computed from the repo (superscript `⁽ᵐ⁾`);
- **declared** — manifest / analyst / metrics input (`⁽ᵈ⁾`);
- **inferred** — LLM judgement grounded in repo evidence (`⁽ⁱ⁾`);
- **not stated** — genuinely unknowable (e.g. deal client name); rendered with no
  marker because such fields are dropped, not shown.

The rendering choice is a **compact trailing superscript marker** appended to the
value, with a one-line legend rendered once near the top of the report. Synthesised
prose sections are labelled `inferred` — wired into BOTH synthesis-injection paths
(`reporter.rs::inject_synthesis_summary` for the executive summary + top-risks
`description`/`cost` fields, and `FindingRow::from_prose`/`merge_prose` for the
per-finding `description`/`evidence`/`business_impact`/`remediation`/`cost_effort`
fields) at field granularity — every LLM-written sentence in the rendered body
carries the marker (live-QA wave-2 defect #1: the tag existed but had zero call
sites until this fix). The numeric guardrail's allowed-set now **includes
repo-scanned computed figures** (they are real source data); qualitative LLM
inferences are permitted but must be labelled inferred — invention of unverifiable
*figures* remains forbidden.

Because several templates append a literal trailing period directly after a prose
placeholder (e.g. `{{finding_description}}.`), a synthesized sentence that already
ends in `.`/`?`/`!` would otherwise double up (`"...concatenation.."`). The
fill/injection layer (`reporter.rs::dedupe_terminal_punctuation`) strips exactly
one trailing `.`/`?`/`!` before the field is tagged; a trailing `)` is left
untouched (no template appends a bare period directly after a field closed with a
parenthesis, so there is no collision to resolve there) — live-QA wave-2 defect #3.

**Omit-empty** (post-render pass, `src/report/polish.rs`, applied after the
deterministic fill and before the appended status notes):

1. **Strip comments** — every template HTML comment is removed from output
   **except** semantic `<!-- dataset: … -->` markers (downstream tooling lifts
   tables by them). Comments inside ```` ``` ```` code fences are out of scope.
   Nested `<!--` inside an instructional comment is balanced so an embedded
   dataset example does not end the strip early.
2. **Drop marker rows/bullets** — a table row whose value cells are all the
   honesty marker is dropped; a marker-only bullet or standalone marker paragraph
   is dropped. An unfilled repeatable block now renders **nothing** (the fill
   engine changed from render-once-with-markers to render-nothing).
3. **Collapse empty sections, recursively** — a section (real `#`-heading OR a
   bold-only pseudo-heading line like `**Health-Factor Scores**`/`**Benchmark
   Position**`) left with no data collapses to a single `_No data available — see
   Gaps & Caveats._` line. The collapse check is **level-aware and recursive**: a
   heading's own span runs until the next boundary of the same or a shallower
   level (real headings by hash count; bold pseudo-headings are always deeper than
   any real heading), and a parent is never falsely collapsed merely because its
   first child happens to be a deeper boundary — its own has-content verdict
   propagates up from its (recursively resolved) descendants (live-QA wave-2
   defect #2: a `##` parent immediately followed by a populated `###` child
   previously collapsed spuriously above the child's real content). Bold-only
   pseudo-heading lines are recognised as boundaries in their own right so an
   orphaned label whose table was entirely dropped (all rows honesty-marked) also
   collapses instead of rendering with nothing beneath it (live-QA wave-2 defect
   #4).
4. **Gaps list** — every dropped field/section is collected into a compact
   `Data gaps: client, native scale, …` line in the Gaps & Caveats section,
   replacing the wall of `not stated` rows.

This **deliberately changes default rendering** — the M2/M3 byte-identical-when-off
guarantees apply to *their* flags (`--synthesize`, `--benchmark`), not to this
default output cleanup; affected tests were updated accordingly.

### Language-breakdown rendering (live-QA wave-2 defect #5)

The per-language LoC breakdown (from either the built-in scanner or an external
metrics file) is rendered with its counts, not just language names —
`reporter_fill.rs::format_language_breakdown` sorts by descending LoC and joins the
top 4 as `"{name} {loc}"` (thousands-grouped) with `" · "`, e.g.
`TypeScript 19,568 · SQL 184 · CSS 43 ⁽ᵐ⁾`. Previously `{{app_tech_stack}}` dropped
the LoC split entirely and rendered language names only.

## Repo-evidence investigation (wave 3, #2357)

**Problem.** A generated exec summary once declared that "no evidence-based
conclusions can be drawn regarding authentication and secret handling, dependency
freshness, state management complexity, or scalability … requiring a full manual
code review" — while the repository sat readable on disk. A report that admits it
did not look at available code is structurally unacceptable. Wave 3 makes that
outcome impossible: when the code is available, the tool inspects it and produces
the findings itself. Implementation lives in `src/report/investigate/`.

**When it runs.** Only under `--synthesize`, and only for repositories whose
source is a local checkout (`RepositoryReport::local_path` is `Some`). The
deterministic base report is unchanged without `--synthesize`; remote entries are
recorded `Skipped` with a reason. One reviewer-role LLM request is issued **per
repository** — the selected file set and the evidence-verification corpus are
inherently per-repo.

### File selection (`select.rs`, deterministic)

The tracked file list (`scan::list_tracked_files`, git-first with a filtered-walk
fallback) is relevance-ranked against (a) keywords extracted from the analyst
brief (#2340) and (b) standard DD dimensions via path/name heuristics:

| Dimension | Path/name heuristics |
|---|---|
| authentication & secrets | `auth*` segment, `*token*`, `*secret*`, `*password*`, `*credential*`, `config` segment, `.env*`, `middleware` |
| dependencies | `package.json`/lockfiles, `Cargo.toml`/`Cargo.lock`, `pyproject.toml`, `go.mod`/`go.sum` |
| state management | `store*`/`reducer*`/`atom*` segment, `*/state/*`, `*_store*`, `context*` |
| error handling | `error*`, `exception*`, `*_error*` |
| scalability | `queue*`, `cache*`, `worker`, `pool`, `*/db/*`, `database*` |
| test coverage | presence of a test dir/file (`/tests/`, `_test.rs`, `.spec.ts`, …) — presence-only |

Ranking score = `3 × instruction-keyword hits + 2 × dimension matches + 1 (source
file)`, sorted by score desc then path asc (fully deterministic). Files are added
greedily until a **budget cap** is hit: default **40 files / 400 KiB** total,
configurable via `[report].investigate_max_files` / `investigate_max_bytes`
manifest keys and the matching CLI flags. An individual file over ~24 KiB is
truncated (marker reserved so the byte total never exceeds the cap). The selection
records what was chosen vs skipped and which dimensions were reached (coverage
data).

### Investigation LLM call (`analyze.rs`, batched — `batch.rs`)

Reuses the reviewer-role provider + forced-structured-output pattern
(`ResponseSchema`). **Batched, not one request per repository** (wave-3.1,
#2357 follow-up): a live-QA acceptance run reproduced a real collapse — a
175-file repository's single unbatched request sent 282 KB of file content,
the findings JSON hit the (then 4096-token) output ceiling mid-array,
`finish_reason = length` correctly failed closed, and ALL findings for a fully
readable repository were discarded. The fix is structural, not a bigger
constant: `batch::partition_batches` splits the selection's files into
size-bounded, order-preserving batches of at most `BATCH_MAX_BYTES` (90 KiB)
content each; `analyze.rs` additionally bounds the OUTPUT shape per batch via
`maxItems` (≤8 findings, 4 on retry) and `evidence_quote`'s `maxLength` (200
chars), and `INVESTIGATION_MAX_TOKENS` is raised to 8192 (matching
`pipeline/prompt.rs::GEMINI_MAX_TOKENS`, the crate's existing precedent for a
demanding structured-output case) as a second line of defence. Each batch
carries: its position ("batch N of M"), the analyst brief, the DD-dimension
checklist, and its files as `path` + verbatim-content blocks. On
`finish_reason = length`/`max_tokens` for a batch, `batch::run_one_batch`
retries ONCE with a tighter cap (4 findings) and a concise directive before
failing THAT BATCH closed — its files are recorded in coverage as "sent but
analysis truncated/failed (batch N)"; **other batches' findings survive**.
Verified findings are merged across batches, deduping by `(file, title)` and
keeping the higher-severity (or first-seen, on a tie) copy. The prompt
mandates: **cite only provided files** (batch-scoped; a system-prompt note
warns the model it may be seeing only one batch of a larger repository), **quote
evidence verbatim** (≤200 chars), **invent no figures, GREEN = title only.**
Response schema (`repo_investigation`), per batch:

```jsonc
{ "findings": [ {
  "title": "...", "severity": "red|amber|green", "dimension": "...",
  "file": "src/...", "line": 42,               // approximate; corrected on verify
  "evidence_quote": "<verbatim snippet, ≤200 chars>", // required for RED/AMBER
  "description": "...", "business_impact": "...",
  "remediation": "...", "cost_effort": "..." } ] }  // maxItems: 8 (4 on retry)
```

The Investigation Coverage section (`render.rs`) additionally reports batch
accounting per repository — `batches: T total, S succeeded, F truncated/failed`
— with a named bullet per failed batch (position, reason, affected files); the
same fact is injected into the top-level synthesis digest's coverage summary
so an exec summary can only claim a gap for a genuinely named failed batch.

### Verifiable-evidence guardrail (`verify.rs`, deterministic)

The analogue of the M2 numeric guardrail — the reason the output can be trusted.
For each finding: (1) the cited `file` must be in the selected set; (2) the
`evidence_quote` must **substring-match** the actual file content, ignoring
whitespace differences; (3) the line number is **corrected** from the real match
position. GREEN findings pass as bare titles (no evidence). Any RED/AMBER finding
that fails is **REJECTED** (never softened), counted, and surfaced in the report as
`investigation: N finding(s) rejected (unverifiable evidence)`.

Verified findings flow through the **same `FindingRow` path** as trusty-analyze
metric findings: each is injected as a `MetricFinding` (severity band placement;
greens → topic list) and its prose is overlaid onto the row via
`Synthesis::findings`, with the description/impact/remediation tagged `inferred`
⁽ⁱ⁾ and the verbatim evidence quote tagged `measured` ⁽ᵐ⁾ (a `FindingProse`
carries an `evidence_measured` flag the reporter honours).

### Deterministic dependency inventory (`deps.rs`)

Parses root manifests **and** lockfiles — `package.json` + `package-lock.json`,
`Cargo.toml` + `Cargo.lock`, `pyproject.toml` (PEP 621 + poetry), `go.mod` — into a
`measured` ⁽ᵐ⁾ table (name, declared spec, locked version where available),
rendered in a new **Dependency Inventory** section. Rows are capped (30) with an
`… and N more` line. **No network calls.** The LLM may reference these deps and
flag staleness as an `inferred` ⁽ⁱ⁾ judgement.

### Coverage honesty (`render.rs`)

An **Investigation Coverage** section states, per repository: files examined /
total tracked, bytes sent (with the budget), DD dimensions covered vs NOT
investigated, and the rejected-finding count. The same coverage summary is injected
into the synthesis prompt so the exec summary can only claim a data gap where one
truly exists (remote-only, budget exhausted) — and must name it. When the
investigation ran, the exec summary must synthesise from the actual findings.

### Alternative / future source

trusty-analyze daemon integration (deterministic findings) remains the future
complement per the epic architecture; the findings schema already accepts both
sources (a `MetricFinding` is source-agnostic).

## Mermaid charts from dataset markers (wave 4, #2366)

**Problem.** The Graph-Ready Data Appendix (§7) already tags each pipe table with
a `<!-- dataset: <slug> | chart: <type> | x: <field> | y: <field>[, group:
<field>] -->` marker, but that marker was opaque to the renderer — a human reader
saw only the machine-readable table. Wave 4 turns each POPULATED dataset table
into a human-viewable Mermaid chart, emitted directly under it. Implementation
lives in `src/report/mermaid.rs`.

**When it runs.** Always on by default, as a post-`polish` pass in
`Reporter::render` (after omit-empty, before the appended status notes). It is
deterministic — pure rendering from the already-filled table rows, **no LLM, no
network**. Disable it with the `--no-mermaid` CLI flag OR the manifest
`[report] mermaid = false` key (the flag forces off regardless of the manifest).
When disabled the pass is skipped entirely and the report is **byte-identical** to
the pre-wave-4 output. Running after `polish` guarantees a chart is only ever
drawn under a table that survived omit-empty — an empty/dropped dataset gets no
chart, consistent with the omit-empty rule. The `<!-- dataset: … -->` marker
itself is preserved (it is semantic; downstream tooling still lifts tables by it).

### Chart-type mapping

| `chart:` | Mermaid | Rendering |
|---|---|---|
| `bar` | `xychart-beta` | Single bar series. Categories = distinct `x` values; each bar = the **sum** of numeric `y` over rows sharing that `x`. A declared `group:` is aggregated away (bars total across groups). |
| `stacked-bar` | `xychart-beta` | One bar series **per `group:` value**, layered/overlaid. **Mermaid has no native stacking** — this is a documented approximation (bars are drawn over one another, not summed on top); a `%%` comment in the block and a front-to-back legend note disclose it. Falls back to a single series when the group column does not resolve. |
| `radar` | `radar-beta` | Axes = distinct `x` values; one `curve` per `group:` value (single curve named after the `y` field when there is no group). **Requires Mermaid ≥ 11.6** — a `%% radar-beta requires Mermaid >= 11.6` comment records the version floor in every emitted block. |
| `heatmap` | *(none)* | **No native Mermaid support.** Fallback: emit NO block and a one-line note `_(heatmap: no Mermaid rendering; see table above)_` — the pipe table remains the authoritative artifact. |
| unknown / absent | *(none)* | No block; a `debug!` log records the skip. Never panics. |

### Dataset population: which §7 tables fill on a bare run (#2366 follow-up)

**Problem.** Live-QA on a real bare `report` run (no `--synthesize`, no external
metrics JSON) found EVERY §7 dataset table empty — nothing but bare `<!--
dataset: … -->` markers, so nothing charted. Root cause: the mandated §7
appendix names ten datasets, but only `tqi_benchmark_position` (gated on
`--benchmark`) had a fill path; the rest — including `loc_by_technology`, whose
data (per-language LoC) the built-in scan (#2342.3) ALREADY computes — had no
wiring at all. A chart feature that never fires on the common case (a bare
scan) is empty scaffolding, which the honesty rule exists to prevent. The fix is
to wire every dataset whose data is already present in the model, and leave the
rest empty **by design** — never fabricate data a bare run cannot produce.

Per-dataset status:

| Dataset | Populates from | On a bare scan-only run |
|---|---|---|
| `loc_by_technology` | `RepositoryReport::scan.by_language` (measured), or `metrics.loc.by_language` (declared) when an external metrics JSON is supplied — declared wins, same precedence as the Profile "Technology stack" line (`fill_profile`) | **Populates** — one row per (application, language), `tech_pct` computed against the breakdown's own total; a real `stacked-bar` chart renders |
| `tqi_benchmark_position` | `ReportModel::benchmark` (only set under `--benchmark`) | Empty — correctly gated on `--benchmark`, not a bare-scan input |
| `complexity_distribution` | `metrics.complexity.buckets` — **metrics-only**; `RepoScan` has no complexity analysis, so the bare scan can never supply this | Empty (omit-empty) — **requires an external trusty-analyze metrics JSON**; never fabricated |
| `health_factors_by_app`, `violations_by_iso_domain` / `violations_by_domain`, `cve_by_component_severity`, `license_risk_tiers`, `cloud_maturity_by_tech`, `violations_by_horizon`, `remediation_cost_by_tier`, `green_deficiencies_top10` | Deal-specific DD findings (violations, CVEs, license tiers, remediation economics) that no deterministic scan or metrics schema currently captures | Empty (omit-empty) by design — populating these requires a future structured-findings source (see the Alternative/future source note above); NOT wired by #2366, and must not be force-populated from unrelated data |

The same precedence rule applies throughout: an external metrics JSON is
**enrichment**, not a prerequisite — where both the scan and metrics provide a
figure, metrics (declared) wins; where only the scan has it, the measured figure
still renders (and still charts). A dataset with genuinely no available
deterministic source stays empty and chartless — that is the honest outcome, not
a bug.

### Column resolution & numeric parsing

The marker's `x`/`y`/`group` field names are semantic hints, not exact header
text (`x: factor` → the `Factor` column; `y: tqi_rank` → `Rank`). Resolution
normalizes both sides to lowercase alphanumerics and matches in priority order —
exact, then header-starts-with, then header-contains — over the full field name
then its last `_`/space token. An unresolvable `y` column yields no chart; an
unresolvable `x` falls back to the first column.

`y`-value cells are parsed tolerant of the report's rendered decoration: the
provenance superscripts (`⁽ᵐ⁾`/`⁽ᵈ⁾`/`⁽ⁱ⁾`), thousands separators, `$`, `%`, and
whitespace are stripped before `f64` parsing. A row whose `y` is non-numeric is
**skipped** (not charted); if **every** row is unparseable, no chart is emitted.

### Escaping & caps

All category / axis / series labels are Mermaid-escaped: emitted double-quoted,
with any interior `"` replaced by `'` (empty labels become `"?"`). To keep charts
legible the renderer caps **12 categories** and **8 series/curves**; the remainder
is dropped with an `_… and N more categories/series omitted from the chart; see
table above._` note (the full set stays in the table).

### Polish interaction

The emitted ` ```mermaid ` block is a fenced region, so `polish`'s fence-state
tracking (any ` ``` ` at line-start opens a fence) already treats it as opaque —
no marker/heading/table interpretation inside it, exactly like an evidence fence.
Because injection runs *after* `polish`, the polish pass never sees the block in
the normal flow; a regression test (`polish_tests::mermaid_fence_is_opaque_to_polish`)
pins the opacity defensively.

## References & Related Docs

- **[crates/trusty-review/templates/](../../../crates/trusty-review/templates/)** — Template instances (generic + CAST-specific)
- **[docs/trusty-review/reports/](../reports/)** — Generated report examples
- **[crates/trusty-analyze/CLAUDE.md](../../../crates/trusty-analyze/CLAUDE.md)** — trusty-analyze architecture and metrics schema (will be updated per task D)
- **[trusty-review README](../../../crates/trusty-review/README.md)** — Usage and quick-start
