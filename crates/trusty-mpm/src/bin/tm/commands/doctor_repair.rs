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
    RepairMode, RepairStep, StepStatus, refuse_legacy_sources, repair_build_tree_binary,
    repair_hooks_contamination, repair_missing_hook_group, repair_output_style, repair_push_guard,
    repair_statusline,
};
use trusty_mpm::core::skill_repair::RepairAction;
use trusty_mpm::core::stray_mcp::{quarantine_explicit, quarantine_strays};

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
        super::doctor_fix_skills::fix_skills_locally(flags.include_frozen, flags.yes);
    }
    // #6649: the agent mirror of `--fix-skills`, and like it never run by
    // `--fix` — `--fix` still never deletes.
    if flags.fix_agents {
        super::doctor_fix_agents::fix_agents_locally(flags.yes);
    }
    if flags.fix {
        run_repairs(flags.yes, flags.include_frozen);
    }
    if let Some(target) = &flags.quarantine_mcp {
        quarantine_one_mcp(target, flags.yes);
    }
}

/// `--quarantine-mcp <PATH>` action — rename ONE named stray `.mcp.json` aside.
///
/// Why: the `--fix` sweep acts only on strays tm's provenance ledger proves it
/// wrote, so it refuses every file that predates the ledger — which is all of
/// them at the moment the ledger ships. The alternative to this flag is
/// loosening the sweep's evidence rule, and that rule is the only thing
/// standing between the repair and a `.mcp.json` the operator wrote by hand.
/// Naming the path moves the judgement to the operator, where it belongs.
/// What: resolves the workspace (so the current project's own `.mcp.json` can
/// be refused), runs [`quarantine_explicit`], and prints the one step. Dry run
/// unless `--yes`.
/// Test: `core::stray_mcp`'s tests cover the refusals and the rename; this
/// function is path resolution and printing.
fn quarantine_one_mcp(target: &std::path::Path, apply: bool) {
    let mode = RepairMode::from_apply_flag(apply);
    println!(
        "\nquarantine-mcp ({})",
        if apply {
            "applying".to_string()
        } else {
            "dry run — nothing will be written".to_string()
        }
    );
    let workspace = std::env::current_dir().ok();
    let step = quarantine_explicit(
        &trusty_mpm::core::mcp_provenance::default_framework_root(),
        workspace.as_deref(),
        target,
        mode,
    );
    print_steps(&[step], apply, &quarantine_apply_hint(target));
}

/// The command that applies a `--quarantine-mcp` preview.
///
/// Why (#5371 critic MEDIUM): the shared step printer originally hardcoded
/// `tm doctor --fix --yes`, so a `--quarantine-mcp` dry run told the operator
/// to run a command that would not touch their file. The fix was a one-line
/// string and had no test, which is exactly how it comes back. Naming the
/// string makes it assertable without capturing stdout.
/// What: `tm doctor --quarantine-mcp <path> --yes`.
/// Test: `quarantine_hint_names_the_quarantine_command_and_path`.
fn quarantine_apply_hint(target: &std::path::Path) -> String {
    format!("tm doctor --quarantine-mcp {} --yes", target.display())
}

/// The command that applies a `--fix` preview.
///
/// Why: the counterpart to [`quarantine_apply_hint`] — pinned so the two
/// cannot collapse back into one shared literal.
/// Test: `fix_hint_names_the_fix_command`.
const FIX_APPLY_HINT: &str = "tm doctor --fix --yes";

/// `--fix` action — preview, or apply, every safe repair.
///
/// Why: see the module doc. The default is a preview because these repairs
/// rewrite files the operator can see and care about; `--yes` is the second
/// deliberate act that lets one write.
/// What: runs, in order, the skill redeploy (`skill_staleness`), the
/// machine-wide build-tree repoint (`hooks_build_tree_binary`, #7262 — first,
/// so the strip below cannot delete a PM guard the repoint would have fixed),
/// the project hook cleanup (`hooks_contamination`), the lifecycle-group
/// re-merge (`hooks_missing_tm_group`, #7490 — after that cleanup, so it
/// restores what a managed launch would write), the push-guard retrofit
/// (`push_guard`), the output-style redeploy at BOTH style tiers
/// (`output_style_staleness`, #5866, #7423),
/// the `legacy_sources` refusals, and the stray-`.mcp.json`
/// sweep (`stray_mcp_json`) — printing each item's path, what would change,
/// and the outcome. In dry run it closes by naming the flag that applies. It
/// never deletes: `legacy_sources` findings are reported as refused, and a
/// stray `.mcp.json` is RENAMED aside rather than removed.
/// Test: `core::doctor_repair`'s and `core::stray_mcp`'s tests; this function
/// is printing.
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
    // #7262: the repoint runs BEFORE the contamination strip. A build-tree
    // command is tm-owned by `is_mpm_hook_command`, so a strip that ran first
    // would delete the PM guard this pass repairs in place.
    steps.extend(repair_build_tree_binary(
        &machine_wide_settings_files(project_dir.as_deref()),
        mode,
    ));
    // #7617: AFTER the repoint, so a build-tree `statusLine` command is fixed by
    // the pass that owns that damage class and this one only seeds what is
    // missing. Scoped to the two tiers the `statusline` check reads — the cwd
    // project and the user tier — NOT the machine-wide sweep above: seeding an
    // entry into every project on the disk is a far larger change than
    // repointing the ones already broken, and nobody asked for it.
    steps.extend(repair_statusline(
        &statusline_settings_files(project_dir.as_deref()),
        mode,
    ));
    if let Some(project) = &project_dir {
        steps.extend(repair_hooks_contamination(project, mode));
        // #7490: AFTER the strip above, never before. That pass removes every
        // `<exe> hook` entry it finds, including the ones this pass restores,
        // so running first would report a merge the strip then undid.
        steps.extend(repair_missing_hook_group(project, mode));
        steps.extend(repair_push_guard(project, mode));
    } else {
        eprintln!("  could not resolve the current directory — skipped the project-scoped repairs");
    }

    if let Some(home) = dirs::home_dir() {
        // #5866: `output_style_staleness` named `tm install` as its remedy and
        // `tm install` has no output-style step, so the finding had no repair at
        // all. This is it.
        // #7423: and the managed `$CLAUDE_CONFIG_DIR` tier beside it — the copy
        // a tm-launched session actually reads, which this repair never touched.
        steps.extend(repair_output_style(
            &home,
            trusty_mpm::core::trusty_tools_config::managed_claude_config_dir().as_deref(),
            mode,
        ));
        steps.extend(refuse_legacy_sources(&home));
        // A `.mcp.json` above the workspace configures every session started
        // beneath it. This quarantines ONLY the ones the provenance ledger
        // proves tm wrote and nobody has edited since; everything else is
        // reported as refused, with the reason and the `--quarantine-mcp`
        // escape hatch. Like every other repair here, it deletes nothing — the
        // file is renamed aside.
        steps.extend(quarantine_strays(
            &trusty_mpm::core::mcp_provenance::default_framework_root(),
            project_dir.as_deref(),
            &home,
            mode,
        ));
    }

    print_steps(&steps, apply, FIX_APPLY_HINT);
}

/// Every `.claude/settings*.json` the build-tree repoint should reach (#7262).
///
/// Why: the corruption is written by whichever build tree ran last, into
/// whichever project that session was pointed at — on 2026-09-10 that was eight
/// projects, none of them the cwd of the operator who ran `tm doctor`. A
/// cwd-scoped repair leaves seven of them broken and reports success, which is
/// why the fix had to be done by hand with `sed`. The sweep therefore uses the
/// same `$HOME`-wide discovery `tm hooks clean` (no `--path`) already owns
/// rather than a second enumerator, plus the cwd, which a `$HOME` walk misses
/// when the project lives outside the home directory.
/// What: the deduplicated union of `<cwd>/.claude/{settings.json,
/// settings.local.json}` and
/// [`trusty_common::claude_config::discover_claude_settings`] under the resolved
/// home. Existence is not checked here — [`repair_build_tree_binary`] treats a
/// missing file as nothing to do. An unresolvable home contributes nothing
/// rather than aborting the repair.
///
/// This walk is the reason it belongs to `--fix` and not to `tm doctor` itself:
/// the diagnostic must stay fast, so its `hooks_build_tree_binary` probe stays
/// scoped to the projects the daemon tracks (see
/// `daemon::doctor_hooks_hygiene::candidate_settings_files`), while the repair
/// the operator explicitly asked for pays the cost once.
/// Test: `machine_wide_settings_files_includes_the_cwd_project`.
fn machine_wide_settings_files(project_dir: Option<&std::path::Path>) -> Vec<std::path::PathBuf> {
    let mut set: std::collections::BTreeSet<std::path::PathBuf> = std::collections::BTreeSet::new();
    if let Some(dir) = project_dir {
        for name in ["settings.json", "settings.local.json"] {
            set.insert(dir.join(".claude").join(name));
        }
    }
    if let Some(home) = dirs::home_dir() {
        set.extend(trusty_common::claude_config::discover_claude_settings(
            &home,
            trusty_common::claude_config::default_settings_max_depth(),
        ));
    }
    set.into_iter().collect()
}

/// The two settings tiers the `statusline` check reads and its repair writes.
///
/// Why (#7617): the machine-wide sweep above exists because build-tree damage
/// lands in whichever project ran last. Seeding a MISSING entry is the opposite
/// case — a project with no `statusLine` key has simply never been prepared, and
/// writing one into every such project on the disk is a change nobody asked
/// for. The two tiers a session actually resolves are the cwd project's and the
/// user's.
/// What: `<cwd>/.claude/settings.json` and `~/.claude/settings.json`, whichever
/// resolve. Existence is not checked — the repair creates an absent file, which
/// is the seeding case.
/// Test: `core::doctor_repair`'s `statusline_repair_*` tests cover the repair;
/// this is path resolution.
fn statusline_settings_files(project_dir: Option<&std::path::Path>) -> Vec<std::path::PathBuf> {
    let mut files = Vec::new();
    if let Some(dir) = project_dir {
        files.push(dir.join(".claude").join("settings.json"));
    }
    if let Some(home) = dirs::home_dir() {
        files.push(home.join(".claude").join("settings.json"));
    }
    files
}

/// Print one repair's worth of steps, with the per-outcome tallies.
///
/// Why: `--fix`, `--quarantine-mcp`, and (since #6586) `--fix-skills`' project-
/// tier sweep all produce the same [`RepairStep`] shape and must render it
/// identically — an operator should not have to learn three formats, and a
/// refusal in particular must read the same way in all of them (it is the safety
/// rule working, not an error).
/// What: one line per step, then the counts, the dry-run hint when anything is
/// merely planned, and the refusal note when anything was refused. `apply_hint`
/// is the exact command that applies THIS run — a shared printer that always
/// named `--fix --yes` told a `--quarantine-mcp` operator to run a command that
/// would not touch their file.
/// Test: `core::doctor_repair` and `core::stray_mcp` cover the step values;
/// this is printing.
pub(crate) fn print_steps(steps: &[RepairStep], apply: bool, apply_hint: &str) {
    if steps.is_empty() {
        println!("  nothing to repair");
        return;
    }

    let mut planned = 0usize;
    let mut applied = 0usize;
    let mut refused = 0usize;
    let mut failed = 0usize;
    for step in steps {
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
        println!("  dry run — re-run with `{apply_hint}` to apply.");
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
    let backup_root = super::doctor_fix_skills::default_backup_root();
    // `--fix` deliberately does not run the #6586 sweep, so it defers nothing.
    let (_origin, outcomes) = super::doctor_fix_skills::skill_repair_outcomes(
        include_frozen,
        mode,
        &backup_root,
        &std::collections::BTreeSet::new(),
    );
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quarantine_hint_names_the_quarantine_command_and_path() {
        let hint = quarantine_apply_hint(std::path::Path::new("/private/tmp/.mcp.json"));
        assert_eq!(
            hint,
            "tm doctor --quarantine-mcp /private/tmp/.mcp.json --yes"
        );
        assert!(
            !hint.contains("--fix"),
            "a quarantine preview must never point at --fix, which would not touch the \
             named file: {hint}"
        );
    }

    #[test]
    fn fix_hint_names_the_fix_command() {
        assert_eq!(FIX_APPLY_HINT, "tm doctor --fix --yes");
        assert!(!FIX_APPLY_HINT.contains("--quarantine-mcp"));
    }

    /// #7262: the eight projects corrupted on 2026-09-09 were not the cwd of the
    /// operator who ran `tm doctor`, so the sweep must reach past it. A project
    /// checked out OUTSIDE `$HOME` is only reachable through the cwd entry,
    /// which is why both sources are unioned rather than one replacing the
    /// other.
    #[test]
    fn machine_wide_settings_files_includes_the_cwd_project() {
        let project = std::path::Path::new("/srv/projects/acme");
        let files = machine_wide_settings_files(Some(project));

        for name in ["settings.json", "settings.local.json"] {
            let expected = project.join(".claude").join(name);
            assert!(files.contains(&expected), "{expected:?} missing: {files:?}");
        }
        // Deduplicated and ordered, so one project can never be repaired twice
        // in one run.
        let mut sorted = files.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted, files, "the list must be sorted and deduplicated");
    }
}
