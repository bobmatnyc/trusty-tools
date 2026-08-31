//! `tm reinstall` — redeploy agents and skills everywhere, and optionally the
//! binary itself.
//!
//! Why: the assets are the point. A merged agent or skill change reaches a
//! running session only after both deploy hops have run into the destination
//! that session reads from, and different launchers read from different
//! directories — so "I reinstalled" has until now meant "one of them is
//! current". This command makes it mean all of them, and says per destination
//! what it did. The binary is secondary and opt-in: `--binary` never fires by
//! default, because replacing the executable is a different, riskier operation
//! than refreshing text files.
//! What: [`run`] resolves the real framework and standalone layouts,
//! runs [`trusty_mpm::core::reinstall::reinstall_assets`], prints a
//! per-destination table, and — only with `--binary` — routes through
//! [`trusty_mpm::core::binary_reinstall::reinstall_route`]. This file is CLI
//! plumbing and printing; the engine and the route decision are library code
//! with their own tests.
//! Test: `reinstall_route`'s decision table lives in
//! `core::binary_reinstall_tests`; the asset engine's in `core::reinstall_tests`.

use anyhow::Context as _;
use colored::Colorize;

use trusty_mpm::core::binary_reinstall::{ReinstallRoute, reinstall_route};
use trusty_mpm::core::reinstall::{AssetCounts, ReinstallReport, reinstall_assets};

/// `reinstall` subcommand — redeploy the bundled assets to every destination.
///
/// Why: see the module doc.
/// What: resolves [`trusty_mpm::core::paths::FrameworkPaths::default`] and the
/// standalone managed root, redeploys assets into every destination, prints the
/// report, then handles `--binary`. Returns an error when any destination
/// failed, so a partial reinstall never exits 0 under a success banner.
/// Test: the engine's behaviour is covered in `core::reinstall_tests`; this
/// wrapper is path resolution and printing.
pub(crate) async fn run(args: crate::cli::ReinstallArgs) -> anyhow::Result<()> {
    let crate::cli::ReinstallArgs { force, binary, yes } = args;
    let paths = trusty_mpm::core::paths::FrameworkPaths::default();
    let managed = super::managed_root::resolve_managed_paths(None)
        .context("could not resolve the standalone managed root")?;
    // The working directory is offered as a project tier, but
    // `reinstall_targets` accepts it only when `<cwd>/.claude/skills` already
    // exists — so a `tm reinstall` run from `$HOME` or `/tmp` refreshes a real
    // project's skills and never creates a stray `.claude/` anywhere else. See
    // that function's PROJECT TIER note.
    let project_dir = std::env::current_dir().ok();
    let backup_root = backup_root(&paths);

    println!(
        "Redeploying bundled agents and skills (source: {})",
        paths.framework.display()
    );
    let report = reinstall_assets(
        &paths,
        &managed.claude_config_dir,
        project_dir.as_deref(),
        force,
        &backup_root,
    );
    print_report(&report);

    if binary {
        reinstall_binary(yes).await?;
    } else {
        println!(
            "\n{}",
            "The binary was not touched — pass `--binary` to reinstall it too.".dimmed()
        );
    }

    if report.has_failures() {
        anyhow::bail!("one or more destinations could not be fully redeployed (see above)");
    }
    Ok(())
}

/// Where a `--force` clobber copies the files it is about to overwrite.
///
/// Why: the existing remediation-backup convention is
/// `~/.trusty-mpm/backup-<what>-<timestamp>/`, and an operator looking for a
/// file this command replaced should find it in the same place `tm doctor
/// --fix-skills` and `tm install --reconcile-skills` put theirs.
/// What: `<framework root>/backup-reinstall-<UTC yyyymmdd-HHMMSS>`.
/// Test: exercised through `reinstall_replaces_a_customized_skill_with_force`,
/// which asserts the backup ledger lands under the root it is given.
fn backup_root(paths: &trusty_mpm::core::paths::FrameworkPaths) -> std::path::PathBuf {
    let stamp = chrono::Utc::now().format("%Y%m%d-%H%M%S");
    paths.root.join(format!("backup-reinstall-{stamp}"))
}

/// Print one block per destination, plus a closing summary.
///
/// Why: "a reinstall that silently no-ops on one of three tiers is the exact
/// failure this command exists to end" — so every destination gets a line
/// whether or not anything happened there.
/// What: the hop-1 refresh state, then per destination the agent and skill
/// counts and any notes.
/// Test: printing only; the counts it renders are asserted in
/// `core::reinstall_tests`.
fn print_report(report: &ReinstallReport) {
    for note in &report.notes {
        eprintln!("  {} {note}", "!".yellow());
    }
    println!(
        "  agent source: {}   skill source: {}",
        refreshed(report.agent_source_refreshed),
        refreshed(report.skill_source_refreshed)
    );

    for tier in &report.tiers {
        println!("\n{} {}", tier.label.bold(), tier.skills_dir.display());
        if let Some(dir) = &tier.agents_dir {
            println!("  agents  {}  {}", counts(&tier.agents), dir.display());
        }
        println!("  skills  {}", counts(&tier.skills));
        if tier.optional {
            println!(
                "          {}",
                "(optional destination — a failure here does not fail the command)".dimmed()
            );
        }
        for note in &tier.notes {
            println!("    {} {note}", "!".yellow());
        }
    }

    let deployed: usize = report
        .tiers
        .iter()
        .map(|t| t.agents.deployed + t.skills.deployed)
        .sum();
    let repaired: usize = report.tiers.iter().map(|t| t.agents.repaired).sum();
    let shadowed: usize = report.tiers.iter().map(|t| t.skills.shadowed).sum();
    println!(
        "\n{deployed} file(s) written, {repaired} repaired, {shadowed} shadowed, across {} \
         destination(s).",
        report.tiers.len()
    );
}

/// `refreshed from this binary` / `already current`.
fn refreshed(yes: bool) -> String {
    if yes {
        "refreshed from this binary".green().to_string()
    } else {
        "already current".dimmed().to_string()
    }
}

/// One-line rendering of an [`AssetCounts`].
fn counts(c: &AssetCounts) -> String {
    format!(
        "{} deployed, {} repaired, {} preserved, {} unchanged, {} shadowed, {} failed",
        c.deployed, c.repaired, c.preserved, c.unchanged, c.shadowed, c.failed
    )
}

/// `--binary`: reinstall the running executable, or refuse and say why.
///
/// Why: a reinstall that guesses how the binary was installed can replace the
/// wrong file, or silently fail to replace anything (issue #4033: a `tm`
/// shadowing another `tm` on PATH, a `--path` install whose source macOS
/// reaped). So provenance decides the route and an unclassifiable install
/// refuses rather than defaulting to crates.io.
/// What: reads cargo's `.crates2.json` ledger and hands it to
/// [`reinstall_route`]. `Registry` goes through
/// `trusty_common::update::{check_crates_io, upgrade_and_restart}` — which may
/// NOT RETURN, calling `std::process::exit(1)` after a successful install when
/// the process is launchd-supervised. `Path` re-runs `cargo install --path <dir>
/// --locked` and health-gates the result — after refusing a source whose HEAD
/// is behind `origin/main` (#4462: one global binary, every managed session
/// regressed at once, and no version number moving to show it), which
/// `TRUSTY_MPM_ALLOW_STALE_INSTALL=1` lifts. `Refuse` prints the reason and
/// errors. Without `yes`, both routes confirm on the terminal first.
/// Test: the route decision is covered in `core::binary_reinstall_tests`, the
/// staleness guard in `core::install_freshness`'s tests; this function is
/// process execution.
async fn reinstall_binary(yes: bool) -> anyhow::Result<()> {
    let exe = std::env::current_exe().context("could not resolve the running executable")?;
    let bin_name = exe
        .file_name()
        .and_then(|n| n.to_str())
        .context("the running executable has no file name")?
        .to_string();
    let cargo_home = match std::env::var("CARGO_HOME") {
        Ok(h) if !h.is_empty() => std::path::PathBuf::from(h),
        _ => dirs::home_dir()
            .context("no `$CARGO_HOME` and no home directory — cannot locate cargo's ledger")?
            .join(".cargo"),
    };
    let ledger = std::fs::read_to_string(cargo_home.join(".crates2.json"))
        .ok()
        .and_then(|raw| trusty_mpm::core::binary_provenance::parse_cargo_installs(&raw));

    let route = reinstall_route(
        &bin_name,
        env!("CARGO_PKG_VERSION"),
        &exe,
        &cargo_home.join("bin"),
        ledger.as_deref(),
        &|dir| dir.exists(),
    );

    match route {
        ReinstallRoute::Refuse(why) => {
            eprintln!("\n{} {why}", "✗".red());
            anyhow::bail!("refusing to reinstall the binary: provenance is not conclusive");
        }
        ReinstallRoute::Registry { package } => {
            println!(
                "\nBinary: `{bin_name}` was installed from crates.io — checking for a newer \
                 {package}…"
            );
            if !confirm(yes, &format!("Reinstall {package} from crates.io"))? {
                return Ok(());
            }
            match trusty_common::update::check_crates_io(&package, env!("CARGO_PKG_VERSION")).await
            {
                Some(info) => println!("  update available: {} → {}", info.current, info.latest),
                None => println!("  already at the latest published version; reinstalling anyway"),
            }
            // May not return: `upgrade_and_restart` exits(1) after a successful
            // install when launchd supervises this process.
            match trusty_common::update::upgrade_and_restart(&package, &bin_name).await {
                Ok(Some(hint)) => println!("  {hint}"),
                Ok(None) => {}
                Err(e) => return Err(anyhow::anyhow!("binary reinstall failed: {e}")),
            }
            Ok(())
        }
        ReinstallRoute::Path { package, dir } => {
            println!(
                "\nBinary: `{bin_name}` was installed with `cargo install --path` from {}",
                dir.display()
            );
            // #4462: a path source behind `origin/main` regresses every managed
            // session on this machine at once, and nothing else would say so.
            let freshness = trusty_mpm::core::install_freshness::probe_source_freshness(&dir);
            if let Some(reason) = trusty_mpm::core::install_freshness::stale_install_refusal(
                &dir,
                &freshness,
                trusty_mpm::core::install_freshness::stale_install_override(),
            ) {
                eprintln!("\n{} {reason}", "✗".red());
                anyhow::bail!("refusing to reinstall the binary from a source behind origin/main");
            }
            if let trusty_mpm::core::install_freshness::SourceFreshness::Unknown(why) = &freshness {
                println!(
                    "  {} could not tell whether this source is current ({why}) — installing anyway",
                    "!".yellow()
                );
            }
            if !confirm(yes, &format!("Reinstall {package} from {}", dir.display()))? {
                return Ok(());
            }
            install_from_path(&dir).await?;
            trusty_common::update::verify_installed_binary(&bin_name)
                .await
                .map_err(|e| {
                    anyhow::anyhow!(
                        "health gate failed after reinstalling {package} from {}: {e}",
                        dir.display()
                    )
                })?;
            println!("  {} `{bin_name}` reinstalled and verified", "✓".green());
            Ok(())
        }
    }
}

/// Run `cargo install --path <dir> --locked`, inheriting stdio.
///
/// Why: the path route has no shared helper — `trusty_common::update` only
/// knows the registry form — and inheriting stdio lets the operator watch a
/// build that takes minutes rather than staring at a silent terminal.
/// What: spawns cargo, waits, and turns a non-zero exit into an error naming
/// the directory. Never captures output.
/// Test: process execution; not exercised in CI (no real cargo install).
async fn install_from_path(dir: &std::path::Path) -> anyhow::Result<()> {
    let status = tokio::process::Command::new("cargo")
        .arg("install")
        .arg("--path")
        .arg(dir)
        .arg("--locked")
        .status()
        .await
        .with_context(|| format!("failed to run `cargo install --path {}`", dir.display()))?;
    if !status.success() {
        anyhow::bail!(
            "`cargo install --path {} --locked` exited with {status}",
            dir.display()
        );
    }
    Ok(())
}

/// Ask for confirmation on the terminal unless `yes` was passed.
///
/// Why: `--binary` replaces the executable the operator is running; doing that
/// without asking is not a default anyone should get by typo.
/// What: `Ok(true)` immediately when `yes`; otherwise prompts on stderr and
/// accepts only `y`/`yes`.
/// Test: terminal interaction; not exercised in CI.
fn confirm(yes: bool, prompt: &str) -> anyhow::Result<bool> {
    if yes {
        return Ok(true);
    }
    use std::io::Write as _;
    eprint!("{prompt}? [y/N] ");
    let _ = std::io::stderr().flush();
    let mut line = String::new();
    std::io::stdin()
        .read_line(&mut line)
        .context("failed to read confirmation")?;
    let answer = line.trim().to_ascii_lowercase();
    if answer != "y" && answer != "yes" {
        eprintln!("Binary reinstall cancelled.");
        return Ok(false);
    }
    Ok(true)
}
