<!--
  trusty-review template: report-technical-dd-cast v0.1

  CAST METHODOLOGY SPECIFIC. This is a variant of the generic report-technical-dd.md
  template, specialized for CAST Highlight's health-factor model and scoring scales.

  WHAT THIS IS
  A reusable skeleton for generating CAST-style technical due-diligence analysis
  from repository inspection (via trusty-analyze + trusty-search). Captures CAST's
  native 1.00–4.00 health-factor scales (TQI, Robustness, Efficiency, Security,
  Changeability, Transferability), ISO-5055 compliance domains, Highlight-style
  scans (open-source risk, CVE exposure, license IP risk, cloud-native maturity,
  green/sustainability impact), and Appmarq-style peer benchmarking (quartiles,
  percentile ranks).

  HOW IT'S USED
  A trusty-review analysis run inspects a repository via trusty-analyze, fills every
  placeholder from metrics/data actually present in the analysis output, and produces
  a standalone markdown instance. The result is committed or shared as a standalone
  technical DD report.

  PLACEHOLDER SYNTAX
  - `{{field_name}}`   — a scalar value, filled verbatim from the source.
  - `<!-- BEGIN per_application --> ... <!-- END per_application -->`
    marks a repeatable block: duplicate the block once per application and fill
    each copy independently. Nested repeatable blocks follow the same convention.
  - Never invent a number. If the source is silent, write exactly:
    `not stated in source report`. If a value was visually estimated off an
    unlabeled chart, preserve the estimation marker `(approx, from chart)`.

  THE NO-GREEN-ANALYSIS RULE
  RED (critical) and AMBER (medium-risk) findings are reproduced in full
  analytical detail. GREEN (positive/healthy) findings get NO analysis: list them
  as a single line per topic, nothing more. Do not editorialize or add root-cause
  narrative to a green item.
-->

# CAST Technical Due-Diligence Analysis: {{target_codename}}

{{provenance_legend}}

{{analyst_instructions_block}}

## 1. Report Metadata

| Field | Value |
|---|---|
| Source document | {{source_document_reference}} |
| Vendor / methodology | CAST (CAST Software) — CAST Highlight + CAST Imaging |
| Report date / version | {{report_date}} / {{report_version}} |
| Target / deal codename | {{target_codename}} |
| Client | {{client_name}} |
| Applications / systems assessed | {{applications_list}} |
| Analysis methodology | Repository inspection via trusty-analyze (static code analysis, structural metrics, complexity measurement) + trusty-search (architecture context, KG-guided focus) |
| Analyst (this instance) | {{analyst_name}} |
| Analysis generated | {{analysis_generated_date}} |

## 2. Executive Summary

{{executive_summary_paragraph}}

<!-- One paragraph, deal-relevant: what matters to an acquirer, synthesized across
     all applications — not a restatement of section headers. -->

### Top Risks

| # | Risk | Severity | Est. cost/effort | Affected application(s) |
|---|---|---|---|---|
| 1 | {{risk_1_description}} | {{risk_1_severity}} | {{risk_1_cost}} | {{risk_1_apps}} |
| 2 | {{risk_2_description}} | {{risk_2_severity}} | {{risk_2_cost}} | {{risk_2_apps}} |
| 3 | {{risk_3_description}} | {{risk_3_severity}} | {{risk_3_cost}} | {{risk_3_apps}} |
| 4 | {{risk_4_description}} | {{risk_4_severity}} | {{risk_4_cost}} | {{risk_4_apps}} |
| 5 | {{risk_5_description}} | {{risk_5_severity}} | {{risk_5_cost}} | {{risk_5_apps}} |

## 3. CAST Scoring Model & Normalization

The CAST health-factor model uses a **1.00–4.00 scale** per factor, where 1 = very high risk (red),
2 = high risk (amber), 3 = medium risk (yellow-green), 4 = low risk (green). The **TQI (Total Quality Index)**
is the aggregate mean of five/six health factors. Age-adjusted acceptability thresholds vary: <2yr old apps
expected >3.40; 2–5yr >3.20; 5–10yr >3.00; >10yr >2.70.

| Field | Value |
|---|---|
| Native scale | 1.00 (very high risk) to 4.00 (low risk); TQI = mean of five health factors |
| Native band definitions | 1 = Very high risk (red); 2 = High risk (orange/amber); 3 = Medium risk (yellow-green); 4 = Low risk (green) |
| Normalized mapping (0-100) | `normalized = (native_score - 1.00) / 3.00 * 100` |
| RED threshold (normalized) | < 33 (native < 2.00) |
| AMBER threshold (normalized) | 33–66 (native 2.00–2.99) |
| GREEN threshold (normalized) | > 66 (native >= 3.00) |
| Health factors aggregated | Application-level grades aggregate from technical criteria and rules directly (not from module-level grades); application and module grades can diverge |
| Peer-benchmark population | All technologies (size TBD from analysis corpus; historical CAST reference: ~3,467 apps), plus technology-specific peer sets (HTML5, .NET, Java, Python, Go, etc.) |

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
| Application age (est.) | {{app_age}} |
| Risk tier (native) | {{app_risk_tier}} |
| Normalized score (0-100) | {{app_normalized_score}} |
| Band | {{app_band}} |

**Health-Factor Scores (native 1-4 scale, mapped to normalized 0-100)**

| Factor | Native score | Normalized (0-100) | Business meaning |
|---|---|---|---|
| Robustness | {{robustness_score}} | {{robustness_normalized}} | Risk of outage, data-integrity, or reliability issues |
| Efficiency | {{efficiency_score}} | {{efficiency_normalized}} | Resource consumption, scalability, performance issues |
| Security | {{security_score}} | {{security_normalized}} | Security vulnerabilities & breach likelihood |
| Changeability | {{changeability_score}} | {{changeability_normalized}} | Adaptability to changing regs/business needs |
| Transferability | {{transferability_score}} | {{transferability_normalized}} | Ramp-up difficulty for newcomers |
| **TQI** | **{{tqi_score}}** | **{{tqi_normalized}}** | Aggregate of all quality rules |

**Peer Benchmark Position**

vs. {{tech_specific_peer_set}}:

| Criterion | Compliance | Quartile | Rank | Notes |
|---|---|---|---|---|
| TQI | {{bench_tqi_comp}}% | {{bench_tqi_q}} | {{bench_tqi_rank}} | {{bench_tqi_notes}} |
| Robustness | {{bench_robust_comp}}% | {{bench_robust_q}} | {{bench_robust_rank}} | |
| Security | {{bench_sec_comp}}% | {{bench_sec_q}} | {{bench_sec_rank}} | |
| Efficiency | {{bench_eff_comp}}% | {{bench_eff_q}} | {{bench_eff_rank}} | |
| Changeability | {{bench_change_comp}}% | {{bench_change_q}} | {{bench_change_rank}} | {{bench_change_notes}} |
| Transferability | {{bench_xfer_comp}}% | {{bench_xfer_q}} | {{bench_xfer_rank}} | |

vs. All technologies (peer set size: {{all_tech_peer_set_size}}):

| Criterion | Compliance | Quartile | Rank | Notes |
|---|---|---|---|---|
| TQI | {{bench_all_tqi_comp}}% | {{bench_all_tqi_q}} | {{bench_all_tqi_rank}} | |
| Robustness | {{bench_all_robust_comp}}% | {{bench_all_robust_q}} | {{bench_all_robust_rank}} | |
| Security | {{bench_all_sec_comp}}% | {{bench_all_sec_q}} | {{bench_all_sec_rank}} | |
| Efficiency | {{bench_all_eff_comp}}% | {{bench_all_eff_q}} | {{bench_all_eff_rank}} | |
| Changeability | {{bench_all_change_comp}}% | {{bench_all_change_q}} | {{bench_all_change_rank}} | |
| Transferability | {{bench_all_xfer_comp}}% | {{bench_all_xfer_q}} | {{bench_all_xfer_rank}} | |

<!-- END per_application -->

## 5. Findings by Severity

### 5.1 RED / CRITICAL Findings (full detail)

<!-- BEGIN per_application_red -->
**{{app_name}}:**

<!-- BEGIN red_finding -->
N. **{{finding_title}}** — {{finding_description}}. Evidence: {{finding_evidence}}. Affected component: {{finding_component}}. Business impact: {{finding_business_impact}}. Root cause: {{finding_root_cause}}. Remediation: {{finding_remediation}} (cost/effort: {{finding_cost_effort}}).
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

<!-- Per the no-green-analysis rule: one line per topic, nothing else. -->

- {{green_topic_1}}
- {{green_topic_2}}
- {{green_topic_3}}

## 6. Risk Registers

### 6.1 ISO-5055 Compliance (Security, Reliability, Performance-Efficiency, Maintainability)

| Application | Domain | Total violations | Compliance % | Band |
|---|---|---|---|---|
| {{app_name}} | Security | {{iso_sec_violations}} | {{iso_sec_pct}}% | {{iso_sec_band}} |
| {{app_name}} | Reliability | {{iso_rel_violations}} | {{iso_rel_pct}}% | {{iso_rel_band}} |
| {{app_name}} | Performance-Efficiency | {{iso_perf_violations}} | {{iso_perf_pct}}% | {{iso_perf_band}} |
| {{app_name}} | Maintainability | {{iso_maint_violations}} | {{iso_maint_pct}}% | {{iso_maint_band}} |

### 6.2 Open-Source & CVE Exposure

| Application | Component | Critical | High | Medium | Risk score |
|---|---|---|---|---|---|
| {{app_name}} | {{component_name}} | {{cve_critical}} | {{cve_high}} | {{cve_medium}} | {{oss_risk_score}}% |

Summary: {{total_components}} components detected; {{critical_cve_components}} with critical CVEs; {{high_cve_components}} with high-severity CVEs.

### 6.3 License / IP Risk

| Application | License | Risk tier | Components | Top component | Remediation |
|---|---|---|---|---|---|
| {{app_name}} | {{license_name}} | {{license_risk}} | {{license_comp_count}} | {{license_top_comp}} | {{license_remediation}} |

### 6.4 Open-Source Component Obsolescence

| Application | Total components | % >=5yr gap | % >=5yr old | % no release 5yr | Action |
|---|---|---|---|---|---|
| {{app_name}} | {{obs_total}} | {{obs_gap_pct}} | {{obs_age_pct}} | {{obs_stale_pct}} | {{obs_action}} |

### 6.5 Cloud-Native Compliance / PaaS Maturity

| Application | Cloud maturity % | Boosters % | Blockers % | Roadblock count | Top-blocker technology | Remediation complexity |
|---|---|---|---|---|---|---|
| {{app_name}} | {{cloud_maturity}} | {{cloud_boosters}} | {{cloud_blockers}} | {{cloud_roadblock_count}} | {{cloud_top_tech}} | {{cloud_complexity}} |

### 6.6 Green Impact / Sustainability Scan

| Application | Green score % | Green IT issues | Industry benchmark % | Worst / Best benchmark % | Top deficiency | Effort (days) |
|---|---|---|---|---|---|---|
| {{app_name}} | {{green_score}} | {{green_issues}} | {{green_industry}} | {{green_worst}} / {{green_best}} | {{green_top_deficiency}} | {{green_effort}} |

### 6.7 Remediation Economics — Horizon Tiers

| Application | Tier | Violations addressed | Own-code effort (PD) | 3rd-party effort (PD) | Cost | TQI improvement (gain %) |
|---|---|---|---|---|---|---|
| {{app_name}} | Fix before/at closing | {{rem_imm_violations}} | {{rem_imm_own_pd}} | {{rem_imm_3p_pd}} | {{rem_imm_cost}} | {{rem_imm_tqi_gain}} |
| {{app_name}} | Near-term after closing | {{rem_near_violations}} | {{rem_near_own_pd}} | {{rem_near_3p_pd}} | {{rem_near_cost}} | {{rem_near_tqi_gain}} |
| {{app_name}} | Mid-term | {{rem_mid_violations}} | {{rem_mid_own_pd}} | {{rem_mid_3p_pd}} | {{rem_mid_cost}} | {{rem_mid_tqi_gain}} |
| {{app_name}} | Long-term | {{rem_long_violations}} | {{rem_long_own_pd}} | {{rem_long_3p_pd}} | {{rem_long_cost}} | {{rem_long_tqi_gain}} |

## 7. Graph-Ready Data Appendix

<!-- dataset: health_factors_by_app | chart: radar | x: factor | y: score, group: application -->
| Application | Factor | Score (native 1-4) | Normalized (0-100) |
|---|---|---|---|
| {{app_name}} | {{factor_name}} | {{factor_score}} | {{factor_normalized}} |

<!-- dataset: tqi_benchmark_position | chart: bar | x: application | y: tqi_rank -->
| Application | Peer set | Compliance % | Quartile | Rank | Rank total |
|---|---|---|---|---|---|<!-- BEGIN benchmark_position -->
| {{app_name}} | {{peer_set}} | {{tqi_comp}} | {{tqi_q}} | {{tqi_rank}} | {{tqi_rank_total}} |<!-- END benchmark_position -->

<!-- dataset: violations_by_iso_domain | chart: stacked-bar | x: application | y: violation_count, group: domain -->
| Application | Domain | Violation count | Compliance % |
|---|---|---|---|
| {{app_name}} | {{domain_name}} | {{domain_count}} | {{domain_comp}} |

<!-- dataset: cve_by_component_severity | chart: heatmap | x: component | y: severity -->
| Application | Component | Severity | CVE ids / Count |
|---|---|---|---|
| {{app_name}} | {{component_name}} | {{severity_level}} | {{cve_ids}} |

<!-- dataset: license_risk_tiers | chart: bar | x: license | y: component_count -->
| Application | License | Risk tier | Component count |
|---|---|---|---|
| {{app_name}} | {{license_name}} | {{risk_tier}} | {{component_count}} |

<!-- dataset: cloud_maturity_by_tech | chart: stacked-bar | x: technology | y: roadblock_count, group: application -->
| Application | Technology | Roadblock count | Effort (days) | Criticality |
|---|---|---|---|---|
| {{app_name}} | {{tech_name}} | {{roadblock_count}} | {{roadblock_effort}} | {{criticality}} |

<!-- dataset: violations_by_horizon | chart: bar | x: horizon | y: violation_count, group: application -->
| Application | Horizon | Violation count | Cost | Effort (PD) |
|---|---|---|---|---|
| {{app_name}} | {{horizon_name}} | {{horizon_count}} | {{horizon_cost}} | {{horizon_effort}} |

<!-- dataset: loc_by_technology | chart: stacked-bar | x: application | y: loc, group: technology -->
| Application | Technology | LoC | % of total |
|---|---|---|---|
| {{app_name}} | {{tech_name}} | {{tech_loc}} | {{tech_pct}} |

<!-- dataset: complexity_distribution | chart: bar | x: complexity_bucket | y: count, group: application -->
| Application | Complexity bucket | Count | % of total complexity |
|---|---|---|---|
| {{app_name}} | {{complexity_bucket}} | {{complexity_count}} | {{complexity_pct}} |

<!-- dataset: green_deficiencies_top10 | chart: bar | x: deficiency | y: occurrences, group: application -->
| Application | Deficiency | Technology | Occurrences | Effort (days) |
|---|---|---|---|---|
| {{app_name}} | {{deficiency_name}} | {{tech_name}} | {{deficiency_count}} | {{deficiency_effort}} |

## 8. Gaps & Caveats

- {{gap_1}}
- {{gap_2}}

<!-- Capture: what the source analysis does not state; approximate readings;
     inferences. Mark estimated values with "(approx, from chart)" to preserve
     honesty markers. -->

---
*Generated by trusty-review report analysis — template report-technical-dd-cast v0.1*
*Analyzer: trusty-analyze (repository inspection); context guidance: trusty-search*
*Generated: {{analysis_generated_date}}*
