//! `trusty-memory palace migrate [<palace> | --all] [--dry-run | --verify] [--yes]` (#10094).
//!
//! Why: a palace behind this binary's format was refused with no command to
//! convert it, so a user converted palaces by hand (#10094). The daemon
//! migrates at its writer open (#9918); this is the same chain as an explicit,
//! previewable and checkable step.
//! What: [`handle_migrate`] routes like `palace backup`: the daemon's
//! `palace_migrate` tool when a daemon serves this data root, otherwise
//! [`run_request`] in-process. Both run [`run_request`], so the daemon and
//! the offline path share one implementation. `--all` prints the dry-run
//! table first, checks the total free space, and refuses without `--yes`
//! when more than one palace would change. [`outcome`] turns a failed check
//! into a non-zero exit after the report is printed.
//! Test: format_upgrade_tests.rs (the engine half) and
//! palace_migrate_tests.rs (requests, the `--all` gate, the exit status).

use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::engine::store::PalaceStore;
use crate::engine::store::format_upgrade::{MigrateReport, PalacePlan, migrate, plan};
use crate::engine::store::format_upgrade_verify::{FormatStatus, VerifyReport, verify};

use super::palace_migrate_render::render;

/// Budget for the daemon's `palace_migrate` call: a migration hashes and
/// copies every primary file, so a large palace takes far longer than a call.
const MIGRATE_CALL_TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// Which of the three things `palace migrate` does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MigrateMode {
    /// Back up, convert, then check.
    Migrate,
    /// Report the plan; write nothing.
    DryRun,
    /// Run the post-migration checks; write nothing.
    Verify,
}

/// One `palace migrate` request, as the CLI and the `palace_migrate` tool take it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MigrateRequest {
    /// One palace id; `None` with `all`.
    pub palace: Option<String>,
    /// Every palace under the data root.
    #[serde(default)]
    pub all: bool,
    pub mode: MigrateMode,
    /// Confirms an `--all` migration that changes more than one palace.
    #[serde(default)]
    pub yes: bool,
}

/// `trusty-memory palace migrate`.
///
/// Why/What: see the module doc. A daemon's answer is final: a failed call is
/// an error, never a fallback to the in-process path, which could not open a
/// palace the daemon holds.
/// Test: `all_without_yes_refuses_and_changes_nothing`,
/// `a_failed_check_is_a_non_zero_exit_naming_the_backup`.
pub async fn handle_migrate(req: MigrateRequest) -> Result<()> {
    let daemon = crate::client::is_running().await;
    if req.all && req.mode == MigrateMode::Migrate {
        // #10094: the table first, so a refusal below follows what it refuses.
        let preview = MigrateRequest {
            mode: MigrateMode::DryRun,
            yes: false,
            ..req.clone()
        };
        print!("{}", render(&call(&preview, daemon).await?)?);
    }
    let report = call(&req, daemon).await?;
    print!("{}", render(&report)?);
    outcome(&report)
}

/// Run `req` on the daemon or in-process.
async fn call(req: &MigrateRequest, daemon: bool) -> Result<Value> {
    if daemon {
        let params = serde_json::to_value(req).context("encode palace_migrate request")?;
        return crate::client::call_with_timeout("palace_migrate", params, MIGRATE_CALL_TIMEOUT)
            .await;
    }
    let data_dir = trusty_common::resolve_data_dir("trusty-memory")
        .context("resolve trusty-memory data dir")?;
    let root = crate::resolve_palace_registry_dir(data_dir);
    let req = req.clone();
    tokio::task::spawn_blocking(move || run_request(&root, &req))
        .await
        .context("join palace migrate")?
}

/// Run one request against the palaces under `registry_dir`.
///
/// Why: the one implementation behind the CLI's offline path and the daemon's
/// `palace_migrate` tool (#10094).
/// What: resolves the palaces (one by id, or all). `dry_run` → `{plans,
/// total_needed_bytes, free_bytes}`; `verify` → `{reports}`; `migrate` →
/// `{results}`, migrating only palaces that would change for `all`, after
/// refusing without `yes` when more than one would, and refusing when the
/// total backup space exceeds the free space. A refusal writes nothing. `ok`
/// is false when any check failed or, for `all`, any palace is refused.
/// Test: `dry_run_all_reports_every_palace_and_writes_nothing`,
/// `all_without_yes_refuses_and_changes_nothing`,
/// `all_with_yes_migrates_every_palace_that_needs_it`,
/// `a_failed_check_is_a_non_zero_exit_naming_the_backup`.
pub fn run_request(registry_dir: &Path, req: &MigrateRequest) -> Result<Value> {
    let palaces = select(registry_dir, req)?;
    match req.mode {
        MigrateMode::DryRun => {
            let plans = plans_of(&palaces)?;
            let total: u64 = plans
                .iter()
                .filter(|p| p.would_change())
                .map(|p| p.needed_bytes)
                .sum();
            let ok = plans.iter().all(|p| p.status != FormatStatus::Refused);
            Ok(json!({
                "mode": req.mode,
                "ok": ok,
                "plans": plans,
                "total_needed_bytes": total,
                "free_bytes": plans.first().and_then(|p| p.free_bytes),
            }))
        }
        MigrateMode::Verify => {
            let reports = palaces
                .iter()
                .map(|(id, dir)| verify(dir, id))
                .collect::<Result<Vec<VerifyReport>>>()?;
            let ok = reports.iter().all(|r| r.gaps.is_empty());
            Ok(json!({"mode": req.mode, "ok": ok, "reports": reports}))
        }
        MigrateMode::Migrate if req.all => migrate_all(&palaces, req.yes),
        MigrateMode::Migrate => {
            let (id, dir) = &palaces[0];
            let result = migrate(dir, id)?;
            let ok = result.problems.is_empty();
            Ok(json!({"mode": req.mode, "ok": ok, "results": [result]}))
        }
    }
}

/// `--all` migration: gate on `--yes` and the total free space, then migrate.
///
/// Test: `all_without_yes_refuses_and_changes_nothing`,
/// `all_with_yes_migrates_every_palace_that_needs_it`.
fn migrate_all(palaces: &[(String, std::path::PathBuf)], yes: bool) -> Result<Value> {
    let plans = plans_of(palaces)?;
    let changing: Vec<&PalacePlan> = plans.iter().filter(|p| p.would_change()).collect();
    if changing.len() > 1 && !yes {
        let names: Vec<&str> = changing.iter().map(|p| p.palace.as_str()).collect();
        bail!(
            "palace migrate --all: {} palaces would change ({}); re-run with --yes to migrate \
             them. Nothing was changed",
            changing.len(),
            names.join(", ")
        );
    }
    let needed: u64 = changing.iter().map(|p| p.needed_bytes).sum();
    if let Some(free) = changing.first().and_then(|p| p.free_bytes)
        && free < needed
    {
        bail!(
            "palace migrate --all: the backups need {needed} bytes free and {free} are free \
             under {}. Nothing was changed",
            changing[0].backup_root.display()
        );
    }
    let mut results: Vec<MigrateReport> = Vec::new();
    for p in &changing {
        results.push(migrate(&p.dir, &p.palace)?);
    }
    let refused: Vec<&PalacePlan> = plans
        .iter()
        .filter(|p| p.status == FormatStatus::Refused)
        .collect();
    let ok = results.iter().all(|r| r.problems.is_empty()) && refused.is_empty();
    Ok(json!({"mode": MigrateMode::Migrate, "ok": ok, "results": results, "refused": refused}))
}

/// The palaces a request names: `(id, directory)`, sorted by id for `all`.
fn select(registry_dir: &Path, req: &MigrateRequest) -> Result<Vec<(String, std::path::PathBuf)>> {
    let mut palaces: Vec<(String, std::path::PathBuf)> = PalaceStore::list_palaces(registry_dir)
        .with_context(|| format!("list palaces under {}", registry_dir.display()))?
        .into_iter()
        .map(|p| (p.id.0, p.data_dir))
        .collect();
    palaces.sort();
    match (&req.palace, req.all) {
        (Some(_), true) => bail!("palace migrate: name one palace or pass --all, not both"),
        (None, false) => bail!("palace migrate: name a palace or pass --all"),
        (None, true) => Ok(palaces),
        (Some(name), false) => palaces
            .into_iter()
            .find(|(id, _)| id == name)
            .map(|p| vec![p])
            .with_context(|| format!("no palace named '{name}' under {}", registry_dir.display())),
    }
}

/// The plan of every palace, in order.
fn plans_of(palaces: &[(String, std::path::PathBuf)]) -> Result<Vec<PalacePlan>> {
    palaces.iter().map(|(id, dir)| plan(dir, id)).collect()
}

/// `Err` when a report says a check failed, naming each palace's gap and
/// its backup; `Ok` otherwise.
///
/// Why: #10094 — a failed post-check must be a non-zero exit that names the
/// backup, after the report is printed.
/// Test: `a_failed_check_is_a_non_zero_exit_naming_the_backup`.
pub fn outcome(report: &Value) -> Result<()> {
    if report["ok"].as_bool() != Some(false) {
        return Ok(());
    }
    let mut lines = Vec::new();
    for r in report["results"].as_array().into_iter().flatten() {
        let problems: Vec<String> = serde_json::from_value(r["problems"].clone())?;
        if problems.is_empty() {
            continue;
        }
        let backup = match r["backup"].as_str() {
            Some(b) => format!("the verified backup is {b}"),
            None => match r["verify"]["backup"].as_str() {
                Some(b) => format!("no backup was taken this run; the newest backup is {b}"),
                None => "no backup was taken this run and none exists".to_string(),
            },
        };
        lines.push(format!(
            "palace {}: {}; {backup}; nothing was deleted",
            r["palace"].as_str().unwrap_or("?"),
            problems.join("; ")
        ));
    }
    for r in report["reports"].as_array().into_iter().flatten() {
        let gaps: Vec<String> = serde_json::from_value(r["gaps"].clone())?;
        if !gaps.is_empty() {
            lines.push(format!(
                "palace {}: {}",
                r["palace"].as_str().unwrap_or("?"),
                gaps.join("; ")
            ));
        }
    }
    for p in report["refused"].as_array().into_iter().flatten() {
        lines.push(format!(
            "palace {} refused: {}",
            p["palace"].as_str().unwrap_or("?"),
            p["status_detail"].as_str().unwrap_or("?")
        ));
    }
    for p in report["plans"].as_array().into_iter().flatten() {
        if p["status"] == "refused" {
            lines.push(format!(
                "palace {} refused: {}",
                p["palace"].as_str().unwrap_or("?"),
                p["status_detail"].as_str().unwrap_or("?")
            ));
        }
    }
    bail!("palace migrate: a check failed:\n  {}", lines.join("\n  "))
}

#[cfg(test)]
#[path = "palace_migrate_tests.rs"]
mod tests;
