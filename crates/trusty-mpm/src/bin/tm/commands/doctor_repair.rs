//! `tm doctor --fix` — run every safely-repairable doctor finding (#4948).
//!
//! Why: the checks were pull-only, so findings persisted for weeks — 23 legacy
//! skill copies, 26 contaminated project settings files, one repairable skill
//! drift, all `Warn`, none actionable in the same breath as the report. This
//! is the action half. It is deliberately narrow: only repairs whose target is
//! something tm itself wrote and can prove it owns.
//! It is also where every OTHER opt-in local action `tm doctor` can be
//! followed by is dispatched from — `--prune-stale-skills`, `--fix-skills`,
//! `--fix` — so `commands/misc.rs` stays the report and the actions stay
//! together (and under the 500-SLOC production cap).
//! What: [`run_post_report_actions`] — the dispatch; [`run_repairs`] — resolve
//! the paths, run each repair from [`trusty_mpm::core::doctor_repair`] plus the
//! skill redeploy, and print one line per item, dry run unless `apply`;
//! [`prune_stale_skills_locally`] — the hidden pre-rename cleanup.
//! Test: `core::doctor_repair`'s tests cover every repair and every refusal;
//! this file is path resolution and printing.

use colored::Colorize;
use trusty_mpm::core::doctor_repair::{
    RepairMode, RepairStep, StepStatus, refuse_legacy_sources, repair_hooks_contamination,
    repair_push_guard,
};
use trusty_mpm::core::skill_repair::RepairAction;

use crate::cli::DoctorFlags;

/// Run whichever post-report local actions the operator opted into.
///
/// Why: `tm doctor` itself is READ-ONLY and stays that way. Every action here
/// requires its own flag, and they run after the report so the operator sees
/// the diagnosis above the remediation. They are local-filesystem operations,
/// independent of where the daemon that produced the report is reachable.
/// What: `--prune-stale-skills`, then `--fix-skills`, then `--fix` — the
/// unified driver last, so an operator who passed both skill flags sees the
/// narrow section first and the full list second rather than an order that
/// implies the second run did nothing.
/// Test: `cli_parses_doctor_fix`, `cli_parses_doctor_fix_skills` pin the flags;
/// the actions' own modules carry the behaviour tests.
pub(crate) fn run_post_report_actions(flags: &DoctorFlags) {
    if flags.prune_stale_skills {
        prune_stale_skills_locally();
    }
    if flags.fix_skills {
        super::doctor_fix_skills::fix_skills_locally(flags.include_frozen);
    }
    if flags.fix {
        run_repairs(flags.yes, flags.include_frozen);
    }
}

/// Hidden `--prune-stale-skills` action — force-remove pre-rename
/// `~/.claude/skills/mpm-*` directories.
///
/// Why: the mpm-*→tm-* skill rename (#1905) never deletes a skill's old
/// directory when it is renamed (`skill_deployer::deploy_skills` only writes,
/// never removes). In normal operation this is handled automatically and
/// silently by the one-time startup migration
/// (`core::stale_skills::run_stale_mpm_skills_migration_once`); this hidden
/// flag exists only as a manual escape hatch for troubleshooting (e.g. the
/// migration marker file was lost or hand-edited).
///
/// It is the ONE action here that deletes, which is why it is hidden, narrowly
/// scoped to trusty-mpm's own frozen pre-rename skill names, and deliberately
/// NOT part of `--fix`: `--fix` never deletes.
/// What: resolves `~/.claude/skills/` via `FrameworkPaths::default`, lists only
/// trusty-mpm's own pre-rename skill names (never an unrelated `mpm-*` skill
/// from another tool), prints what it found, then removes them.
/// Test: `core::stale_skills::tests` covers the detection/removal logic this
/// wraps; this function itself is thin I/O + printing.
fn prune_stale_skills_locally() {
    use trusty_mpm::core::paths::FrameworkPaths;
    use trusty_mpm::core::stale_skills::{find_stale_mpm_skills, remove_stale_mpm_skills};

    let paths = FrameworkPaths::default();
    let dir = paths.claude_skills_dir();
    let stale = find_stale_mpm_skills(&dir);
    if stale.is_empty() {
        println!(
            "\nno stale pre-rename mpm-* skills found in {}",
            dir.display()
        );
        return;
    }

    println!("\nremoving {} stale pre-rename skill(s):", stale.len());
    for skill in &stale {
        println!("  - {}", skill.name);
    }
    match remove_stale_mpm_skills(&stale) {
        Ok(n) => println!("removed {n} stale skill directory(ies)"),
        Err(e) => eprintln!("failed to remove stale skills: {e}"),
    }
}

/// `--fix` action — preview, or apply, every safe repair.
///
/// Why: see the module doc. The default is a preview because these repairs
/// rewrite files the operator can see and care about; `--yes` is the second
/// deliberate act that lets one write.
/// What: runs, in order, the skill redeploy (`skill_staleness`), the project
/// hook cleanup (`hooks_contamination`), the push-guard retrofit
/// (`push_guard`), and the `legacy_sources` refusals — printing each item's
/// path, what would change, and the outcome. In dry run it closes by naming
/// the flag that applies. It never deletes, and every `legacy_sources` finding
/// is reported as refused rather than acted on.
/// Test: `core::doctor_repair`'s tests; this function is printing.
pub(crate) fn run_repairs(apply: bool, include_frozen: bool) {
    let mode = RepairMode::from_apply_flag(apply);
    println!(
        "\nfix ({})",
        if apply {
            "applying".to_string()
        } else {
            "dry run — nothing will be written".to_string()
        }
    );

    let mut steps = skill_steps(include_frozen, mode);

    let project_dir = std::env::current_dir().ok();
    if let Some(project) = &project_dir {
        steps.extend(repair_hooks_contamination(project, mode));
        steps.extend(repair_push_guard(project, mode));
    } else {
        eprintln!("  could not resolve the current directory — skipped the project-scoped repairs");
    }

    if let Some(home) = dirs::home_dir() {
        steps.extend(refuse_legacy_sources(&home));
    }

    if steps.is_empty() {
        println!("  nothing to repair");
        return;
    }

    let mut planned = 0usize;
    let mut applied = 0usize;
    let mut refused = 0usize;
    let mut failed = 0usize;
    for step in &steps {
        let (icon, detail) = match &step.status {
            StepStatus::Planned => {
                planned += 1;
                ("~".yellow(), format!("would {}", step.what))
            }
            StepStatus::Applied { backup } => {
                applied += 1;
                match backup {
                    Some(p) => (
                        "✓".green(),
                        format!("{} (backup: {})", step.what, p.display()),
                    ),
                    None => ("✓".green(), step.what.clone()),
                }
            }
            StepStatus::Refused(why) => {
                refused += 1;
                ("-".blue(), format!("{} — REFUSED: {why}", step.what))
            }
            StepStatus::Failed(why) => {
                failed += 1;
                ("✗".red(), format!("{} — FAILED: {why}", step.what))
            }
        };
        println!(
            "  {icon} [{}] {}: {detail}",
            step.check,
            step.path.display()
        );
    }

    println!("\n  {applied} applied, {planned} planned, {refused} refused, {failed} failed");
    if !apply && planned > 0 {
        println!("  dry run — re-run with `tm doctor --fix --yes` to apply.");
    }
    if refused > 0 {
        println!(
            "  refused items are reported, never repaired — tm does not delete files it \
             cannot prove it owns."
        );
    }
}

/// The skill redeploy, expressed as [`RepairStep`]s.
///
/// Why: `--fix` prints one uniform list, so the skill repair's richer outcome
/// type is projected onto the shared shape here rather than printed through a
/// second format the operator has to learn. The mapping is deliberately lossy
/// in one direction only: a FROZEN skip becomes a [`StepStatus::Refused`],
/// which is exactly what it is.
/// What: calls [`super::doctor_fix_skills::skill_repair_outcomes`] — the same
/// function `--fix-skills` uses, in the mode this run selected — and maps each
/// outcome onto a step. The classification itself is never re-derived here.
/// Test: `core::skill_repair`'s tests produce every outcome variant.
fn skill_steps(include_frozen: bool, mode: RepairMode) -> Vec<RepairStep> {
    let (_origin, outcomes) = super::doctor_fix_skills::skill_repair_outcomes(include_frozen, mode);
    outcomes
        .into_iter()
        .map(|o| {
            let what = format!(
                "redeploy `{}` at the {} tier from the bundled asset",
                o.stem, o.tier
            );
            let status = match o.action {
                RepairAction::Repaired { backup } => StepStatus::Applied { backup },
                RepairAction::WouldRepair { .. } => StepStatus::Planned,
                RepairAction::SkippedFrozen => StepStatus::Refused(
                    "hand-edited after deployment — pass `--include-frozen` to overwrite it, \
                     backing it up first"
                        .to_string(),
                ),
                RepairAction::SkippedUnverifiable(why) => StepStatus::Refused(why),
                RepairAction::Failed(why) => StepStatus::Failed(why),
            };
            RepairStep {
                check: "skill_staleness",
                path: o.path,
                what,
                status,
            }
        })
        .collect()
}
