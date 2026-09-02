//! `tm doctor --fix-skills` — the local skill-repair action (issue #4604).
//!
//! Why: split out of `commands/misc.rs`, which the 500-SLOC production cap
//! would otherwise reject. The behaviour and its constraints live in
//! [`trusty_mpm::core::skill_repair`]; this file is CLI plumbing and printing.
//! `tm doctor --fix` (#4948) drives the same functions in dry-run mode, so the
//! path resolution and the per-outcome wording live here once rather than
//! being written twice and drifting.
//! What: [`resolve_inputs`] — resolve the deploy tiers and the skill reference
//! once; [`skill_repair_outcomes`] — run the redeploy in a given mode;
//! [`fix_skills_halves`] — one whole `--fix-skills` run against resolved
//! inputs; [`describe`] — one line per outcome; [`fix_skills_locally`] — the
//! `--fix-skills` handler, which runs the redeploy and the #6586 project-tier
//! stray sweep in ONE mode.
//! Test: `core::skill_repair`'s and `core::project_tier_strays`' own tests cover
//! every outcome; the `tests` module below pins the gate.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use trusty_mpm::core::doctor_repair::{RepairMode, RepairStep};
use trusty_mpm::core::paths::FrameworkPaths;
use trusty_mpm::core::project_tier_strays::{remove_project_tier_strays, stems_being_removed};
use trusty_mpm::core::skill_drift::SkillReference;
use trusty_mpm::core::skill_repair::{
    RepairAction, RepairOutcome, repair_skills_in_mode_deferring,
};

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

/// Everything a `--fix-skills` run acts on, resolved from this machine once.
///
/// Why (#6620): the redeploy and the sweep must see the SAME tiers and the same
/// project, and the handler prints the reference's origin. Resolving them twice
/// let two halves of one command disagree about what they were acting on.
/// What: the deploy tiers, the current directory, and the skill reference.
/// Test: `fix_skills_halves`' tests drive the same shape with fixture values.
struct FixSkillsInputs {
    paths: FrameworkPaths,
    project_dir: Option<PathBuf>,
    reference: SkillReference,
}

/// Resolve [`FixSkillsInputs`] from this machine.
///
/// Why (#4604): the reference point must be the RUNNING BINARY's embedded
/// assets — never the `~/.trusty-mpm/framework/skills` extraction cache, which
/// is what lagged the installed binary and made every skill it covered report
/// clean regardless of what shipped.
/// What: `FrameworkPaths::default`, the current directory, and the skill
/// reference (preferring a checked-out `agents/skills` submodule when this is a
/// source checkout). Local-filesystem only — independent of where the daemon
/// that produced the report happens to be reachable.
/// Test: path resolution; the behaviour tests drive the resolved values.
fn resolve_inputs() -> FixSkillsInputs {
    use trusty_mpm::core::skill_drift::skill_reference;

    let paths = FrameworkPaths::default();
    let source = paths.skill_source_dir();
    let submodule = (source != paths.skills).then_some(source);
    FixSkillsInputs {
        reference: skill_reference(submodule.as_deref()),
        project_dir: std::env::current_dir().ok(),
        paths,
    }
}

/// Run the skill repair against this machine, in `mode`.
///
/// Why: `tm doctor --fix` projects the redeploy onto its own step list and needs
/// the outcomes WITHOUT the sweep, so this stays the narrow entry point for that
/// caller.
/// What: [`resolve_inputs`], then the redeploy, returning the reference's origin
/// label alongside the outcomes. In [`RepairMode::DryRun`] nothing is written.
/// Test: `core::skill_repair`'s own tests; this function is path resolution.
pub(crate) fn skill_repair_outcomes(
    include_frozen: bool,
    mode: RepairMode,
    backup_root: &Path,
    deferred_project_stems: &BTreeSet<String>,
) -> (String, Vec<RepairOutcome>) {
    let inputs = resolve_inputs();
    let outcomes = repair_skills_in_mode_deferring(
        &inputs.reference,
        &inputs.paths,
        inputs.project_dir.as_deref(),
        include_frozen,
        backup_root,
        mode,
        deferred_project_stems,
    );
    (inputs.reference.origin, outcomes)
}

/// Both halves of ONE `--fix-skills` run, in one mode, against `inputs`.
///
/// Why (#6620): the halves used to be gated separately — the sweep on `--yes`,
/// the redeploy on the flag alone — so a bare `--fix-skills` printed "dry run —
/// re-run with `tm doctor --fix-skills --yes` to apply" for the sweep and then
/// rewrote files and created a backup root for the redeploy in the same
/// invocation. Taking ONE mode is what makes that unrepresentable. Splitting the
/// composition out of the handler is also what lets a test drive it against
/// fixture directories instead of the operator's real home.
/// What: the sweep FIRST — sweeping after the redeploy would rewrite, back up,
/// and then delete the same project-tier copies inside one command — then the
/// redeploy, DEFERRING the stems the sweep planned or applied so the redeploy
/// cannot write back what the sweep is taking out.
/// Test: `a_bare_fix_skills_writes_nothing`,
/// `fix_skills_with_yes_applies_both_halves`.
fn fix_skills_halves(
    inputs: &FixSkillsInputs,
    include_frozen: bool,
    mode: RepairMode,
    backup_root: &Path,
) -> (Vec<RepairStep>, Vec<RepairOutcome>) {
    let strays = remove_project_tier_strays(
        &inputs.paths,
        inputs.project_dir.as_deref(),
        backup_root,
        mode,
    );
    // #6586 critic HIGH: refreshing a copy the sweep is removing is work in the
    // opposite direction whichever mode the run is in.
    let deferred = stems_being_removed(&strays);
    let outcomes = repair_skills_in_mode_deferring(
        &inputs.reference,
        &inputs.paths,
        inputs.project_dir.as_deref(),
        include_frozen,
        backup_root,
        // #6620: was `RepairMode::Apply` regardless of the flag, so a bare
        // `--fix-skills` wrote files while the sweep printed "dry run".
        mode,
        &deferred,
    );
    (strays, outcomes)
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
/// `tm doctor`'s worktree checks are report-only and stay that way. #6586 adds
/// one narrow deletion, and only here: the project-tier stray sweep, which
/// removes a bundled skill copy stranded under `<project>/.claude/skills` when —
/// and only when — that tier's own deploy ledger proves tm wrote every file
/// under it and nobody has changed one. Every other copy is refused, the removal
/// is backed up whole first, and `tm doctor --fix` deliberately does NOT run it,
/// so "`--fix` never deletes" still holds.
///
/// #6620: BOTH halves run in the one mode the flag selects. A bare
/// `--fix-skills` is a pure preview — no file, no backup root — and
/// `--fix-skills --yes` applies both. `tm doctor --fix` still previews the
/// redeploy alone and `--fix --yes` still applies it; neither ever runs the
/// sweep, and `--fix` carries five repairs `--fix-skills` does not, so the two
/// commands stay distinct in both directions with or without `--yes`.
/// What: [`fix_skills_halves`] in [`fix_skills_mode`], printing the sweep's
/// steps in the shared `--fix` format and one path-tagged line per redeploy
/// outcome. Both halves back up under one [`default_backup_root`].
/// Test: `core::skill_repair`'s and `core::project_tier_strays`' own tests cover
/// every outcome; `a_bare_fix_skills_writes_nothing` and
/// `fix_skills_with_yes_applies_both_halves` pin the gate.
pub(crate) fn fix_skills_locally(include_frozen: bool, yes: bool) {
    let backup_root = default_backup_root();
    let mode = fix_skills_mode(yes);
    let inputs = resolve_inputs();
    let origin = inputs.reference.origin.clone();

    let (strays, outcomes) = fix_skills_halves(&inputs, include_frozen, mode, &backup_root);
    if !strays.is_empty() {
        println!("\nfix-skills: stray bundled copies at the project tier (#6586)");
        super::doctor_repair::print_steps(&strays, yes, FIX_SKILLS_APPLY_HINT);
    }

    println!(
        "\nfix-skills: redeploy ({}, compared against {origin})",
        run_label(yes)
    );
    print_outcomes(&outcomes, yes);
}

/// `applying` or `dry run — nothing will be written`, matching `--fix`.
fn run_label(yes: bool) -> &'static str {
    if yes {
        "applying"
    } else {
        "dry run — nothing will be written"
    }
}

/// Print the redeploy half's outcomes and its tallies.
///
/// Why (#6620): the old summary counted only `repaired`, `skipped` and `failed`,
/// so a preview would have reported every planned rewrite as "skipped" — and the
/// only "dry run" line the command printed belonged to the sweep. A preview has
/// to say what it WOULD write and name the flag that writes it.
/// What: one path-tagged line per outcome, the four tallies, and the apply hint
/// when anything is merely planned.
/// Test: `a_bare_fix_skills_writes_nothing` covers the values behind it.
fn print_outcomes(outcomes: &[RepairOutcome], yes: bool) {
    if outcomes.is_empty() {
        println!("  nothing to repair — every deployed skill already matches");
        return;
    }

    let mut repaired = 0usize;
    let mut planned = 0usize;
    let mut failed = 0usize;
    for outcome in outcomes {
        match &outcome.action {
            RepairAction::Repaired { .. } => repaired += 1,
            RepairAction::WouldRepair { .. } => planned += 1,
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
        "  {repaired} repaired, {planned} planned, {} skipped, {failed} failed",
        outcomes.len() - repaired - planned - failed
    );
    if !yes && planned > 0 {
        println!("  dry run — re-run with `{FIX_SKILLS_APPLY_HINT}` to apply.");
    }
}

/// The mode BOTH `--fix-skills` halves run in for a given `--yes`.
///
/// Why (#6620): this crate's rule is that a write is previewed by default, and
/// one command answering that rule two different ways is what made the printed
/// "dry run" untrue. Naming the mapping makes the gate assertable without
/// capturing stdout.
/// What: [`RepairMode::Apply`] only when `yes` is set.
/// Test: `a_bare_fix_skills_previews_both_halves`.
fn fix_skills_mode(yes: bool) -> RepairMode {
    RepairMode::from_apply_flag(yes)
}

/// The command that applies a `--fix-skills` preview.
///
/// Why: the shared step printer names the command that applies THIS run; for the
/// sweep that is `--fix-skills --yes`, never `--fix --yes`, which does not run
/// it at all.
/// Test: `fix_skills_hint_names_the_fix_skills_command`.
const FIX_SKILLS_APPLY_HINT: &str = "tm doctor --fix-skills --yes";

#[cfg(test)]
mod tests {
    use super::*;
    use trusty_mpm::core::doctor_repair::StepStatus;
    use trusty_mpm::core::skill_deploy_tiers::project_skill_tier;
    use trusty_mpm::core::skill_deployer::deploy_skills;
    use trusty_mpm::core::skill_drift::deployed_path;

    #[test]
    fn fix_skills_hint_names_the_fix_skills_command() {
        assert_eq!(FIX_SKILLS_APPLY_HINT, "tm doctor --fix-skills --yes");
        assert!(
            !FIX_SKILLS_APPLY_HINT.contains("--fix "),
            "`tm doctor --fix` does not run the #6586 sweep, so it must never be the hint"
        );
    }

    /// #6586 critic HIGH / #6620: the bare flag must only PREVIEW, and both
    /// halves must answer to the same gate.
    #[test]
    fn a_bare_fix_skills_previews_both_halves() {
        assert_eq!(fix_skills_mode(false), RepairMode::DryRun);
        assert_eq!(fix_skills_mode(true), RepairMode::Apply);
    }

    /// A bundled roster, a project tier holding a recorded stray, and a drifted
    /// repairable copy at the OPERATOR HOME tier — the exact shape #6620 was
    /// reported against. Returns the inputs plus those two paths.
    fn fixture(base: &Path) -> (FixSkillsInputs, PathBuf, PathBuf) {
        let mut paths = FrameworkPaths::under(base);
        // Never consult a real `agents/skills` submodule — the bundled roster
        // this fixture classifies against is its own.
        paths.trusty_mpm_root = None;

        let source = paths.skill_source_dir();
        std::fs::create_dir_all(&source).expect("bundled source dir");
        for stem in ["tm-ticketing", "tm-workflow"] {
            std::fs::write(source.join(format!("{stem}.md")), "# bundled\n")
                .expect("write bundled skill");
        }

        // The stray: deployed into the project tier through the REAL deployer,
        // so the tier ledger records it the way the sweep's evidence rule needs.
        let project = base.join("project");
        let stray_src = base.join("stray-src");
        std::fs::create_dir_all(&stray_src).expect("stray source dir");
        std::fs::write(stray_src.join("tm-ticketing.md"), "# stray\n").expect("write stray source");
        deploy_skills(&stray_src, &project_skill_tier(&project)).expect("deploy the stray");

        // The drifted repairable copy at the operator home tier: deployed, so
        // the ledger owns it, and the reference below asks for other bytes.
        let home_src = base.join("home-src");
        std::fs::create_dir_all(&home_src).expect("home source dir");
        std::fs::write(home_src.join("tm-workflow.md"), "# deployed\n").expect("write home source");
        deploy_skills(&home_src, &paths.claude_skills_dir()).expect("deploy the home copy");

        let stray = deployed_path(&project_skill_tier(&project), "tm-ticketing");
        let drifted = deployed_path(&paths.claude_skills_dir(), "tm-workflow");
        let inputs = FixSkillsInputs {
            paths,
            project_dir: Some(project),
            reference: SkillReference {
                assets: [
                    ("tm-ticketing".to_string(), "# refreshed\n".to_string()),
                    ("tm-workflow".to_string(), "# refreshed\n".to_string()),
                ]
                .into_iter()
                .collect(),
                origin: "test".to_string(),
            },
        };
        (inputs, stray, drifted)
    }

    /// #6620: a bare `tm doctor --fix-skills` writes NOTHING — not the stray it
    /// only planned to remove, not the drifted operator-home copy, and not a
    /// backup root.
    ///
    /// Fails before this fix: [`fix_skills_halves`] passed the redeploy
    /// `RepairMode::Apply` whatever the flag said, so the drifted home copy was
    /// rewritten and a backup root appeared in the same invocation the sweep
    /// printed "dry run — re-run with `tm doctor --fix-skills --yes`" for.
    #[test]
    fn a_bare_fix_skills_writes_nothing() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let (inputs, stray, drifted) = fixture(tmp.path());
        let backup_root = tmp.path().join("backup-doctor-remediation-test");
        let drifted_before = std::fs::read_to_string(&drifted).expect("the drifted copy");

        let (strays, outcomes) =
            fix_skills_halves(&inputs, false, fix_skills_mode(false), &backup_root);

        assert!(
            strays
                .iter()
                .any(|s| matches!(s.status, StepStatus::Planned)),
            "the recorded stray must be PLANNED, not applied: {strays:?}"
        );
        assert!(
            outcomes
                .iter()
                .any(|o| matches!(o.action, RepairAction::WouldRepair { existed: true })),
            "the drifted operator-home copy must be previewed: {outcomes:?}"
        );
        assert!(
            !outcomes.iter().any(RepairOutcome::changed),
            "a bare --fix-skills must repair nothing: {outcomes:?}"
        );
        assert_eq!(
            std::fs::read_to_string(&drifted).expect("the drifted copy after"),
            drifted_before,
            "the drifted operator-home copy must keep its bytes: {outcomes:?}"
        );
        assert!(
            stray.is_file(),
            "the stray it only planned to remove must survive: {strays:?}"
        );
        assert!(
            !backup_root.exists(),
            "and a preview that wrote nothing must leave no backup root: {outcomes:?}"
        );
    }

    /// The counterpart: `--yes` still applies both halves, so the gate is a gate
    /// and not a removal of the remedy.
    #[test]
    fn fix_skills_with_yes_applies_both_halves() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let (inputs, stray, drifted) = fixture(tmp.path());
        let backup_root = tmp.path().join("backup-doctor-remediation-test");

        let (strays, outcomes) =
            fix_skills_halves(&inputs, false, fix_skills_mode(true), &backup_root);

        assert!(
            strays
                .iter()
                .any(|s| matches!(s.status, StepStatus::Applied { .. })),
            "the recorded stray must be removed: {strays:?}"
        );
        assert!(!stray.exists(), "and gone from disk: {strays:?}");
        assert_eq!(
            std::fs::read_to_string(&drifted).expect("the repaired copy"),
            "# refreshed\n",
            "the drifted operator-home copy must be rewritten: {outcomes:?}"
        );
        assert!(
            backup_root.is_dir(),
            "and the overwrite must be backed up: {outcomes:?}"
        );
    }
}
