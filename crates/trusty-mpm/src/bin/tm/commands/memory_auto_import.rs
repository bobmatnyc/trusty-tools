//! `tm memory import-auto-memory` — the auto-memory → palace migration (#7685).
//!
//! Why: a thin translation layer, for the same reason
//! [`super::memory`] is one — every decision (which config dir, which palace,
//! what today's date is, what happens on a failed store) lives in
//! [`trusty_mpm::core::auto_memory_import`] so it is testable without a CLI.
//! What: [`import_auto_memory`] resolves the options, runs the migration, and
//! renders either the JSON report (`--json`) or a per-file summary. Exits
//! non-zero when any fact failed to store, so the operator is told the index was
//! deliberately left intact.
//! Test: `cli_parses_memory_import_auto_memory` in `tests.rs`; the migration
//! behaviour is covered by `core::auto_memory_import::tests`.

use std::path::PathBuf;

use anyhow::Context as _;
use trusty_mpm::core::auto_memory_import::{
    AutoImportReport, AutoImportStatus, resolve_auto_import_options, run_auto_memory_import,
};

/// Run `tm memory import-auto-memory`.
///
/// Why: keeps `commands::memory`'s dispatch one line per action.
/// What: defaults `project` to the cwd, resolves the rest through
/// [`resolve_auto_import_options`], runs the migration, prints the report, and
/// returns `Err` when any file failed so the process exits non-zero.
/// Test: `cli_parses_memory_import_auto_memory`.
pub(crate) async fn import_auto_memory(
    project: Option<PathBuf>,
    palace: Option<String>,
    json: bool,
    memory_socket: Option<PathBuf>,
) -> anyhow::Result<()> {
    let project_dir = match project {
        Some(dir) => dir,
        None => std::env::current_dir().context("resolve the current directory")?,
    };
    let opts = resolve_auto_import_options(&project_dir, palace, memory_socket)?;
    let report = run_auto_memory_import(&opts).await?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report).context("serialise the migration report")?
        );
    } else {
        print_summary(&report);
    }
    if report.failed > 0 {
        anyhow::bail!(
            "{} auto-memory fact(s) could not be stored — they are still on disk and \
             MEMORY.md was left intact; fix the cause and re-run",
            report.failed
        );
    }
    Ok(())
}

/// Render the human-readable summary.
///
/// Why: `--json` is the machine surface; an operator running this by hand wants
/// one line per fact and a statement of what happened to the index.
/// What: prints `<status> <file> <drawer-id> [error]` per fact, then the totals
/// and the archive location.
/// Test: exercised indirectly by the CLI parse test; output is cosmetic.
fn print_summary(report: &AutoImportReport) {
    for file in &report.files {
        let status = match file.status {
            AutoImportStatus::Stored => "stored",
            AutoImportStatus::Failed => "FAILED",
        };
        let drawer = file.drawer_id.as_deref().unwrap_or("-");
        let error = file.error.as_deref().unwrap_or("");
        println!("{status:<8} {:<48} {drawer} {error}", file.file);
    }
    println!(
        "\n{} fact(s) in {}: {} stored, {} failed → palace {}",
        report.total, report.dir, report.stored, report.failed, report.palace,
    );
    match &report.archive {
        Some(archive) => println!("archive: {archive}"),
        None => println!("archive: none — nothing was moved"),
    }
    if report.index_cleared {
        println!("MEMORY.md: archived and emptied");
    } else if report.total > 0 {
        println!("MEMORY.md: left intact — not every fact reached the palace");
    }
}
