//! `tga report` — stage 3 (report generation) entry point.

use tga::core::config::{Config, OutputConfig};
use tga::core::db::Database;
use tga::report::ReportPipeline;

use crate::ReportArgs;

/// Generate reports from the classified commits stored in `db`.
///
/// Why: provides a focused entry point for stage-3 report generation so the
/// binary can apply CLI overrides (output directory, formats, author filter)
/// without polluting the pipeline or aggregator with argument-parsing logic.
/// What: materialises an `OutputConfig` if none exists yet, applies
/// `--output` / `--formats` / `--author` overrides, then delegates to
/// [`ReportPipeline::run`].
/// Test: covered indirectly via `report::tests::pipeline_runs_all_formats_when_unspecified`;
/// the `--author` path is unit-tested in `report::tests::aggregator_author_filter_*`.
pub fn run(config: Config, db: &Database, args: ReportArgs) -> anyhow::Result<()> {
    let mut cfg = config;

    // Materialize an OutputConfig if any override is supplied and none exists yet.
    if cfg.output.is_none() && (args.output.is_some() || !args.formats.is_empty()) {
        cfg.output = Some(OutputConfig::default());
    }
    if let Some(ref mut out) = cfg.output {
        if let Some(dir) = args.output {
            out.directory = Some(dir);
        }
        if !args.formats.is_empty() {
            out.formats = args.formats;
        }
    }

    let pipeline = ReportPipeline::new(cfg);
    let stats = pipeline.run_with_filter(db, args.author.as_deref())?;

    println!(
        "Generated {} report file(s) ({} commits, {} authors)",
        stats.files_written.len(),
        stats.total_commits,
        stats.total_authors
    );
    for f in &stats.files_written {
        println!("  {}", f.display());
    }
    Ok(())
}
