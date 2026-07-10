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
    CorpusSnapshot, Reporter, TemplateLoader, benchmark, load_manifest, manifest::Manifest,
    model::ReportModel, synthesize::Synthesis, synthesize::Synthesizer, template::DEFAULT_TEMPLATE,
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

    let mut model = ReportModel::build(&manifest, &args.manifest, &template_name)
        .context("failed to assemble report model")?;

    // M2: opt-in LLM synthesis of narrative sections.  Fails closed — on any
    // provider/parse/guardrail failure the deterministic output stands and the
    // reason is recorded on the model (surfaced as a `synthesis:` note).
    if args.synthesize {
        eprintln!("[trusty-review report] Synthesis enabled — calling LLM provider...");
        let synthesis = run_synthesis(&config, &model).await;
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

    let reporter = Reporter::new(&args.out);
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

/// Build the LLM provider and run one synthesis pass, failing closed.
///
/// Why: keeps `cmd_report` readable and isolates the provider-build failure path
/// — a build error (missing API key, bad model id) must NOT abort the report; it
/// degrades to the deterministic output with an `Unavailable` synthesis, exactly
/// like the runtime failure paths inside [`Synthesizer::synthesize`].
/// What: resolves the reviewer role's provider/model (the same construction path
/// the review pipeline uses), builds the provider, and runs synthesis; a build
/// error returns `Synthesis::unavailable(reason)`.
/// Test: build path is network-bound; the fail-closed decisions are covered by
/// `report::synthesize::tests` with stub providers.
async fn run_synthesis(config: &ReviewConfig, model: &ReportModel) -> Synthesis {
    let role = &config.role_models.reviewer;
    match build_provider(&role.model, &role.provider, &config.openrouter_api_key).await {
        Ok(provider) => {
            let synthesizer = Synthesizer::new(provider, role.model.clone());
            synthesizer.synthesize(model).await
        }
        Err(e) => Synthesis::unavailable(format!("provider build failed: {e}")),
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

    /// Parse a manifest from an in-memory string for CLI validation tests.
    fn load_manifest_from_str(toml: &str) -> Manifest {
        trusty_review::report::manifest::parse_manifest(toml, Path::new("m.toml"))
            .expect("manifest parses")
    }
}
