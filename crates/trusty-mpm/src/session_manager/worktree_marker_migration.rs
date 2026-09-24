//! The one-shot marker migration over every registered project (#8511).
//!
//! Why: readers migrate a legacy in-tree marker the first time they see it,
//! but a tree nothing reads stays dirty. This pass visits every tree of every
//! registered project once, and adds the shared `info/exclude` entries while
//! it is there.
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
    MarkerMigration, admin_sentinel_path, legacy_is_tracked, legacy_sentinel_path,
    migrate_legacy_sentinel,
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
fn trees_of(checkout: &Path) -> Vec<PathBuf> {
    list_registered_worktrees(checkout)
        .unwrap_or_default()
        .into_iter()
        .filter(|w| !w.bare && !w.prunable)
        .map(|w| w.path)
        .collect()
}

/// What one checkout's pass did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct CheckoutTally {
    /// Legacy markers moved to the admin dir (including removed duplicates).
    pub migrated: usize,
    /// Legacy markers kept because a step failed or the two copies conflict.
    pub kept: usize,
}

/// Exclude entries, then migrate every tree of `checkout` (#8511).
///
/// Why: the registration paths call this so a newly registered project is
/// clean from its first `git status`.
/// What: [`crate::core::harness_exclude::ensure_and_log`], then
/// [`migrate_legacy_sentinel`] per tree. Best-effort: logs, never fails.
/// Test: `the_fleet_pass_migrates_every_tree_and_excludes_once`.
pub fn prepare_checkout(checkout: &Path) -> CheckoutTally {
    crate::core::harness_exclude::ensure_and_log(checkout);
    let mut tally = CheckoutTally::default();
    for tree in trees_of(checkout) {
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
pub fn migrate_registered_projects(repos_root: &Path, adopted: &[PathBuf]) -> CheckoutTally {
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
/// What: per checkout, one step for its pending exclude entries; per tree, one
/// step for a legacy marker that can move (the tree has an admin dir and git
/// does not track the marker). Dry run
/// plans; apply runs [`crate::core::harness_exclude::ensure_harness_files_excluded`]
/// and [`migrate_legacy_sentinel`] and reports what each actually did. A clean
/// fleet produces no steps.
/// Test: `the_doctor_repair_previews_then_applies`.
pub fn repair_worktree_markers(
    repos_root: &Path,
    adopted: &[PathBuf],
    mode: RepairMode,
) -> Vec<RepairStep> {
    let mut steps = Vec::new();
    for checkout in registered_checkouts(repos_root, adopted) {
        steps.extend(exclude_step(&checkout, mode));
        for tree in trees_of(&checkout) {
            let legacy = legacy_sentinel_path(&tree);
            // A committed marker (#8368) is never moved; see the location module.
            if !legacy.exists() || admin_sentinel_path(&tree).is_none() || legacy_is_tracked(&tree)
            {
                continue;
            }
            let status = match mode {
                RepairMode::DryRun => StepStatus::Planned,
                RepairMode::Apply => match migrate_legacy_sentinel(&tree) {
                    MarkerMigration::Migrated | MarkerMigration::DuplicateRemoved => {
                        StepStatus::Applied { backup: None }
                    }
                    MarkerMigration::Conflict => StepStatus::Refused(
                        "the admin-dir marker differs; it wins and the in-tree one is kept"
                            .to_string(),
                    ),
                    MarkerMigration::Tracked => {
                        StepStatus::Refused("git tracks this marker; it stays".to_string())
                    }
                    MarkerMigration::Failed(reason) => StepStatus::Failed(reason),
                    MarkerMigration::NoLegacy | MarkerMigration::NoAdminDir => continue,
                },
            };
            steps.push(RepairStep {
                check: CHECK,
                path: legacy,
                what: "move the ownership marker into the git admin dir".to_string(),
                status,
            });
        }
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
