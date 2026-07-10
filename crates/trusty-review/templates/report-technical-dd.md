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
  Under `--synthesize`, three narrative sections are LLM-written: the executive
  summary, the top-risks rows, and per-finding elaboration prose. Each has a
  generic built-in instruction (see `src/report/section_instructions.rs`) that
  a template may override for its own methodology's voice by embedding a
  `<!-- instruct:<section_id> ... -->` comment anywhere in the file, where
  `<section_id>` is one of `executive_summary`, `top_risks`,
  `finding_elaboration`. The override REPLACES the generic default for that
  section only. This generic template ships no override (it uses the crate's
  defaults for all three sections) — see `report-technical-dd-cast.md` for a
  worked example overriding `executive_summary` with CAST health-factor
  language. An analyst's `--instructions` brief still applies ADDITIVELY on top
  of whichever instruction is active — it steers emphasis, never replaces it.
  `instruct:` comments are parsed at template-load time and, like every other
  comment here, are stripped from rendered output.
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
| Target / deal codename | {{target_codename}} |
| Client | {{client_name}} |
| Applications / systems assessed | {{applications_list}} |
| Analyst (this instance) | {{analyst_name}} |
| Analysis generated | {{analysis_generated_date}} |

## 2. Executive Summary

{{executive_summary_paragraph}}

<!-- One paragraph, deal-relevant: what matters to an acquirer, synthesized
     across all applications — not a restatement of section headers. -->

### Top Risks

| # | Risk | Severity | Est. cost/effort | Affected application(s) |
|---|---|---|---|---|
| 1 | {{risk_1_description}} | {{risk_1_severity}} | {{risk_1_cost}} | {{risk_1_apps}} |
| 2 | {{risk_2_description}} | {{risk_2_severity}} | {{risk_2_cost}} | {{risk_2_apps}} |
| 3 | {{risk_3_description}} | {{risk_3_severity}} | {{risk_3_cost}} | {{risk_3_apps}} |
<!-- extend to 4-5 rows if the source supports it; do not pad with filler -->

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
### 4.N. {{app_name}}

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
N. **{{finding_title}}** — {{finding_description}}. Evidence: {{finding_evidence}}. Affected component: {{finding_component}}. Business impact: {{finding_business_impact}}. Remediation: {{finding_remediation}} (cost/effort: {{finding_cost_effort}}).
<!-- END red_finding -->
<!-- END per_application_red -->

### 5.2 AMBER / MEDIUM Findings (full detail, more compact)

<!-- BEGIN per_application_amber -->
**{{app_name}}:**

<!-- BEGIN amber_finding -->
N. **{{finding_title}}** — {{finding_description}}. Evidence: {{finding_evidence}}. Remediation: {{finding_remediation}}.
<!-- END amber_finding -->
<!-- END per_application_amber -->

### 5.3 GREEN / POSITIVE Findings (topic list ONLY — no elaboration)

<!-- Per the no-green-analysis rule: one line per topic, nothing else.
     Do not add root cause, evidence, or remediation for green items. -->

- {{green_topic_1}}
- {{green_topic_2}}
- {{green_topic_3}}
<!-- one bullet per positive topic -->

## 6. Risk Registers

### 6.1 Security Violations

<!-- BEGIN security_violations_table -->
| Application | Domain | Total violations |
|---|---|---|
| {{app_name}} | {{violation_domain}} | {{violation_count}} |
<!-- END security_violations_table -->

### 6.2 Open-Source / CVE Exposure

<!-- BEGIN cve_table -->
| Application | Component | Critical | High | Medium |
|---|---|---|---|---|
| {{app_name}} | {{component_name}} | {{cve_critical}} | {{cve_high}} | {{cve_medium}} |
<!-- END cve_table -->

### 6.3 License / IP Risk

<!-- BEGIN license_risk_table -->
| Application | License | Risk factor | Component count | Top component |
|---|---|---|---|---|
| {{app_name}} | {{license_name}} | {{license_risk_factor}} | {{license_component_count}} | {{license_top_component}} |
<!-- END license_risk_table -->

### 6.4 Obsolescence

<!-- BEGIN obsolescence_table -->
| Application | Total components | % ≥5yr gap | % ≥5yr old | % no release 5yr |
|---|---|---|---|---|
| {{app_name}} | {{obs_total}} | {{obs_gap_pct}} | {{obs_age_pct}} | {{obs_stale_pct}} |
<!-- END obsolescence_table -->

### 6.5 Cloud Readiness Blockers

<!-- BEGIN cloud_readiness_table -->
| Application | Cloud maturity % | Blockers % | Roadblock count | Top blocking technology |
|---|---|---|---|---|
| {{app_name}} | {{cloud_maturity_pct}} | {{cloud_blockers_pct}} | {{cloud_roadblock_count}} | {{cloud_top_tech}} |
<!-- END cloud_readiness_table -->

### 6.6 Technical-Debt / Remediation Economics

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
|---|---|---|---|
| {{app_name}} | {{tech_name}} | {{tech_loc}} | {{tech_pct}} |

<!-- dataset: complexity_distribution | chart: bar | x: complexity_bucket | y: count, group: application -->
| Application | Complexity bucket | Count | % of total complexity |
|---|---|---|---|
| {{app_name}} | {{complexity_bucket}} | {{complexity_count}} | {{complexity_pct}} |

<!-- Add further `<!-- dataset: ... -->` blocks for any other chart the
     source report presents (size distributions, green-impact deficiencies,
     documentation scores, etc.) — one dataset per canonical chart. -->

## 8. Gaps & Caveats

- {{gap_1}}
- {{gap_2}}
<!-- Cover: what the source report does not state; values marked
     "(approx, from chart)"; any inference the analyst made beyond the
     literal source text (e.g. inferring a real-world target identity from
     internal naming conventions). -->

---
*Generated by trusty-review report analysis — template report-technical-dd v0.1*
*Source: {{source_document_filename}}*
