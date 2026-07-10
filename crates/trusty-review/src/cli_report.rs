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

use std::path::PathBuf;

use anyhow::{Context as _, Result};
use clap::Parser;

use trusty_review::config::ReviewConfig;
use trusty_review::llm::build_provider;
use trusty_review::report::{
    Reporter, TemplateLoader, load_manifest, model::ReportModel, synthesize::Synthesis,
    synthesize::Synthesizer, template::DEFAULT_TEMPLATE,
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

    let reporter = Reporter::new(&args.out);
    let written = reporter
        .write(&model, &template)
        .context("failed to write report output")?;

    eprintln!("[trusty-review report] Wrote {} file(s):", written.len());
    for path in &written {
        // Paths to STDOUT so `$(trusty-review report ...)` is scriptable.
        println!("{}", path.display());
    }

    Ok(())
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
    }
}
