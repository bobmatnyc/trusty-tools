//! `tm memory <action>` dispatcher.
//!
//! Why (issue #4837): a thin translation layer — clap args in, library call
//! out, report rendered. All import logic lives in
//! [`trusty_mpm::core::memory_import`] so it is testable without a CLI.
//! What: [`memory`] routes [`MemoryAction`] and renders the result either as
//! the machine-readable JSON report (`--json`) or a per-file human summary.
//! Exits non-zero when any file failed, so a script can gate on it.
//! Test: `cli_parses_memory_import*` in `tests.rs`; the import behaviour is
//! covered by `core::memory_import::tests`.

use anyhow::Context as _;
use trusty_mpm::core::memory_import::{ImportOptions, ImportReport, ImportStatus};

use crate::cli::MemoryAction;

/// Run a `tm memory` action.
///
/// Why: keeps `main.rs`'s dispatch table one line per command group.
/// What: builds [`ImportOptions`] from the parsed flags, runs the import, and
/// prints the report. Returns `Err` when at least one file failed so the
/// process exits non-zero.
/// Test: `cli_parses_memory_import`, `cli_parses_memory_import_dry_run`.
pub(crate) async fn memory(action: MemoryAction) -> anyhow::Result<()> {
    match action {
        MemoryAction::Import {
            dir,
            palace,
            dry_run,
            json,
            allow_secret_like,
            memory_url,
        } => {
            let opts = ImportOptions {
                dir,
                palace,
                dry_run,
                allow_secret_like,
                memory_url,
            };
            let report = trusty_mpm::core::memory_import::run_import(&opts).await?;
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&report).context("serialise import report")?
                );
            } else {
                print_summary(&report);
            }
            if report.failed > 0 {
                anyhow::bail!("{} file(s) failed to import", report.failed);
            }
            Ok(())
        }
    }
}

/// Render the human-readable summary.
///
/// Why: the JSON report is for scripts; an operator running this by hand wants
/// one line per file plus a total, not 148 JSON objects.
/// What: prints `<status> <file> [drawer_id] [detail]` per file, then a count
/// line. Nothing here is parsed by anything — `--json` is the machine surface.
/// Test: exercised indirectly by the CLI parse tests; output is cosmetic.
fn print_summary(report: &ImportReport) {
    for file in &report.files {
        let status = match file.status {
            ImportStatus::Created => "created",
            ImportStatus::Skipped => "skipped",
            ImportStatus::WouldCreate => "would-create",
            ImportStatus::WouldSkip => "would-skip",
            ImportStatus::Failed => "FAILED",
        };
        let drawer = file.drawer_id.as_deref().unwrap_or("-");
        let detail = file.detail.as_deref().unwrap_or("");
        println!("{status:<12} {:<48} {drawer} {detail}", file.file);
    }
    let mode = if report.dry_run { " (dry run)" } else { "" };
    println!(
        "\n{} file(s) in {}{mode}: {} created, {} skipped, {} failed → palace {}",
        report.total, report.dir, report.created, report.skipped, report.failed, report.palace,
    );
}
