//! `install` command handler and framework-artifact helpers.
//!
//! Why: the install handler is self-contained — it writes bundled artifacts,
//! assembles the PM prompt, deploys agents and skills, and wires Claude Code
//! hooks — and benefits from its own file so it stays reviewable.
//! What: `install`, `install_claude_hooks`, `mpm_hook_additions`, `install_to`,
//! `deploy_report_lines`, `skill_report_lines`.
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
/// What: resolves [`FrameworkPaths::default`], calls [`install_to`] then
/// [`install_system_prompt_to`] to write the real assembled PM prompt,
/// deploys agents and skills, then calls [`install_claude_hooks`].
/// Test: `install_writes_all_artifacts`, `install_skips_existing_without_force`,
/// `install_claude_hooks_is_idempotent`,
/// `instruction_pipeline::install_system_prompt_to_writes_assembled`.
pub(crate) fn install(force: bool) -> anyhow::Result<()> {
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

    // Deploy bundled skills into `~/.claude/skills/`. The bundle install above
    // (`install_to`) already wrote the skill sources under the framework root,
    // so the source dir is populated before this copy runs — mirroring the
    // agent ordering. Without this step a fresh `tm install` left the skills
    // directory empty and `tm doctor` reported `skills: Fail` (#386); skills
    // only deployed lazily on `tm session start` (see `prepare_session`).
    println!(
        "Deploying skills into {}",
        paths.claude_skills_dir().display()
    );
    let skill_deploy = trusty_mpm::core::skill_deployer::deploy_skills(
        &paths.skill_source_dir(),
        &paths.claude_skills_dir(),
    )?;
    for line in skill_report_lines(&skill_deploy) {
        println!("  {line}");
    }

    // Wire the MPM lifecycle hooks into every Claude settings file on the
    // machine. Per-file failures are non-fatal so one bad file does not sink
    // the whole install.
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
/// must produce identical files (idempotency requirement).
/// What: discovers every `.claude/settings*.json` under `$HOME` via
/// [`trusty_common::claude_config::discover_claude_settings`], loads each
/// one, deep-merges the MPM hook block using
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

    let additions = mpm_hook_additions();
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

/// Build the MPM lifecycle hook additions JSON block.
///
/// Why: every call site (the install handler and its unit test) needs the
/// exact same shape so [`merge_hook_entries`] can dedup by deep equality;
/// centralising the literal avoids two slightly-different copies racing in
/// the idempotency check.
/// What: returns a JSON object with `PreToolUse`, `PostToolUse`, and `Stop`
/// arrays, each carrying one `{matcher: "*", hooks: [{type: "command",
/// command: "trusty-mpm hook", timeout: ...}]}` block. `PostToolUse` runs
/// asynchronously (Claude Code does not block on its completion) because
/// the daemon may take longer to ingest tool results; `PreToolUse` and
/// `Stop` keep the default synchronous behaviour with short timeouts.
/// Test: covered indirectly by `install_claude_hooks_is_idempotent`.
pub(crate) fn mpm_hook_additions() -> serde_json::Value {
    serde_json::json!({
        "hooks": {
            "PreToolUse": [{
                "matcher": "*",
                "hooks": [{
                    "type": "command",
                    "command": "trusty-mpm hook",
                    "timeout": 5
                }]
            }],
            "PostToolUse": [{
                "matcher": "*",
                "hooks": [{
                    "type": "command",
                    "command": "trusty-mpm hook",
                    "timeout": 60,
                    "async": true
                }]
            }],
            "Stop": [{
                "matcher": "*",
                "hooks": [{
                    "type": "command",
                    "command": "trusty-mpm hook",
                    "timeout": 5
                }]
            }]
        }
    })
}

/// Render per-file status lines for an agent [`DeployResult`].
///
/// Why: `install` and `session start` both print agent deploy results; one
/// formatter keeps the output identical and the call sites small.
/// What: a `✓ <file> (composed: a → b → c)` line per deployed agent, a
/// `~ <file> (skipped — user-modified)` line per skipped one, and a `=` line
/// per unchanged one; the chain comes from the agent's resolved source chain.
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
    for file in &deploy.skipped {
        lines.push(format!("~ {file} (skipped \u{2014} user-modified)"));
    }
    for file in &deploy.unchanged {
        lines.push(format!("= {file} (unchanged)"));
    }
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
