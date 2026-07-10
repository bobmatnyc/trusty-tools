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
use trusty_review::report::{
    Reporter, TemplateLoader, load_manifest, model::ReportModel, template::DEFAULT_TEMPLATE,
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
pub async fn cmd_report(_config: ReviewConfig, args: ReportArgs) -> Result<()> {
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

    let model = ReportModel::build(&manifest, &args.manifest, &template_name)
        .context("failed to assemble report model")?;

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
