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
//! What: [`run_post_report_actions`] — the dispatch; the `--fix` runner lives
//! in `doctor_fix_run` (#10425), and this file keeps the helpers it shares
//! with the narrow flags; [`prune_stale_skills_locally`] — the hidden
//! pre-rename cleanup.
//! Test: `core::doctor_repair`'s tests cover every repair and every refusal;
//! this file is path resolution and printing.

use colored::Colorize;
use trusty_mpm::core::doctor_repair::{RepairMode, RepairStep, StepStatus, repair_output_style};
use trusty_mpm::core::skill_repair::RepairAction;
use trusty_mpm::core::stray_mcp::quarantine_explicit;

use crate::cli::DoctorFlags;

/// Run whichever post-report local actions the operator opted into.
///
/// Why: `tm doctor` itself is READ-ONLY and stays that way. Every action here
/// requires its own flag, and they run after the report so the operator sees
/// the diagnosis above the remediation. They are local-filesystem operations,
/// independent of where the daemon that produced the report is reachable.
/// What: runs [`post_report_actions`] in order — the unified `--fix` driver
/// after the narrow flags, so an operator who passed both sees the narrow
/// section first and the full list second rather than an order that implies
/// the second run did nothing.
/// Test: `cli_parses_doctor_fix`, `cli_parses_doctor_fix_skills` pin the flags;
/// `the_scoped_launchd_flag_selects_exactly_one_repair` pins the dispatch; the
/// actions' own modules carry the behaviour tests.
/// `Err` (after every action has run) when `--rotate-deps` printed a FAIL row;
/// `Err` before any action runs when `--only` names no `--fix` repair (#10425).
pub(crate) fn run_post_report_actions(flags: &DoctorFlags) -> anyhow::Result<()> {
    super::doctor_fix_run::resolve_scope(&flags.only)?;
    // #10425: `--dry-run` wins over `--yes`; clap also refuses the pair.
    let yes = flags.yes && !flags.dry_run;
    let mut failed = Ok(());
    for action in post_report_actions(flags) {
        match action {
            PostReportAction::PruneStaleSkills => prune_stale_skills_locally(),
            PostReportAction::FixSkills => {
                super::doctor_fix_skills::fix_skills_locally(flags.include_frozen, yes);
            }
            // #6649: the agent mirror of `--fix-skills`, and like it never run
            // by `--fix` — `--fix` still never deletes.
            PostReportAction::FixAgents => super::doctor_fix_agents::fix_agents_locally(yes),
            // #8236: the credential strip alone, never the other repair classes.
            PostReportAction::FixLaunchdSecrets => {
                super::doctor_fix_launchd_secrets::fix_launchd_secrets_locally(yes);
            }
            // #10212: the oversized-deps rotation alone; `--fix` never runs it.
            PostReportAction::RotateDeps => {
                failed = super::doctor_rotate_deps::rotate_deps_locally(yes);
            }
            PostReportAction::Fix => {
                super::doctor_fix_run::run_repairs(yes, flags.include_frozen, &flags.only)?;
            }
            PostReportAction::QuarantineMcp => {
                if let Some(target) = &flags.quarantine_mcp {
                    quarantine_one_mcp(target, yes);
                }
            }
        }
    }
    failed
}

/// One post-report action `tm doctor` can run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PostReportAction {
    /// `--prune-stale-skills`.
    PruneStaleSkills,
    /// `--fix-skills`.
    FixSkills,
    /// `--fix-agents`.
    FixAgents,
    /// `--fix-launchd-secrets` (#8236).
    FixLaunchdSecrets,
    /// `--rotate-deps` (#10212).
    RotateDeps,
    /// `--fix` — every repair class.
    Fix,
    /// `--quarantine-mcp <PATH>`.
    QuarantineMcp,
}

/// The actions `flags` selects, in the order they run.
///
/// Why (#8236): a pure selection is what lets a test prove the scoped flag
/// runs one repair and never the `--fix` sweep, without running either.
/// What: one entry per set flag, in dispatch order.
/// Test: `the_scoped_launchd_flag_selects_exactly_one_repair`.
fn post_report_actions(flags: &DoctorFlags) -> Vec<PostReportAction> {
    [
        (flags.prune_stale_skills, PostReportAction::PruneStaleSkills),
        (flags.fix_skills, PostReportAction::FixSkills),
        (flags.fix_agents, PostReportAction::FixAgents),
        (
            flags.fix_launchd_secrets,
            PostReportAction::FixLaunchdSecrets,
        ),
        (flags.rotate_deps, PostReportAction::RotateDeps),
        (flags.fix, PostReportAction::Fix),
        (
            flags.quarantine_mcp.is_some(),
            PostReportAction::QuarantineMcp,
        ),
    ]
    .into_iter()
    .filter_map(|(on, action)| on.then_some(action))
    .collect()
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
pub(super) const FIX_APPLY_HINT: &str = "tm doctor --fix --yes";

/// Every `.claude/settings*.json` the build-tree repoint should reach (#7262).
///
/// Why: the corruption is written by whichever build tree ran last, into
/// whichever project that session was pointed at — on 2026-09-10 that was eight
/// projects, none of them the cwd of the operator who ran `tm doctor`. A
/// cwd-scoped repair leaves seven of them broken and reports success, which is
/// why the fix had to be done by hand with `sed`. The sweep therefore walks
/// `$HOME`, plus the cwd, which a `$HOME` walk misses when the project lives
/// outside the home directory.
/// What: the deduplicated union of `<cwd>/.claude/{settings.json,
/// settings.local.json}` and
/// [`trusty_common::claude_config::discover_claude_settings`] under the
/// resolved home, which never enters a top-level synced folder or another
/// device. Existence is not checked here — the build-tree repoint treats
/// a missing file as nothing to do. An unresolvable home contributes nothing
/// rather than aborting the repair.
///
/// Plain `tm doctor` walks it too: the `hook-interpreter` row reads this list,
/// so the diagnostic pays the machine-wide walk on every run. The
/// `hooks_build_tree_binary` probe still stays scoped to the projects the
/// daemon tracks (see `daemon::doctor_hooks_hygiene::candidate_settings_files`).
/// `home` is the walk root (`None` contributes nothing).
/// Test: `machine_wide_settings_files_includes_the_cwd_project`.
// #10263: plain `tm doctor` now walks this list for the `hook-interpreter` row.
pub(crate) fn machine_wide_settings_files(
    project_dir: Option<&std::path::Path>,
    home: Option<&std::path::Path>,
) -> Vec<std::path::PathBuf> {
    let mut set: std::collections::BTreeSet<std::path::PathBuf> = std::collections::BTreeSet::new();
    if let Some(dir) = project_dir {
        for name in ["settings.json", "settings.local.json"] {
            set.insert(dir.join(".claude").join(name));
        }
    }
    // #9293: `home` is supplied, never resolved here, so the walk root is the
    // caller's; production passes `dirs::home_dir()`, a test passes a temp dir.
    // #10267: the shared trusty-common walk, which never enters iCloud-synced
    // home folders or another device — a read there froze a session.
    if let Some(home) = home {
        set.extend(trusty_common::claude_config::discover_claude_settings(
            home,
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
pub(super) fn statusline_settings_files(
    project_dir: Option<&std::path::Path>,
    home: Option<&std::path::Path>,
) -> Vec<std::path::PathBuf> {
    let mut files = Vec::new();
    if let Some(dir) = project_dir {
        files.push(dir.join(".claude").join("settings.json"));
    }
    if let Some(home) = home {
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

/// The output-style repair, or one failed step when there is no content.
///
/// Why (#9012): the style bodies are runtime content. Without content the
/// repair cannot run, and a skip printed only to stderr left the `--fix`
/// summary reading clean.
/// What: [`repair_output_style`] over `content`; on `Err`, one
/// [`StepStatus::Failed`] step against `<home>/.claude/output-styles` whose
/// reason is the content error, which names its remedy.
/// Test: `a_missing_content_source_fails_the_output_style_repair`.
pub(super) fn output_style_steps(
    content: Result<
        trusty_mpm::core::content_source::FrameworkContent,
        trusty_mpm::core::content_source::AgentContentError,
    >,
    home: &std::path::Path,
    managed_config: Option<&std::path::Path>,
    mode: RepairMode,
) -> Vec<RepairStep> {
    match content {
        Ok(content) => repair_output_style(&content, home, managed_config, mode),
        Err(err) => vec![RepairStep {
            check: "output_style_staleness",
            path: home.join(".claude").join("output-styles"),
            what: "redeploy the trusty-mpm output styles".to_string(),
            status: StepStatus::Failed(err.to_string()),
        }],
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
pub(super) fn skill_steps(include_frozen: bool, mode: RepairMode) -> Vec<RepairStep> {
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

    /// #9012: no content is a failed output-style step naming the remedy, so
    /// the `--fix` summary counts it instead of reading clean.
    #[test]
    fn a_missing_content_source_fails_the_output_style_repair() {
        let cache = tempfile::tempdir().expect("empty content cache");
        let home = tempfile::tempdir().expect("home");
        let content = trusty_mpm::core::content_source::framework_content_in(
            cache.path(),
            trusty_mpm::core::content_source::DevOverride::Off,
        );
        let steps = output_style_steps(content, home.path(), None, RepairMode::Apply);
        assert_eq!(steps.len(), 1, "{steps:?}");
        assert_eq!(steps[0].check, "output_style_staleness");
        match &steps[0].status {
            StepStatus::Failed(why) => assert!(why.contains("tm content install"), "{why}"),
            other => panic!("expected a failed step, got {other:?}"),
        }
        assert!(!home.path().join(".claude").exists(), "nothing written");
    }

    /// Parse `tm doctor <args>` into its flags.
    fn doctor_flags(args: &[&str]) -> DoctorFlags {
        use clap::Parser;
        let argv = ["tm", "doctor"].iter().chain(args).copied();
        match crate::cli::Cli::try_parse_from(argv)
            .expect("parses")
            .command
        {
            Some(crate::cli::Command::Doctor { flags }) => flags,
            _ => panic!("expected the doctor command"),
        }
    }

    /// #8236: the scoped flag must run the credential strip and nothing else —
    /// in particular never the machine-wide `--fix` sweep — with or without
    /// `--yes`, and `--fix` must not gain it as a second, separate run.
    #[test]
    fn the_scoped_launchd_flag_selects_exactly_one_repair() {
        for args in [
            &["--fix-launchd-secrets"][..],
            &["--fix-launchd-secrets", "--yes"][..],
        ] {
            let flags = doctor_flags(args);
            assert_eq!(
                post_report_actions(&flags),
                vec![PostReportAction::FixLaunchdSecrets],
                "{args:?}"
            );
            assert_eq!(flags.yes, args.contains(&"--yes"));
        }
        assert_eq!(
            post_report_actions(&doctor_flags(&["--fix", "--yes"])),
            vec![PostReportAction::Fix]
        );
    }

    /// #10212: `--rotate-deps` selects the rotation alone, and `--fix` never
    /// gains it.
    #[test]
    fn the_rotate_deps_flag_selects_exactly_one_repair() {
        for args in [&["--rotate-deps"][..], &["--rotate-deps", "--yes"][..]] {
            let flags = doctor_flags(args);
            assert_eq!(
                post_report_actions(&flags),
                vec![PostReportAction::RotateDeps],
                "{args:?}"
            );
            assert_eq!(flags.yes, args.contains(&"--yes"));
        }
        assert!(
            !post_report_actions(&doctor_flags(&["--fix"])).contains(&PostReportAction::RotateDeps)
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
        // #9293: a temp home, so the walk never touches the real `$HOME`.
        let home = tempfile::tempdir().expect("temp home");
        let home_project = home.path().join("proj").join(".claude");
        std::fs::create_dir_all(&home_project).expect("plant project");
        let planted = home_project.join("settings.json");
        std::fs::write(&planted, "{}").expect("plant settings");
        // #10263: an iCloud-synced top-level folder is never walked.
        let synced = home.path().join("Documents").join("proj").join(".claude");
        std::fs::create_dir_all(&synced).expect("plant synced project");
        std::fs::write(synced.join("settings.json"), "{}").expect("plant synced settings");

        let project = std::path::Path::new("/srv/projects/acme");
        let files = machine_wide_settings_files(Some(project), Some(home.path()));
        assert!(
            files
                .iter()
                .all(|f| !f.starts_with(home.path().join("Documents"))),
            "a file under <home>/Documents was returned: {files:?}"
        );

        for name in ["settings.json", "settings.local.json"] {
            let expected = project.join(".claude").join(name);
            assert!(files.contains(&expected), "{expected:?} missing: {files:?}");
        }
        assert!(files.contains(&planted), "{planted:?} missing: {files:?}");
        if let Some(real_home) = dirs::home_dir() {
            assert!(
                files.iter().all(|f| !f.starts_with(&real_home)),
                "a result lies under the real home {real_home:?}: {files:?}"
            );
        }
        // Deduplicated and ordered, so one project can never be repaired twice
        // in one run.
        let mut sorted = files.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted, files, "the list must be sorted and deduplicated");
    }
}
