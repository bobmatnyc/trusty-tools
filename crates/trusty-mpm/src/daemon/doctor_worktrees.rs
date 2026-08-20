//! `tm doctor`'s `worktrees` probe — orphan count and its remediation hint.
//!
//! Why: split out of `doctor.rs` to keep that file under the 500-SLOC
//! production cap when #5947 replaced the probe's own orphan heuristic with the
//! reconciled inventory's classification.
//! What: [`WORKTREE_REMEDIATION_COMMAND`], [`WorktreeOrphanCounts`],
//! [`gather_worktree_counts`], and [`check_worktrees`].
//! Test: `worktrees_no_orphans_is_ok`, `worktrees_unowned_worktree_is_not_an_orphan`,
//! `worktrees_with_orphan_is_warn`, `worktrees_without_a_reconciled_inventory_is_unknown`
//! (all in `doctor_tests.rs`), plus `doctor_worktree_remediation_command_parses`
//! in the `tm` binary's CLI suite.

use std::path::Path;

use crate::core::doctor::{CheckStatus, DoctorCheck};
use crate::session_manager::worktree_reconcile::ReconcileReport;

/// The command `tm doctor` tells an operator to run when it finds orphaned
/// worktrees (#5947).
///
/// Why: the hint used to read `tm session prune --worktrees`, and that flag has
/// never existed — `tm session prune` takes `--state` only, so the suggested
/// fix did not parse. A named constant is what lets the CLI's own clap parser
/// assert the string still resolves, so a renamed or removed flag fails a test
/// instead of silently leaving the hint pointing at nothing.
/// What: the dry-run form of the real reclaim verb. It previews and removes
/// nothing; `--force` is the operator's separate decision.
/// Test: `doctor_worktree_remediation_command_parses` (in the `tm` binary's
/// suite, against the real `Cli`), `worktrees_with_orphan_is_warn`.
pub const WORKTREE_REMEDIATION_COMMAND: &str = "tm session prune-worktrees";

/// The reconciled worktree inventory, reduced to the four numbers doctor
/// reports (#5947).
///
/// Why: [`crate::daemon::doctor::run_doctor`] is public and the reconciled
/// report is crate-internal,
/// so the check takes this instead — and taking only counts also makes the
/// no-inventory case (`None`) explicit at the call site rather than something
/// the probe has to infer.
/// What: `orphaned` is the ONLY count that drives the verdict; the other three
/// are report text, so an operator can see how many worktrees were examined and
/// why the rest were spared.
/// Test: `worktrees_unowned_worktree_is_not_an_orphan`,
/// `worktrees_with_orphan_is_warn`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WorktreeOrphanCounts {
    /// Every worktree the inventory classified.
    pub total: usize,
    /// Classified LIVE — some signal vetoes reclamation.
    pub live: usize,
    /// Classified UNKNOWN — not established as reclaimable, so not an orphan.
    pub unknown: usize,
    /// Classified ORPHANED — the count doctor warns on.
    pub orphaned: usize,
}

impl WorktreeOrphanCounts {
    /// Reduce a reconciled inventory to the counts doctor reports (#5947).
    pub(crate) fn from_reconcile(report: &ReconcileReport) -> Self {
        Self {
            total: report.counts.total,
            live: report.counts.live,
            unknown: report.counts.unknown,
            orphaned: report.counts.orphaned,
        }
    }
}

/// Gather the reconciled worktree counts for [`run_doctor`] (#5947).
///
/// Why: the inventory needs the session store and live tmux panes, which only
/// the manager has — and a failure to gather it must reach doctor as `None`
/// (reported `Unknown`) rather than as a zero that would read as healthy.
/// What: runs the same report-only reconciliation `tm session
/// reconcile-worktrees` serves, reduced to counts. Reads only.
/// Test: `worktrees_unowned_worktree_is_not_an_orphan` covers the classification
/// this feeds; `doctor_endpoint_returns_report` covers the wiring.
pub(crate) async fn gather_worktree_counts(
    mgr: &crate::session_manager::SessionManager,
    repos_root: &Path,
) -> Option<WorktreeOrphanCounts> {
    match mgr.reconcile_worktree_inventory(repos_root).await {
        Ok(report) => Some(WorktreeOrphanCounts::from_reconcile(&report)),
        Err(e) => {
            tracing::error!("doctor: worktree inventory unavailable: {e}");
            None
        }
    }
}

/// Probe for orphaned per-session git worktrees under the managed workspace root.
///
/// Why: `decommission` now calls `git worktree remove` (#1840), but sessions
/// that were decommissioned before this fix—or where `git worktree remove`
/// failed—may leave stale `.worktrees/<session-id>/` directories on disk. This
/// probe surfaces orphaned dirs so operators know to reclaim them.
///
/// #5947: it used to count orphans itself, as "every git-registered worktree
/// under a managed project that no live session lists as its workspace". That
/// is not what an orphan is, and it reported 198 of them on a fleet where
/// `tm session prune-worktrees` and `tm session reconcile-worktrees` both found
/// zero — because a worktree owned by a running agent, locked by the operator,
/// holding unsaved work, or carrying no ownership sentinel at all is not
/// reclaimable and never was. Doctor now reads the SAME classification those
/// two verbs share
/// ([`crate::session_manager::worktree_reconcile::reconcile_worktrees`]) and
/// reports its `ORPHANED` count, so a third independent answer to "is this
/// worktree an orphan" no longer exists.
/// What: reports `Ok` (no managed workspace root, or root absent), `Unknown`
/// (the reconciled inventory could not be gathered — a count was never
/// established, so neither `Ok` nor `Warn` would be honest), `Ok` (zero
/// orphans, with the LIVE/UNKNOWN split so the operator can see what was
/// examined), or `Warn` naming [`WORKTREE_REMEDIATION_COMMAND`].
///
/// Still REPORT-ONLY — it counts, it never removes.
/// Test: `worktrees_no_orphans_is_ok`,
/// `worktrees_unowned_worktree_is_not_an_orphan`, `worktrees_with_orphan_is_warn`,
/// `worktrees_without_a_reconciled_inventory_is_unknown`.
pub(super) async fn check_worktrees(
    repos_root: Option<&Path>,
    reconcile: Option<WorktreeOrphanCounts>,
) -> DoctorCheck {
    let Some(root) = repos_root else {
        // No managed workspace root configured — this is a normal state for
        // operators who don't use in-project worktree sessions (#1840).
        return DoctorCheck::new(
            "worktrees",
            CheckStatus::Ok,
            "no managed workspace root configured — worktree scan skipped",
        );
    };
    if !root.is_dir() {
        return DoctorCheck::new(
            "worktrees",
            CheckStatus::Ok,
            format!("{} does not exist — no worktrees to check", root.display()),
        );
    }
    // #5947: no inventory means no count — say so rather than reporting a
    // number nothing produced.
    let Some(counts) = reconcile else {
        return DoctorCheck::new(
            "worktrees",
            CheckStatus::Unknown,
            "worktree inventory unavailable — orphan count not established; run \
             `tm session reconcile-worktrees`",
        );
    };

    if counts.orphaned == 0 {
        DoctorCheck::new(
            "worktrees",
            CheckStatus::Ok,
            format!(
                "no orphaned worktrees found ({} worktree(s) examined: {} live, {} not \
                 reclaimable — `tm session reconcile-worktrees` explains each)",
                counts.total, counts.live, counts.unknown
            ),
        )
    } else {
        DoctorCheck::new(
            "worktrees",
            CheckStatus::Warn,
            format!(
                "{} orphaned worktree dir(s) found — run `{WORKTREE_REMEDIATION_COMMAND}` to \
                 preview, then `--force` to remove",
                counts.orphaned
            ),
        )
    }
}
