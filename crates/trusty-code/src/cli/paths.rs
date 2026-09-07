//! `tcode paths show` / `tcode paths import` — configuration-layout diagnostics
//! and the legacy import (#5426).
//!
//! Why: after #5426 a project can be configured under `.trusty-code/`, under
//! `.claude/`, or under both, and the operator's first question is "which one is
//! actually winning?". Answering it by reading the source is exactly the drift
//! the single resolver exists to prevent, so the resolver reports it directly.
//! `import` is the other half: it moves an existing `.claude/` catalog into
//! Trusty Code's own directory, showing the plan before it does anything.
//!
//! What: [`show`] prints the winning source and path for agents, skills,
//! plugins, `settings.json`, and `CLAUDE.md`, plus the private state directory
//! and whether its permissions are restrictive. [`import`] prints the plan and,
//! unless `--dry-run`, applies it. Every decision lives in
//! [`trusty_code::paths`]; this file is argument-shaped glue and rendering, like
//! every other handler here.
//!
//! Test: `tests/cli_e2e.rs::paths_show_reports_the_winning_source`,
//! `tests/cli_e2e.rs::paths_import_dry_run_writes_nothing`.

use std::path::Path;

use anyhow::Result;
use trusty_code::paths::{self, EntryKind, private_state};

/// The entries `tcode paths show` reports, in display order.
///
/// Why: one table so the human rendering and the JSON rendering cannot list
/// different things.
/// What: `(label, relative path, kind)` triples.
/// Test: `tests/cli_e2e.rs::paths_show_reports_the_winning_source`.
const REPORTED: &[(&str, &str, EntryKind)] = &[
    ("agents", "agents", EntryKind::Dir),
    ("skills", "skills", EntryKind::Dir),
    ("plugins", "plugins", EntryKind::Dir),
    ("settings", paths::SETTINGS_FILENAME, EntryKind::File),
    ("context", "CLAUDE.md", EntryKind::File),
];

/// Print which configuration root wins for each entry.
///
/// Why: the diagnostic #5426 asks for — "report the winning source and target".
/// What: one line per [`REPORTED`] entry as `<label>  <source>  <path>`, then
/// the write root and the private state directory with its privacy status. With
/// `json`, the same facts as one object on stdout so a script can assert on
/// them. Unreadable candidates that were skipped are listed too — they already
/// warned to stderr, and a diagnostic that hid them would be the wrong tool for
/// the job it exists to do.
/// Test: `tests/cli_e2e.rs::paths_show_reports_the_winning_source`.
pub fn show(project_root: &Path, json: bool) -> Result<()> {
    let state_dir = private_state::private_state_dir();
    let private_ok = private_state::is_restrictive(&state_dir).ok();

    if json {
        let entries: serde_json::Map<String, serde_json::Value> = REPORTED
            .iter()
            .map(|(label, relative, kind)| {
                let r = paths::resolve_project_entry(project_root, relative, *kind);
                (
                    (*label).to_string(),
                    serde_json::json!({
                        "source": r.source.as_str(),
                        "path": r.path.display().to_string(),
                        "skipped_unreadable": r
                            .unreadable
                            .iter()
                            .map(|p| p.display().to_string())
                            .collect::<Vec<_>>(),
                    }),
                )
            })
            .collect();
        let doc = serde_json::json!({
            "project_root": project_root.display().to_string(),
            "write_root": paths::native_config_dir(project_root).display().to_string(),
            "private_state_dir": state_dir.display().to_string(),
            "private_state_restrictive": private_ok,
            "entries": entries,
        });
        println!("{}", serde_json::to_string_pretty(&doc)?);
        return Ok(());
    }

    println!("project root      {}", project_root.display());
    println!(
        "write root        {}  (trusty-code writes only here)",
        paths::native_config_dir(project_root).display()
    );
    println!();
    for (label, relative, kind) in REPORTED {
        let r = paths::resolve_project_entry(project_root, relative, *kind);
        println!("{label:<10} {:<12} {}", r.source.as_str(), r.path.display());
        for skipped in &r.unreadable {
            println!("{:<10} {:<12} {}", "", "unreadable", skipped.display());
        }
    }
    println!();
    println!("private state     {}", state_dir.display());
    println!(
        "permissions       {}",
        match private_ok {
            Some(true) => "owner-only".to_string(),
            Some(false) => "PERMISSIVE — run `tcode paths import` or chmod 700".to_string(),
            None => "unknown (directory not created yet)".to_string(),
        }
    );
    Ok(())
}

/// Plan, print, and (unless `dry_run`) apply the `.claude/` → `.trusty-code/`
/// import.
///
/// Why: the plan is printed in both modes so `--dry-run` output and the real
/// run's output describe the same work — the property that makes a dry run
/// worth trusting.
/// What: [`paths::import::plan_import`], printed one line per entry; with
/// `dry_run` it stops there, otherwise it applies the plan and prints the
/// created and refused counts. It also creates and tightens the private state
/// directory, since that is the other half of the layout an operator running
/// this command is adopting.
/// Test: `tests/cli_e2e.rs::paths_import_dry_run_writes_nothing`.
pub fn import(project_root: &Path, dry_run: bool) -> Result<()> {
    let plan = paths::import::plan_import(project_root);
    if plan.entries.is_empty() {
        println!("nothing to import: no .claude/agents, .claude/skills, or .claude/settings.json");
    }
    for entry in &plan.entries {
        match &entry.action {
            paths::import::ImportAction::Copy => {
                println!("copy    {}", entry.to.display());
            }
            paths::import::ImportAction::Refuse(reason) => {
                println!("skip    {} — {reason}", entry.from.display());
            }
        }
    }

    if dry_run {
        println!("\n--dry-run: nothing was written.");
        return Ok(());
    }

    let report = paths::import::apply_import(&plan);
    match private_state::ensure_private_state_dir() {
        Ok(dir) => println!("\nprivate state {} (owner-only)", dir.display()),
        Err(e) => eprintln!("\nwarning: could not create the private state directory: {e}"),
    }
    println!(
        "imported {} file(s); skipped {}.",
        report.created.len(),
        report.refused.len()
    );
    if !report.created.is_empty() {
        println!("to undo, delete exactly these files:");
        for path in &report.created {
            println!("  {}", path.display());
        }
    }
    Ok(())
}
