//! `install` command handler and framework-artifact helpers.
//!
//! Why: the install handler is self-contained — it writes bundled artifacts,
//! deploys agents and skills, and wires Claude Code hooks — and benefits from
//! its own file so it stays reviewable. (#4752: it no longer assembles a PM
//! prompt; that is per-project and written at launch.)
//! What: `install`, `install_claude_hooks` / `install_claude_hooks_at`,
//! `mpm_hook_additions`, `install_to`, `install_one`, `deploy_report_lines`,
//! `reset_report_lines`, `skill_report_lines`, `write_project_hooks_for_dir`.
//! Test: `install_writes_all_artifacts`,
//! `overwrite_artifact_refreshes_modified_file_without_force`,
//! `seed_once_artifact_is_not_clobbered_without_force`,
//! `seed_once_artifact_force_resets_to_shipped_default`,
//! `install_claude_hooks_at_writes_only_the_managed_config_dir`,
//! `install_claude_hooks_at_is_idempotent`,
//! `install_claude_hooks_at_never_touches_a_sibling_project_dir`,
//! `install_then_deploy_deploys_skills` (#7102: in
//! `tests_behavior_install_skills_tests.rs`).

/// `install` subcommand — deploy the bundled framework artifacts and wire
/// MPM lifecycle hooks into every Claude Code settings file.
///
/// Why: a fresh machine has no `~/.trusty-mpm/framework/`; `trusty-mpm install`
/// writes the compile-time-embedded artifacts (optimizer policy, framework
/// instructions, placeholder agent/skill) so the daemon has a working policy
/// and launchers have a working framework tree. It deploys composed agents and
/// skills and wires MPM lifecycle hooks into every Claude Code settings file.
/// All edits are idempotent. #4752: it no longer assembles a PM system prompt —
/// the compiled prompt is per-project and written by the launch path.
///
/// `reset_agents` (issue #2504) is the explicit reconciliation path for
/// composed agent files a normal deploy conservatively skips because they
/// predate per-file manifest tracking. `Some(vec![])` (the CLI's `--reset-
/// agents` with no value) resets every bundled agent; `Some(names)` restricts
/// the reset to those agents. The normal deploy always runs first — reset
/// only forces the specific files a plain deploy would otherwise leave
/// alone, and re-registers everything else it touches.
///
/// `reconcile_skills` (issue #4605) is the same explicit reconciliation path
/// for SKILLS. A bundled skill absent from a deploy target's ownership
/// manifest is classified project-custom by the tier planner and excluded from
/// `bundled_deploy` before the deployer's ownership check ever runs, so no
/// deploy reaches it and the normal install output cannot even mention it.
/// When `false` (the default) the install REPORTS those skills, per tier; when
/// `true` it adopts and refreshes them, backing up every file it touches
/// first. Skills whose name matches nothing bundled are never touched either
/// way — they are the operator's.
///
/// `reset_agents_workspaces` (issue #2508) additionally sweeps every intact
/// session workspace's project-local `.claude/agents/` through the SAME
/// reset, each gated by that workspace's OWN resolved harness plan (so an
/// agent one project's manifest excludes is never resurrected there — the
/// #2462 cross-warning). Only takes effect when `reset_agents` is `Some`.
/// What: resolves `FrameworkPaths::default`, calls [`install_to`], removes a
/// stale pre-#4752 `instructions/INSTRUCTIONS.md`, deploys agents and skills,
/// optionally resets the requested agent scope (user-level, then every intact
/// session workspace when requested), then calls [`install_claude_hooks`].
/// #4752: it no longer writes a compiled PM prompt — that is per-project and
/// written at launch (`instruction_pipeline::compiled_prompt_path`).
/// Test: `install_writes_all_artifacts`,
/// `overwrite_artifact_refreshes_modified_file_without_force`,
/// `seed_once_artifact_is_not_clobbered_without_force`,
/// `seed_once_artifact_force_resets_to_shipped_default`,
/// `install_claude_hooks_is_idempotent`,
/// `remove_stale_bundled_instructions_deletes_a_leftover`.
pub(crate) async fn install(
    force: bool,
    reset_agents: Option<Vec<String>>,
    reset_agents_workspaces: bool,
    reconcile_skills: bool,
) -> anyhow::Result<()> {
    let paths = trusty_mpm::core::paths::FrameworkPaths::default();
    let report = install_to(&paths, force)?;
    println!(
        "Installing trusty-mpm framework artifacts to {}",
        paths.framework.display()
    );
    for line in &report {
        println!("  {line}");
    }

    // #4752: `tm install` no longer writes a compiled PM prompt at all. The
    // compiled prompt is the text ONE project's session runs with, and install
    // has no project — writing the bundled assembly to a global path is exactly
    // the "wrong content kind on a shared path" collision this issue closes.
    // Each launch writes its own project's copy (see
    // `instruction_pipeline::compiled_prompt_path`).

    // #4752: a pre-#4752 install left the compiled prompt at
    // `instructions/INSTRUCTIONS.md`. Nothing writes that path now, but
    // `build_instructions` still READS it — so an upgraded machine would fold a
    // frozen copy of an old prompt into the pipeline forever. Remove it here
    // rather than leaving a stale input no writer can refresh.
    match trusty_mpm::core::instruction_pipeline::remove_stale_bundled_instructions(
        &paths.framework_instructions_path(),
    ) {
        Ok(true) => println!("  \u{2717} instructions/INSTRUCTIONS.md (stale, removed)"),
        Ok(false) => {}
        Err(e) => eprintln!("warning: failed to remove stale instructions/INSTRUCTIONS.md: {e:#}"),
    }

    // #4409: bundled agents deploy ONLY into the tm-managed config-dir tier.
    // `tm install` previously wrote them into the operator's generic
    // `~/.claude/agents/`, contaminating a Claude Code install that has nothing
    // to do with trusty-mpm — the highest-severity breach on that issue.
    println!(
        "Composing agents into {}",
        paths.agent_deploy_dir().display()
    );
    // #7727: agent bodies point at the bundled skill tier this install deploys.
    let deploy = trusty_mpm::core::agent_deployer::deploy_agents(
        &paths.agent_source_dir(),
        &paths.agent_deploy_dir(),
        &paths.skill_deploy_dir(),
    )?;
    for line in deploy_report_lines(&deploy, &paths.agent_source_dir()) {
        println!("  {line}");
    }

    // Issue #2504: force-recompose the requested agent scope. Runs AFTER the
    // normal deploy above so it only has to touch files the conservative
    // deploy left alone (untracked + differing from the bundle); everything
    // else is already at the fresh composition and gets adopted, not rewritten.
    if let Some(names) = reset_agents {
        let filter = if names.is_empty() { None } else { Some(names) };
        println!(
            "Resetting agents ({}) into {}",
            filter
                .as_ref()
                .map(|n| n.join(", "))
                .unwrap_or_else(|| "all".to_string()),
            paths.agent_deploy_dir().display()
        );
        let reset = trusty_mpm::core::agent_reset::reset_agents(
            &paths.agent_source_dir(),
            &paths.agent_deploy_dir(),
            &paths.skill_deploy_dir(),
            filter.as_deref(),
        )?;
        for line in reset_report_lines(&reset) {
            println!("  {line}");
        }

        // Issue #2508: the user-level reset above never touches a project's
        // own `.claude/agents/` — sweep every intact session workspace through
        // the identical reset, each gated by ITS OWN resolved harness plan.
        if reset_agents_workspaces {
            println!("Resetting agents in every intact session workspace…");
            let outcomes = trusty_mpm::core::agent_reset_workspace::reset_active_workspace_agents(
                &paths.root,
                filter.as_deref(),
            )
            .await?;
            if outcomes.is_empty() {
                println!("  (no intact session workspaces found)");
            }
            for outcome in &outcomes {
                println!(
                    "  {} ({})",
                    outcome.tmux_name,
                    outcome.workspace_path.display()
                );
                // Issue #1511: a session the ownership gate rejected is
                // reported explicitly (never silently absent) and its
                // `result` is always empty — do not run it through
                // `reset_report_lines`, which would print a misleading
                // all-zero summary instead of the actual reason.
                if let Some(reason) = &outcome.skipped_reason {
                    println!("    {reason}");
                    continue;
                }
                for line in reset_report_lines(&outcome.result) {
                    println!("    {line}");
                }
            }
        }
    }

    // Deploy skills. The bundle install above (`install_to`) already wrote the
    // skill sources under the framework root, so the source dir is populated
    // before this copy runs — mirroring the agent ordering. Without this step a
    // fresh `tm install` left the skill tier empty and `tm doctor` reported
    // `skills: Fail` (#386); skills only deployed lazily on `tm session start`
    // (see `prepare_session`). Routes through the SAME multi-tier orchestrator
    // `session_launch`/`apply_catalog` use (PR #2818 review round 3): a raw
    // `deploy_skills` call here would see a previously user-tier-deployed skill
    // as "managed, checksum matches" and silently refresh it back to bundled
    // content on every routine `tm install` — the same clobber bug fixed for
    // `tm catalog apply` in the prior review round, at a third, independent
    // call site.
    //
    // #7102: the destination split lives in
    // `core::skill_install_tiers::deploy_install_skill_tiers`. This step used to
    // write the WHOLE roster into `paths.claude_skills_dir()`, which is the
    // directory `tm doctor`'s `legacy_sources` check flags as a pre-#6586 global
    // deploy — so the check's own remediation ("run `tm install`") put the
    // flagged copies straight back.
    println!(
        "Deploying bundled skills into {}",
        paths.skill_deploy_dir().display()
    );
    println!(
        "Deploying user-custom skills into {}",
        paths.claude_skills_dir().display()
    );
    let skill_deploy =
        trusty_mpm::core::skill_install_tiers::deploy_install_skill_tiers(&paths)?.stats;
    for line in skill_report_lines(&skill_deploy) {
        println!("  {line}");
    }

    // #5224: the deploy above only ever WRITES, so a skill this binary stopped
    // shipping keeps its deployed directory and its ledger entry — which pins
    // `tm doctor`'s `skill_staleness` check to Unknown. Sweep every deploy
    // tier, not just the one deployed into above; the orphan occurs at each one
    // independently.
    for retired in trusty_mpm::core::skill_retire::retire_orphaned_skills(&paths, None) {
        match &retired.reason {
            None => println!(
                "  - {} removed from {} (no longer shipped)",
                retired.stem, retired.tier
            ),
            Some(why) => println!(
                "  ! {} is no longer shipped but its files were kept at {} ({why}) — \
                 trusty-mpm no longer tracks them",
                retired.stem, retired.tier
            ),
        }
    }

    // Issue #4605: the deploy above is structurally unable to mention a
    // bundled skill that its target's manifest does not track — the tier
    // planner classifies it project-custom and drops it before
    // `deploy_skills_filtered` runs, so it appears as neither deployed,
    // skipped, NOR unchanged. Say so, for every deploy tier, and offer the one
    // command that fixes it. `--reconcile-skills` runs that command.
    if reconcile_skills {
        crate::commands::install_skills::reconcile_skills(&paths, None)?;
    } else {
        for line in crate::commands::install_skills::unmanaged_report_lines(&paths, None) {
            println!("  {line}");
        }
    }

    // DOC-42 (issue #2889): a full `tm install` already deploys every bundled
    // skill (unfiltered into the managed tier above, #7102), so no co-deploy
    // `select` override is needed —
    // but a `skills:` entry declared by an agent that resolves in NO tier at
    // all is still a real gap worth surfacing, so run the same resolution
    // logging `session_launch` uses.
    let bundled_stems = trusty_mpm::core::skill_tiers::list_source_stems(&paths.skill_source_dir())
        .unwrap_or_default();
    let user_stems =
        trusty_mpm::core::skill_tiers::list_source_stems(&paths.user_skill_source_dir())
            .unwrap_or_default();
    let project_stems =
        trusty_mpm::core::skill_tiers::list_project_custom_stems(&paths.claude_skills_dir())
            .unwrap_or_default();
    let dangling = trusty_mpm::core::agent_skill_codeploy::log_declared_skills(
        &deploy.declared_skills,
        &project_stems,
        &user_stems,
        &bundled_stems,
    );
    for (agent, skill) in &dangling {
        println!(
            "  warning: agent `{agent}` declares skill `{skill}`, but it was not found in any tier"
        );
    }

    // Wire the MPM lifecycle hooks into every Claude settings file on the
    // machine. Per-file failures are non-fatal so one bad file does not sink
    // the whole install. Uses absolute-path hook commands resolved from the
    // currently-running binary so hooks fire even in stripped-PATH environments.
    if let Err(e) = install_claude_hooks() {
        eprintln!("warning: failed to install Claude Code hooks: {e:#}");
    }

    println!("Framework installed. Run `trusty-mpm daemon` to start.");
    Ok(())
}

/// Idempotently install the MPM `PreToolUse` / `PostToolUse` / `Stop` hook
/// triad into the tm-owned managed `CLAUDE_CONFIG_DIR` settings file — and
/// ONLY there.
///
/// Why (issue #2940): before this fix, `tm install` discovered and mutated
/// EVERY `.claude/settings.json` / `settings.local.json` reachable under
/// `$HOME` (via `discover_claude_settings`'s depth-8 walk), wiring tm hooks
/// into every project it found on the machine — including projects never
/// launched via tm. That was a one-way street (a contaminated project could
/// no longer cleanly go back to claude-mpm, since the tm entries lived in a
/// file the project itself owns) and could silently start firing tm hooks
/// alongside a project's own pre-existing claude-mpm hooks. Per the DOC-34
/// managed-session launch model, every daemon-managed spawn already points
/// `CLAUDE_CONFIG_DIR` at `managed_claude_config_dir()` — that is the ONE
/// place tm hooks belong. (`tm hooks clean`, in `commands::hooks`, is the
/// cleanup path for settings files a pre-fix `tm install` already
/// contaminated.)
/// What: resolves [`trusty_mpm::core::trusty_tools_config::managed_claude_config_dir`],
/// deep-merges the MPM hook triad into `<that dir>/settings.json` via
/// [`trusty_mpm::core::standalone::hooks::write_project_hooks`] (idempotent —
/// running it twice produces an identical file), and reports whether it
/// changed. Returns `1` when the file was created/updated, `0` when already
/// current. Errors only when the home directory cannot be resolved at all.
/// Test: `install_claude_hooks_at_writes_only_the_managed_config_dir`,
/// `install_claude_hooks_at_is_idempotent`,
/// `install_claude_hooks_at_never_touches_a_sibling_project_dir`.
pub(crate) fn install_claude_hooks() -> anyhow::Result<usize> {
    let config_dir = trusty_mpm::core::trusty_tools_config::managed_claude_config_dir()
        .ok_or_else(|| anyhow::anyhow!("could not resolve home directory"))?;
    let config = trusty_mpm::core::config::MpmConfig::load_default();
    install_claude_hooks_at_with_pm_guard(
        &config_dir,
        trusty_mpm::core::standalone::hooks::resolve_current_exe(),
        // #8392: the opt-in lives in the user-level config only.
        config.notification_hook.enabled,
        // #9018: `[pm_guard] enabled = false` strips a guard entry here too.
        config.pm_guard.enabled,
    )
}

/// Hermetic worker behind [`install_claude_hooks`] — writes the MPM hook
/// triad into `<config_dir>/settings.json`.
///
/// Why: separated so tests can point `config_dir` at a `tempfile::TempDir`
/// instead of the real tm-owned config home, and so the caller's home-
/// resolution failure is the only thing [`install_claude_hooks`] adds.
/// #7244: `exe` moved out of here for the same reason `config_dir` did — a test
/// process's own binary is a build artifact the writer refuses, and a CI runner
/// has no installed `tm` for the PATH fallback, so resolving it here made these
/// tests assert whether the host has `tm` installed. Production still passes
/// [`trusty_mpm::core::standalone::hooks::resolve_current_exe`].
/// What: calls [`trusty_mpm::core::standalone::hooks::write_project_hooks`]
/// against `<config_dir>/settings.json` with `exe` as the pinned binary, and
/// prints a status line. #8392: then writes (`notify`) or removes tm's opt-in
/// `Notification` entry through
/// [`trusty_mpm::core::standalone::hooks::notification::apply_notification_hook`].
/// Test: see [`install_claude_hooks`]'s test list, plus
/// `opt_in_off_installs_exactly_the_six_lifecycle_events` and
/// `opt_in_on_installs_one_notification_entry_idempotently`.
#[cfg_attr(not(test), allow(dead_code))]
fn install_claude_hooks_at(
    config_dir: &std::path::Path,
    exe: Option<std::path::PathBuf>,
    notify: bool,
) -> anyhow::Result<usize> {
    // #9018: `true` is the pre-existing behaviour.
    install_claude_hooks_at_with_pm_guard(config_dir, exe, notify, true)
}

/// [`install_claude_hooks_at`], honouring `[pm_guard] enabled` (#9018).
///
/// Why: an additive seam rather than a fourth parameter, because every test of
/// the function above asserts a shape this flag does not change.
/// What: `pm_guard_enabled = false` routes the lifecycle write through
/// [`trusty_mpm::core::standalone::hooks::write_project_hooks_honoring_pm_guard`],
/// which also strips a `hook --pm-guard` entry; `true` is unchanged.
/// Test: `install_with_the_guard_off_strips_an_existing_guard_entry`.
fn install_claude_hooks_at_with_pm_guard(
    config_dir: &std::path::Path,
    exe: Option<std::path::PathBuf>,
    notify: bool,
    pm_guard_enabled: bool,
) -> anyhow::Result<usize> {
    use colored::Colorize;

    let settings_path = config_dir.join("settings.json");
    println!(
        "Wiring MPM hooks into the tm-owned settings at {}…",
        settings_path.display()
    );

    // #8392: the lifecycle write first, so the opt-in step reads what it left.
    let written = trusty_mpm::core::standalone::hooks::write_project_hooks_honoring_pm_guard(
        &settings_path,
        exe.as_deref(),
        pm_guard_enabled,
    )
    .and_then(|changed| {
        trusty_mpm::core::standalone::hooks::notification::apply_notification_hook(
            &settings_path,
            exe.as_deref(),
            notify,
        )
        .map(|notified| changed || notified)
    });
    match written {
        Ok(true) => {
            println!("  {} {}", "✓".green(), settings_path.display());
            Ok(1)
        }
        Ok(false) => {
            println!(
                "  {} {} {}",
                "↻".cyan(),
                settings_path.display().to_string().dimmed(),
                "(already configured)".dimmed()
            );
            Ok(0)
        }
        Err(e) => {
            eprintln!(
                "  {} {} {}",
                "✗".red(),
                settings_path.display(),
                format!("({e})").red()
            );
            Err(e)
        }
    }
}

/// Write project-scoped MPM hooks into `<project_dir>/.claude/settings.json`.
///
/// Why: project-scoped hooks fire only inside the specific project directory,
/// so they cannot break unrelated build environments or foreign repos —
/// unlike global hooks. This mirrors trusty-memory's approach of writing
/// project-local hook entries at session start.
/// What: resolves `<project_dir>/.claude/settings.json` and calls
/// [`trusty_mpm::core::standalone::hooks::write_project_hooks`] with the
/// absolute exe path from `current_exe()`. Returns `true` if the file was
/// updated (new or changed), `false` when already configured. #9018: honours
/// `[pm_guard] enabled` from the user config, stripping a guard entry when off.
/// Test: `test_write_project_hooks_for_dir_targets_project_dir` in
/// `tests_behavior_a_tests.rs`.
pub(crate) fn write_project_hooks_for_dir(
    project_dir: &std::path::Path,
    exe_override: Option<&std::path::Path>,
) -> anyhow::Result<bool> {
    let settings_path = project_dir.join(".claude").join("settings.json");
    // #7244: `None` resolves the running installed binary and now REFUSES a
    // build artifact outright rather than baking its path in. A test passes a
    // path so it exercises the write, not the host's `tm` installation.
    let exe = exe_override
        .map(std::path::Path::to_path_buf)
        .or_else(trusty_mpm::core::standalone::hooks::resolve_current_exe);
    // #9018: `[pm_guard] enabled = false` also strips a guard entry.
    trusty_mpm::core::standalone::hooks::write_project_hooks_honoring_pm_guard(
        &settings_path,
        exe.as_deref(),
        trusty_mpm::core::config::MpmConfig::load_default()
            .pm_guard
            .enabled,
    )
}

// #7244: the test-only `mpm_hook_additions` wrapper is gone. Its one caller
// (`tests_behavior_a::mpm_hook_additions_is_idempotent_under_merge`) now pins
// the binary through `mpm_hook_additions_with_exe`, because the no-override
// form resolves `current_exe()` — under `cargo test` a build artifact the
// resolver refuses — and a wrapper that only forwards `None` had nothing left
// to forward.

/// Render per-file status lines for an agent `DeployResult`.
///
/// Why: `install` and `session start` both print agent deploy results; one
/// formatter keeps the output identical and the call sites small. Issue
/// #2504 added silent adoption of untracked-but-identical files and a
/// dedicated warning for untracked files that differ from the bundle — the
/// operator needs to see both without 36 near-duplicate warning lines.
/// What: a `✓ <file> (composed: a → b → c)` line per deployed agent, a
/// `◆ <file> (adopted — untracked, now managed)` line per silently-adopted
/// file, a `~ <file> (skipped — user-modified)` line per skipped file the
/// manifest already tracked with a diverged checksum, a `= <file>
/// (unchanged)` line per already-current file, and — ONLY when
/// `DeployResult::untracked_modified` is non-empty — one trailing summary
/// line naming the count and `tm install --reset-agents` (not one line per
/// file, to stay readable when dozens of files predate manifest tracking).
/// Test: covered indirectly by `install_writes_all_artifacts`.
pub(crate) fn deploy_report_lines(
    deploy: &trusty_mpm::core::agent_deployer::DeployResult,
    source_dir: &std::path::Path,
) -> Vec<String> {
    let mut lines = Vec::new();
    for file in &deploy.deployed {
        let name = file.trim_end_matches(".md");
        let chain = trusty_mpm::core::agent_builder::source_chain(name, source_dir)
            .map(|c| c.join(" \u{2192} "))
            .unwrap_or_else(|_| name.to_string());
        lines.push(format!("\u{2713} {file} (composed: {chain})"));
    }
    for file in &deploy.adopted {
        lines.push(format!(
            "\u{25c6} {file} (adopted \u{2014} untracked, now managed)"
        ));
    }
    for file in &deploy.skipped {
        if deploy.untracked_modified.contains(file) {
            lines.push(format!(
                "~ {file} (skipped \u{2014} untracked, differs from bundle)"
            ));
        } else {
            lines.push(format!("~ {file} (skipped \u{2014} user-modified)"));
        }
    }
    for file in &deploy.unchanged {
        lines.push(format!("= {file} (unchanged)"));
    }
    if !deploy.untracked_modified.is_empty() {
        lines.push(format!(
            "\u{26a0} {} untracked agent file(s) differ from the bundle and were skipped; \
             run `tm install --reset-agents` to review and reconcile them.",
            deploy.untracked_modified.len()
        ));
    }
    // DOC-42 issue #2906 review (CRITICAL finding): a per-agent compose
    // failure no longer aborts the whole deploy — surface it here so it is
    // never silently dropped from `tm install`'s output.
    for failure in &deploy.failed {
        lines.push(format!(
            "\u{2717} {failure} (agent skipped, compose failed)"
        ));
    }
    lines
}

/// Render per-file status lines plus a summary for a `ResetResult`.
///
/// Why: `--reset-agents` (issue #2504) can touch dozens of files at once;
/// the operator needs both the per-file detail and a one-line total so a
/// large reset is scannable at a glance.
/// What: a `\u{27f3} <file> (recomposed, backed up)` or `\u{27f3} <file>
/// (recomposed)` line per rewritten file, a `\u{25c6} <file> (adopted)` line
/// per already-current file, a `? <name>` line per requested name with no
/// matching source agent, and — for a `reset_project_agents`
/// result (issue #2508) — a `\u{2298} <name> (excluded by this workspace's
/// manifest)` line per requested name the target's own harness plan rejected,
/// followed by one summary line with all five totals.
/// Test: `reset_report_lines_summarizes_counts`,
/// `reset_report_lines_shows_deselected`.
pub(crate) fn reset_report_lines(
    reset: &trusty_mpm::core::agent_reset::ResetResult,
) -> Vec<String> {
    let mut lines = Vec::new();
    for file in &reset.recomposed {
        if reset.backed_up.contains(file) {
            lines.push(format!("\u{27f3} {file} (recomposed, backed up)"));
        } else {
            lines.push(format!("\u{27f3} {file} (recomposed)"));
        }
    }
    for file in &reset.adopted {
        lines.push(format!("\u{25c6} {file} (adopted)"));
    }
    for name in &reset.not_found {
        lines.push(format!("? {name} (no matching source agent)"));
    }
    for name in &reset.deselected {
        lines.push(format!(
            "\u{2298} {name} (excluded by this workspace's manifest)"
        ));
    }
    // #4409: the workspace sweep removes shadowing bundled copies rather than
    // recomposing them, so its removals need their own line.
    for file in &reset.retracted {
        lines.push(format!(
            "\u{2717} {file} (retracted — bundled agents live in the tm-managed config dir)"
        ));
    }
    lines.push(format!(
        "reset summary: {} recomposed, {} adopted, {} backed up, {} not found, \
         {} deselected, {} retracted",
        reset.recomposed.len(),
        reset.adopted.len(),
        reset.backed_up.len(),
        reset.not_found.len(),
        reset.deselected.len(),
        reset.retracted.len()
    ));
    lines
}

/// Render a `deploy_skills` result into human-readable status lines.
///
/// Why: skills deploy alongside agents during `tm install`, and the operator
/// needs the same per-file feedback (deployed / skipped / unchanged) so a
/// fresh install visibly populates `~/.claude/skills/` instead of leaving the
/// directory silently empty (#386). Unlike agents, skills carry no inheritance,
/// so there is no composed-chain to show — a plain content copy is reported.
/// What: maps each filename in the `DeployStats` vectors to a status line
/// using the same glyph vocabulary as [`deploy_report_lines`] (`\u{2713}`
/// deployed, `~` skipped, `=` unchanged).
/// Test: `install_then_deploy_deploys_skills` asserts a deployed skill line
/// is emitted.
pub(crate) fn skill_report_lines(
    deploy: &trusty_mpm::core::skill_deployer::DeployStats,
) -> Vec<String> {
    let mut lines = Vec::new();
    for file in &deploy.deployed {
        lines.push(format!("\u{2713} {file}"));
    }
    for file in &deploy.skipped {
        lines.push(format!("~ {file} (skipped \u{2014} user-modified)"));
    }
    for file in &deploy.unchanged {
        lines.push(format!("= {file} (unchanged)"));
    }
    lines
}

/// Write one bundled artifact to `dest` according to its declared
/// [`InstallPolicy`](trusty_mpm::core::bundle::InstallPolicy), returning its
/// report line.
///
/// Why: split out of [`install_to`] so the two policy branches are testable
/// against a synthetic [`BundledArtifact`](trusty_mpm::core::bundle::BundledArtifact)
/// fixture without adding a fake entry to the real
/// [`ALL`](trusty_mpm::core::bundle::ALL) table (issue #3381 — no shipped
/// artifact currently uses `SeedOnce`).
/// What: [`Overwrite`](trusty_mpm::core::bundle::InstallPolicy::Overwrite)
/// always writes the embedded contents, replacing any existing file — this is
/// what makes `tm install` actually deliver refreshed framework assets on
/// upgrade; `force` has no additional effect. `SeedOnce` writes only if
/// `dest` is absent, preserving a user edit across upgrades; `force` is the
/// escape hatch that resets it back to the shipped default. The report line
/// distinguishes written / refreshed / preserved-user-file outcomes so
/// `tm install` output stays honest about what it did and did not touch.
/// Test: `overwrite_artifact_refreshes_modified_file_without_force`,
/// `seed_once_artifact_is_not_clobbered_without_force`,
/// `seed_once_artifact_force_resets_to_shipped_default`.
pub(crate) fn install_one(
    dest: &std::path::Path,
    artifact: &trusty_mpm::core::bundle::BundledArtifact,
    force: bool,
) -> anyhow::Result<String> {
    use trusty_mpm::core::bundle::InstallPolicy;

    let exists = dest.exists();
    match artifact.install {
        InstallPolicy::Overwrite => overwrite_one(dest, artifact.rel_path, artifact.contents),
        InstallPolicy::SeedOnce => {
            if exists && !force {
                return Ok(format!(
                    "- {} (exists, preserved \u{2014} user-owned)",
                    artifact.rel_path
                ));
            }
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(dest, artifact.contents)?;
            Ok(if exists {
                format!("\u{2713} {} (reset to shipped default)", artifact.rel_path)
            } else {
                format!("\u{2713} {}", artifact.rel_path)
            })
        }
    }
}

/// Write `contents` to `dest`, replacing any existing file, and return the
/// report line naming `rel_path`.
///
/// Why (#9012): the [`InstallPolicy::Overwrite`](trusty_mpm::core::bundle::InstallPolicy::Overwrite)
/// write, shared with the runtime-content files (skills, docs) that are not
/// `'static` and so cannot be a [`BundledArtifact`](trusty_mpm::core::bundle::BundledArtifact).
/// Test: `overwrite_artifact_refreshes_modified_file_without_force`.
fn overwrite_one(dest: &std::path::Path, rel_path: &str, contents: &str) -> anyhow::Result<String> {
    let exists = dest.exists();
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(dest, contents)?;
    Ok(if exists {
        format!("\u{2713} {rel_path} (refreshed)")
    } else {
        format!("\u{2713} {rel_path}")
    })
}

/// Write every bundled artifact under `paths`, returning a per-file report.
///
/// Why: separating the filesystem work from argument parsing and stdout makes
/// the installer unit-testable against a `tempfile::TempDir`.
/// What: resolves the agent roster (#9011) and the skills and bundled docs
/// (#9012) from content and hands them to [`install_to_with`]. #9396: with no
/// content installed and no checkout, the content release is fetched once
/// first. With no content resolvable this fails before writing anything, and
/// the error names `tm content update` and the offline `tm content install`.
/// Test: `install_writes_all_artifacts`,
/// `install_without_content_fails_naming_tm_content_install`,
/// `install_with_a_failed_first_use_fetch_fails_closed`.
pub(crate) fn install_to(
    paths: &trusty_mpm::core::paths::FrameworkPaths,
    force: bool,
) -> anyhow::Result<Vec<String>> {
    install_to_resolving(
        paths,
        force,
        // #9396: the gate fetches the content release on first use.
        trusty_mpm::core::content_source::agent_roster_or_fetch,
        trusty_mpm::core::content_source::framework_content,
    )
}

/// [`install_to`] with the content resolvers given, so a test can point them
/// at an empty cache.
pub(crate) fn install_to_resolving(
    paths: &trusty_mpm::core::paths::FrameworkPaths,
    force: bool,
    roster: impl FnOnce() -> Result<
        trusty_mpm::core::content_source::AgentRoster,
        trusty_mpm::core::content_source::AgentContentError,
    >,
    content: impl FnOnce() -> Result<
        trusty_mpm::core::content_source::FrameworkContent,
        trusty_mpm::core::content_source::AgentContentError,
    >,
) -> anyhow::Result<Vec<String>> {
    // #9011/#9012: resolve before writing anything, so missing content leaves
    // the framework tree untouched.
    let roster = roster()?;
    let content = content()?;
    install_to_with(paths, force, &roster, &content)
}

/// The bundled docs `tm install` writes under `<framework>/docs/`, as content
/// paths relative to `instructions/` (#9012: runtime content).
const BUNDLED_DOCS: [&str; 2] = [
    "docs/WHAT-IS-TRUSTY-MPM.md",
    "docs/ARCHITECTURE-MEMORY-SESSIONS-SEARCH.md",
];

/// [`install_to`] with the roster and content given.
///
/// What: for each [`trusty_mpm::core::bundle::ALL`] artifact (the hook
/// policies), resolves its destination under `paths.framework` and delegates
/// the policy-driven write to [`install_one`]; then writes every content skill
/// into `<framework>/skills/`, the bundled docs into `<framework>/docs/`
/// (#9012; a content source without them fails, naming the missing path), and
/// every roster file into `<framework>/agents/`. Content is framework-owned,
/// so always overwritten.
/// Test: `install_writes_all_artifacts`.
pub(crate) fn install_to_with(
    paths: &trusty_mpm::core::paths::FrameworkPaths,
    force: bool,
    roster: &trusty_mpm::core::content_source::AgentRoster,
    content: &trusty_mpm::core::content_source::FrameworkContent,
) -> anyhow::Result<Vec<String>> {
    // #9012: check the docs first, so a source without them writes nothing.
    let docs = BUNDLED_DOCS
        .iter()
        .map(|rel| Ok((*rel, content.require_instruction(rel)?)))
        .collect::<Result<Vec<_>, trusty_mpm::core::content_source::AgentContentError>>()?;
    let mut report = Vec::new();
    for artifact in trusty_mpm::core::bundle::ALL {
        let dest = paths.framework.join(artifact.rel_path);
        report.push(install_one(&dest, artifact, force)?);
    }
    for (rel_path, contents) in content.skills().chain(docs.iter().copied()) {
        report.push(overwrite_one(
            &paths.framework.join(rel_path),
            rel_path,
            contents,
        )?);
    }
    for name in roster.materialize(&paths.framework.join("agents"))? {
        report.push(format!("\u{2713} agents/{name}"));
    }
    Ok(report)
}

/// The checkout's agent roster, for bin-target tests that install.
#[cfg(test)]
pub(crate) fn test_roster() -> trusty_mpm::core::content_source::AgentRoster {
    // #9298: the runtime checkout, never the compile-time build path.
    let root = trusty_common::test_harness::test_repo_root().expect("resolve the checkout");
    let content =
        trusty_agents_common::agent_content::checkout_content(&root).expect("repo content");
    trusty_mpm::core::content_source::AgentRoster::load(&content).expect("repo roster")
}

/// The checkout's skills and instructions, for bin-target tests that install
/// (#9012).
#[cfg(test)]
pub(crate) fn test_content() -> trusty_mpm::core::content_source::FrameworkContent {
    // #9298: the runtime checkout, never the compile-time build path.
    let root = trusty_common::test_harness::test_repo_root().expect("resolve the checkout");
    let content =
        trusty_agents_common::agent_content::checkout_content(&root).expect("repo content");
    trusty_mpm::core::content_source::FrameworkContent::load(&content).expect("repo content")
}

/// [`test_content`], loaded once per test binary (#9012).
#[cfg(test)]
pub(crate) fn test_content_ref() -> &'static trusty_mpm::core::content_source::FrameworkContent {
    static CONTENT: std::sync::OnceLock<trusty_mpm::core::content_source::FrameworkContent> =
        std::sync::OnceLock::new();
    CONTENT.get_or_init(test_content)
}

/// One skill file's text from the checkout's content (#9012).
#[cfg(test)]
pub(crate) fn test_skill(path: &str) -> &'static str {
    test_content_ref()
        .skill(path)
        .unwrap_or_else(|| panic!("{path} must be in the content tree"))
}

/// [`test_roster`], loaded once per test binary, for classifier tests.
#[cfg(test)]
pub(crate) fn test_roster_ref() -> &'static trusty_mpm::core::content_source::AgentRoster {
    static ROSTER: std::sync::OnceLock<trusty_mpm::core::content_source::AgentRoster> =
        std::sync::OnceLock::new();
    ROSTER.get_or_init(test_roster)
}

// Unit tests live in install_tests.rs (test-file budget: 1500 SLOC).
#[cfg(test)]
#[path = "install_tests.rs"]
mod tests;
