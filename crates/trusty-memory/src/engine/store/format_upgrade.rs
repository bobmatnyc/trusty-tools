//! `trusty-memory palace migrate`'s engine half: plan, migrate, check (#10094).
//!
//! Why: a user who upgraded found palaces refused with no command to convert
//! them, and converted them by hand (#10094). The daemon already migrates a
//! palace at its writer open (#9918); this exposes that one chain as an
//! explicit step with a preview before it and a check after it.
//! What: [`plan`] reads a palace's format and the backup a migration would
//! take, and writes nothing. [`migrate_with`] records the format and the
//! drawer and triple counts, runs `format_migration::writer_gate` — the same
//! backup, locks and resume as the daemon's open — then runs
//! [`verify`](super::format_upgrade_verify::verify) and compares the counts.
//! It deletes nothing; a failed check is reported with the backup path.
//! Test: format_upgrade_tests.rs.

use super::format_backup::{free_bytes_for, planned_bytes};
use super::format_migration::{self, read_record, writer_gate};
use super::format_upgrade_verify::{
    FormatStatus, VerifyReport, backup_root_of, read_kg_facts, status_of, verify,
};
use super::palace_format::PALACE_FORMAT_SUPPORTED;
use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// What a migration of one palace would do; `--dry-run` prints it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PalacePlan {
    pub palace: String,
    pub dir: PathBuf,
    pub status: FormatStatus,
    pub status_detail: Option<String>,
    /// The format found; `None` when it could not be read.
    pub found: Option<u32>,
    pub target: u32,
    pub backup_root: PathBuf,
    /// The backup directory a migration would write or resume behind.
    pub planned_backup: Option<String>,
    /// Bytes of the primary files the backup copies.
    pub estimated_bytes: u64,
    /// Free bytes the backup needs (1.1x).
    pub needed_bytes: u64,
    /// Free bytes at the backup root; `None` when unknown.
    pub free_bytes: Option<u64>,
}

impl PalacePlan {
    /// Whether a migration may change this palace: behind, interrupted, or
    /// not readable without the writer's exclusive open.
    pub fn would_change(&self) -> bool {
        matches!(
            self.status,
            FormatStatus::NeedsMigration | FormatStatus::Interrupted | FormatStatus::Unverified
        )
    }
}

/// Drawer and triple row counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Counts {
    pub drawers: u64,
    pub triples: u64,
}

/// The result of migrating one palace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MigrateReport {
    pub palace: String,
    /// The format before the run; `None` when it could not be read.
    pub from: Option<u32>,
    pub to: u32,
    /// Whether the chain migrated (or resumed) the palace.
    pub migrated: bool,
    /// The verified backup the migration ran behind; `None` for a no-op.
    pub backup: Option<PathBuf>,
    pub before: Counts,
    pub after: Counts,
    /// The post-migration checks.
    pub verify: VerifyReport,
    /// Every failed post-migration check; empty means it verified.
    pub problems: Vec<String>,
}

/// Fault seams for the tests; production uses [`UpgradeSeams::default`].
#[derive(Clone, Copy)]
pub(crate) struct UpgradeSeams {
    /// Passed to the chain unchanged.
    pub(crate) chain: format_migration::Seams,
    /// Runs after the chain and before the checks.
    pub(crate) after_chain: fn(&Path),
}

impl Default for UpgradeSeams {
    fn default() -> Self {
        Self {
            chain: format_migration::Seams::default(),
            after_chain: |_| {},
        }
    }
}

/// Plan a migration of the palace `id` at `dir`; writes nothing.
///
/// Why: `--dry-run` and the `--all` table show what would happen and whether
/// the backup fits before anything is written (#10094).
/// What: the format status; for a palace behind, the backup directory the
/// chain would write (`<root>/<from>-to-<to>-<UTC>`), or, for an interrupted
/// one, the backup its record names; the primary-file bytes, the 1.1x the
/// backup needs, and the free bytes at the backup root.
/// Test: `dry_run_reports_the_plan_and_writes_nothing`.
pub(crate) fn plan(dir: &Path, id: &str) -> Result<PalacePlan> {
    let target = PALACE_FORMAT_SUPPORTED;
    let (status, found, status_detail) = status_of(dir);
    let root = backup_root_of(dir, id);
    let planned_backup = match status {
        FormatStatus::Interrupted => match read_record(dir) {
            Ok(Some(record)) => Some(record.backup.display().to_string()),
            _ => Some(format!("the backup named in {}", dir.display())),
        },
        FormatStatus::NeedsMigration | FormatStatus::Unverified => {
            let from = found.map_or_else(|| "<format>".into(), |f| f.to_string());
            Some(format!(
                "{}/{from}-to-{target}-<UTC timestamp>",
                root.display()
            ))
        }
        FormatStatus::Current | FormatStatus::Refused => None,
    };
    let (estimated_bytes, needed_bytes) = planned_bytes(dir)?;
    // #10094: an unknown free-space figure is shown as unknown, never as 0.
    let free_bytes = free_bytes_for(&root).unwrap_or_else(|e| {
        tracing::warn!(root = %root.display(), "#10094: free-space probe failed: {e}");
        None
    });
    Ok(PalacePlan {
        palace: id.to_string(),
        dir: dir.to_path_buf(),
        status,
        status_detail,
        found,
        target,
        backup_root: root,
        planned_backup,
        estimated_bytes,
        needed_bytes,
        free_bytes,
    })
}

/// [`migrate_with`] with the production seams.
pub(crate) fn migrate(dir: &Path, id: &str) -> Result<MigrateReport> {
    migrate_with(dir, id, UpgradeSeams::default())
}

/// Migrate the palace `id` at `dir` through the daemon's chain, then check it.
///
/// Why: see the module doc. The counts are taken before the chain so a step
/// that lost or duplicated a row is caught.
/// What: a refused palace fails before any write. The chain's own error is
/// returned with the backup its record names and the resume command; the
/// palace stays at its last committed step. After the chain, `problems`
/// lists each failed check (status, markers, record, counts); the caller
/// turns a non-empty list into a non-zero exit. A current palace runs the
/// gate as a no-op: no backup, `migrated: false`.
/// Test: `palace_migrate_takes_a_backup_and_reports_it`,
/// `a_current_palace_is_a_no_op_with_no_backup`,
/// `an_injected_fault_names_the_backup_and_a_rerun_resumes`,
/// `a_failed_check_is_a_non_zero_exit_naming_the_backup`.
pub(crate) fn migrate_with(dir: &Path, id: &str, seams: UpgradeSeams) -> Result<MigrateReport> {
    let (status, from, detail) = status_of(dir);
    if status == FormatStatus::Refused {
        bail!(
            "palace {id} is refused, nothing was changed: {}",
            detail.unwrap_or_default()
        );
    }
    let before = counts(dir)?;
    let backup = writer_gate(dir, id, seams.chain).map_err(|e| chain_failure(dir, id, e))?;
    (seams.after_chain)(dir);
    let verify = verify(dir, id)?;
    let after = Counts {
        drawers: verify.drawers,
        triples: verify.triples,
    };
    let mut problems = verify.gaps.clone();
    if after != before {
        problems.push(format!(
            "row counts changed: {} drawers and {} triples before, {} and {} after",
            before.drawers, before.triples, after.drawers, after.triples
        ));
    }
    Ok(MigrateReport {
        palace: id.to_string(),
        from,
        to: PALACE_FORMAT_SUPPORTED,
        migrated: backup.is_some(),
        backup,
        before,
        after,
        verify,
        problems,
    })
}

/// The drawer and triple counts of `dir`'s `kg.redb`; zero with no file.
fn counts(dir: &Path) -> Result<Counts> {
    Ok(read_kg_facts(dir)?.map_or(
        Counts {
            drawers: 0,
            triples: 0,
        },
        |f| Counts {
            drawers: f.drawers,
            triples: f.triples,
        },
    ))
}

/// The chain's error, naming the backup and how to resume.
///
/// Why: #10094 — the user's next step after a failure is a re-run (resume)
/// or a rollback, and both need the backup path.
fn chain_failure(dir: &Path, id: &str, e: anyhow::Error) -> anyhow::Error {
    let backup = match read_record(dir) {
        Ok(Some(record)) => format!("verified backup: {}", record.backup.display()),
        _ => format!(
            "no migration record was written; backups live under {}",
            backup_root_of(dir, id).display()
        ),
    };
    e.context(format!(
        "palace migrate {id}: the migration stopped; the palace stays at the format of its \
         last committed step and nothing was deleted; {backup}; re-run `trusty-memory palace \
         migrate {id}` to resume"
    ))
}

#[cfg(test)]
#[path = "format_upgrade_tests.rs"]
mod tests;
