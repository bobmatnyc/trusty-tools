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
use trusty_code::agents::deploy::{ManifestStatus, roster_manifest_status};
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
/// the deployed-roster manifest's state (#2074), the write root, and the private
/// state directory with its privacy status. With `json`, the same facts as one
/// object on stdout so a script can assert on them. Unreadable candidates that
/// were skipped are listed too — they already warned to stderr, and a diagnostic
/// that hid them would be the wrong tool for the job it exists to do.
/// Test: `tests/cli_e2e.rs::paths_show_reports_the_winning_source`,
/// `tests/roster_deploy_e2e.rs::paths_show_reports_the_roster_manifest`.
pub fn show(project_root: &Path, json: bool) -> Result<()> {
    let state_dir = private_state::private_state_dir();
    let private_ok = private_state::is_restrictive(&state_dir).ok();
    // #2074: the deployed roster's ledger — absent, present with a count, or
    // corrupt. An operator debugging a stale or hand-edited agent asks this
    // immediately after "which root wins?".
    let (manifest_path, manifest_status) = roster_manifest_status(project_root);

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
            "roster_manifest": {
                "path": manifest_path.display().to_string(),
                "status": manifest_status.as_str(),
                "managed": match &manifest_status {
                    ManifestStatus::Present { managed } => Some(*managed),
                    _ => None,
                },
                "detail": match &manifest_status {
                    ManifestStatus::Corrupt { detail } => Some(detail.clone()),
                    _ => None,
                },
            },
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
    println!(
        "roster manifest   {:<12} {}",
        manifest_status.as_str(),
        manifest_path.display()
    );
    match &manifest_status {
        ManifestStatus::Absent => println!(
            "{:<18}no roster deployed here yet — one lands on the next run",
            ""
        ),
        ManifestStatus::Present { managed } => {
            println!("{:<18}{managed} deployed agent file(s) tracked", "")
        }
        ManifestStatus::Corrupt { detail } => println!(
            "{:<18}UNREADABLE — repair or delete it; deploys refuse until then ({detail})",
            ""
        ),
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

/// The exit code `tcode paths import` uses when at least one entry was refused.
///
/// Why: #6999 — the refusal was printed but the process exited 0, so a caller
/// scripting the import could not tell a clean migration from one that silently
/// left a symlink escape, an executable, or a secret-bearing `settings.json`
/// behind. A distinct nonzero code is the only signal a script can branch on.
/// What: `1`. Deliberately outside the `run-task` ladder
/// (`trusty_code::run_task::ExitCode` uses `0` and `2`–`6`), so no caller can
/// confuse the two.
/// Test: `tests/cli_e2e.rs::paths_import_exits_nonzero_when_an_entry_is_refused`.
pub const IMPORT_REFUSED_EXIT_CODE: i32 = 1;

/// Plan, print, and (unless `dry_run`) apply the `.claude/` → `.trusty-code/`
/// import, returning the process exit code.
///
/// Why: the plan is printed in both modes so `--dry-run` output and the real
/// run's output describe the same work — the property that makes a dry run
/// worth trusting. #6999 extends that property to the exit code: a plan holding
/// a refusal reports [`IMPORT_REFUSED_EXIT_CODE`] in BOTH modes, so a dry run
/// and the run it previews cannot disagree on whether the migration is clean.
/// What: [`paths::import::plan_import`], printed one line per entry; with
/// `dry_run` it stops there, otherwise it applies the plan and prints the
/// created and refused counts. It also creates and tightens the private state
/// directory, since that is the other half of the layout an operator running
/// this command is adopting. Returns `0` when nothing was refused and
/// [`IMPORT_REFUSED_EXIT_CODE`] otherwise — including the benign
/// "target already exists" refusal, which is what a re-run of a completed
/// import reports.
/// Test: `tests/cli_e2e.rs::paths_import_dry_run_writes_nothing`,
/// `tests/cli_e2e.rs::paths_import_exits_nonzero_when_an_entry_is_refused`.
pub fn import(project_root: &Path, dry_run: bool) -> Result<i32> {
    let plan = paths::import::plan_import(project_root);
    if plan.entries.is_empty() {
        println!("nothing to import: no .claude/agents, .claude/skills, or .claude/settings.json");
    }
    let mut refused = 0usize;
    for entry in &plan.entries {
        match &entry.action {
            paths::import::ImportAction::Copy => {
                println!("copy    {}", entry.to.display());
            }
            paths::import::ImportAction::Refuse(reason) => {
                refused += 1;
                println!("skip    {} — {reason}", entry.from.display());
            }
        }
    }

    if dry_run {
        println!("\n--dry-run: nothing was written.");
        // #6999: the dry run reports the exit code the real run would.
        return Ok(exit_code_for(refused));
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
    Ok(exit_code_for(report.refused.len()))
}

/// Map a refusal count onto the import's exit code.
///
/// Why: the dry-run and applied paths must agree, so the mapping is written
/// once (#6999).
/// What: `0` for no refusals, [`IMPORT_REFUSED_EXIT_CODE`] otherwise.
/// Test: `tests/cli_e2e.rs::paths_import_exits_nonzero_when_an_entry_is_refused`.
fn exit_code_for(refused: usize) -> i32 {
    if refused == 0 {
        0
    } else {
        IMPORT_REFUSED_EXIT_CODE
    }
}
