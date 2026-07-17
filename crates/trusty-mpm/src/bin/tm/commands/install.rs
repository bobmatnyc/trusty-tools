//! `install` command handler and framework-artifact helpers.
//!
//! Why: the install handler is self-contained — it writes bundled artifacts,
//! assembles the PM prompt, deploys agents and skills, and wires Claude Code
//! hooks — and benefits from its own file so it stays reviewable.
//! What: `install`, `install_claude_hooks`, `mpm_hook_additions`, `install_to`,
//! `deploy_report_lines`, `reset_report_lines`, `skill_report_lines`,
//! `remove_global_trusty_mpm_hooks`, `write_project_hooks_for_dir`.
//! Test: `install_writes_all_artifacts`, `install_skips_existing_without_force`,
//! `install_claude_hooks_is_idempotent`, `install_then_deploy_deploys_skills`.

/// `install` subcommand — deploy the bundled framework artifacts and wire
/// MPM lifecycle hooks into every Claude Code settings file.
///
/// Why: a fresh machine has no `~/.trusty-mpm/framework/`; `trusty-mpm install`
/// writes the compile-time-embedded artifacts (optimizer policy, framework
/// instructions, placeholder agent/skill) so the daemon has a working policy
/// and launchers have instructions to point sessions at. It also assembles
/// the full PM system prompt (overwriting the bundle stub — fixes #383),
/// deploys composed agents and skills, and wires MPM lifecycle hooks into
/// every Claude Code settings file. All edits are idempotent.
///
/// `reset_agents` (issue #2504) is the explicit reconciliation path for
/// composed agent files a normal deploy conservatively skips because they
/// predate per-file manifest tracking. `Some(vec![])` (the CLI's `--reset-
/// agents` with no value) resets every bundled agent; `Some(names)` restricts
/// the reset to those agents. The normal deploy always runs first — reset
/// only forces the specific files a plain deploy would otherwise leave
/// alone, and re-registers everything else it touches.
///
/// `reset_agents_workspaces` (issue #2508) additionally sweeps every intact
/// session workspace's project-local `.claude/agents/` through the SAME
/// reset, each gated by that workspace's OWN resolved harness plan (so an
/// agent one project's manifest excludes is never resurrected there — the
/// #2462 cross-warning). Only takes effect when `reset_agents` is `Some`.
/// What: resolves [`FrameworkPaths::default`], calls [`install_to`] then
/// [`install_system_prompt_to`] to write the real assembled PM prompt,
/// deploys agents and skills, optionally resets the requested agent scope
/// (user-level, then every intact session workspace when requested), then
/// calls [`install_claude_hooks`].
/// Test: `install_writes_all_artifacts`, `install_skips_existing_without_force`,
/// `install_claude_hooks_is_idempotent`,
/// `instruction_pipeline::install_system_prompt_to_writes_assembled`.
pub(crate) async fn install(
    force: bool,
    reset_agents: Option<Vec<String>>,
    reset_agents_workspaces: bool,
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

    // Overwrite the bundle stub with the fully assembled PM prompt (#383).
    match trusty_mpm::core::instruction_pipeline::install_system_prompt_to(
        &paths.framework_instructions_path(),
    ) {
        Ok(()) => println!("  \u{2713} instructions/INSTRUCTIONS.md (assembled)"),
        Err(e) => eprintln!("warning: failed to assemble system prompt: {e:#}"),
    }

    println!(
        "Composing agents into {}",
        paths.claude_agents_dir().display()
    );
    let deploy = trusty_mpm::core::agent_deployer::deploy_agents(
        &paths.agent_source_dir(),
        &paths.claude_agents_dir(),
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
            paths.claude_agents_dir().display()
        );
        let reset = trusty_mpm::core::agent_reset::reset_agents(
            &paths.agent_source_dir(),
            &paths.claude_agents_dir(),
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

    // Deploy bundled + user-custom skills into `~/.claude/skills/`. The bundle
    // install above (`install_to`) already wrote the skill sources under the
    // framework root, so the source dir is populated before this copy runs —
    // mirroring the agent ordering. Without this step a fresh `tm install`
    // left the skills directory empty and `tm doctor` reported `skills: Fail`
    // (#386); skills only deployed lazily on `tm session start` (see
    // `prepare_session`). Routes through the SAME multi-tier orchestrator
    // `session_launch`/`apply_catalog` use (PR #2818 review round 3): `tm
    // install`'s destination (`paths.claude_skills_dir()`, home-rooted) is the
    // IDENTICAL directory the ordinary non-managed `tm session start`/`tm
    // launch` path deploys user-tier overrides into. A raw `deploy_skills`
    // call here would see a previously user-tier-deployed skill as "managed,
    // checksum matches" and silently refresh it back to bundled content on
    // every routine `tm install` — the same clobber bug fixed for `tm catalog
    // apply` in the prior review round, at a third, independent call site.
    println!(
        "Deploying skills into {}",
        paths.claude_skills_dir().display()
    );
    let skill_deploy = trusty_mpm::core::skill_tiers::deploy_all_skill_tiers(
        &paths.skill_source_dir(),
        &paths.user_skill_source_dir(),
        &paths.claude_skills_dir(),
        |_| true,
    )?
    .stats;
    for line in skill_report_lines(&skill_deploy) {
        println!("  {line}");
    }

    // DOC-42 (issue #2889): a full `tm install` already deploys every bundled
    // skill (`|_| true` above), so no co-deploy `select` override is needed —
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
/// block into every Claude Code settings file on the machine.
///
/// Why: without these hooks the daemon never receives Claude Code lifecycle
/// events, so the circuit breaker, audit log, and dashboard all sit blind.
/// `tm install` is the canonical point to wire them up — running it twice
/// must produce identical files (idempotency requirement). The hook command
/// is emitted as an absolute path to the running binary so it fires even in
/// build shells where `~/.cargo/bin` is not on `$PATH`.
/// What: discovers every `.claude/settings*.json` under `$HOME` via
/// [`trusty_common::claude_config::discover_claude_settings`], loads each
/// one, deep-merges the MPM hook block (with absolute binary path) using
/// [`trusty_common::claude_config::merge_hook_entries`], and writes the
/// result back atomically when it differs from disk. Falls back to creating
/// `~/.claude/settings.json` when no settings files exist. Returns a count
/// of files modified for the caller to report.
/// Test: `install_claude_hooks_is_idempotent`,
/// `install_claude_hooks_creates_fallback`.
pub(crate) fn install_claude_hooks() -> anyhow::Result<usize> {
    use colored::Colorize;
    use trusty_common::claude_config::{
        default_settings_max_depth, discover_claude_settings, merge_hook_entries, write_json_atomic,
    };

    let home =
        dirs::home_dir().ok_or_else(|| anyhow::anyhow!("could not resolve home directory"))?;
    println!(
        "Wiring MPM hooks into Claude Code settings under {}…",
        home.display()
    );

    // Resolve the absolute binary path once at install time.
    let exe = trusty_mpm::core::standalone::hooks::resolve_current_exe();
    let additions = mpm_hook_additions_with_exe(exe.as_deref());
    let files = discover_claude_settings(&home, default_settings_max_depth());

    let target_files: Vec<std::path::PathBuf> = if files.is_empty() {
        let fallback = home.join(".claude").join("settings.json");
        println!("  no settings files found; creating {}", fallback.display());
        vec![fallback]
    } else {
        files
    };

    let mut changed = 0usize;
    for path in &target_files {
        let original: serde_json::Value = match std::fs::read_to_string(path) {
            Ok(s) if s.trim().is_empty() => serde_json::Value::Object(serde_json::Map::new()),
            Ok(s) => match serde_json::from_str(&s) {
                Ok(v) => v,
                Err(e) => {
                    eprintln!(
                        "  {} {} {}",
                        "✗".red(),
                        path.display(),
                        format!("(parse error: {e})").red()
                    );
                    continue;
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                serde_json::Value::Object(serde_json::Map::new())
            }
            Err(e) => {
                eprintln!(
                    "  {} {} {}",
                    "✗".red(),
                    path.display(),
                    format!("(read error: {e})").red()
                );
                continue;
            }
        };
        let merged = merge_hook_entries(&original, &additions);
        if merged == original {
            println!(
                "  {} {} {}",
                "↻".cyan(),
                path.display().to_string().dimmed(),
                "(already configured)".dimmed()
            );
            continue;
        }
        match write_json_atomic(path, &merged) {
            Ok(()) => {
                changed += 1;
                println!("  {} {}", "✓".green(), path.display());
            }
            Err(e) => {
                eprintln!(
                    "  {} {} {}",
                    "✗".red(),
                    path.display(),
                    format!("({e})").red()
                );
            }
        }
    }

    if changed > 0 {
        println!(
            "  installed MPM hooks in {} settings file{}.",
            changed,
            if changed == 1 { "" } else { "s" }
        );
    }
    Ok(changed)
}

/// Strip trusty-mpm global hook entries from all discovered Claude settings files.
///
/// Why: `tm install` previously wrote hooks into every discovered global
/// settings file. After switching to project-scoped hooks, these global
/// entries must be cleaned up so hooks no longer fire in unrelated projects
/// (mirrors `remove_global_trusty_memory_hooks` in trusty-memory). Non-fatal
/// per-file errors are swallowed so one bad file does not abort cleanup.
/// What: delegates to
/// [`trusty_mpm::core::standalone::hooks::remove_global_trusty_mpm_hooks`].
/// Returns the count of files modified.
/// Test: see `hooks.rs` tests for the underlying logic; this is a thin
/// re-export.
pub(crate) fn remove_global_trusty_mpm_hooks() -> anyhow::Result<usize> {
    trusty_mpm::core::standalone::hooks::remove_global_trusty_mpm_hooks()
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
/// updated (new or changed), `false` when already configured.
/// Test: `test_write_project_hooks_for_dir_targets_project_dir` in
/// `tests_behavior_a.rs`.
pub(crate) fn write_project_hooks_for_dir(project_dir: &std::path::Path) -> anyhow::Result<bool> {
    let settings_path = project_dir.join(".claude").join("settings.json");
    let exe = trusty_mpm::core::standalone::hooks::resolve_current_exe();
    trusty_mpm::core::standalone::hooks::write_project_hooks(&settings_path, exe.as_deref())
}

/// Build the MPM lifecycle hook additions JSON block.
///
/// Why: every call site (the install handler and its unit test) needs the
/// exact same shape so [`merge_hook_entries`] can dedup by deep equality;
/// delegating to the shared library definition avoids two slightly-different
/// copies racing in the idempotency check. The canonical definition lives in
/// [`trusty_mpm::core::standalone::hooks::mpm_hook_additions`] so the managed
/// driver (`ensure_global_config_dir`) and the install path both use the same
/// literal (WI-3).
/// What: delegates to the shared library function with no exe override.
/// Test: covered indirectly by `install_claude_hooks_is_idempotent`.
/// Only used in tests (idempotency / merge-shape tests in `tests_behavior_a.rs`).
#[cfg(test)]
pub(crate) fn mpm_hook_additions() -> serde_json::Value {
    trusty_mpm::core::standalone::hooks::mpm_hook_additions()
}

/// Build the MPM hook additions block with an explicit exe path.
///
/// Why: `install_claude_hooks` resolves the binary path once and passes it
/// through so every settings file receives the same absolute command.
/// What: delegates to
/// [`trusty_mpm::core::standalone::hooks::mpm_hook_additions_with_exe`].
/// Test: covered by `install_claude_hooks_is_idempotent`.
pub(crate) fn mpm_hook_additions_with_exe(
    exe_override: Option<&std::path::Path>,
) -> serde_json::Value {
    trusty_mpm::core::standalone::hooks::mpm_hook_additions_with_exe(exe_override)
}

/// Render per-file status lines for an agent [`DeployResult`].
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
/// [`DeployResult::untracked_modified`] is non-empty — one trailing summary
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
    lines
}

/// Render per-file status lines plus a summary for a [`ResetResult`].
///
/// Why: `--reset-agents` (issue #2504) can touch dozens of files at once;
/// the operator needs both the per-file detail and a one-line total so a
/// large reset is scannable at a glance.
/// What: a `\u{27f3} <file> (recomposed, backed up)` or `\u{27f3} <file>
/// (recomposed)` line per rewritten file, a `\u{25c6} <file> (adopted)` line
/// per already-current file, a `? <name>` line per requested name with no
/// matching source agent, and — for a [`reset_project_agents`](trusty_mpm::core::agent_reset::reset_project_agents)
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
    lines.push(format!(
        "reset summary: {} recomposed, {} adopted, {} backed up, {} not found, {} deselected",
        reset.recomposed.len(),
        reset.adopted.len(),
        reset.backed_up.len(),
        reset.not_found.len(),
        reset.deselected.len()
    ));
    lines
}

/// Render a [`deploy_skills`] result into human-readable status lines.
///
/// Why: skills deploy alongside agents during `tm install`, and the operator
/// needs the same per-file feedback (deployed / skipped / unchanged) so a
/// fresh install visibly populates `~/.claude/skills/` instead of leaving the
/// directory silently empty (#386). Unlike agents, skills carry no inheritance,
/// so there is no composed-chain to show — a plain content copy is reported.
/// What: maps each filename in the [`DeployStats`] vectors to a status line
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

/// Write every bundled artifact under `paths`, returning a per-file report.
///
/// Why: separating the filesystem work from argument parsing and stdout makes
/// the installer unit-testable against a `tempfile::TempDir`.
/// What: for each [`trusty_mpm::core::bundle::ALL`] artifact, creates parent
/// directories and writes the file; an existing file is skipped unless `force`.
/// Returns one human-readable status line per artifact.
/// Test: `install_writes_all_artifacts`, `install_skips_existing_without_force`.
pub(crate) fn install_to(
    paths: &trusty_mpm::core::paths::FrameworkPaths,
    force: bool,
) -> anyhow::Result<Vec<String>> {
    let mut report = Vec::new();
    for artifact in trusty_mpm::core::bundle::ALL {
        let dest = paths.framework.join(artifact.rel_path);
        if dest.exists() && !force {
            report.push(format!("- {} (exists, skipped)", artifact.rel_path));
            continue;
        }
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&dest, artifact.contents)?;
        report.push(format!("\u{2713} {}", artifact.rel_path));
    }
    Ok(report)
}
