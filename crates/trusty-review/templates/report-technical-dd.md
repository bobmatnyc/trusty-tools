<!--
  trusty-review template: report-technical-dd v0.1

  WHAT THIS IS
  A vendor-agnostic skeleton for turning an ingested technical due-diligence
  (DD) report — CAST, BlackDuck, Crosslake, a homegrown audit, or any other
  findings document — into a structured markdown analysis. It captures the
  generic shape shared by these reports: exec summary, per-target scorecard,
  findings by severity, risk registers, graph-ready data, and caveats.

  HOW IT'S USED
  A trusty-review analysis run ingests the source report (today: an already
  text-extracted digest of the source document; a future `trusty-review
  report` subcommand may automate PDF/deck ingestion — no such ingestion
  exists in the crate yet) and fills every placeholder below from data
  actually present in that source. The result is committed as a standalone
  instance under docs/trusty-review/reports/<date>-<vendor>-<codename>.md.

  PLACEHOLDER SYNTAX
  - `{{field_name}}`   — a scalar value, filled verbatim from the source.
  - `<!-- BEGIN per_application --> ... <!-- END per_application -->`
    marks a repeatable block: duplicate the block once per application,
    target, or finding and fill each copy independently. Nested repeatable
    blocks (e.g. a findings list inside a per-application block) follow the
    same BEGIN/END convention with their own name.
  - Never invent a number. If the source is silent, write exactly:
    `not stated in source report`. If a value was visually estimated off an
    unlabeled chart, preserve the source's own approximation marker
    (typically `(approx, from chart)`) rather than presenting it as exact.

  THE NO-GREEN-ANALYSIS RULE
  RED (critical) and AMBER (medium-risk) findings are reproduced in full
  analytical detail — finding, evidence, affected component, business
  impact, remediation. GREEN (positive/healthy) findings get NO analysis:
  list them as a single line per topic, nothing more. Do not editorialize,
  expand, or add root-cause narrative to a green item. This keeps the
  analysis focused on what the acquirer/reader actually needs to act on.

  SECTION-INSTRUCTION OVERRIDES (#2357 layered instructions)
  Under `--synthesize`, five narrative sections are LLM-written: the executive
  summary, the top-risks rows, per-finding elaboration prose, the Code Quality
  & Architecture summary, and the Security Posture summary. Each has a generic
  built-in instruction (see `src/report/section_instructions.rs`) that a
  template may override for its own methodology's voice by embedding a
  `<!-- instruct:<section_id> ... -->` comment anywhere in the file, where
  `<section_id>` is one of `executive_summary`, `top_risks`,
  `finding_elaboration`, `code_quality_summary`, `security_summary`. The
  override REPLACES the generic default for that section only. This generic
  template ships no override (it uses the crate's defaults, which are
  balanced/adversarial — acquirer-side, skeptical of risk, evenhanded about
  genuine strengths, never promotional) — see `report-technical-dd-cast.md`
  for a worked example overriding `executive_summary` with CAST health-factor
  language. An analyst's `--instructions` brief still applies ADDITIVELY on top
  of whichever instruction is active — it steers emphasis, never replaces it.
  `instruct:` comments are parsed at template-load time and, like every other
  comment here, are stripped from rendered output.

  JUMP-LIST PLACEHOLDER (#6004)
  `{{report_contents_block}}` is deterministic post-render structure, not a
  data field — never remove it and never expect it to honesty-mark. It is
  replaced, after every other section has rendered, with links to whichever
  `##`-level headings actually survived in THIS document (see
  `src/report/contents_links.rs`).
-->

# Technical Due-Diligence Analysis: {{target_codename}}

{{provenance_legend}}

{{analyst_instructions_block}}

## 1. Report Metadata

| Field | Value |
|---|---|
| Source document | {{source_document_filename}} |
| Vendor / methodology | {{vendor_methodology}} |
| Report date / version | {{report_date}} |
| Inference models | {{inference_models}} |
| Target / deal codename | {{target_codename}} |
| Client | {{client_name}} |
| Applications / systems assessed | {{applications_list}} |
| Analyst (this instance) | {{analyst_name}} |
| Analysis generated | {{analysis_generated_date}} |

## Key Facts

<!-- Owner ruling 2026-08-18: frontload facts — density, complexity, author
     count, estimated work volume, and its trajectory by month — ahead of the
     narrative below. Every row is deterministic, never LLM-touched (it is
     the same anchor set the numeric guardrail validates narrative against).
     Density rows (LoC, file count, languages) come from a `--analyze` metrics
     file when one was supplied, else from the built-in repository scan every
     run produces (#6029). Complexity needs `--analyze`; author count and
     monthly trajectory need the tga-side authorship artifact (#5453);
     "Estimated work volume" is absent in every run today, because tga has no
     effort-estimation metric (its `story_points` fields are documented
     placeholders, always zero). Each of those rows states its own missing
     input by name — never an invented figure, and never a blanket "no data"
     covering the rows the run did populate. -->

| Metric | Value |
|---|---|
| Codebase size (total LoC) | {{facts_total_loc}} |
| File count | {{facts_total_files}} |
| Primary languages | {{facts_languages}} |
| Complexity profile | {{facts_complexity_summary}} |
| Number of authors | {{facts_author_count}} |
| Estimated work volume | {{facts_work_estimate}} |
| 12-month trajectory | {{facts_trajectory}} |

## 2. Executive Summary

{{executive_summary_paragraph}}

<!-- Deal-relevant, synthesized across all applications — not a restatement of
     section headers. Owner ruling 2026-08-19 (#6030): it opens by describing
     what the codebase IS and DOES, then analyzes the major components and
     each one's role, then covers risk. Risk is still the core; the purpose
     and component narrative sets it up rather than replacing it. Voice is
     balanced/adversarial: acquirer-side, skeptical of risk, evenhanded about
     genuine strengths, never promotional. Every claim is grounded in the
     provided scan/dependency/authorship data — no invented product, market,
     customer, component, or figure. -->

{{report_contents_block}}

### Top Risks

| # | Risk | Severity | Est. cost/effort | Affected application(s) |
|---|---|---|---|---|<!-- BEGIN top_risk_row -->
| {{risk_rank}} | {{risk_description}} | {{risk_severity}} | {{risk_cost}} | {{risk_apps}} |<!-- END top_risk_row -->

## 3. Scoring Model Normalization

Captures the SOURCE report's native scale so a reader can compare this
report against others normalized through this same template.

| Field | Value |
|---|---|
| Native scale | {{native_scale_description}} |
| Native band definitions | {{native_band_definitions}} |
| Normalized mapping (0-100) | {{normalized_mapping_formula}} |
| RED threshold (normalized) | {{red_threshold}} |
| AMBER threshold (normalized) | {{amber_threshold}} |
| GREEN threshold (normalized) | {{green_threshold}} |
| Aggregation method (app-level from module/rule level) | {{aggregation_method}} |
| Peer-benchmark population (if any) | {{benchmark_population}} |

## 4. Per-Application Scorecard

<!-- BEGIN per_application -->
### 4.{{app_index}}. {{app_name}}

**Profile**

| Field | Value |
|---|---|
| Technology stack | {{app_tech_stack}} |
| Frameworks / manifests | {{app_frameworks}} |
| Technical size (LoC) | {{app_loc}} |
| Files / classes / DB artifacts | {{app_file_counts}} |
| Risk tier (native) | {{app_risk_tier}} |
| Normalized score (0-100) | {{app_normalized_score}} |
| Band | {{app_band}} |

**Health-Factor Scores**

| Factor | Native score | Normalized | Notes |
|---|---|---|---|
| {{factor_1_name}} | {{factor_1_score}} | {{factor_1_normalized}} | {{factor_1_notes}} |
| {{factor_2_name}} | {{factor_2_score}} | {{factor_2_normalized}} | {{factor_2_notes}} |
<!-- one row per health factor the source defines -->

**Benchmark Position**

| Criterion | Compliance | Quartile | Rank | Peer set |
|---|---|---|---|---|<!-- BEGIN bench_row -->
| {{bench_criterion}} | {{bench_compliance}} | {{bench_quartile}} | {{bench_rank}} | {{bench_peer_set}} |<!-- END bench_row -->
<!-- END per_application -->

## 5. Findings by Severity

### 5.1 RED / CRITICAL Findings (full detail)

<!-- BEGIN per_application_red -->
**{{app_name}}:**

<!-- BEGIN red_finding -->
{{finding_index}}. **{{finding_title}}** — {{finding_description}}
- **Component:** {{finding_component}}
- **Business impact:** {{finding_business_impact}}
- **Remediation:** {{finding_remediation}} (cost/effort: {{finding_cost_effort}})
{{finding_evidence_block}}
<!-- END red_finding -->
<!-- END per_application_red -->

### 5.2 AMBER / MEDIUM Findings (full detail, more compact)

<!-- BEGIN per_application_amber -->
**{{app_name}}:**

<!-- BEGIN amber_finding -->
{{finding_index}}. **{{finding_title}}** — {{finding_description}}
- **Component:** {{finding_component}}
- **Remediation:** {{finding_remediation}}
{{finding_evidence_block}}
<!-- END amber_finding -->
<!-- END per_application_amber -->

### 5.3 GREEN / POSITIVE Findings (topic list ONLY — no elaboration)

<!-- Per the no-green-analysis rule: one line per topic, nothing else.
     Do not add root cause, evidence, or remediation for green items. -->

<!-- BEGIN green_topic -->
- {{green_topic}}
<!-- END green_topic -->

## Code Quality & Architecture

<!-- #6004: re-projects data already loaded for §3/§4/§5 — complexity
     distribution, LoC/tech-stack, and maintainability (refactor/code-smell)
     findings — into its own section. No new data source. -->

{{code_quality_summary_paragraph}}

<!-- BEGIN code_quality_row -->
| Application | LoC | Primary tech | Complexity profile | Maintainability findings |
|---|---|---|---|---|
| {{cq_app_name}} | {{cq_loc}} | {{cq_tech}} | {{cq_complexity}} | {{cq_maintainability_count}} |
<!-- END code_quality_row -->

<!-- #6147: the crate topology, read from the audited workspace's own cargo
     metadata. Absent for a repository that is not a Cargo workspace, and the
     whole block then renders as nothing. -->

<!-- BEGIN crate_topology -->
{{ct_summary}}

| Crate | Direct internal deps | Depended on by |
|---|---|---|
<!-- BEGIN ct_row -->
| {{ct_crate}} | {{ct_deps}} | {{ct_inbound}} |
<!-- END ct_row -->
<!-- END crate_topology -->

## Security Posture

*This table counts the RED and AMBER findings the repo-evidence investigation
raised in its "authentication & secrets" dimension — an LLM reading the
selected source files, with every finding's evidence quote mechanically
verified against the file it cites. Its scope is code hygiene around
credentials, tokens, and authentication paths in the files that were read. It
is not a SAST scan, not a dependency/CVE scan, and not a secrets scan of the
whole tree; the share of files read is stated under Investigation Coverage.
Read a low count as a small sample, not as a clean bill of health.*

{{security_summary_paragraph}}

| Application | Dimension | RED/AMBER findings |
|---|---|---|<!-- BEGIN security_violations_table -->
| {{app_name}} | {{violation_domain}} | {{violation_count}} |<!-- END security_violations_table -->

{{security_clean_signals}}

## Performance & Scalability

<!-- #6004: fixed text, never LLM-generated — DOC-67 §3 declares this
     dimension unavailable; no performance data source exists in this
     pipeline. -->

{{performance_assessment_note}}

## Authorship & Key-Person Risk

<!-- #5453/#6004: derivation lives in tga, never here (#5468 ruling). Key-man
     risks render IN this section — bus factor, ownership concentration,
     single-author subsystems — never scattered across Top Risks. Five known
     data traps (issue #5453): bot commits and merge commits are excluded by
     the derivation; squash-merge attribution, missing .mailmap identity
     merging, and vendored-path exclusion are NOT corrected for and are
     caveated below verbatim. A repository whose artifact failed to load
     contributes no row here — its own gap line names the omission under
     Gaps & Caveats, never a silently absent section. -->

{{authorship_summary_paragraph}}

<!-- High-level trailing-12-month development-trajectory narrative — codebase
     health from an authorship perspective, not a data dump. -->

| Application | Distinct authors | Bus factor | Top author share | Single-author subsystems | 12-mo trajectory |
|---|---|---|---|---|---|<!-- BEGIN authorship_row -->
| {{au_app_name}} | {{au_distinct_authors}} | {{au_bus_factor}} | {{au_top_author_share}} | {{au_single_author_subsystems}} | {{au_trajectory}} |<!-- END authorship_row -->

{{au_caveats}}

## 6. Risk Registers

### 6.1 Open-Source / CVE Exposure

<!-- BEGIN cve_table -->
| Application | Component | Critical | High | Medium |
|---|---|---|---|---|
| {{app_name}} | {{component_name}} | {{cve_critical}} | {{cve_high}} | {{cve_medium}} |
<!-- END cve_table -->

### 6.2 License / IP Risk

<!-- BEGIN license_risk_table -->
| Application | License | Risk factor | Component count | Top component |
|---|---|---|---|---|
| {{app_name}} | {{license_name}} | {{license_risk_factor}} | {{license_component_count}} | {{license_top_component}} |
<!-- END license_risk_table -->

### 6.3 Obsolescence

<!-- BEGIN obsolescence_table -->
| Application | Total components | % ≥5yr gap | % ≥5yr old | % no release 5yr |
|---|---|---|---|---|
| {{app_name}} | {{obs_total}} | {{obs_gap_pct}} | {{obs_age_pct}} | {{obs_stale_pct}} |
<!-- END obsolescence_table -->

### 6.4 Cloud Readiness Blockers

<!-- BEGIN cloud_readiness_table -->
| Application | Cloud maturity % | Blockers % | Roadblock count | Top blocking technology |
|---|---|---|---|---|
| {{app_name}} | {{cloud_maturity_pct}} | {{cloud_blockers_pct}} | {{cloud_roadblock_count}} | {{cloud_top_tech}} |
<!-- END cloud_readiness_table -->

### 6.5 Technical-Debt / Remediation Economics

<!-- BEGIN remediation_economics_table -->
| Application | Tier | Violations addressed | Effort (person-days) | Cost |
|---|---|---|---|---|
| {{app_name}} | {{remediation_tier}} | {{remediation_violations}} | {{remediation_effort}} | {{remediation_cost}} |
<!-- END remediation_economics_table -->

## 7. Graph-Ready Data Appendix

<!-- Mandated section: one pipe-table dataset per canonical chart, each
     introduced by a dataset comment so downstream tooling can lift it
     mechanically without parsing prose. -->

<!-- dataset: health_factors_by_app | chart: radar | x: factor | y: score, group: application -->
| Application | Factor | Score |
|---|---|---|
| {{app_name}} | {{factor_name}} | {{factor_score}} |

<!-- dataset: tqi_benchmark_position | chart: bar | x: application | y: quartile_rank -->
| Application | Peer set | Compliance % | Quartile | Rank |
|---|---|---|---|---|<!-- BEGIN benchmark_position -->
| {{app_name}} | {{peer_set}} | {{compliance_pct}} | {{quartile}} | {{rank}} |<!-- END benchmark_position -->

<!-- dataset: violations_by_domain | chart: stacked-bar | x: application | y: violation_count, group: domain -->
| Application | Domain | Violation count |
|---|---|---|
| {{app_name}} | {{domain_name}} | {{domain_count}} |

<!-- dataset: cve_by_component_severity | chart: heatmap | x: component | y: severity -->
| Application | Component | Severity | CVE ids |
|---|---|---|---|
| {{app_name}} | {{component_name}} | {{severity_level}} | {{cve_ids}} |

<!-- dataset: license_risk_tiers | chart: bar | x: license | y: component_count -->
| Application | License | Risk tier | Component count |
|---|---|---|---|
| {{app_name}} | {{license_name}} | {{risk_tier}} | {{component_count}} |

<!-- dataset: cloud_maturity_by_tech | chart: stacked-bar | x: technology | y: roadblock_count, group: application -->
| Application | Technology | Roadblock count | Effort (days) |
|---|---|---|---|
| {{app_name}} | {{tech_name}} | {{roadblock_count}} | {{roadblock_effort}} |

<!-- dataset: violations_by_horizon | chart: bar | x: horizon | y: violation_count, group: application -->
| Application | Horizon | Violation count |
|---|---|---|
| {{app_name}} | {{horizon_name}} | {{horizon_count}} |

<!-- dataset: remediation_cost_by_tier | chart: bar | x: tier | y: cost, group: application -->
| Application | Tier | Cost | Effort (person-days) |
|---|---|---|---|
| {{app_name}} | {{tier_name}} | {{tier_cost}} | {{tier_effort}} |

<!-- dataset: loc_by_technology | chart: stacked-bar | x: application | y: loc, group: technology -->
| Application | Technology | LoC | % of total |
|---|---|---|---|<!-- BEGIN loc_by_tech_row -->
| {{app_name}} | {{tech_name}} | {{tech_loc}} | {{tech_pct}} |<!-- END loc_by_tech_row -->

<!-- dataset: complexity_distribution | chart: bar | x: complexity_bucket | y: count, group: application -->
| Application | Complexity bucket | Count | % of total complexity |
|---|---|---|---|<!-- BEGIN complexity_bucket_row -->
| {{app_name}} | {{complexity_bucket}} | {{complexity_count}} | {{complexity_pct}} |<!-- END complexity_bucket_row -->

<!-- Add further `<!-- dataset: ... -->` blocks for any other chart the
     source report presents (size distributions, green-impact deficiencies,
     documentation scores, etc.) — one dataset per canonical chart. -->

## 8. Ticketing & Delivery Traceability

<!-- Engagement-wide commit ↔ board-item correlation, measured by the
     collection sweep that produced this report's data. Counts and the boards
     they came from — deliberately no linkage-quality score, grade, or band:
     nothing here is calibrated against a peer population, and an uncalibrated
     ratio in this document would be read as one. When the producing run
     supplied no correlation data this section collapses and is named under
     Gaps & Caveats, never silently omitted. -->

{{ticketing_coverage}}

## 9. Gaps & Caveats

- {{gap_1}}
- {{gap_2}}
<!-- Cover: what the source report does not state; values marked
     "(approx, from chart)"; any inference the analyst made beyond the
     literal source text (e.g. inferring a real-world target identity from
     internal naming conventions). -->

---
*Generated by trusty-review report analysis — template report-technical-dd v0.1*
*Source: {{source_document_filename}}*
