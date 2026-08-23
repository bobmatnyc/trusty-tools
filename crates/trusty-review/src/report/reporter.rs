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

use super::contents_links;
use super::error::{ReportError, Result};
use super::fill::{Scope, render, strip_leading_comment};
use super::manifest::slugify;
use super::metrics::Severity;
use super::model::{ReportModel, RepositoryReport};
use super::polish::polish_with_gaps;
use super::provenance::{self, Provenance, tag};
use super::reporter_authorship::{fill_authorship_facts, push_authorship_rows};
use super::reporter_codesec::{push_code_quality_rows, push_security_violation_rows};
use super::reporter_facts::fill_key_facts;
use super::reporter_fill::{
    crate_version, fill_profile, instructions_block, set_executive_summary, set_scoring_model,
};
use super::reporter_findings::{finding_citations, push_finding_band, unplaced_narrative_lines};
use super::reporter_graph_datasets::{
    inject_complexity_distribution_dataset, inject_loc_by_technology_dataset,
};
use super::reporter_performance::fill_performance_note;
// #6046: the authorship section leaves as its own document.
use super::split;

/// Renders a [`ReportModel`] to markdown + JSON and writes them atomically.
///
/// Why: separating rendering/output from model assembly lets tests render a
/// model without a filesystem and keeps the CLI handler thin.
/// What: `output_dir` is where `{slug}.md` and `{slug}.json` are written.
/// Test: `reporter_tests.rs::{render_contains_expected, write_emits_both}`.
pub struct Reporter {
    output_dir: PathBuf,
    /// Whether to render Mermaid charts under populated dataset tables (#2366).
    ///
    /// Why: charts are on by default but disabled via `--no-mermaid` / manifest
    /// `[report] mermaid = false`; when off the output is byte-identical to the
    /// pre-wave-4 report (the injection pass is simply skipped).
    mermaid: bool,
}

impl Reporter {
    /// Create a reporter writing to `output_dir` (Mermaid charts on by default).
    ///
    /// Why: callers choose the output directory (`--out`, default `./reports`).
    /// What: stores the directory; it is created on `write` if absent.
    /// Test: `reporter_tests.rs::write_emits_both`.
    pub fn new(output_dir: impl Into<PathBuf>) -> Self {
        Self {
            output_dir: output_dir.into(),
            mermaid: true,
        }
    }

    /// Set whether Mermaid charts are rendered (#2366).
    ///
    /// Why: the CLI resolves the on/off decision (flag OR manifest key) and threads
    /// it in without changing `render`'s signature.
    /// What: consumes and returns `self` with the flag set; `false` disables the
    /// post-polish injection pass, keeping output byte-identical to pre-wave-4.
    /// Test: `reporter_tests.rs::no_mermaid_byte_identical`.
    pub fn with_mermaid(mut self, mermaid: bool) -> Self {
        self.mermaid = mermaid;
        self
    }

    /// Render the model into the code-review markdown document.
    ///
    /// Why: exposed separately so tests can assert on rendered markdown without
    /// touching disk. Since #6046 this is the CODE-REVIEW half only — the
    /// authorship section renders as its own document, reachable through
    /// [`render_documents`](Self::render_documents).
    /// What: delegates to [`render_documents`](Self::render_documents) and
    /// returns its code-review document.
    /// Test: `reporter_tests.rs::{render_contains_expected,
    /// reporter_strips_leading_comment_header}`.
    pub fn render(&self, model: &ReportModel, template: &str) -> String {
        self.render_documents(model, template).code_review
    }

    /// Render the model into both documents one `report` run produces (#6046).
    ///
    /// Why: the code review and the authorship assessment answer different
    /// diligence questions from different data — analyze metrics versus git
    /// history — and the owner asked to read them apart. Rendering both from
    /// ONE fill pass is what keeps them consistent: a single [`Scope`], one
    /// polish, one set of provenance markers, no second template to drift.
    /// What: strips the template's leading instructional `<!-- … -->` comment
    /// (live-QA defect #2314 — a generated report must never carry template
    /// authoring instructions, and the header's own literal `{{field}}` /
    /// BEGIN-END documentation examples would otherwise be mangled by the fill
    /// engine), builds the fill [`Scope`] from the model, renders and polishes
    /// the whole document, then cuts the authorship section out of it. The cut
    /// happens BEFORE the jump-list injection so the code-review document never
    /// links a heading it no longer carries.
    /// Test: `reporter_tests.rs::{render_splits_authorship_into_its_own_document,
    /// render_without_authorship_data_still_produces_the_document}`.
    pub fn render_documents(&self, model: &ReportModel, template: &str) -> RenderedReports {
        let scope = build_scope(model);
        // Fill deterministically, then polish the OUTPUT (#2342): strip every
        // non-dataset template comment, drop honesty-marker rows, collapse empty
        // sections, and gather the gaps.  The leading-comment strip stays a
        // pre-fill step so the header's literal `{{…}}`/BEGIN-END examples are
        // never mistaken for real placeholders by the fill engine.
        let filled = render(strip_leading_comment(template), &scope);
        // #5239: the model's named gaps (an upstream orchestrator's unassessed
        // areas, plus any repo the live analyze fetch could not populate) lead
        // the Gaps & Caveats section.
        let mut out = polish_with_gaps(&filled, &model.gaps);
        // #2366 wave-4: render a ```mermaid chart under every populated dataset
        // table.  Runs AFTER polish (so it sees the omit-empty'd tables and never
        // charts a dropped/empty dataset) and BEFORE the appended status notes.
        // When disabled the pass is skipped entirely — output stays byte-identical.
        if self.mermaid {
            out = super::mermaid::inject(&out);
        }
        // Status notes are appended AFTER polish so their bullets are not subject
        // to omit-empty (they are always meaningful provenance, never markers).
        append_synthesis_note(&mut out, model);
        append_benchmark_note(&mut out, model);
        // Wave-3 (#2357): the Dependency Inventory and Investigation Coverage
        // sections, plus the rejected-evidence note, are appended after polish so
        // their measured/inferred rows are never subject to omit-empty.
        out.push_str(&super::investigate::report_sections(model));
        // #6082 lap 7: the template signs off at its own end, which is no longer
        // the document's — three sections are appended above. Move the signature
        // down so nothing follows it.
        out = split::signature_last(&out);
        // #6046: cut the authorship section out before the jump list is built,
        // so the code-review document links only what it still carries.
        let (code_body, authorship_section) = split::split_authorship(&out);
        // #6004: LAST — every section this render pass can produce has now been
        // appended, so this is the one point where the final `##` heading set is
        // known. Replaces the exec-summary jump-list sentinel with real links to
        // whichever of those headings actually survived.
        RenderedReports {
            code_review: contents_links::inject(&code_body),
            authorship: authorship_section.map(|section| {
                // #6046: the model's authorship-load gaps travel WITH the
                // section — the Gaps & Caveats section its no-data line points
                // at renders in the code-review document, not this one.
                split::authorship_document(
                    &model.title,
                    &model.generated_date,
                    &section,
                    &model.gaps,
                )
            }),
        }
    }

    /// Render and write `{slug}.md` + `{slug}.json` atomically to `output_dir`.
    ///
    /// Why: the CLI's terminal step — persist both the human report and its
    /// machine twin so downstream tooling can consume the JSON.  #5454 makes this
    /// the boundary that enforces required inference: [`render`](Self::render)
    /// stays infallible so unit tests can exercise deterministic composition, but
    /// nothing reaches DISK without a synthesis pass behind it, so there is no
    /// path by which a deterministic-only report becomes a deliverable.
    /// What: refuses a model carrying no synthesis, then creates `output_dir`,
    /// renders markdown, serializes the model to pretty JSON, and writes each via
    /// a temp-file + rename (atomic on the same filesystem).  Returns the two
    /// written paths.
    ///
    /// # Errors
    ///
    /// [`ReportError::SynthesisRequired`] when `model.synthesis` is `None`, and
    /// any I/O or serialisation failure.
    ///
    /// Test: `reporter_tests.rs::{write_emits_both,
    /// write_emits_the_authorship_document_alongside,
    /// write_refuses_a_model_with_no_synthesis}`.
    pub fn write(&self, model: &ReportModel, template: &str) -> Result<Vec<PathBuf>> {
        // #5454: inference is required, so a synthesis-free model is a bug in the
        // caller, not a mode to serve.
        if model.synthesis.is_none() {
            return Err(ReportError::SynthesisRequired);
        }

        std::fs::create_dir_all(&self.output_dir).map_err(|source| ReportError::Io {
            path: self.output_dir.clone(),
            source,
        })?;

        let stem = report_stem(model);
        let documents = self.render_documents(model, template);
        let json = serde_json::to_string_pretty(model).map_err(|source| ReportError::Metrics {
            path: PathBuf::from("<model.json>"),
            source,
        })?;

        let md_path = self.output_dir.join(format!("{stem}.md"));
        let json_path = self.output_dir.join(format!("{stem}.json"));
        atomic_write(&md_path, documents.code_review.as_bytes())?;
        atomic_write(&json_path, json.as_bytes())?;
        info!(md = %md_path.display(), json = %json_path.display(), "report written");

        let mut written = vec![md_path, json_path];
        // #6046: appended, never inserted — tga reads the `.json` path out of
        // this list by extension, and every consumer that indexes it expects
        // the code-review report first.
        if let Some(authorship) = &documents.authorship {
            let path = self
                .output_dir
                .join(format!("{stem}{}.md", split::AUTHORSHIP_STEM_SUFFIX));
            atomic_write(&path, authorship.as_bytes())?;
            info!(md = %path.display(), "authorship report written");
            written.push(path);
        }

        Ok(written)
    }
}

/// The documents one render pass produces (#6046).
///
/// Why: `report` renders the code review and the authorship assessment as
/// separate deliverables, and a caller needs both back from one call rather
/// than rendering twice.
/// What: `code_review` is every section except authorship; `authorship` is the
/// standalone authorship document, `None` only when the template carries no
/// authorship section at all. A run with no authorship data still produces the
/// document, carrying the polished section's no-data line.
/// Test: `reporter_tests.rs::{render_splits_authorship_into_its_own_document,
/// render_without_authorship_data_still_produces_the_document}`.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct RenderedReports {
    /// The code-review report: every rendered section except authorship.
    pub code_review: String,
    /// The standalone authorship report, when the render produced one.
    pub authorship: Option<String>,
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
///
/// `pub(super)` since #6137: `figures::printed_figures` walks the same scope to
/// tell the numeric guardrail which figures the report actually prints.
/// Test: `reporter_tests.rs::render_contains_expected`.
pub(super) fn build_scope(model: &ReportModel) -> Scope {
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
    // #6135: which models produced this report. Measured — the tool resolved it
    // and knows it. A model built outside the report command carries none, and
    // the row then says so rather than reading as a report with no inference.
    root.set(
        "inference_models",
        match &model.inference {
            Some(attribution) => tag(attribution.line(), Provenance::Measured),
            None => "not recorded — this report was rendered without a resolved model selection"
                .to_string(),
        },
    );
    root.set("provenance_legend", provenance::LEGEND);
    root.set("analyst_instructions_block", instructions_block(model));
    // #6004: the Key Facts block frontloads density/complexity/author/
    // trajectory facts ahead of the executive summary.
    fill_key_facts(&mut root, model);
    // #5453/#6004: completes the author-count/trajectory rows PR A left as
    // named gaps, once a repository's authorship artifact loaded. Must run
    // AFTER `fill_key_facts`, which seeds those rows with gap text (#6029) —
    // pinned by `reporter_tests::key_facts_authorship_rows_survive_the_fill_order`.
    fill_authorship_facts(&mut root, model);
    // #6004: deterministic structure, never data — always set.
    contents_links::set_contents_placeholder(&mut root);
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

    // #5405: the board-correlation figures. Set only when the producing run
    // supplied them; left unset otherwise, so the section collapses and polish
    // names it under Gaps & Caveats rather than the page reading as if the
    // codebase simply had no tracker. A zero-coverage run still SETS the
    // scalar — "no commit referenced a tracked board item" is a finding, and
    // must not be mistaken for an absent artifact.
    if let Some(t) = &model.ticketing {
        root.set(
            "ticketing_coverage",
            tag(t.coverage_line(), Provenance::Measured),
        );
    }

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
    // `app_index` is a real 1-based sequential number (findings-rendering fix,
    // #2357 wave-3.2 follow-up defect #4) — the bundled templates' `### 4.N.`
    // sub-heading was never substituted; `{{app_index}}` fixes that the same
    // way `{{finding_index}}` fixed the finding-list literal `N.`.
    for (i, repo) in model.repositories.iter().enumerate() {
        let bench = model
            .benchmark
            .as_ref()
            .and_then(|b| b.repositories.iter().find(|r| r.slug == repo.slug));
        root.push_block("per_application", per_application_scope(repo, bench, i + 1));
    }

    // M3: fill the graph-appendix benchmark dataset (one headline row per ranked
    // application).  Absent benchmarking leaves the block to render once empty,
    // byte-identical to the M2 honesty-marked output.
    if let Some(bench) = &model.benchmark {
        inject_benchmark_dataset(&mut root, model, bench);
    }

    // #2366 follow-up (live-QA): the §7 graph appendix must produce REAL charts
    // on a bare run, not empty scaffolding — these two datasets are wired to data
    // the model already computes (scan/metrics), never fabricated.
    inject_loc_by_technology_dataset(&mut root, model);
    inject_complexity_distribution_dataset(&mut root, model);

    // Live-QA defect #2314: RED/AMBER finding blocks are deterministic, not
    // synthesis-gated — title/category/component come verbatim from
    // `metrics.findings` regardless of `--synthesize`.  When verified synthesis
    // IS available, its prose is merged onto the SAME row for a matching title
    // (never pushed as a second, duplicate row); see `push_finding_band`.
    // Live-QA defect (findings-rendering wave-3.2): each severity section gets
    // its own 1-based sequential counter, restarting at 1 for RED and again for
    // AMBER — the counter accumulates ACROSS repositories within one band (not
    // per-app) so the rendered list is `1. 2. 3. …` instead of a literal `N.`.
    // #5454: the `.filter(|s| s.is_available())` that used to sit here WAS the
    // deterministic-only fallback — an attempted-but-failed synthesis quietly
    // rendered metrics-only rows. Inference is required now, so a `Synthesis`
    // that exists is verified prose and there is nothing left to filter.
    let available_synthesis = model.synthesis.as_ref();
    let mut red_index = 0usize;
    let mut amber_index = 0usize;
    for repo in &model.repositories {
        push_finding_band(
            &mut root,
            repo,
            available_synthesis,
            Severity::Red,
            "RED",
            "per_application_red",
            "red_finding",
            &mut red_index,
        );
        push_finding_band(
            &mut root,
            repo,
            available_synthesis,
            Severity::Amber,
            "AMBER",
            "per_application_amber",
            "amber_finding",
            &mut amber_index,
        );
    }

    // GREEN topics are metrics-only (title only, no-green-analysis rule) and are
    // never synthesized — filled independent of synthesis availability.
    push_green_topics(&mut root, model);

    // #6004: Code Quality & Architecture and Security Posture re-project data
    // already loaded above (complexity/findings/LoC) — no new data source, no
    // LLM involvement in the rows themselves. Performance & Scalability is
    // FIXED text (DOC-67 §3: no performance data source exists at all).
    push_code_quality_rows(&mut root, model);
    // #6147: the same section's deterministic architecture input — the crate
    // graph trusty-audit measured. Absent for every repository that is not a
    // Cargo workspace, and then it renders as nothing.
    super::topology::push_crate_topology(&mut root, model);
    push_security_violation_rows(&mut root, model);
    fill_performance_note(&mut root, model);
    // #5453/#6004: key-man risk rows render IN this section, never scattered
    // across Top Risks — the deterministic half of Authorship & Key-Person
    // Risk. A repository whose artifact failed to load contributes no row;
    // its gap already lives in `model.gaps` (fail-open, set by model.rs).
    push_authorship_rows(&mut root, model);

    // #5318: §2 has a deterministic source.  It used to be filled ONLY from
    // verified synthesis, so a run without `--synthesize` — which was every
    // `tga audit` run before #5454 — collapsed the first section a diligence
    // reader opens while the report listed real RED/AMBER findings two sections
    // below.  #5454 keeps this composition: synthesis is required now, but the
    // numeric guardrail can still reject the summary field on its own, and that
    // is exactly when §2 would otherwise go empty again.  The roll-up counts what
    // the report already contains; synthesis prose overwrites it below.
    let synthesized_risks = available_synthesis.is_some_and(|s| !s.top_risks.is_empty());
    set_executive_summary(&mut root, model, !synthesized_risks);
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
/// and pushes one `top_risk_row` block per verified top-risk row — the
/// `risk_description` / `risk_cost` prose fields are tagged per field, while
/// `risk_severity` / `risk_apps` are left untagged (they restate
/// categorical/identifier data — the RED/AMBER band, the affected application
/// names — rather than narrative prose).  `risk_rank` numbers the rows
/// sequentially (1..N).  Because the template row is a repeatable block, ALL
/// synthesized rows render (previously the template hard-capped at 3/5 fixed
/// placeholder rows, silently dropping any beyond the cap — #2373); with zero
/// top risks no block is pushed and the section collapses via omit-empty.
/// #6009 shape 2: a live capture omitted `severity`/`cost` entirely, and
/// [`RiskRow`](super::synthesize::RiskRow) now defaults each to `""` rather
/// than failing the whole response — `risk_severity`/`risk_cost` are left
/// UNSET (never set to `""`) when empty, so the fill engine's own honesty
/// rule renders `not stated in source data` instead of a blank table cell
/// that could be misread as "no severity/cost", matching the precedent
/// `reporter_fill.rs::set_executive_summary` already uses for the
/// deterministic path's `risk_cost`.
/// Test: `reporter_tests.rs::{reporter_injects_synthesis_prose,
/// reporter_tags_top_risks_as_inferred, reporter_renders_all_top_risk_rows,
/// reporter_collapses_empty_top_risks,
/// reporter_renders_defaulted_top_risk_severity_honestly}`.
fn inject_synthesis_summary(root: &mut Scope, syn: &super::synthesize::Synthesis) {
    if let Some(exec) = &syn.executive_summary {
        root.set(
            "executive_summary_paragraph",
            tag(exec.clone(), Provenance::Inferred),
        );
    }
    // #6004: same injection shape as the executive summary — verified prose
    // only, tagged inferred; the deterministic rows built by
    // `push_code_quality_rows`/`push_security_violation_rows` fill the section
    // regardless of whether either narrative slot survived the guardrail.
    if let Some(cq) = &syn.code_quality_summary {
        root.set(
            "code_quality_summary_paragraph",
            tag(cq.clone(), Provenance::Inferred),
        );
    }
    if let Some(sec) = &syn.security_summary {
        root.set(
            "security_summary_paragraph",
            tag(sec.clone(), Provenance::Inferred),
        );
    }
    if let Some(au) = &syn.authorship_summary {
        root.set(
            "authorship_summary_paragraph",
            tag(au.clone(), Provenance::Inferred),
        );
    }

    for (i, risk) in syn.top_risks.iter().enumerate() {
        let mut row = Scope::new();
        row.set("risk_rank", (i + 1).to_string());
        row.set(
            "risk_description",
            tag(risk.description.clone(), Provenance::Inferred),
        );
        if !risk.severity.is_empty() {
            row.set("risk_severity", risk.severity.clone());
        }
        if !risk.cost.is_empty() {
            row.set("risk_cost", tag(risk.cost.clone(), Provenance::Inferred));
        }
        row.set("risk_apps", risk.apps.clone());
        root.push_block("top_risk_row", row);
    }
}

/// Fill the GREEN-topic bullets — one `green_topic` block repetition per GREEN
/// finding.
///
/// Why: per the no-green-analysis rule a GREEN topic carries ONLY the finding
/// title — no evidence, root cause, or remediation — and, unlike RED/AMBER, is
/// never synthesized (M2 excludes greens structurally, so this fill is
/// independent of synthesis availability). #6137 made the bullets a repeatable
/// block: the templates carried exactly three fixed `{{green_topic_N}}` slots,
/// so a run with 21 GREEN findings silently dropped 18 of them — constant-time
/// token comparison and atomic redb batch upserts among them. The
/// no-elaboration rule is about DEPTH per topic, not about how many topics a
/// reader is allowed to see, and this is the same fixed-slot defect #2373 fixed
/// for the top-risk rows.
/// What: pushes one `green_topic` scope (a single `green_topic` scalar) per
/// `Severity::Green` finding across all repositories, in manifest order. Each
/// bullet is the finding title followed by its `file:line`, in backticks, when
/// the finding carries one. With no green findings no block is pushed and the
/// section collapses via omit-empty.
///
/// #6080: the bullets were titles alone — 0 of 23 carried an attribution — and
/// Security Posture then cited five of them as clean signals a reader had no
/// way to check. One of those five, "Raw SQL string interpolation via
/// multi-line concatenation for PR upsert", describes a defect and was listed
/// as a strength. A citation does not elaborate the topic; it says where to
/// look, which is what makes a claimed strength falsifiable.
/// Test: `reporter_tests.rs::{reporter_fills_green_topics,
/// reporter_renders_every_green_topic, green_topic_carries_its_citation,
/// reporter_leaves_findings_honesty_marked_without_metrics}`.
fn push_green_topics(root: &mut Scope, model: &ReportModel) {
    let greens = model
        .repositories
        .iter()
        .filter_map(|r| r.metrics.as_ref())
        .flat_map(|m| m.findings.iter())
        .filter(|f| f.severity == Severity::Green);
    for finding in greens {
        let mut row = Scope::new();
        row.set("green_topic", green_topic_line(finding));
        root.push_block("green_topic", row);
    }
}

/// One GREEN bullet: the title, plus its `file:line` when the finding cites one.
///
/// Why/What: see [`push_green_topics`]. An uncited GREEN still renders — it is
/// data the investigation produced — but it renders WITHOUT a citation, which
/// is also what keeps it out of the Security Posture clean-signals list.
/// Test: `reporter_tests::green_topic_carries_its_citation`.
fn green_topic_line(finding: &super::metrics::MetricFinding) -> String {
    if finding.component.trim().is_empty() {
        return finding.title.clone();
    }
    format!("{} — `{}`", finding.title, finding.component.trim())
}

/// Append the Synthesis Status block to the rendered markdown.
///
/// Why: a reader must never mistake a deterministically-composed section for one
/// the model wrote, and the block is where this report discloses what the
/// guardrails refused. #6082 lap 8 made every line of it one voice: the block
/// used to open with the raw log line `synthesis: available` and carry
/// log-prefixed rejections beside reader-voiced withheld-narrative lines. The
/// banner is gone — a report that reaches a reader always had a successful
/// synthesis pass behind it (#5454), so its presence was never news — and each
/// remaining line names the §5.1/§5.2 finding it is about by the number the
/// reader can look up.
/// What: when `model.synthesis` is present AND has something to disclose,
/// appends one line per [`StatusNote`] — cited by [`finding_citations`] when its
/// subject renders as a numbered row — then one line per narrative the finding
/// bands could not place (#6082 lap 6, see [`unplaced_narrative_lines`]). An
/// empty block is omitted entirely. The absent branch survives only for
/// [`Reporter::render`], which stays infallible for unit tests —
/// [`Reporter::write`] rejects a synthesis-free model outright (#5454).
/// Test: `reporter_tests.rs::{reporter_appends_guardrail_rejection_note,
/// a_status_note_cites_its_finding_number, an_empty_synthesis_status_is_omitted,
/// an_unmeasured_orphan_narrative_is_not_numbered}`.
fn append_synthesis_note(out: &mut String, model: &ReportModel) {
    let Some(syn) = &model.synthesis else {
        return;
    };
    let citations = finding_citations(model);
    let mut lines: Vec<String> = syn
        .notes
        .iter()
        .map(|n| {
            let cite = n
                .subject
                .as_deref()
                .and_then(|s| citations.get(s.trim()))
                .and_then(Option::as_deref);
            n.render(cite)
        })
        .collect();
    lines.extend(unplaced_narrative_lines(model));
    if lines.is_empty() {
        return;
    }
    out.push_str("\n\n## Synthesis Status\n\n");
    for line in lines {
        out.push_str(&format!("- {line}\n"));
    }
}

/// Build the per-application child scope for one repository.
///
/// Why: maps a repository's deterministic data (git provenance + metrics) onto
/// the per-application placeholders; git fields are also emitted so a custom
/// template can surface provenance, while the bundled templates carry it in JSON.
/// What: sets app identity (including the real 1-based `app_index`), tech
/// stack / LoC / counts from metrics (when present), git branch/SHA/remote/
/// dirty scalars, and — when `bench` is supplied — one `bench_row` block per
/// comparable-metric placement (or a single small-n honesty row).  Leaves
/// scoring/health factors unset (M1 has no scoring) so they render as honesty
/// markers.
/// Test: `reporter_tests.rs::{render_contains_expected, reporter_fills_benchmark,
/// scorecard_heading_renders_real_index}`.
fn per_application_scope(
    repo: &RepositoryReport,
    bench: Option<&super::benchmark::RepositoryBenchmark>,
    app_index: usize,
) -> Scope {
    let mut scope = Scope::new();
    scope.set("app_index", app_index.to_string());
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
