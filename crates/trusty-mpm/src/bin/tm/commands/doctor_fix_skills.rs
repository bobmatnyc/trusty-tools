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
//! outcome; [`fix_skills_locally`] — the `--fix-skills` handler, which also runs
//! the #6586 project-tier stray sweep.
//! Test: `core::skill_repair`'s and `core::project_tier_strays`' own tests cover
//! every outcome.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use trusty_mpm::core::doctor_repair::{RepairMode, RepairStep};
use trusty_mpm::core::project_tier_strays::stems_being_removed;
use trusty_mpm::core::skill_repair::{RepairAction, RepairOutcome};

/// The timestamped root this run backs every file up under.
///
/// Why: both halves of `--fix-skills` back files up, and computing the root
/// twice put the redeploy's copies and the sweep's copies in two directories
/// whose names differ by however long the redeploy took. One run, one recovery
/// point.
/// What: `backup_root_for(<home>, now)`; `.` when the home directory cannot be
/// resolved, matching every other path fallback in this file.
/// Test: `core::skill_repair`'s `backup_root_for` tests cover the naming.
pub(crate) fn default_backup_root() -> PathBuf {
    use trusty_mpm::core::skill_repair::backup_root_for;

    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    backup_root_for(&home, chrono::Utc::now())
}

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
    backup_root: &Path,
    deferred_project_stems: &BTreeSet<String>,
) -> (String, Vec<RepairOutcome>) {
    use trusty_mpm::core::paths::FrameworkPaths;
    use trusty_mpm::core::skill_drift::skill_reference;
    use trusty_mpm::core::skill_repair::repair_skills_in_mode_deferring;

    let paths = FrameworkPaths::default();
    let project_dir = std::env::current_dir().ok();
    let source = paths.skill_source_dir();
    let submodule = (source != paths.skills).then_some(source);
    let reference = skill_reference(submodule.as_deref());

    let outcomes = repair_skills_in_mode_deferring(
        &reference,
        &paths,
        project_dir.as_deref(),
        include_frozen,
        backup_root,
        mode,
        deferred_project_stems,
    );
    (reference.origin, outcomes)
}

/// Sweep this machine's project tier of stranded bundled skill copies (#6586).
///
/// Why: the `skill_project_tier` probe reports a stray and cannot remove one, so
/// without this the finding is permanent. The evidence rule that licenses the
/// deletion — and the refusals that protect the operator's own work — live in
/// [`trusty_mpm::core::project_tier_strays`]; this function is path resolution.
/// What: resolves the tiers from `FrameworkPaths::default` and the current
/// directory and runs the sweep in `mode`. In [`RepairMode::DryRun`] nothing is
/// written.
/// Test: `core::project_tier_strays`' own tests cover every outcome.
pub(crate) fn project_tier_sweep(backup_root: &Path, mode: RepairMode) -> Vec<RepairStep> {
    use trusty_mpm::core::paths::FrameworkPaths;
    use trusty_mpm::core::project_tier_strays::remove_project_tier_strays;

    let paths = FrameworkPaths::default();
    let project_dir = std::env::current_dir().ok();

    remove_project_tier_strays(&paths, project_dir.as_deref(), backup_root, mode)
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
/// The REDEPLOY half never deletes anything and never touches worktrees —
/// `tm doctor`'s worktree checks are report-only and stay that way. To see what
/// it would do before it does it, use `tm doctor --fix`, which previews this
/// repair alongside the others.
///
/// #6586 adds one narrow deletion, and only here: the project-tier stray sweep,
/// which removes a bundled skill copy stranded under `<project>/.claude/skills`
/// when — and only when — that tier's own deploy ledger proves tm wrote every
/// file under it and nobody has changed one. Because it DELETES it follows this
/// crate's rule for a write: a bare `--fix-skills` previews it and `--yes`
/// applies it. Every other copy is refused, the removal is backed up whole
/// first, and `tm doctor --fix` deliberately does NOT run it, so "`--fix` never
/// deletes" still holds.
/// The two halves stay gated differently, and the reading is deliberate. The
/// redeploy overwrites tm's own files with tm's own bytes, backs each one up,
/// and already has a preview command in `tm doctor --fix` — so it keeps
/// applying on the flag alone. The sweep removes a directory, which re-running
/// the command cannot undo, so it alone waits for `--yes`.
/// What: runs [`project_tier_sweep`] FIRST — sweeping after the redeploy would
/// rewrite, back up, and then delete the same project-tier copies inside one
/// command — then [`skill_repair_outcomes`] in [`RepairMode::Apply`], DEFERRING
/// the stems the sweep planned or applied so the redeploy cannot write back
/// what the sweep is taking out. Prints the sweep's steps in the shared `--fix`
/// format and one path-tagged line per redeploy outcome. Both halves back up
/// under one [`default_backup_root`].
/// Test: `core::skill_repair`'s and `core::project_tier_strays`' own tests cover
/// every outcome; `a_bare_fix_skills_previews_the_sweep` pins the gate and
/// `a_bare_fix_skills_leaves_a_planned_stray_alone` pins the deferral.
pub(crate) fn fix_skills_locally(include_frozen: bool, yes: bool) {
    let backup_root = default_backup_root();

    // #6586: the action half of the `skill_project_tier` check. It runs BEFORE
    // the redeploy so the redeploy does not first rewrite the very copies this
    // is about to remove.
    let strays = project_tier_sweep(&backup_root, sweep_mode(yes));
    if !strays.is_empty() {
        println!("\nfix-skills: stray bundled copies at the project tier (#6586)");
        super::doctor_repair::print_steps(&strays, yes, FIX_SKILLS_APPLY_HINT);
    }

    // #6586 critic HIGH: the redeploy is an APPLY whatever `--yes` says, so
    // without this it rewrote every stem the dry-run sweep had just called
    // removable — 51 files and 51 backups, written straight after the sweep
    // printed that nothing would be written.
    let deferred = stems_being_removed(&strays);
    let (origin, outcomes) =
        skill_repair_outcomes(include_frozen, RepairMode::Apply, &backup_root, &deferred);

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

/// The mode the #6586 sweep runs in for a given `--yes`.
///
/// Why (#6586 critic HIGH): the sweep is the only thing in `--fix-skills` that
/// DELETES, and this crate's rule is that a write is previewed by default. It
/// shipped applying on the bare flag. Naming the mapping makes the gate
/// assertable without capturing stdout.
/// What: [`RepairMode::Apply`] only when `yes` is set.
/// Test: `a_bare_fix_skills_previews_the_sweep`.
fn sweep_mode(yes: bool) -> RepairMode {
    RepairMode::from_apply_flag(yes)
}

/// The command that applies a `--fix-skills` sweep preview.
///
/// Why: the shared step printer names the command that applies THIS run; for the
/// sweep that is `--fix-skills --yes`, never `--fix --yes`, which does not run
/// it at all.
/// Test: `fix_skills_hint_names_the_fix_skills_command`.
const FIX_SKILLS_APPLY_HINT: &str = "tm doctor --fix-skills --yes";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fix_skills_hint_names_the_fix_skills_command() {
        assert_eq!(FIX_SKILLS_APPLY_HINT, "tm doctor --fix-skills --yes");
        assert!(
            !FIX_SKILLS_APPLY_HINT.contains("--fix "),
            "`tm doctor --fix` does not run the #6586 sweep, so it must never be the hint"
        );
    }

    /// #6586 critic HIGH: the sweep DELETES, so the bare flag must only
    /// preview.
    ///
    /// Fails before this fix: `fix_skills_locally` passed `RepairMode::Apply`
    /// unconditionally, so `tm doctor --fix-skills` removed 51 project-tier
    /// directories with no confirming flag.
    #[test]
    fn a_bare_fix_skills_previews_the_sweep() {
        assert_eq!(sweep_mode(false), RepairMode::DryRun);
        assert_eq!(sweep_mode(true), RepairMode::Apply);
    }
}
