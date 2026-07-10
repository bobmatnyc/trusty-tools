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
use super::fill::{Scope, render};
use super::manifest::slugify;
use super::model::{ReportModel, RepositoryReport};

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
    /// What: builds the fill [`Scope`] from the model and renders `template`.
    /// Test: `reporter_tests.rs::render_contains_expected`.
    pub fn render(&self, model: &ReportModel, template: &str) -> String {
        let scope = build_scope(model);
        let mut out = render(template, &scope);
        append_synthesis_note(&mut out, model);
        append_benchmark_note(&mut out, model);
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

    // Report metadata (report-level scalars).
    root.set("target_codename", model.title.clone());
    root.set("report_date", model.report_date.clone());
    root.set("analysis_generated_date", model.generated_date.clone());
    root.set_opt("analyst_name", model.analyst.clone());
    let source_ref = format!("repository inspection (manifest: {})", model.manifest_path);
    root.set("source_document_filename", source_ref.clone());
    root.set("source_document_reference", source_ref);

    let apps: Vec<String> = model.repositories.iter().map(|r| r.name.clone()).collect();
    if !apps.is_empty() {
        root.set("applications_list", apps.join(", "));
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

    // M2: inject verified LLM synthesis into the narrative placeholders/blocks.
    // Absent or unavailable synthesis leaves every narrative field to the M1
    // honesty marker (deterministic behaviour unchanged).
    if let Some(syn) = &model.synthesis
        && syn.is_available()
    {
        inject_synthesis(&mut root, model, syn);
    }

    root
}

/// Inject verified synthesis prose into the narrative placeholders and blocks.
///
/// Why: M2 fills exactly the fields M1 leaves as honesty markers — executive
/// summary, top-risk rows, and RED/AMBER finding elaborations — and only with
/// prose that already passed the numeric guardrail.  Greens are never touched.
/// What: sets the executive-summary scalar, the `risk_N_*` scalars, and pushes
/// `per_application_red` / `per_application_amber` blocks (with nested
/// `red_finding` / `amber_finding` blocks) for each application that has
/// verified findings of that band.
/// Test: `reporter_tests.rs::reporter_injects_synthesis_prose`.
fn inject_synthesis(root: &mut Scope, model: &ReportModel, syn: &super::synthesize::Synthesis) {
    if let Some(exec) = &syn.executive_summary {
        root.set("executive_summary_paragraph", exec.clone());
    }

    for (i, risk) in syn.top_risks.iter().enumerate() {
        let n = i + 1;
        root.set(format!("risk_{n}_description"), risk.description.clone());
        root.set(format!("risk_{n}_severity"), risk.severity.clone());
        root.set(format!("risk_{n}_cost"), risk.cost.clone());
        root.set(format!("risk_{n}_apps"), risk.apps.clone());
    }

    for repo in &model.repositories {
        push_band_blocks(
            root,
            syn,
            &repo.name,
            &repo.slug,
            "RED",
            "per_application_red",
            "red_finding",
        );
        push_band_blocks(
            root,
            syn,
            &repo.name,
            &repo.slug,
            "AMBER",
            "per_application_amber",
            "amber_finding",
        );
    }
}

/// Push one per-application findings block for a single severity band.
///
/// Why: the RED and AMBER template sections share an identical shape (an app
/// header block wrapping a repeated finding block); one helper covers both.
/// What: collects this repo's verified findings of `band`, and if any exist,
/// pushes an `app_block` scope carrying `app_name` plus one `finding_block`
/// child per finding with all elaboration scalars set.
/// Test: `reporter_tests.rs::reporter_injects_synthesis_prose`.
fn push_band_blocks(
    root: &mut Scope,
    syn: &super::synthesize::Synthesis,
    app_name: &str,
    app_slug: &str,
    band: &str,
    app_block: &str,
    finding_block: &str,
) {
    let matches: Vec<&super::synthesize::FindingProse> = syn
        .findings
        .iter()
        .filter(|f| f.app_slug == app_slug && f.severity.to_uppercase() == band)
        .collect();
    if matches.is_empty() {
        return;
    }
    let mut app_scope = Scope::new();
    app_scope.set("app_name", app_name.to_string());
    for f in matches {
        let mut fs = Scope::new();
        fs.set("finding_title", f.title.clone());
        fs.set("finding_description", f.description.clone());
        fs.set("finding_evidence", f.evidence.clone());
        fs.set("finding_component", f.component.clone());
        fs.set("finding_business_impact", f.business_impact.clone());
        fs.set("finding_remediation", f.remediation.clone());
        fs.set("finding_cost_effort", f.cost_effort.clone());
        app_scope.push_block(finding_block, fs);
    }
    root.push_block(app_block, app_scope);
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

    if let Some(metrics) = &repo.metrics {
        let langs = metrics.primary_languages(4);
        if !langs.is_empty() {
            scope.set("app_tech_stack", langs.join(", "));
        }
        if metrics.loc.total > 0 {
            scope.set("app_loc", metrics.loc.total.to_string());
        }
        scope.set(
            "app_file_counts",
            format!(
                "{} files, {} functions",
                metrics.counts.files, metrics.counts.functions
            ),
        );
    }

    if let Some(rb) = bench {
        push_bench_rows(&mut scope, rb);
    }

    scope
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
                row.set("bench_compliance", format!("{:.0}th pct", p.percentile));
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
