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

  SECTION-INSTRUCTION OVERRIDES (#2357 layered instructions)
  Under `--synthesize`, three narrative sections are LLM-written: the executive
  summary, the top-risks rows, and per-finding elaboration prose. Each has a
  generic built-in instruction (see `src/report/section_instructions.rs`) that
  this template may override for its OWN methodology's voice by embedding a
  `<!-- instruct:<section_id> ... -->` comment anywhere in this file, where
  `<section_id>` is one of `executive_summary`, `top_risks`,
  `finding_elaboration`. The override text REPLACES the generic default for
  that section only; other sections keep their generic default. An analyst's
  `--instructions` brief still applies ADDITIVELY on top of whichever
  instruction (generic or template-overridden) is active — it steers emphasis,
  never replaces the section instruction. `instruct:` comments are parsed at
  template-load time and, like every other comment here, are stripped from
  rendered output — they never appear in a generated report. This template
  demonstrates one override below (`executive_summary`, CAST health-factor
  voice).

  CODE-ONLY MARKERS (#6669)
  A code-only audit reaches sections no repository can answer. Rather than
  hardcode their names in Rust, this template marks its own regions:

    <!-- code_only:non_code <why it cannot be measured from code> -->
    ...the section's normal tables and placeholders...
    <!-- code_only:end -->

    <!-- code_only:partial -->
    ...code-derived content that is never cross-checked...
    <!-- code_only:end -->

  Under `--code-only`, a `non_code` region's body is REPLACED by a stated
  out-of-scope boundary (the heading always survives — a missing heading and a
  deliberate boundary must never look alike), and a `partial` region keeps its
  body and gains "Inferred from code; not validated by interview or
  operational data". Without `--code-only` both are ordinary comments and are
  stripped from output like every other comment here, so a full-scope render is
  unchanged. Regions must not nest: a region that opens another region before
  its own `code_only:end` is left untransformed and logged, exactly as one that
  is never closed. Parser: `src/report/code_only.rs`.

  #6004 (LANDED, #6669): the Code Quality & Architecture / Security Posture /
  Performance & Scalability sections the generic template carries now render
  here too, between §5 and §6. They are trusty-DERIVED, not CAST-scored: they
  are stated on trusty's own 0-100 / RED-AMBER-GREEN terms and are deliberately
  NOT expressed on CAST's 1.00-4.00 health-factor scale, because a
  health-factor-shaped number there would read as a CAST measurement and CAST
  measures no such thing. The Key Facts block and Contents jump-list remain
  deferred under #6004.
-->

<!-- instruct:executive_summary
Write ONE deal-analytic paragraph in CAST's health-factor voice: lead with the
TQI (Technical Quality Index) posture and name which of the five health
factors (Robustness, Efficiency, Security, Changeability, Transferability)
drive the RED/AMBER findings, severity-weighted (RED first), tied to what an
acquirer must act on. Reference a coverage gap ONLY if one is genuinely named
in the coverage data provided — and name it specifically.
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
| Inference models | {{inference_models}} |
| Target / deal codename | {{target_codename}} |
| Client | {{client_name}} |
| Applications / systems assessed | {{applications_list}} |
| Analysis methodology | Repository inspection via trusty-analyze (static code analysis, structural metrics, complexity measurement) + trusty-search (architecture context, KG-guided focus) |
| Audit scope | {{audit_scope}} |
| Analyst (this instance) | {{analyst_name}} |
| Analysis generated | {{analysis_generated_date}} |

## 2. Executive Summary

{{executive_summary_paragraph}}

<!-- One paragraph, deal-relevant: what matters to an acquirer, synthesized across
     all applications — not a restatement of section headers. -->

### Top Risks

| # | Risk | Severity | Est. cost/effort | Affected application(s) |
|---|---|---|---|---|<!-- BEGIN top_risk_row -->
| {{risk_rank}} | {{risk_description}} | {{risk_severity}} | {{risk_cost}} | {{risk_apps}} |<!-- END top_risk_row -->

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
### 4.{{app_index}}. {{app_name}}

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

<!-- code_only:non_code CAST's proprietary reference corpus of thousands of
     scanned applications, against which a quartile and a rank are computed -->
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
<!-- code_only:end -->

<!-- END per_application -->

## 5. Findings by Severity

### 5.1 RED / CRITICAL Findings (full detail)

<!-- BEGIN per_application_red -->
**{{app_name}}:**

<!-- BEGIN red_finding -->
{{finding_index}}. **{{finding_title}}** — {{finding_description}}
- **Component:** {{finding_component}}
- **Business impact:** {{finding_business_impact}}
- **Root cause:** {{finding_root_cause}}
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
- **Business impact:** {{finding_business_impact}}
- **Remediation:** {{finding_remediation}}
{{finding_evidence_block}}
<!-- END amber_finding -->
<!-- END per_application_amber -->

### 5.3 GREEN / POSITIVE Findings (topic list ONLY — no elaboration)

<!-- Per the no-green-analysis rule: one line per topic, nothing else. -->

<!-- BEGIN green_topic -->
- {{green_topic}}
<!-- END green_topic -->

## Code Quality & Architecture

<!-- #6004/#6669: ported from the generic template. Re-projects data already
     loaded for §4/§5 — complexity distribution, LoC/tech-stack, and
     maintainability (refactor/code-smell) findings. No new data source. -->

*trusty-derived, NOT CAST-scored. The figures below are trusty-analyze's own
structural measurements. They are deliberately not restated on CAST's 1.00–4.00
health-factor scale — CAST's Changeability grade is computed from its own
certified rule catalog, which this pipeline does not implement, and a
health-factor-shaped number here would be read as a CAST measurement.*

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
|---|---|---|<!-- BEGIN ct_row -->
| {{ct_crate}} | {{ct_deps}} | {{ct_inbound}} |<!-- END ct_row -->
<!-- END crate_topology -->

## Security Posture

*trusty-derived, NOT CAST-scored, and NOT the §6.1 ISO-5055 domain. This table
counts the RED and AMBER findings the repo-evidence investigation raised in its
"authentication & secrets" dimension — an LLM reading the selected source files,
with each counted finding's evidence quote mechanically verified against the file
it cites. Its scope is code hygiene around credentials, tokens, and
authentication paths in the files that were read. It is not a SAST scan, not a
dependency/CVE scan, and not a secrets scan of the whole tree; the share of files
read is stated under Investigation Coverage. Read a low count as a small sample,
not as a clean bill of health.*

{{security_summary_paragraph}}

| Application | Dimension | RED/AMBER findings |
|---|---|---|<!-- BEGIN security_violations_table -->
| {{app_name}} | {{violation_domain}} | {{violation_count}} |<!-- END security_violations_table -->

{{security_clean_signals}}

## Performance & Scalability

<!-- #6004: fixed text, never LLM-generated — DOC-67 §3 declares this dimension
     unavailable; no performance data source exists in this pipeline. CAST's own
     Efficiency health factor is likewise not computed here. -->

{{performance_assessment_note}}

## 6. Risk Registers

### 6.1 ISO-5055 Compliance (Security, Reliability, Performance-Efficiency, Maintainability)

| Application | Domain | Total violations | Compliance % | Band |
|---|---|---|---|---|
| {{app_name}} | Security | {{iso_sec_violations}} | {{iso_sec_pct}}% | {{iso_sec_band}} |
| {{app_name}} | Reliability | {{iso_rel_violations}} | {{iso_rel_pct}}% | {{iso_rel_band}} |
| {{app_name}} | Performance-Efficiency | {{iso_perf_violations}} | {{iso_perf_pct}}% | {{iso_perf_band}} |
| {{app_name}} | Maintainability | {{iso_maint_violations}} | {{iso_maint_pct}}% | {{iso_maint_band}} |

### 6.2 Open-Source & CVE Exposure

<!-- code_only:partial -->
| Application | Component | Critical | High | Medium | Risk score |
|---|---|---|---|---|---|
| {{app_name}} | {{component_name}} | {{cve_critical}} | {{cve_high}} | {{cve_medium}} | {{oss_risk_score}}% |

Summary: {{total_components}} components detected; {{critical_cve_components}} with critical CVEs; {{high_cve_components}} with high-severity CVEs.
<!-- code_only:end -->

### 6.3 License / IP Risk

<!-- code_only:partial -->
| Application | License | Risk tier | Components | Top component | Remediation |
|---|---|---|---|---|---|
| {{app_name}} | {{license_name}} | {{license_risk}} | {{license_comp_count}} | {{license_top_comp}} | {{license_remediation}} |

Legal review of any copyleft or unusual-tier license here is a human step this
audit does not perform.
<!-- code_only:end -->

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

<!-- code_only:partial -->
| Application | Tier | Violations addressed | Own-code effort (PD) | 3rd-party effort (PD) | Cost | TQI improvement (gain %) |
|---|---|---|---|---|---|---|
| {{app_name}} | Fix before/at closing | {{rem_imm_violations}} | {{rem_imm_own_pd}} | {{rem_imm_3p_pd}} | {{rem_imm_cost}} | {{rem_imm_tqi_gain}} |
| {{app_name}} | Near-term after closing | {{rem_near_violations}} | {{rem_near_own_pd}} | {{rem_near_3p_pd}} | {{rem_near_cost}} | {{rem_near_tqi_gain}} |
| {{app_name}} | Mid-term | {{rem_mid_violations}} | {{rem_mid_own_pd}} | {{rem_mid_3p_pd}} | {{rem_mid_cost}} | {{rem_mid_tqi_gain}} |
| {{app_name}} | Long-term | {{rem_long_violations}} | {{rem_long_own_pd}} | {{rem_long_3p_pd}} | {{rem_long_cost}} | {{rem_long_tqi_gain}} |

Any cost figure here converts an effort estimate at a day rate the engagement
declared; the rate is not derived from the code.
<!-- code_only:end -->

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
|---|---|---|---|<!-- BEGIN loc_by_tech_row -->
| {{app_name}} | {{tech_name}} | {{tech_loc}} | {{tech_pct}} |<!-- END loc_by_tech_row -->

<!-- dataset: complexity_distribution | chart: bar | x: complexity_bucket | y: count, group: application -->
| Application | Complexity bucket | Count | % of total complexity |
|---|---|---|---|<!-- BEGIN complexity_bucket_row -->
| {{app_name}} | {{complexity_bucket}} | {{complexity_count}} | {{complexity_pct}} |<!-- END complexity_bucket_row -->

<!-- dataset: green_deficiencies_top10 | chart: bar | x: deficiency | y: occurrences, group: application -->
| Application | Deficiency | Technology | Occurrences | Effort (days) |
|---|---|---|---|---|
| {{app_name}} | {{deficiency_name}} | {{tech_name}} | {{deficiency_count}} | {{deficiency_effort}} |

## 8. Ticketing & Delivery Traceability

<!-- Engagement-wide commit ↔ board-item correlation, measured by the
     collection sweep that produced this report's data. Counts and the boards
     they came from — deliberately no linkage-quality score, grade, or band:
     nothing here is calibrated against a peer population, and an uncalibrated
     ratio in this document would be read as one. Deliberately NOT expressed on
     the CAST 1.00–4.00 scale for the same reason — a health-factor-shaped
     number here would read as a CAST measurement, and CAST measures no such
     thing. When the producing run supplied no correlation data this section
     collapses and is named under Gaps & Caveats, never silently omitted. -->

{{ticketing_coverage}}

## 9. Gaps & Caveats

- {{gap_1}}
- {{gap_2}}

<!-- Capture: what the source analysis does not state; approximate readings;
     inferences. Mark estimated values with "(approx, from chart)" to preserve
     honesty markers. -->

## 10. Next Steps

<!-- #6669: CAST's own Next Steps page is organizational — Scrum Master
     engagement, team-behaviour change, modernization-readiness timing. None of
     it is derivable from a repository, so under `--code-only` the heading
     stands and states that, rather than being dropped (indistinguishable from
     an omission) or filled with an invented recommendation. -->

<!-- code_only:non_code interviews with the delivery organization, whose team
     structure, process maturity and modernization appetite are what shape
     these recommendations -->
| Recommendation | Horizon | Owner |
|---|---|---|
| {{next_step_recommendation}} | {{next_step_horizon}} | {{next_step_owner}} |
<!-- code_only:end -->

---
*Generated by trusty-review report analysis — template report-technical-dd-cast v0.1*
*Analyzer: trusty-analyze (repository inspection); context guidance: trusty-search*
*Generated: {{analysis_generated_date}}*
