//! `tm doctor --fix-skills` — the local skill-repair action (issue #4604).
//!
//! Why: split out of `commands/misc.rs`, which the 500-SLOC production cap
//! would otherwise reject. The behaviour and its constraints live in
//! [`trusty_mpm::core::skill_repair`]; this file is CLI plumbing and printing.
//! `tm doctor --fix` (#4948) drives the same functions in dry-run mode, so the
//! path resolution and the per-outcome wording live here once rather than
//! being written twice and drifting.
//! What: [`skill_repair_outcomes`] — resolve the deploy tiers and the skill
//! reference and run the repair in a given mode; [`describe`] — one line per
//! outcome; [`fix_skills_locally`] — the `--fix-skills` handler.
//! Test: `core::skill_repair`'s own tests cover every outcome.

use trusty_mpm::core::doctor_repair::RepairMode;
use trusty_mpm::core::skill_repair::{RepairAction, RepairOutcome};

/// Run the skill repair against this machine, in `mode`.
///
/// Why (#4604): the reference point must be the RUNNING BINARY's embedded
/// assets — never the `~/.trusty-mpm/framework/skills` extraction cache, which
/// is what lagged the installed binary and made every skill it covered report
/// clean regardless of what shipped.
/// What: resolves the deploy tiers from `FrameworkPaths::default` and the
/// current directory, builds the reference (preferring a checked-out
/// `agents/skills` submodule when this is a source checkout), and returns the
/// reference's origin label alongside the outcomes. Local-filesystem only —
/// independent of where the daemon that produced the report happens to be
/// reachable. In [`RepairMode::DryRun`] nothing is written.
/// Test: `core::skill_repair`'s own tests; this function is path resolution.
pub(crate) fn skill_repair_outcomes(
    include_frozen: bool,
    mode: RepairMode,
) -> (String, Vec<RepairOutcome>) {
    use trusty_mpm::core::paths::FrameworkPaths;
    use trusty_mpm::core::skill_drift::skill_reference;
    use trusty_mpm::core::skill_repair::{backup_root_for, repair_skills_in_mode};

    let paths = FrameworkPaths::default();
    let project_dir = std::env::current_dir().ok();
    let source = paths.skill_source_dir();
    let submodule = (source != paths.skills).then_some(source);
    let reference = skill_reference(submodule.as_deref());

    let home = dirs::home_dir().unwrap_or_else(|| std::path::PathBuf::from("."));
    let backup_root = backup_root_for(&home, chrono::Utc::now());

    let outcomes = repair_skills_in_mode(
        &reference,
        &paths,
        project_dir.as_deref(),
        include_frozen,
        &backup_root,
        mode,
    );
    (reference.origin, outcomes)
}

/// One human line for one repair outcome.
///
/// Why: `--fix-skills` and `--fix` must describe the same outcome the same
/// way; two renderers would let the preview and the applied run disagree about
/// what happened.
/// What: a phrase per [`RepairAction`] variant. A frozen skip names the opt-in
/// that would overwrite it, and says the overwrite is backed up first.
/// Test: `core::skill_repair`'s own tests produce every variant.
pub(crate) fn describe(action: &RepairAction) -> String {
    match action {
        RepairAction::Repaired { backup: Some(p) } => {
            format!("repaired and verified from disk (backup: {})", p.display())
        }
        RepairAction::Repaired { backup: None } => {
            "written and verified from disk (was absent)".to_string()
        }
        RepairAction::WouldRepair { existed: true } => {
            "would be overwritten from the bundled asset (backed up first)".to_string()
        }
        RepairAction::WouldRepair { existed: false } => {
            "would be written (currently absent)".to_string()
        }
        RepairAction::SkippedFrozen => "FROZEN — hand-edited after deployment; left untouched \
             (pass `--include-frozen` to overwrite it, backing it up first)"
            .to_string(),
        RepairAction::SkippedUnverifiable(why) => format!("skipped — {why}"),
        RepairAction::Failed(why) => format!("FAILED — {why}"),
    }
}

/// `--fix-skills` action — redeploy drifted skills and VERIFY from disk.
///
/// Why (#4604): the `skill_staleness` check reports which deployed skills no
/// longer match this binary's bundled assets, but until now the only remedy was
/// `tm install`, which deliberately SKIPS a hand-edited (frozen) file — so a
/// frozen skill could stay stale forever with no way to repair it short of
/// hand-copying. This is that remedy, with the owner's three constraints
/// enforced in [`trusty_mpm::core::skill_repair`]: a frozen skill is never
/// silently overwritten, every overwrite is backed up first, and the fix
/// re-reads each file from disk rather than reporting success from its own
/// intent.
///
/// It never deletes anything and never touches worktrees — `tm doctor`'s
/// worktree checks are report-only and stay that way. To see what it would do
/// before it does it, use `tm doctor --fix`, which previews this repair
/// alongside the others.
/// What: runs [`skill_repair_outcomes`] in [`RepairMode::Apply`] and prints one
/// path-tagged line per outcome plus a summary.
/// Test: `core::skill_repair`'s own tests cover every outcome; this function is
/// printing.
pub(crate) fn fix_skills_locally(include_frozen: bool) {
    let (origin, outcomes) = skill_repair_outcomes(include_frozen, RepairMode::Apply);

    println!("\nfix-skills (compared against {origin})");
    if outcomes.is_empty() {
        println!("  nothing to repair — every deployed skill already matches");
        return;
    }

    let mut repaired = 0usize;
    let mut failed = 0usize;
    for outcome in &outcomes {
        match &outcome.action {
            RepairAction::Repaired { .. } => repaired += 1,
            RepairAction::Failed(_) => failed += 1,
            _ => {}
        }
        println!(
            "  {}/{} [{}]: {}",
            outcome.tier,
            outcome.stem,
            outcome.path.display(),
            describe(&outcome.action)
        );
    }
    println!(
        "  {repaired} repaired, {} skipped, {failed} failed",
        outcomes.len() - repaired - failed
    );
}
