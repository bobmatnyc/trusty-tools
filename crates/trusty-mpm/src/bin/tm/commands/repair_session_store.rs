//! `tm repair session-store` — recover a corrupt `sessions.json` (#5007).
//!
//! Why: `SessionStore` reloads before every save, so a store it cannot parse is
//! a store it can never rewrite. On 2026-08-06 that left the owner's daemon
//! unable to complete a single mutation, and the only fix available was a human
//! hand-truncating JSON. There has to be a supported command.
//! What: [`repair_session_store`] resolves the store path, plans the cut, prints
//! it, and (unless `--dry-run`) backs the file up and truncates it.
//! Test: `repair_session_store_reports_a_healthy_store`,
//! `repair_session_store_dry_run_writes_nothing`,
//! `repair_session_store_truncates_and_backs_up`.

use std::path::PathBuf;

use anyhow::{Context, Result};
use trusty_mpm::core::paths::FrameworkPaths;
use trusty_mpm::session_manager::store_integrity::{RepairError, plan, repair};

/// Resolve the store file this command operates on.
///
/// Why: the default has to match what the daemon actually writes, and an
/// explicit `--path` is what makes the command testable without touching the
/// operator's real store.
/// What: `--path` when given, else
/// `<framework root>/session-manager/sessions.json`.
/// Test: `resolve_store_path_defaults_under_the_framework_root`.
pub(crate) fn resolve_store_path(explicit: Option<String>) -> PathBuf {
    match explicit {
        Some(p) => PathBuf::from(p),
        None => FrameworkPaths::default()
            .root
            .join("session-manager")
            .join("sessions.json"),
    }
}

/// `tm repair session-store` — back up and truncate a corrupt store.
///
/// Why: see the module doc.
/// What: prints the diagnosis and the planned cut. With `--dry-run` it stops
/// there having written nothing. Otherwise it copies the file to
/// `<path>.corrupt-backup-<YYYYMMDD-HHMMSS>` and truncates it to the longest
/// complete JSON document at its head. A store that parses is reported as
/// healthy and left untouched — that is a success, not an error, so re-running
/// the command after a repair is safe. A refusal (the discarded bytes name an
/// unknown session id) is an error that names the ids and the `--force`
/// override.
/// Test: `repair_session_store_reports_a_healthy_store`,
/// `repair_session_store_dry_run_writes_nothing`,
/// `repair_session_store_truncates_and_backs_up`.
pub(crate) fn repair_session_store(path: Option<String>, dry_run: bool, force: bool) -> Result<()> {
    let path = resolve_store_path(path);
    let planned = match plan(&path) {
        Ok(p) => p,
        Err(RepairError::NotCorrupt(p)) => {
            println!(
                "session store {} parses cleanly — nothing to do",
                p.display()
            );
            return Ok(());
        }
        Err(e) => return Err(anyhow::Error::new(e)),
    };
    println!("{}", planned.integrity.diagnostic());
    println!(
        "  recovered document: {} byte(s), {} session record(s)",
        planned.integrity.valid_prefix_bytes.unwrap_or(0),
        planned.recovered_sessions
    );
    println!(
        "  would discard:      {} trailing byte(s)",
        planned.integrity.trailing_bytes()
    );
    if !planned.orphan_ids.is_empty() {
        println!(
            "  ⚠ the discarded bytes name {} session id(s) absent from the recovered document: {}",
            planned.orphan_ids.len(),
            planned.orphan_ids.join(", ")
        );
    }
    if dry_run {
        println!("--dry-run: nothing was written");
        return Ok(());
    }
    let outcome = repair(&path, force, chrono::Utc::now())
        .with_context(|| format!("repairing session store {}", path.display()))?;
    println!("backed up to {}", outcome.backup.display());
    println!(
        "truncated to {} byte(s); discarded {} byte(s); {} session record(s) recovered",
        outcome.kept_bytes, outcome.discarded_bytes, outcome.sessions
    );
    if !outcome.forced_orphans.is_empty() {
        println!(
            "--force discarded {} orphaned session id(s): {}",
            outcome.forced_orphans.len(),
            outcome.forced_orphans.join(", ")
        );
    }
    println!("restart the daemon so it reloads the repaired store");
    Ok(())
}

#[cfg(test)]
#[path = "repair_session_store_tests.rs"]
mod repair_session_store_tests;
