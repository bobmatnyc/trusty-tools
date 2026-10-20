//! The text `trusty-memory palace migrate` prints (#10094).
//!
//! Why: the daemon route and the offline route return the same JSON, so one
//! renderer reads it and the two routes print the same text.
//! What: [`render`] dispatches on `mode`: a plan table for `dry_run`, a
//! per-marker block for `verify`, a per-palace result for `migrate`.
//! Test: `dry_run_all_reports_every_palace_and_writes_nothing`,
//! `a_failed_check_is_a_non_zero_exit_naming_the_backup`.

use anyhow::{Context, Result};
use serde_json::Value;

use crate::engine::store::format_upgrade::{MigrateReport, PalacePlan};
use crate::engine::store::format_upgrade_verify::{VerifyReport, describe};

/// Render a `palace migrate` report as text.
pub fn render(report: &Value) -> Result<String> {
    let field = |name: &str| report.get(name).cloned().unwrap_or(Value::Null);
    match report["mode"].as_str() {
        Some("dry_run") => {
            let plans: Vec<PalacePlan> =
                serde_json::from_value(field("plans")).context("decode plans")?;
            Ok(render_plans(
                &plans,
                report["total_needed_bytes"].as_u64(),
                report["free_bytes"].as_u64(),
            ))
        }
        Some("verify") => {
            let reports: Vec<VerifyReport> =
                serde_json::from_value(field("reports")).context("decode reports")?;
            Ok(reports.iter().map(render_verify).collect())
        }
        Some("migrate") => {
            let results: Vec<MigrateReport> =
                serde_json::from_value(field("results")).context("decode results")?;
            Ok(results.iter().map(render_result).collect())
        }
        other => anyhow::bail!("palace migrate: unknown report mode {other:?}"),
    }
}

/// The dry-run table: one row per palace, then the totals.
fn render_plans(plans: &[PalacePlan], total: Option<u64>, free: Option<u64>) -> String {
    let mut out = String::from("palace migrate --dry-run: nothing is written\n");
    out.push_str(&format!(
        "{:<24} {:<16} {:>5} {:>6} {:>10} {:>10} {:>10}  planned backup\n",
        "palace", "status", "found", "target", "size", "needs", "free"
    ));
    for p in plans {
        out.push_str(&format!(
            "{:<24} {:<16} {:>5} {:>6} {:>10} {:>10} {:>10}  {}\n",
            p.palace,
            p.status.label(),
            p.found.map_or_else(|| "?".into(), |f| f.to_string()),
            p.target,
            bytes(Some(p.estimated_bytes)),
            bytes(Some(p.needed_bytes)),
            bytes(p.free_bytes),
            p.planned_backup.as_deref().unwrap_or("-"),
        ));
        if let Some(detail) = &p.status_detail {
            out.push_str(&format!("  {}: {detail}\n", p.palace));
        }
    }
    let changing = plans.iter().filter(|p| p.would_change()).count();
    out.push_str(&format!(
        "{changing} of {} palace(s) would change; backups need {} free, {} free\n",
        plans.len(),
        bytes(total),
        bytes(free)
    ));
    out
}

/// One palace's `--verify` block.
fn render_verify(r: &VerifyReport) -> String {
    let mut out = format!(
        "palace {} ({}): format status {}\n",
        r.palace,
        r.dir.display(),
        r.status.label()
    );
    if let Some(detail) = &r.status_detail {
        out.push_str(&format!("  note: {detail}\n"));
    }
    out.push_str(&format!(
        "  kg_schema palace_format         {}\n  kg_schema triple_key            {}\n  \
         palace.json format_version      {}\n  format_migration.json           {}\n  \
         drawers {}  triples {}{}\n",
        describe(r.palace_format),
        describe(r.triple_key),
        describe(r.mirror),
        if r.migration_record {
            "present"
        } else {
            "absent"
        },
        r.drawers,
        r.triples,
        if r.read_from_copy {
            "  (read from a copy)"
        } else {
            ""
        },
    ));
    out.push_str(&match (&r.backup, r.backup_verified) {
        (Some(b), Some(true)) => format!("  backup  {} (verifies)\n", b.display()),
        (Some(b), _) => format!("  backup  {} (DOES NOT VERIFY)\n", b.display()),
        (None, _) => format!("  backup  none under {}\n", r.backup_root.display()),
    });
    if r.gaps.is_empty() {
        out.push_str("  OK: every check passed\n");
    } else {
        for g in &r.gaps {
            out.push_str(&format!("  GAP: {g}\n"));
        }
    }
    out
}

/// One palace's migration result.
fn render_result(r: &MigrateReport) -> String {
    let from = r.from.map_or_else(|| "?".into(), |f| f.to_string());
    let mut out = if r.migrated {
        format!("palace {}: migrated format {from} -> {}\n", r.palace, r.to)
    } else {
        format!(
            "palace {}: already at format {}; nothing to do, no backup taken\n",
            r.palace, r.to
        )
    };
    if let Some(b) = &r.backup {
        out.push_str(&format!("  backup  {}\n", b.display()));
    }
    out.push_str(&format!(
        "  drawers {} -> {}  triples {} -> {}\n",
        r.before.drawers, r.after.drawers, r.before.triples, r.after.triples
    ));
    if r.problems.is_empty() {
        out.push_str(
            "  checks: format current; markers agree; no migration record; counts unchanged\n",
        );
    } else {
        for p in &r.problems {
            out.push_str(&format!("  FAILED: {p}\n"));
        }
    }
    out
}

/// Bytes in the largest whole binary unit, or `unknown`.
fn bytes(n: Option<u64>) -> String {
    let Some(n) = n else {
        return "unknown".into();
    };
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = n as f64;
    let mut unit = 0;
    while v >= 1024.0 && unit < UNITS.len() - 1 {
        v /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", UNITS[unit])
    }
}
