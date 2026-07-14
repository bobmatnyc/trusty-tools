//! `trusty-review report` CLI subcommand (M1, #2313).
//!
//! Why: the report subcommand is the single entry point for manifest-driven
//! technical-DD report generation — it loads a manifest, enriches each repo
//! deterministically (git provenance + pre-produced metrics), fills a bundled
//! template, and writes markdown + JSON.  No LLM (M1 is deterministic only).
//! What: defines [`ReportArgs`] (clap-derive) and [`cmd_report`] (the handler).
//! Template precedence is `--template` flag > manifest `[report].template` >
//! the default template.  Progress goes to STDERR; the written paths go to
//! STDOUT for scripting.
//! Test: `tests::report_args_parse_defaults` verifies clap parsing; the render
//! path is covered end to end by `tests/report_e2e.rs`.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use clap::Parser;

use trusty_review::config::ReviewConfig;
use trusty_review::llm::build_provider;
use trusty_review::report::{
    Budget, CorpusSnapshot, Instructions, Reporter, TemplateLoader, benchmark,
    investigate::{apply_investigation, merge_investigation_prose},
    load_instructions, load_manifest,
    manifest::Manifest,
    model::ReportModel,
    parse_section_instructions, run_investigation,
    synthesize::Synthesis,
    synthesize::Synthesizer,
    template::DEFAULT_TEMPLATE,
};

// ─── Report args ──────────────────────────────────────────────────────────────

/// Arguments for the `report` subcommand.
///
/// Why: groups the manifest-driven report flags in one testable struct.
/// What: the manifest path (required), an optional template override, and the
/// output directory (default `./reports`).
/// Test: `tests::report_args_parse_defaults`.
#[derive(Debug, Parser)]
pub struct ReportArgs {
    /// Path to the report manifest TOML file (required).
    #[arg(long, value_name = "FILE")]
    pub manifest: PathBuf,

    /// Template name override (e.g. `report-technical-dd-cast`).
    /// Precedence: this flag > manifest `[report].template` > default.
    #[arg(long, value_name = "NAME")]
    pub template: Option<String>,

    /// Path to a free-form analyst instructions markdown file (#2340).  The brief
    /// is recorded verbatim in the report and, under `--synthesize`, injected as
    /// focus directives.  Precedence: this flag > manifest `[report].instructions`.
    /// A missing file is an error; an empty file is a warning (proceeds as absent).
    #[arg(long, value_name = "FILE")]
    pub instructions: Option<PathBuf>,

    /// Output directory for the generated report pair (`{slug}.md`/`.json`).
    #[arg(long, value_name = "DIR", default_value = "./reports")]
    pub out: PathBuf,

    /// Opt in to M2 LLM synthesis of the narrative sections (executive summary,
    /// top-risk rationale, RED/AMBER finding prose).  OFF by default: the report
    /// is deterministic (M1) unless this flag is set, because synthesis spends
    /// LLM tokens.  Fails closed — any provider/parse/guardrail failure keeps the
    /// deterministic output and records a visible `synthesis:` note.
    #[arg(long)]
    pub synthesize: bool,

    /// Wave-3 investigation budget: max files sent per repository (#2357).
    /// Precedence: this flag > manifest `[report].investigate_max_files` > default.
    #[arg(long, value_name = "N")]
    pub investigate_max_files: Option<usize>,

    /// Wave-3 investigation budget: max total content bytes sent per repository
    /// (#2357).  Precedence: this flag > manifest `[report].investigate_max_bytes`
    /// > default.
    #[arg(long, value_name = "BYTES")]
    pub investigate_max_bytes: Option<usize>,

    /// Benchmark corpus directory (M3).  Overrides the manifest `[report].corpus`
    /// key and the per-user XDG default (`~/.local/share/trusty-review/benchmark/`).
    #[arg(long, value_name = "DIR")]
    pub corpus: Option<PathBuf>,

    /// After a successful run, append each analyzed repository's metrics snapshot
    /// to the corpus (one `<slug>-<sha-or-date>.json` per repo; overwrites the
    /// same key).  Repos without metrics contribute nothing.
    #[arg(long)]
    pub corpus_add: bool,

    /// Compute cross-repo percentile/quartile placement against the corpus and
    /// fill the benchmark tables.  Requires a resolvable corpus directory; a
    /// corpus with fewer than five peers is disclosed, never silently ranked.
    #[arg(long)]
    pub benchmark: bool,

    /// Disable Mermaid chart rendering under the Graph-Ready Data Appendix tables
    /// (#2366).  Charts are ON by default; this flag OR the manifest key
    /// `[report] mermaid = false` disables them, yielding byte-identical
    /// pre-wave-4 output.  Precedence: this flag forces off regardless of the
    /// manifest key.
    #[arg(long)]
    pub no_mermaid: bool,

    /// Populate the complexity-distribution chart and RED/AMBER finding bands
    /// deterministically from the trusty-analyze daemon (epic #2445).  OFF by
    /// default: a bare run stays scan-only.  Fills metrics ONLY for local-path
    /// repositories that declare no `metrics` file (declared metrics always win),
    /// and only when the repo is already indexed in trusty-search/trusty-analyze.
    /// Fully fail-open — an unindexed repo or an unreachable daemon logs a
    /// warning and falls through to the built-in scan; it never aborts the report.
    /// Daemon URL precedence: manifest `[report].analyze_url` > env
    /// `PR_INTELLIGENCE_ANALYZER_URL` > default `http://127.0.0.1:7879`.
    #[arg(long)]
    pub analyze: bool,
}

// ─── Command handler ──────────────────────────────────────────────────────────

/// Execute the `report` subcommand.
///
/// Why: orchestrates the deterministic report pipeline from a single CLI call so
/// the binary is a thin wrapper over the library.
/// What: loads + validates the manifest, resolves the template name by
/// precedence, loads the template source, builds the enriched model, and writes
/// the markdown + JSON pair.  Progress → STDERR; written paths → STDOUT.
/// Test: arg parsing via `tests::report_args_parse_defaults`; full render via
/// `tests/report_e2e.rs`.
pub async fn cmd_report(config: ReviewConfig, args: ReportArgs) -> Result<()> {
    eprintln!(
        "[trusty-review report] Loading manifest: {}",
        args.manifest.display()
    );
    let manifest = load_manifest(&args.manifest)
        .with_context(|| format!("failed to load manifest {}", args.manifest.display()))?;

    // Template precedence: CLI flag > manifest [report].template > default.
    let template_name = args
        .template
        .clone()
        .or_else(|| manifest.report.template.clone())
        .unwrap_or_else(|| DEFAULT_TEMPLATE.to_string());
    eprintln!(
        "[trusty-review report] Template: {template_name}; repositories: {}",
        manifest.repositories.len()
    );

    let template = TemplateLoader::new()
        .load(&template_name)
        .with_context(|| format!("failed to load template '{template_name}'"))?;

    // Analyst instructions (#2340): CLI flag wins over the manifest key; a
    // relative manifest path resolves against the manifest directory.
    let instructions = load_report_instructions(&args, &manifest)?;
    if let Some(instr) = &instructions {
        eprintln!(
            "[trusty-review report] Analyst instructions: {}",
            instr.source.display()
        );
    }

    let mut model = ReportModel::build(
        &manifest,
        &args.manifest,
        &template_name,
        instructions.as_ref(),
    )
    .context("failed to assemble report model")?;

    // #2357 layered instructions: parse the active template's own
    // `<!-- instruct:<section_id> ... -->` overrides (if any) BEFORE the render
    // pipeline strips them as ordinary instructional comments; recorded on the
    // model so the synthesis prompt builder resolves template-override-else-
    // generic-default per section, and the JSON twin stays a faithful record.
    model.section_instructions = parse_section_instructions(&template);
    if !model.section_instructions.is_empty() {
        eprintln!(
            "[trusty-review report] Template section-instruction overrides: {}",
            model
                .section_instructions
                .keys()
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        );
    }

    // Epic #2445: opt-in deterministic analyze fetch.  Runs AFTER the model is
    // built (so declared metrics files already loaded and win) and BEFORE
    // synthesis/benchmark/mermaid (so the complexity chart + finding bands see
    // the live metrics).  Fully fail-open — populates only local-path repos that
    // declared no metrics, and only when their index is served.
    if args.analyze {
        let analyze_url = manifest
            .report
            .analyze_url
            .clone()
            .unwrap_or_else(|| config.analyzer_url.clone());
        eprintln!("[trusty-review report] --analyze: fetching from {analyze_url}");
        match trusty_review::report::HttpAnalyzeMetricsSource::new(analyze_url) {
            Ok(source) => {
                trusty_review::report::enrich_with_analyze(&mut model, &source).await;
            }
            Err(e) => {
                eprintln!(
                    "[trusty-review report] --analyze: could not build HTTP client ({e}); \
                     falling back to scan"
                );
            }
        }
    }

    // M2 + wave-3: opt-in LLM synthesis plus the repo-evidence investigation.
    // Fails closed — on any provider/parse/guardrail failure the deterministic
    // output stands and the reason is recorded on the model.
    if args.synthesize {
        eprintln!("[trusty-review report] Synthesis enabled — calling LLM provider...");
        let budget = resolve_budget(&args, &manifest);
        let synthesis = run_synthesis(&config, &mut model, budget).await;
        eprintln!(
            "[trusty-review report] {}",
            synthesis
                .status_lines()
                .first()
                .cloned()
                .unwrap_or_default()
        );
        model.synthesis = Some(synthesis);
    }

    // M3: resolve the corpus directory once (shared by --benchmark and
    // --corpus-add).  A `--corpus` flag with neither consumer is a no-op — warn.
    let corpus_dir = if args.benchmark || args.corpus_add {
        Some(resolve_corpus_dir(&args, &manifest)?)
    } else {
        if args.corpus.is_some() {
            eprintln!(
                "[trusty-review report] --corpus given without --benchmark or --corpus-add; ignoring"
            );
        }
        None
    };

    // M3: compute placement BEFORE writing so it renders into the report.  The
    // target's fresh snapshot is always included in the population; loading the
    // corpus here (before any --corpus-add write) avoids double-counting a stale
    // copy of the target.
    if args.benchmark {
        let dir = corpus_dir.as_ref().expect("resolved when --benchmark");
        eprintln!(
            "[trusty-review report] Benchmarking against corpus: {}",
            dir.display()
        );
        let corpus = benchmark::load_corpus(dir).context("failed to load benchmark corpus")?;
        let targets = target_snapshots(&model);
        let report = benchmark::build_benchmark_report(&corpus, &targets);
        eprintln!(
            "[trusty-review report] Benchmark: corpus size {}, {} target(s)",
            report.corpus_size,
            report.repositories.len()
        );
        model.benchmark = Some(report);
    }

    // #2366: Mermaid charts on by default; disabled by --no-mermaid OR the
    // manifest `[report] mermaid = false`.  The flag forces off unconditionally.
    let mermaid = manifest.report.mermaid.unwrap_or(true) && !args.no_mermaid;
    let reporter = Reporter::new(&args.out).with_mermaid(mermaid);
    let written = reporter
        .write(&model, &template)
        .context("failed to write report output")?;

    // M3: persist snapshots only after a successful write.
    if args.corpus_add {
        let dir = corpus_dir.as_ref().expect("resolved when --corpus-add");
        let mut added = 0usize;
        for snap in target_snapshots(&model) {
            let path = benchmark::write_snapshot(dir, &snap)
                .with_context(|| format!("failed to write corpus snapshot for {}", snap.slug))?;
            eprintln!(
                "[trusty-review report] Corpus += {}",
                path.file_name().and_then(|n| n.to_str()).unwrap_or("?")
            );
            added += 1;
        }
        eprintln!(
            "[trusty-review report] Added {added} snapshot(s) to {}",
            dir.display()
        );
    }

    eprintln!("[trusty-review report] Wrote {} file(s):", written.len());
    for path in &written {
        // Paths to STDOUT so `$(trusty-review report ...)` is scriptable.
        println!("{}", path.display());
    }

    Ok(())
}

/// Resolve the benchmark corpus directory or fail with a clear message.
///
/// Why: `--benchmark` / `--corpus-add` both require a corpus directory; the
/// resolution precedence (CLI flag > manifest `[report].corpus` > XDG default)
/// lives in the library, and the only failure is a platform with no data dir and
/// no explicit source — which must be a clear CLI error, not a panic.
/// What: delegates to [`benchmark::corpus_dir`] with the manifest directory as
/// the base for a relative manifest key; maps `None` to an actionable error.
/// Test: covered by `tests::report_args_parse_benchmark` (parse) and the corpus
/// resolver unit tests in `benchmark_tests.rs`.
fn resolve_corpus_dir(args: &ReportArgs, manifest: &Manifest) -> Result<PathBuf> {
    let manifest_dir = args.manifest.parent().unwrap_or_else(|| Path::new("."));
    benchmark::corpus_dir(
        args.corpus.as_deref(),
        manifest.report.corpus.as_deref(),
        manifest_dir,
    )
    .ok_or_else(|| {
        anyhow::anyhow!(
            "no benchmark corpus directory: pass --corpus <dir> or set [report].corpus (the per-user default is unavailable on this platform)"
        )
    })
}

/// Resolve and load the analyst instructions brief, honouring precedence (#2340).
///
/// Why: the brief may come from the `--instructions` flag or the manifest
/// `[report].instructions` key; the flag wins, and a manifest-relative path is
/// resolved against the manifest directory so authors write portable paths.
/// What: returns `Ok(None)` when no source is configured; otherwise loads via
/// [`load_instructions`] (missing file → error; empty file → warn + `None`).
/// Test: instructions loading/validation is covered by `instructions_tests.rs`;
/// precedence is exercised by `tests::report_args_parse_defaults` shape.
fn load_report_instructions(
    args: &ReportArgs,
    manifest: &Manifest,
) -> Result<Option<Instructions>> {
    let manifest_dir = args.manifest.parent().unwrap_or_else(|| Path::new("."));
    let resolved: Option<PathBuf> = match &args.instructions {
        Some(p) => Some(p.clone()),
        None => manifest.report.instructions.as_ref().map(|rel| {
            let p = PathBuf::from(rel);
            if p.is_absolute() {
                p
            } else {
                manifest_dir.join(p)
            }
        }),
    };
    match resolved {
        Some(path) => Ok(load_instructions(&path)
            .with_context(|| format!("failed to load analyst instructions {}", path.display()))?),
        None => Ok(None),
    }
}

/// Build one corpus snapshot per analyzed repository that has metrics.
///
/// Why: both benchmarking (the target population) and `--corpus-add` (persisted
/// records) need the same identity+metrics snapshots derived from this run; the
/// run timestamp is the model's generated date (kept out of the core for
/// testability).
/// What: maps each repository with metrics to a [`CorpusSnapshot`]; metric-less
/// repos are skipped.
/// Test: exercised end to end by `tests/report_e2e.rs`.
fn target_snapshots(model: &ReportModel) -> Vec<CorpusSnapshot> {
    model
        .repositories
        .iter()
        .filter_map(|r| CorpusSnapshot::from_repository(r, &model.generated_date))
        .collect()
}

/// Resolve the investigation file/byte budget by precedence (#2357).
///
/// Why: the wave-3 investigation caps how much of a repo is sent to the LLM;
/// operators tune it via a CLI flag or a manifest key, falling back to sane
/// defaults, in one place so both budget dimensions resolve consistently.
/// What: CLI flag > manifest `[report].investigate_max_*` > [`Budget::default`].
/// Test: `tests::report_args_parse_investigate_budget`.
fn resolve_budget(args: &ReportArgs, manifest: &Manifest) -> Budget {
    let default = Budget::default();
    Budget {
        max_files: args
            .investigate_max_files
            .or(manifest.report.investigate_max_files)
            .unwrap_or(default.max_files),
        max_bytes: args
            .investigate_max_bytes
            .or(manifest.report.investigate_max_bytes)
            .unwrap_or(default.max_bytes),
    }
}

/// Build the LLM provider, run the repo-evidence investigation, then synthesis.
///
/// Why: keeps `cmd_report` readable and isolates the provider-build failure path
/// — a build error (missing API key, bad model id) must NOT abort the report; it
/// degrades to the deterministic output with an `Unavailable` synthesis.  The
/// investigation (#2357) runs FIRST so its verified findings are injected into the
/// model before synthesis, and synthesis sees the coverage summary; its verified
/// (measured-evidence) prose then wins over any synthesis prose for the same
/// finding.
/// What: resolves the reviewer role's provider/model (the same construction path
/// the review pipeline uses); on success runs `run_investigation` (local repos
/// only), `apply_investigation`, `synthesize`, and `merge_investigation_prose`; a
/// build error returns `Synthesis::unavailable(reason)`.
/// Test: build path is network-bound; the fail-closed decisions are covered by
/// `report::synthesize::tests` and `tests/report_investigate.rs` with stubs.
async fn run_synthesis(
    config: &ReviewConfig,
    model: &mut ReportModel,
    budget: Budget,
) -> Synthesis {
    let role = &config.role_models.reviewer;
    let provider =
        match build_provider(&role.model, &role.provider, config).await {
            Ok(p) => p,
            Err(e) => return Synthesis::unavailable(format!("provider build failed: {e}")),
        };

    // Wave-3 investigation (local checkouts only): select → LLM → verify, then
    // inject verified findings so synthesis and the reporter render them.
    if let Some(inv) = run_investigation(provider.clone(), &role.model, model, budget).await {
        let verified: usize = inv.repos.iter().map(|r| r.findings.len()).sum();
        eprintln!(
            "[trusty-review report] Investigation: {verified} verified finding(s) across {} local repo(s)",
            inv.repos.len()
        );
        apply_investigation(model, &inv);
        let mut synthesis = Synthesizer::new(provider, role.model.clone())
            .synthesize(model)
            .await;
        merge_investigation_prose(&mut synthesis, &inv);
        synthesis
    } else {
        Synthesizer::new(provider, role.model.clone())
            .synthesize(model)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: clap parsing of the report flags must be stable.
    /// What: parses a minimal invocation and asserts defaults + overrides.
    /// Test: this test itself.
    #[test]
    fn report_args_parse_defaults() {
        let args = ReportArgs::try_parse_from([
            "report",
            "--manifest",
            "m.toml",
            "--template",
            "report-technical-dd-cast",
        ])
        .expect("parse");
        assert_eq!(args.manifest, PathBuf::from("m.toml"));
        assert_eq!(args.template.as_deref(), Some("report-technical-dd-cast"));
        assert_eq!(args.out, PathBuf::from("./reports"));
        // M3 flags default off / unset.
        assert!(!args.benchmark);
        assert!(!args.corpus_add);
        assert!(args.corpus.is_none());
        // Epic #2445: --analyze defaults off.
        assert!(!args.analyze);
    }

    /// Why: the epic #2445 `--analyze` flag must parse.
    /// What: parses `--analyze` and asserts the boolean is set.
    /// Test: this test itself.
    #[test]
    fn report_args_parse_analyze() {
        let args = ReportArgs::try_parse_from(["report", "--manifest", "m.toml", "--analyze"])
            .expect("parse");
        assert!(args.analyze);
    }

    /// Why: the M3 corpus/benchmark flags must parse and resolve a corpus dir.
    /// What: parses `--corpus`, `--corpus-add`, `--benchmark` and asserts the
    /// resolver honours the explicit `--corpus` directory (CLI precedence).
    /// Test: this test itself.
    #[test]
    fn report_args_parse_benchmark() {
        let args = ReportArgs::try_parse_from([
            "report",
            "--manifest",
            "m.toml",
            "--corpus",
            "/tmp/corpus",
            "--corpus-add",
            "--benchmark",
        ])
        .expect("parse");
        assert!(args.benchmark);
        assert!(args.corpus_add);
        assert_eq!(args.corpus.as_deref(), Some(Path::new("/tmp/corpus")));

        // The resolver prefers the explicit --corpus directory.
        let manifest = load_manifest_from_str(
            "[report]\ntitle = \"T\"\n\n[[repositories]]\nname = \"A\"\npath = \"/x\"\n",
        );
        let dir = resolve_corpus_dir(&args, &manifest).expect("resolve");
        assert_eq!(dir, PathBuf::from("/tmp/corpus"));
    }

    /// Why: the wave-3 investigation budget resolves by CLI > manifest > default.
    /// What: a CLI `--investigate-max-files` overrides the manifest key; the byte
    /// cap falls back to the manifest; an unset dimension uses the default.
    /// Test: this test itself.
    #[test]
    fn report_args_parse_investigate_budget() {
        let args = ReportArgs::try_parse_from([
            "report",
            "--manifest",
            "m.toml",
            "--synthesize",
            "--investigate-max-files",
            "7",
        ])
        .expect("parse");
        assert_eq!(args.investigate_max_files, Some(7));
        assert!(args.investigate_max_bytes.is_none());

        let manifest = load_manifest_from_str(
            "[report]\ntitle = \"T\"\ninvestigate_max_files = 3\ninvestigate_max_bytes = 1024\n\n[[repositories]]\nname = \"A\"\npath = \"/x\"\n",
        );
        let budget = resolve_budget(&args, &manifest);
        assert_eq!(budget.max_files, 7, "CLI flag wins over the manifest key");
        assert_eq!(
            budget.max_bytes, 1024,
            "manifest key fills the unset CLI flag"
        );
    }

    /// Parse a manifest from an in-memory string for CLI validation tests.
    fn load_manifest_from_str(toml: &str) -> Manifest {
        trusty_review::report::manifest::parse_manifest(toml, Path::new("m.toml"))
            .expect("manifest parses")
    }
}
