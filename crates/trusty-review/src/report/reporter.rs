//! Report reporter — model → scope → markdown + JSON, atomic write (M1, #2313).
//!
//! Why: the reporter is the deterministic rendering layer: it maps the resolved
//! [`ReportModel`] onto template placeholders/blocks, renders markdown, and
//! writes a `{slug}.md` / `{slug}.json` pair atomically so a concurrent reader
//! never sees a half-written file.  All fill is deterministic — no LLM (M1).
//! What: [`Reporter`] holds the output directory; `render` builds the [`Scope`]
//! from the model and fills the template; `write` renders and persists both
//! outputs, returning their paths.  Unmapped placeholders fall through to the
//! honesty marker via the fill engine.
//! Test: `reporter_tests.rs` covers scope mapping, markdown substrings, JSON
//! round-trip, and the atomic-write file layout.

use std::path::{Path, PathBuf};

use tracing::info;

use super::error::{ReportError, Result};
use super::fill::{Scope, render, strip_leading_comment};
use super::manifest::slugify;
use super::metrics::Severity;
use super::model::{ReportModel, RepositoryReport};
use super::polish::polish;
use super::provenance::{self, Provenance, tag};
use super::reporter_fill::{crate_version, fill_profile, instructions_block, set_scoring_model};

/// Renders a [`ReportModel`] to markdown + JSON and writes them atomically.
///
/// Why: separating rendering/output from model assembly lets tests render a
/// model without a filesystem and keeps the CLI handler thin.
/// What: `output_dir` is where `{slug}.md` and `{slug}.json` are written.
/// Test: `reporter_tests.rs::{render_contains_expected, write_emits_both}`.
pub struct Reporter {
    output_dir: PathBuf,
}

impl Reporter {
    /// Create a reporter writing to `output_dir`.
    ///
    /// Why: callers choose the output directory (`--out`, default `./reports`).
    /// What: stores the directory; it is created on `write` if absent.
    /// Test: `reporter_tests.rs::write_emits_both`.
    pub fn new(output_dir: impl Into<PathBuf>) -> Self {
        Self {
            output_dir: output_dir.into(),
        }
    }

    /// Render the model into markdown using the supplied template source.
    ///
    /// Why: exposed separately so tests can assert on rendered markdown without
    /// touching disk.
    /// What: strips the template's leading instructional `<!-- … -->` comment
    /// (live-QA defect #2314 — a generated report must never carry template
    /// authoring instructions, and the header's own literal `{{field}}` /
    /// BEGIN-END documentation examples would otherwise be mangled by the fill
    /// engine), builds the fill [`Scope`] from the model, and renders the
    /// remainder.
    /// Test: `reporter_tests.rs::{render_contains_expected,
    /// reporter_strips_leading_comment_header}`.
    pub fn render(&self, model: &ReportModel, template: &str) -> String {
        let scope = build_scope(model);
        // Fill deterministically, then polish the OUTPUT (#2342): strip every
        // non-dataset template comment, drop honesty-marker rows, collapse empty
        // sections, and gather the gaps.  The leading-comment strip stays a
        // pre-fill step so the header's literal `{{…}}`/BEGIN-END examples are
        // never mistaken for real placeholders by the fill engine.
        let filled = render(strip_leading_comment(template), &scope);
        let mut out = polish(&filled);
        // Status notes are appended AFTER polish so their bullets are not subject
        // to omit-empty (they are always meaningful provenance, never markers).
        append_synthesis_note(&mut out, model);
        append_benchmark_note(&mut out, model);
        // Wave-3 (#2357): the Dependency Inventory and Investigation Coverage
        // sections, plus the rejected-evidence note, are appended after polish so
        // their measured/inferred rows are never subject to omit-empty.
        out.push_str(&super::investigate::report_sections(model));
        out
    }

    /// Render and write `{slug}.md` + `{slug}.json` atomically to `output_dir`.
    ///
    /// Why: the CLI's terminal step — persist both the human report and its
    /// machine twin so downstream tooling can consume the JSON.
    /// What: creates `output_dir`, renders markdown, serializes the model to
    /// pretty JSON, and writes each via a temp-file + rename (atomic on the same
    /// filesystem).  Returns the two written paths.
    /// Test: `reporter_tests.rs::write_emits_both`.
    pub fn write(&self, model: &ReportModel, template: &str) -> Result<Vec<PathBuf>> {
        std::fs::create_dir_all(&self.output_dir).map_err(|source| ReportError::Io {
            path: self.output_dir.clone(),
            source,
        })?;

        let stem = report_stem(model);
        let markdown = self.render(model, template);
        let json = serde_json::to_string_pretty(model).map_err(|source| ReportError::Metrics {
            path: PathBuf::from("<model.json>"),
            source,
        })?;

        let md_path = self.output_dir.join(format!("{stem}.md"));
        let json_path = self.output_dir.join(format!("{stem}.json"));
        atomic_write(&md_path, markdown.as_bytes())?;
        atomic_write(&json_path, json.as_bytes())?;
        info!(md = %md_path.display(), json = %json_path.display(), "report written");

        Ok(vec![md_path, json_path])
    }
}

/// Compute the output file stem for a report: `{date}-{title-slug}`.
///
/// Why: a date-prefixed slug matches the spec's example filenames and keeps
/// repeated runs chronologically ordered.
/// What: joins the generation date with the slugified title.
/// Test: `reporter_tests.rs::stem_is_date_slug`.
fn report_stem(model: &ReportModel) -> String {
    format!("{}-{}", model.generated_date, slugify(&model.title))
}

/// Write `bytes` to `path` atomically via a temp file + rename.
///
/// Why: a reader must never observe a partially written report.
/// What: writes to a temp file in the same directory, then persists (renames)
/// it over `path`; rename is atomic on the same filesystem.
/// Test: `reporter_tests.rs::write_emits_both` (file exists and parses).
fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let tmp = tempfile::NamedTempFile::new_in(dir).map_err(|source| ReportError::Io {
        path: dir.to_path_buf(),
        source,
    })?;
    std::fs::write(tmp.path(), bytes).map_err(|source| ReportError::Io {
        path: tmp.path().to_path_buf(),
        source,
    })?;
    tmp.persist(path).map_err(|e| ReportError::Io {
        path: path.to_path_buf(),
        source: e.error,
    })?;
    Ok(())
}

/// Build the root fill [`Scope`] from a report model.
///
/// Why: this is the single place mapping model fields onto template placeholder
/// names; everything it does not set falls through to the honesty marker.
/// What: sets report-level scalars (codename, dates, analyst, applications list,
/// source provenance) and pushes one `per_application` child scope per repo.
/// Test: `reporter_tests.rs::render_contains_expected`.
fn build_scope(model: &ReportModel) -> Scope {
    let mut root = Scope::new();

    // Report metadata (report-level scalars).  Identity/title fields are left
    // untagged (they would clutter the H1); data-bearing fields carry a
    // provenance marker (see the legend rendered near the top).
    root.set("target_codename", model.title.clone());
    root.set("report_date", tag(&model.report_date, Provenance::Measured));
    root.set(
        "analysis_generated_date",
        tag(&model.generated_date, Provenance::Measured),
    );
    // #2342.2: self-derived metadata the tool KNOWS — never honesty-marked.
    root.set(
        "vendor_methodology",
        tag(&model.vendor_methodology, Provenance::Measured),
    );
    root.set("report_version", tag(crate_version(), Provenance::Measured));
    root.set("provenance_legend", provenance::LEGEND);
    root.set("analyst_instructions_block", instructions_block(model));
    // Declared deal-side fields: filled + tagged when the manifest supplies them,
    // otherwise omitted (→ Gaps) rather than rendered as a "not stated" row.
    if let Some(analyst) = &model.analyst {
        root.set("analyst_name", tag(analyst, Provenance::Declared));
    }
    if let Some(client) = &model.client {
        root.set("client_name", tag(client, Provenance::Declared));
    }
    let source_ref = format!("repository inspection (manifest: {})", model.manifest_path);
    root.set("source_document_filename", source_ref.clone());
    root.set("source_document_reference", source_ref);

    // #2342.2: Section 3 self-describes trusty-review's own scoring model — the
    // tool defines this scale, so it is measured, never "not stated".
    set_scoring_model(&mut root);

    let apps: Vec<String> = model.repositories.iter().map(|r| r.name.clone()).collect();
    if !apps.is_empty() {
        root.set(
            "applications_list",
            tag(apps.join(", "), Provenance::Declared),
        );
    }

    // One per_application block repetition per repository.  When benchmarking is
    // active, the matching per-repo placement is threaded into the scope so its
    // Benchmark Position table fills; without it the table stays honesty-marked.
    for repo in &model.repositories {
        let bench = model
            .benchmark
            .as_ref()
            .and_then(|b| b.repositories.iter().find(|r| r.slug == repo.slug));
        root.push_block("per_application", per_application_scope(repo, bench));
    }

    // M3: fill the graph-appendix benchmark dataset (one headline row per ranked
    // application).  Absent benchmarking leaves the block to render once empty,
    // byte-identical to the M2 honesty-marked output.
    if let Some(bench) = &model.benchmark {
        inject_benchmark_dataset(&mut root, model, bench);
    }

    // Live-QA defect #2314: RED/AMBER finding blocks are deterministic, not
    // synthesis-gated — title/category/component come verbatim from
    // `metrics.findings` regardless of `--synthesize`.  When verified synthesis
    // IS available, its prose is merged onto the SAME row for a matching title
    // (never pushed as a second, duplicate row); see `push_finding_band`.
    let available_synthesis = model.synthesis.as_ref().filter(|s| s.is_available());
    for repo in &model.repositories {
        push_finding_band(
            &mut root,
            repo,
            available_synthesis,
            Severity::Red,
            "RED",
            "per_application_red",
            "red_finding",
        );
        push_finding_band(
            &mut root,
            repo,
            available_synthesis,
            Severity::Amber,
            "AMBER",
            "per_application_amber",
            "amber_finding",
        );
    }

    // GREEN topics are metrics-only (title only, no-green-analysis rule) and are
    // never synthesized — filled independent of synthesis availability.
    push_green_topics(&mut root, model);

    // M2: the executive summary and top-risk rationale have no deterministic M1
    // source; fill them only from verified synthesis.  Absent/unavailable
    // synthesis leaves both to the M1 honesty marker (unchanged behaviour).
    if let Some(syn) = available_synthesis {
        inject_synthesis_summary(&mut root, syn);
    }

    root
}

/// Inject verified synthesis prose into the report-level narrative placeholders.
///
/// Why: the executive summary and top-risk rationale have no deterministic M1
/// source (no `metrics` field maps to them); M2 fills exactly these — and only
/// with prose that already passed the numeric guardrail.  RED/AMBER finding
/// prose is handled per-finding by [`push_finding_band`] (merged onto the
/// deterministic row, never duplicated).  Every LLM-written field is tagged
/// `inferred` (live-QA wave-2 defect #1 — the marker was defined but never
/// wired to any synthesized content).
/// What: sets the executive-summary scalar (tagged once, section granularity)
/// and the `risk_N_description` / `risk_N_cost` scalars (tagged per field) from
/// the synthesis's verified top-risk rows.  `risk_N_severity` / `risk_N_apps`
/// are left untagged — they restate categorical/identifier data (the RED/AMBER
/// band, the affected application names) rather than narrative prose.
/// Test: `reporter_tests.rs::{reporter_injects_synthesis_prose,
/// reporter_tags_top_risks_as_inferred}`.
fn inject_synthesis_summary(root: &mut Scope, syn: &super::synthesize::Synthesis) {
    if let Some(exec) = &syn.executive_summary {
        root.set(
            "executive_summary_paragraph",
            tag(exec.clone(), Provenance::Inferred),
        );
    }

    for (i, risk) in syn.top_risks.iter().enumerate() {
        let n = i + 1;
        root.set(
            format!("risk_{n}_description"),
            tag(risk.description.clone(), Provenance::Inferred),
        );
        root.set(format!("risk_{n}_severity"), risk.severity.clone());
        root.set(
            format!("risk_{n}_cost"),
            tag(risk.cost.clone(), Provenance::Inferred),
        );
        root.set(format!("risk_{n}_apps"), risk.apps.clone());
    }
}

/// One merged RED/AMBER finding row: deterministic metrics fields plus an
/// optional verified-synthesis prose overlay (live-QA defect #2314).
///
/// Why: a plain struct (rather than building [`Scope`]s directly) lets
/// [`push_finding_band`] merge synthesis prose onto an already-built
/// metrics-derived row by field, then convert to a `Scope` once, in one place.
/// What: `title` is always set (metrics or synthesis); every other field is
/// `Option` so an unset one falls through to the fill engine's honesty marker.
#[derive(Default)]
struct FindingRow {
    title: String,
    category: Option<String>,
    component: Option<String>,
    description: Option<String>,
    evidence: Option<String>,
    business_impact: Option<String>,
    remediation: Option<String>,
    cost_effort: Option<String>,
}

impl FindingRow {
    /// A bare deterministic row from one `metrics.findings` entry.
    ///
    /// Why: title/category/component are verbatim, always-available facts from
    /// trusty-analyze; the prose-only fields (description/evidence/business
    /// impact/remediation/cost) are synthesis's job and start unset so they
    /// render as the honesty marker until (and unless) synthesis fills them.
    /// What: copies `title`/`category`/`component`, mapping an empty source
    /// string to `None`.
    fn from_metric(f: &super::metrics::MetricFinding) -> Self {
        FindingRow {
            title: f.title.clone(),
            category: non_empty(&f.category),
            component: dedupe_field(&f.component),
            ..Default::default()
        }
    }

    /// A synthesis-only row for a verified finding with no `metrics.findings`
    /// title match (e.g. metrics were absent or reported the finding
    /// differently) — preserves synthesis's own title/component alongside its
    /// prose rather than silently dropping verified data.
    ///
    /// Why: the narrative fields are LLM-written, so each is tagged `inferred`
    /// (live-QA wave-2 defect #1); `component` is identifier-like routing data
    /// copied from context rather than a narrative sentence, so it is left
    /// untagged (dedupe-only, no provenance marker).
    /// What: builds the row from `f`, applying [`prose_field`] (dedupe trailing
    /// terminal punctuation + tag `Inferred`) to description/evidence/
    /// business_impact/remediation/cost_effort.
    fn from_prose(f: &super::synthesize::FindingProse) -> Self {
        FindingRow {
            title: f.title.clone(),
            category: None,
            component: dedupe_field(&f.component),
            description: prose_field(&f.description),
            evidence: evidence_field(&f.evidence, f.evidence_measured),
            business_impact: prose_field(&f.business_impact),
            remediation: prose_field(&f.remediation),
            cost_effort: prose_field(&f.cost_effort),
        }
    }

    /// Overlay verified synthesis prose onto this already metrics-backed row.
    ///
    /// Why: this is the "layer prose on top" step — the row already exists
    /// (from metrics), so synthesis fills the gaps rather than creating a
    /// second, duplicate entry for the same finding.  Each narrative field is
    /// tagged `inferred` (live-QA wave-2 defect #1).
    /// What: sets description/evidence/business_impact/remediation/cost_effort
    /// from `f` via [`prose_field`]; `component` is kept metrics-verbatim unless
    /// metrics left it unset, in which case synthesis's value is used as a
    /// dedupe-only (untagged) fallback.
    fn merge_prose(&mut self, f: &super::synthesize::FindingProse) {
        self.description = prose_field(&f.description);
        self.evidence = evidence_field(&f.evidence, f.evidence_measured);
        self.business_impact = prose_field(&f.business_impact);
        self.remediation = prose_field(&f.remediation);
        self.cost_effort = prose_field(&f.cost_effort);
        if self.component.is_none() {
            self.component = dedupe_field(&f.component);
        }
    }

    /// Convert this row into a fill [`Scope`] for one `finding_block` repetition.
    fn into_scope(self) -> Scope {
        let mut s = Scope::new();
        s.set("finding_title", self.title);
        s.set_opt("finding_category", self.category);
        s.set_opt("finding_component", self.component);
        s.set_opt("finding_description", self.description);
        s.set_opt("finding_evidence", self.evidence);
        s.set_opt("finding_business_impact", self.business_impact);
        s.set_opt("finding_remediation", self.remediation);
        s.set_opt("finding_cost_effort", self.cost_effort);
        s
    }
}

/// Map an empty string to `None` so a blank/absent value falls to the honesty
/// marker instead of rendering as literal empty text.
fn non_empty(s: &str) -> Option<String> {
    if s.is_empty() {
        None
    } else {
        Some(s.to_string())
    }
}

/// Strip a single trailing sentence-terminal character (`.`/`?`/`!`) from prose
/// destined for a template slot that appends its own literal period right after
/// the value — avoids a doubled terminal punctuation mark (live-QA wave-2
/// defect #3: `report-technical-dd{,-cast}.md` write `{{finding_description}}.`,
/// `{{finding_evidence}}.`, and (in the AMBER row) `{{finding_remediation}}.`).
///
/// Why: LLM-written prose routinely ends its own sentence with terminal
/// punctuation; the template's trailing literal `.` then collides with it
/// (`"...concatenation.."`).  A trailing `)` is left untouched — no template in
/// this crate appends a bare period directly after a field closed with a
/// parenthesis (the RED row wraps `finding_cost_effort` in `(cost/effort: …).`,
/// not the description/evidence/remediation fields), so there is no collision
/// to resolve for that case, and blindly chopping a legitimate closing paren
/// would corrupt otherwise-correct prose.
/// What: trims trailing whitespace, then removes exactly one trailing `.`, `?`,
/// or `!` if present; a trailing `)` (or anything else) is returned unchanged.
/// Test: `reporter_tests.rs::{dedupe_strips_trailing_period,
/// dedupe_strips_trailing_question_mark, dedupe_leaves_trailing_paren}`.
fn dedupe_terminal_punctuation(s: &str) -> &str {
    let trimmed = s.trim_end();
    match trimmed.as_bytes().last() {
        Some(b'.') | Some(b'?') | Some(b'!') => &trimmed[..trimmed.len() - 1],
        _ => trimmed,
    }
}

/// Map an empty string (after terminal-punctuation dedupe) to `None`, with no
/// provenance tag — for identifier-like fields (e.g. `component`) that share the
/// same template punctuation collision as narrative prose but are not
/// themselves LLM-authored sentences.
fn dedupe_field(s: &str) -> Option<String> {
    let cleaned = dedupe_terminal_punctuation(s);
    if cleaned.is_empty() {
        None
    } else {
        Some(cleaned.to_string())
    }
}

/// Map an empty string (after terminal-punctuation dedupe) to `None`, tagging a
/// non-empty result `inferred` — for the LLM-authored narrative fields of a
/// synthesized finding (live-QA wave-2 defect #1: the marker existed but was
/// never applied to any synthesized content).
fn prose_field(s: &str) -> Option<String> {
    dedupe_field(s).map(|v| tag(v, Provenance::Inferred))
}

/// Like [`prose_field`], but tags the value `measured` ⁽ᵐ⁾ when `measured` is
/// true — for a wave-3 investigation evidence quote that was mechanically
/// verified verbatim against the file (#2357).  When `measured` is false the
/// value is LLM-claimed and tagged `inferred` ⁽ⁱ⁾, identical to [`prose_field`].
///
/// Why: an evidence quote proven to exist in the source is `measured` data, not
/// an LLM judgement; tagging it ⁽ᵐ⁾ tells a reader the snippet is real code they
/// can grep for, while the surrounding description/impact/remediation stay ⁽ⁱ⁾.
/// What: dedupes trailing terminal punctuation, maps empty → `None`, and applies
/// the `Measured` tag when `measured` else `Inferred`.
/// Test: `reporter_tests::reporter_tags_investigation_evidence_measured`.
fn evidence_field(s: &str, measured: bool) -> Option<String> {
    let prov = if measured {
        Provenance::Measured
    } else {
        Provenance::Inferred
    };
    dedupe_field(s).map(|v| tag(v, prov))
}

/// Fill one severity band's per-application finding blocks for one repository,
/// merging deterministic metrics data with verified synthesis prose.
///
/// Why: M1 must never leave a RED/AMBER finding entirely unlisted just because
/// synthesis is off, unavailable, or guardrail-rejected — title/severity
/// (implied by which block)/category/component are verbatim, always-available
/// facts from `metrics.findings`.  When synthesis IS available, its prose is
/// merged onto the SAME row for a matching title — never pushed as a second row
/// — so a finding renders exactly once no matter the synthesis outcome.
/// What: builds one [`FindingRow`] per `repo.metrics.findings` entry of `band`
/// (bare fields), then — when `syn` is `Some` — overlays matching
/// `FindingProse` prose by title; a synthesis finding with no metrics-title
/// match is appended as an additional, synthesis-only row so no verified data
/// is silently dropped.  Pushes the `app_block` (`app_name` + one
/// `finding_block` per row) only when the merged list is non-empty.
/// Test: `reporter_tests.rs::{reporter_fills_red_findings_deterministically,
/// reporter_merges_synthesis_prose_onto_deterministic_finding,
/// reporter_injects_synthesis_prose,
/// reporter_leaves_findings_honesty_marked_without_metrics}`.
fn push_finding_band(
    root: &mut Scope,
    repo: &RepositoryReport,
    syn: Option<&super::synthesize::Synthesis>,
    band: Severity,
    band_label: &str,
    app_block: &str,
    finding_block: &str,
) {
    let mut rows: Vec<FindingRow> = repo
        .metrics
        .iter()
        .flat_map(|m| m.findings.iter())
        .filter(|f| f.severity == band)
        .map(FindingRow::from_metric)
        .collect();

    if let Some(syn) = syn {
        for f in syn
            .findings
            .iter()
            .filter(|f| f.app_slug == repo.slug && f.severity.to_uppercase() == band_label)
        {
            match rows.iter_mut().find(|r| r.title == f.title) {
                Some(row) => row.merge_prose(f),
                None => rows.push(FindingRow::from_prose(f)),
            }
        }
    }

    if rows.is_empty() {
        return;
    }

    let mut app_scope = Scope::new();
    app_scope.set("app_name", repo.name.clone());
    for row in rows {
        app_scope.push_block(finding_block, row.into_scope());
    }
    root.push_block(app_block, app_scope);
}

/// Fill the root GREEN-topic bullets (`{{green_topic_N}}`, N = 1..=3) from
/// metrics.
///
/// Why: the bundled templates model GREEN findings as three fixed report-level
/// bullet placeholders (not a repeatable block); per the no-green-analysis rule
/// they carry ONLY the finding title — no evidence, root cause, or remediation
/// — and, unlike RED/AMBER, are never synthesized (M2 excludes greens
/// structurally, so this fill is independent of synthesis availability).
/// What: collects every `Severity::Green` finding across all repositories, in
/// manifest order, and fills up to the first three titles; any remaining slot
/// (or all three, when there are no green findings) falls through to the
/// honesty marker.
/// Test: `reporter_tests.rs::{reporter_fills_green_topics,
/// reporter_leaves_findings_honesty_marked_without_metrics}`.
fn push_green_topics(root: &mut Scope, model: &ReportModel) {
    let greens: Vec<&str> = model
        .repositories
        .iter()
        .filter_map(|r| r.metrics.as_ref())
        .flat_map(|m| m.findings.iter())
        .filter(|f| f.severity == Severity::Green)
        .map(|f| f.title.as_str())
        .collect();
    for (i, title) in greens.iter().take(3).enumerate() {
        root.set(format!("green_topic_{}", i + 1), (*title).to_string());
    }
}

/// Append the visible `synthesis:` status note to the rendered markdown.
///
/// Why: a reader must never mistake deterministic fallback for synthesized
/// analysis; the note makes the outcome (available / unavailable-with-reason /
/// per-field guardrail rejections) explicit at the end of the report.
/// What: when `model.synthesis` is present, appends a fenced status block whose
/// lines come from `Synthesis::status_lines`.  Absent synthesis appends nothing
/// (M1 output is byte-identical).
/// Test: `reporter_tests.rs::reporter_appends_unavailable_note`.
fn append_synthesis_note(out: &mut String, model: &ReportModel) {
    let Some(syn) = &model.synthesis else {
        return;
    };
    out.push_str("\n\n## Synthesis Status\n\n");
    for line in syn.status_lines() {
        out.push_str(&format!("- {line}\n"));
    }
}

/// Build the per-application child scope for one repository.
///
/// Why: maps a repository's deterministic data (git provenance + metrics) onto
/// the per-application placeholders; git fields are also emitted so a custom
/// template can surface provenance, while the bundled templates carry it in JSON.
/// What: sets app identity, tech stack / LoC / counts from metrics (when
/// present), git branch/SHA/remote/dirty scalars, and — when `bench` is supplied
/// — one `bench_row` block per comparable-metric placement (or a single small-n
/// honesty row).  Leaves scoring/health factors unset (M1 has no scoring) so they
/// render as honesty markers.
/// Test: `reporter_tests.rs::{render_contains_expected, reporter_fills_benchmark}`.
fn per_application_scope(
    repo: &RepositoryReport,
    bench: Option<&super::benchmark::RepositoryBenchmark>,
) -> Scope {
    let mut scope = Scope::new();
    scope.set("app_name", repo.name.clone());
    scope.set("app_slug", repo.slug.clone());
    scope.set("app_source", repo.source.clone());
    scope.set("app_source_kind", repo.source_kind.clone());
    scope.set_opt("app_username", repo.username.clone());
    scope.set_opt("app_git_ref", repo.git_ref.clone());

    if let Some(git) = &repo.git_info {
        scope.set("git_branch", git.branch.clone());
        scope.set("git_head_sha", git.head_sha.clone());
        scope.set_opt("git_origin_url", git.origin_url.clone());
        scope.set("git_dirty", if git.dirty { "dirty" } else { "clean" });
    }

    fill_profile(&mut scope, repo);

    if let Some(rb) = bench {
        push_bench_rows(&mut scope, rb);
    }

    scope
}

/// The ordinal-suffixed rendering of a non-negative integer (live-QA defect).
///
/// Why: naive `{n}th` formatting misrenders e.g. `"71th"` instead of `"71st"`;
/// the benchmark compliance column reads as an ordinal percentile and must be
/// grammatically correct.
/// What: the suffix is `th` whenever `n % 100` is 11, 12, or 13 (the teens
/// exception, e.g. 11th/111th); otherwise it follows `n % 10` (`1→st`, `2→nd`,
/// `3→rd`, else `th`).
/// Test: `reporter_tests.rs::ordinal_edge_cases` (1,2,3,11,12,13,21,71,101,111).
fn ordinal(n: u64) -> String {
    let rem100 = n % 100;
    let suffix = if (11..=13).contains(&rem100) {
        "th"
    } else {
        match n % 10 {
            1 => "st",
            2 => "nd",
            3 => "rd",
            _ => "th",
        }
    };
    format!("{n}{suffix}")
}

/// Push the per-application `bench_row` blocks for one repository's placement.
///
/// Why: the Benchmark Position table is a repeatable row block; a ranked repo
/// contributes one row per comparable metric, a held-back repo contributes a
/// single explicit small-n honesty row so a reader is never left to infer that
/// ranking silently did not happen.
/// What: for `Ranked`, one row per placement (criterion, percentile compliance,
/// quartile, `rank of n`, and the population size as the peer set); for
/// `CorpusTooSmall`, one row whose criterion carries the small-n marker.
/// Test: `reporter_tests.rs::{reporter_fills_benchmark, reporter_small_corpus_marks}`.
fn push_bench_rows(scope: &mut Scope, rb: &super::benchmark::RepositoryBenchmark) {
    use super::benchmark::{BenchmarkStatus, metric_label};
    match &rb.status {
        BenchmarkStatus::CorpusTooSmall(peers) => {
            let mut row = Scope::new();
            row.set(
                "bench_criterion",
                format!("benchmark: corpus too small (n={peers})"),
            );
            scope.push_block("bench_row", row);
        }
        BenchmarkStatus::Ranked => {
            for p in &rb.placements {
                let mut row = Scope::new();
                row.set("bench_criterion", metric_label(&p.metric));
                row.set(
                    "bench_compliance",
                    format!("{} pct", ordinal(p.percentile.round() as u64)),
                );
                row.set("bench_quartile", format!("Q{}", p.quartile));
                row.set("bench_rank", format!("{} of {}", p.rank, p.population));
                row.set("bench_peer_set", format!("{} repos", p.population));
                scope.push_block("bench_row", row);
            }
        }
    }
}

/// Fill the graph-appendix `benchmark_position` dataset — one row per ranked app.
///
/// Why: the mandated dataset appendix expects one benchmark row per application;
/// a single headline placement (Total LoC) keys that row, while the full
/// per-metric breakdown lives in each application's Benchmark Position table.
/// What: for each ranked repository with a Total-LoC placement, pushes a root
/// `benchmark_position` child carrying both the generic (`peer_set`,
/// `compliance_pct`, `quartile`, `rank`) and CAST (`tqi_*`) placeholder aliases,
/// so either bundled template fills from the same data.  Held-back / metric-less
/// repos are skipped (the block renders once empty → honesty markers).
/// Test: `reporter_tests.rs::reporter_fills_benchmark`.
fn inject_benchmark_dataset(
    root: &mut Scope,
    model: &ReportModel,
    bench: &super::benchmark::BenchmarkReport,
) {
    use super::benchmark::BenchmarkStatus;
    for repo in &model.repositories {
        let Some(rb) = bench.repositories.iter().find(|r| r.slug == repo.slug) else {
            continue;
        };
        if !matches!(rb.status, BenchmarkStatus::Ranked) {
            continue;
        }
        let Some(p) = rb.placements.iter().find(|p| p.metric == "total_loc") else {
            continue;
        };
        let mut row = Scope::new();
        row.set("app_name", repo.name.clone());
        row.set("peer_set", format!("{} repos", p.population));
        row.set("compliance_pct", format!("{:.0}", p.percentile));
        row.set("quartile", format!("Q{}", p.quartile));
        row.set("rank", format!("{} of {}", p.rank, p.population));
        // CAST template aliases (same headline placement).
        row.set("tqi_comp", format!("{:.0}", p.percentile));
        row.set("tqi_q", format!("Q{}", p.quartile));
        row.set("tqi_rank", p.rank.to_string());
        row.set("tqi_rank_total", p.population.to_string());
        root.push_block("benchmark_position", row);
    }
}

/// Append the visible `benchmark:` status note to the rendered markdown.
///
/// Why: like the synthesis note, a reader must see the benchmark provenance —
/// the corpus size, how many peers each app ranked against, any small-n gating,
/// and any corpus load warnings — so placement is never mistaken for absolute
/// truth and small/absent corpora are disclosed.
/// What: when `model.benchmark` is present, appends a `## Benchmark Status`
/// section listing the corpus size, one line per repository (ranked-against-N or
/// the small-n marker), and one line per load warning.  Absent benchmarking
/// appends nothing (output byte-identical to M2).
/// Test: `reporter_tests.rs::{reporter_fills_benchmark, reporter_small_corpus_marks}`.
fn append_benchmark_note(out: &mut String, model: &ReportModel) {
    use super::benchmark::BenchmarkStatus;
    let Some(bench) = &model.benchmark else {
        return;
    };
    out.push_str("\n\n## Benchmark Status\n\n");
    out.push_str(&format!(
        "- benchmark: corpus size {} snapshot(s)\n",
        bench.corpus_size
    ));
    for rb in &bench.repositories {
        match &rb.status {
            BenchmarkStatus::Ranked => {
                out.push_str(&format!(
                    "- {}: ranked against {} peer(s)\n",
                    rb.name, rb.peers
                ));
            }
            BenchmarkStatus::CorpusTooSmall(peers) => {
                out.push_str(&format!(
                    "- {}: benchmark: corpus too small (n={peers})\n",
                    rb.name
                ));
            }
        }
    }
    for w in &bench.warnings {
        out.push_str(&format!("- warning: {w}\n"));
    }
}

#[cfg(test)]
#[path = "reporter_tests.rs"]
mod tests;
