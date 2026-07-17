//! `tm hooks` command handlers — project-settings hook hygiene (issue #2940).
//!
//! Why: a machine that ran a pre-#2940 `tm install` has tm hook entries
//! scattered across every project under `$HOME` that had a `.claude/`
//! directory at install time — a one-way street away from other harnesses,
//! and a source of conflicts with a project's own claude-mpm hooks. This
//! module is the standalone (no daemon needed) cleanup surface: `tm hooks
//! clean`.
//! What: [`clean`] — the `tm hooks clean` handler. Thin CLI/printing layer
//! over [`trusty_mpm::core::standalone::hooks::cleanup`], which owns every
//! actual read/write.
//! Test: `clean_dry_run_reports_without_writing`,
//! `clean_force_applies_and_reports_backup`,
//! `clean_with_explicit_path_scopes_to_that_project`.

use std::path::PathBuf;

use colored::Colorize;
use trusty_mpm::core::standalone::hooks::cleanup::clean_settings_file;

/// Resolve the settings files `tm hooks clean` should scan.
///
/// Why: `--path <dir>` targets exactly one project's two settings files
/// without paying for a `$HOME`-wide walk; omitting it reproduces the same
/// discovery a pre-#2940 `tm install` used, so `clean` can undo exactly what
/// `install` used to contaminate.
/// What: with `path` given, returns
/// `[<dir>/.claude/settings.json, <dir>/.claude/settings.local.json]`
/// (existence is checked later by [`clean_settings_file`], not here).
/// Without it, delegates to
/// [`trusty_common::claude_config::discover_claude_settings`] under the
/// resolved home directory.
/// Test: `resolve_targets_with_explicit_path_returns_both_settings_files`.
fn resolve_targets(path: Option<&str>) -> anyhow::Result<Vec<PathBuf>> {
    if let Some(p) = path {
        let dir = PathBuf::from(p);
        return Ok(vec![
            dir.join(".claude").join("settings.json"),
            dir.join(".claude").join("settings.local.json"),
        ]);
    }
    let home =
        dirs::home_dir().ok_or_else(|| anyhow::anyhow!("could not resolve home directory"))?;
    Ok(trusty_common::claude_config::discover_claude_settings(
        &home,
        trusty_common::claude_config::default_settings_max_depth(),
    ))
}

/// `tm hooks clean` handler.
///
/// Why: the single entry point for [`crate::cli::HooksAction::Clean`] — see
/// that variant's doc comment for the full behavior contract (dry-run
/// default, `--force` to apply, `--path` to scope).
/// What: resolves the target settings files via [`resolve_targets`], calls
/// [`clean_settings_file`] on each, and prints one line per file that had tm
/// hook contamination (dry-run: "would remove"; `--force`: "removed … backup
/// at …"). Files with no contamination are silent. Prints a trailing summary
/// and, in dry-run mode, a reminder that `--force` is required to apply.
/// Test: `clean_dry_run_reports_without_writing`,
/// `clean_force_applies_and_reports_backup`,
/// `clean_with_explicit_path_scopes_to_that_project`.
pub(crate) fn clean(path: Option<String>, force: bool) -> anyhow::Result<()> {
    let targets = resolve_targets(path.as_deref())?;

    let mut modified = 0usize;
    for file in &targets {
        match clean_settings_file(file, force) {
            Ok(Some(outcome)) => {
                modified += 1;
                let events = outcome.removed_events.join(", ");
                if let Some(backup) = &outcome.backup_path {
                    println!(
                        "  {} {} — removed [{}] (backup: {})",
                        "✓".green(),
                        outcome.path.display(),
                        events,
                        backup.display()
                    );
                } else {
                    println!(
                        "  {} {} — would remove [{}]",
                        "~".yellow(),
                        outcome.path.display(),
                        events
                    );
                }
            }
            Ok(None) => {}
            Err(e) => {
                eprintln!("  {} {}: {e}", "✗".red(), file.display());
            }
        }
    }

    if modified == 0 {
        println!("no tm hook contamination found.");
    } else if force {
        println!(
            "\ncleaned {} file{}.",
            modified,
            if modified == 1 { "" } else { "s" }
        );
    } else {
        println!(
            "\ndry run — {} file{} would be modified. Re-run with --force to apply.",
            modified,
            if modified == 1 { "" } else { "s" }
        );
    }
    Ok(())
}

#[cfg(test)]
#[path = "hooks_tests.rs"]
mod tests;
