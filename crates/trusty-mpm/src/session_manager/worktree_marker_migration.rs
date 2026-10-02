//! The one-shot marker migration over every registered project (#8511).
//!
//! Why: readers never move a marker (a read-only or dry-run path must not
//! write), so a legacy in-tree marker stays until an explicit pass moves it.
//! This pass visits every tree of every registered project, and adds the
//! shared `info/exclude` entries while it is there.
//! What: [`registered_checkouts`] — the checkouts the sweeps already
//! interrogate (the repos-root walk plus adopted anchors);
//! [`prepare_checkout`] — one checkout: exclude entries, then every tree's
//! migration; [`migrate_registered_projects`] — all of them, run once at daemon
//! start; [`repair_worktree_markers`] — the same work as `tm doctor --fix`
//! steps, previewed unless applied.
//! Test: `the_fleet_pass_migrates_every_tree_and_excludes_once`,
//! `the_doctor_repair_previews_then_applies`.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use super::worktree_ownership_location::{
    MarkerMigration, legacy_sentinel_path, migrate_legacy_sentinel, preview_migration,
};
use super::worktree_registry::{list_registered_worktrees, scan_registered_worktrees};
use crate::core::doctor_repair::{RepairMode, RepairStep, StepStatus};

/// The doctor-repair check name these steps report under.
const CHECK: &str = "worktree_markers";

/// Every checkout whose `git worktree list` the sweeps consult (#8511).
///
/// Why: "all registered projects" must mean the same set the reclaim and
/// reconcile paths already walk, not a second enumeration that can drift.
/// What: the distinct `registry_root` of every [`scan_registered_worktrees`]
/// row, sorted.
pub(crate) fn registered_checkouts(repos_root: &Path, adopted: &[PathBuf]) -> Vec<PathBuf> {
    let roots: BTreeSet<PathBuf> = scan_registered_worktrees(repos_root, adopted)
        .into_iter()
        .map(|s| s.registry_root)
        .collect();
    roots.into_iter().collect()
}

/// Every tree `checkout`'s registry lists that has a directory on disk.
///
/// A listing failure is an `Err`, never an empty list: the startup pass logs
/// it and the doctor reports it as a failed step (round 2 finding 4).
fn trees_of(checkout: &Path) -> Result<Vec<PathBuf>, String> {
    let listed = list_registered_worktrees(checkout)
        .ok_or_else(|| "could not list this checkout's worktrees".to_string())?;
    Ok(listed
        .into_iter()
        .filter(|w| !w.bare && !w.prunable)
        .map(|w| w.path)
        .collect())
}

/// What one checkout's pass did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct CheckoutTally {
    /// Legacy markers moved to the admin dir (including removed duplicates).
    pub(crate) migrated: usize,
    /// Legacy markers kept because a step failed or the two copies conflict.
    pub(crate) kept: usize,
}

/// Exclude entries, then migrate every tree of `checkout` (#8511).
///
/// Why: the registration paths call this so a newly registered project is
/// clean from its first `git status`.
/// What: [`crate::core::harness_exclude::ensure_and_log`], then
/// [`migrate_legacy_sentinel`] per tree. Best-effort: logs, never fails.
/// Test: `the_fleet_pass_migrates_every_tree_and_excludes_once`.
pub(crate) fn prepare_checkout(checkout: &Path) -> CheckoutTally {
    crate::core::harness_exclude::ensure_and_log(checkout);
    let mut tally = CheckoutTally::default();
    let trees = trees_of(checkout).unwrap_or_else(|reason| {
        tracing::warn!(
            checkout = %checkout.display(),
            "ownership markers: {reason}; skipped (#8511)"
        );
        Vec::new()
    });
    for tree in trees {
        match migrate_legacy_sentinel(&tree) {
            MarkerMigration::Migrated | MarkerMigration::DuplicateRemoved => tally.migrated += 1,
            MarkerMigration::Conflict | MarkerMigration::Tracked | MarkerMigration::Failed(_) => {
                tally.kept += 1
            }
            MarkerMigration::NoLegacy | MarkerMigration::NoAdminDir => {}
        }
    }
    if tally != CheckoutTally::default() {
        tracing::info!(
            checkout = %checkout.display(),
            migrated = tally.migrated,
            kept = tally.kept,
            "ownership markers moved into the git admin dir (#8511)"
        );
    }
    tally
}

/// [`prepare_checkout`] over every [`registered_checkouts`] entry — the
/// daemon's one-shot startup pass (#8511).
/// Test: `the_fleet_pass_migrates_every_tree_and_excludes_once`.
pub(crate) fn migrate_registered_projects(repos_root: &Path, adopted: &[PathBuf]) -> CheckoutTally {
    let mut total = CheckoutTally::default();
    for checkout in registered_checkouts(repos_root, adopted) {
        let t = prepare_checkout(&checkout);
        total.migrated += t.migrated;
        total.kept += t.kept;
    }
    total
}

/// The `tm doctor --fix` steps for every registered project (#8511).
///
/// Why: the operator-facing form of the one-shot migration, previewed by
/// default like every other doctor repair.
/// What: per checkout, one step for its pending exclude entries, and one
/// failed step when its worktrees cannot be listed; per tree, one step for a
/// legacy marker the migration would act on. A dry run reports
/// [`preview_migration`]'s answer, apply runs [`migrate_legacy_sentinel`] —
/// the same decision table, so both modes show the same refusals and failures
/// (round 2 finding 3). A committed marker (#8368) and a tree with nothing to
/// move produce no step, so a clean fleet produces none.
/// Test: `the_doctor_repair_previews_then_applies`,
/// `the_doctor_dry_run_reports_what_apply_will_do`,
/// `a_listing_failure_is_a_failed_doctor_step`.
pub fn repair_worktree_markers(
    repos_root: &Path,
    adopted: &[PathBuf],
    mode: RepairMode,
) -> Vec<RepairStep> {
    registered_checkouts(repos_root, adopted)
        .iter()
        .flat_map(|checkout| checkout_steps(checkout, mode))
        .collect()
}

/// One checkout's doctor steps; see [`repair_worktree_markers`].
fn checkout_steps(checkout: &Path, mode: RepairMode) -> Vec<RepairStep> {
    let mut steps: Vec<RepairStep> = exclude_step(checkout, mode).into_iter().collect();
    let trees = match trees_of(checkout) {
        Ok(trees) => trees,
        Err(reason) => {
            steps.push(RepairStep {
                check: CHECK,
                path: checkout.to_path_buf(),
                what: "move the ownership markers into the git admin dir".to_string(),
                status: StepStatus::Failed(reason),
            });
            return steps;
        }
    };
    for tree in trees {
        let outcome = match mode {
            RepairMode::DryRun => preview_migration(&tree),
            RepairMode::Apply => migrate_legacy_sentinel(&tree),
        };
        let status = match outcome {
            MarkerMigration::Migrated | MarkerMigration::DuplicateRemoved => match mode {
                RepairMode::DryRun => StepStatus::Planned,
                RepairMode::Apply => StepStatus::Applied { backup: None },
            },
            MarkerMigration::Conflict => StepStatus::Refused(
                "the admin-dir marker differs; it wins and the in-tree one is kept".to_string(),
            ),
            MarkerMigration::Failed(reason) => StepStatus::Failed(reason),
            // A committed marker (#8368) is never moved; see the location module.
            MarkerMigration::Tracked | MarkerMigration::NoLegacy | MarkerMigration::NoAdminDir => {
                continue;
            }
        };
        steps.push(RepairStep {
            check: CHECK,
            path: legacy_sentinel_path(&tree),
            what: "move the ownership marker into the git admin dir".to_string(),
            status,
        });
    }
    steps
}

/// One checkout's exclude step, or nothing when no entry is pending.
fn exclude_step(checkout: &Path, mode: RepairMode) -> Option<RepairStep> {
    let (path, pending) = match crate::core::harness_exclude::pending_excludes(checkout) {
        Ok(found) => found,
        Err(e) => {
            return Some(RepairStep {
                check: CHECK,
                path: checkout.to_path_buf(),
                what: "exclude the harness files from git status".to_string(),
                status: StepStatus::Failed(e),
            });
        }
    };
    if pending.is_empty() {
        return None;
    }
    let what = format!("add {} to the shared info/exclude", pending.join(", "));
    let status = match mode {
        RepairMode::DryRun => StepStatus::Planned,
        RepairMode::Apply => {
            match crate::core::harness_exclude::ensure_harness_files_excluded(checkout) {
                Ok(_) => StepStatus::Applied { backup: None },
                Err(e) => StepStatus::Failed(e),
            }
        }
    };
    Some(RepairStep {
        check: CHECK,
        path,
        what,
        status,
    })
}

#[cfg(test)]
#[path = "worktree_marker_migration_tests.rs"]
mod worktree_marker_migration_tests;
