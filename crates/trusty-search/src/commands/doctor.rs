//! Handler for `trusty-search doctor` — 6-check diagnostic + auto-repair.

use super::daemon_utils::daemon_base_url;
use super::doctor_checks::{fix_stale_lock, CheckResult, EmptyIndex};
use super::doctor_pipeline::run_doctor_checks;
use super::reindex_engine::run_reindex;
use anyhow::Result;
use colored::Colorize;

/// Why: extracted from `main()`. The diagnostic runs six independent checks
/// and the `--fix` branch has three sub-fixes (stale lock, empty indexes,
/// missing model). Keeping all of that inline pushed `main()` past
/// cyclomatic-complexity 100; lifting it out gives `doctor` its own scope.
/// What: prints check results, optionally repairs (stale lockfile, empty
/// indexes via `run_reindex`, missing model is informational only), then
/// prints a summary. Exits 1 if any check is an error.
/// Test: `cargo run -- doctor` on a healthy install prints
/// "Everything looks good!"; with a stale lockfile and `--fix`, the lockfile
/// is removed and the summary reports the repair.
pub async fn handle_doctor(fix: bool) -> Result<()> {
    println!("\ntrusty-search doctor\n");
    println!("Checking configuration...\n");

    crate::commands::daemon_guard::ensure_daemon_running_or_exit(&daemon_base_url()).await?;

    let (checks, empty_indexes) = run_doctor_checks().await;

    // Print all checks (index sub-lines were already printed inline by
    // run_doctor_checks, so we skip the index summary line itself to avoid
    // double-printing — it carries the per-index detail).
    for check in &checks {
        check.print();
    }

    let errors = checks.iter().filter(|c| c.is_error()).count();
    let warnings = checks.iter().filter(|c| c.is_warn()).count();

    if fix {
        apply_fixes(&checks, &empty_indexes).await;
    }

    print_summary(errors, warnings, fix);

    if errors > 0 {
        // Non-zero exit code without printing another line (summary already
        // showed the error count). Empty-message bail keeps `main()`'s
        // central ✗ printer silent for this exit path.
        return Err(anyhow::anyhow!(""));
    }
    Ok(())
}

/// Run the three auto-repair branches: stale lockfile removal, reindex of
/// zero-chunk indexes, informational note for missing models.
// Fix 5's `fixed_any = true` is a dead store today (PR #3560 review, LOW
// fix): Fix 5 is currently the LAST branch, so nothing reads it afterward.
// It is set anyway, for symmetry with Fixes 1-4 and so a future Fix 6 does
// not silently reintroduce the double-printed-"Fixing issues..."-header bug
// this PR fixed for Fix 4. `unused_assignments` would otherwise fail
// `-D warnings` on that currently-provably-dead write.
#[allow(unused_assignments)]
async fn apply_fixes(checks: &[CheckResult], empty_indexes: &[EmptyIndex]) {
    let mut fixed_any = false;

    // Fix 1: Stale lock file. Honour TRUSTY_DATA_DIR so doctor targets the
    // correct daemon when an isolated data dir is active (issue #281).
    let data_dir = if let Ok(dir) = std::env::var("TRUSTY_DATA_DIR") {
        std::path::PathBuf::from(dir)
    } else {
        dirs::data_local_dir()
            .map(|d| d.join("trusty-search"))
            .unwrap_or_else(|| std::path::PathBuf::from("~/.local/share/trusty-search"))
    };
    let lock_path = data_dir.join("daemon.lock");
    if lock_path.exists() {
        let pid_opt = std::fs::read_to_string(&lock_path)
            .ok()
            .and_then(|s| s.trim().parse::<u32>().ok());
        let stale = pid_opt
            .map(|pid| {
                nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid as i32), None).is_err()
            })
            .unwrap_or(true);
        if stale {
            println!("\nFixing issues...");
            fix_stale_lock(&data_dir);
            fixed_any = true;
        }
    }

    // Fix 2: Zero-chunk indexes — reindex each one.
    if !empty_indexes.is_empty() {
        if !fixed_any {
            println!("\nFixing issues...");
            fixed_any = true;
        }
        for idx in empty_indexes {
            if idx.root_path.is_empty() {
                eprintln!(
                    "  {} '{}' has no root_path — cannot auto-fix; run `trusty-search index` manually",
                    "⚠".yellow(),
                    idx.name
                );
                continue;
            }
            let root = std::path::PathBuf::from(&idx.root_path);
            println!("  Indexing '{}'...", idx.name);
            // Doctor auto-repair uses the default 600s timeout so very large
            // repos still get a reasonable window without blocking forever.
            match run_reindex(&idx.name, &root, 600).await {
                Ok(()) => println!("  {} '{}' done", "✓".green(), idx.name),
                Err(e) => println!("  {} '{}' failed: {e}", "✗".red(), idx.name),
            }
        }
    }

    // Fix 3: Missing model — cannot pre-download, just inform.
    let has_model_warn = checks.iter().any(|c| {
        matches!(c, CheckResult::Warn(msg) if msg.contains("not cached") || msg.contains("not found"))
    });
    if has_model_warn {
        if !fixed_any {
            println!("\nFixing issues...");
            fixed_any = true;
        }
        println!(
            "  {} Model downloads automatically on `trusty-search start` — no manual action needed",
            "·".dimmed()
        );
    }

    // Fix 4: stderr.log has no rotation policy — install one (issue #127).
    let has_rotation_warn = checks
        .iter()
        .any(|c| matches!(c, CheckResult::Warn(msg) if msg.contains("no rotation policy")));
    if has_rotation_warn {
        if !fixed_any {
            println!("\nFixing issues...");
            fixed_any = true;
        }
        fix_log_rotation();
    }

    // Fix 5: stale/corrupt or not-yet-bootstrapped Python/MPS venv (epic
    // #3524 slice 5) — re-run the SAME eager bootstrap `trusty-search start`
    // uses, so `--fix` produces byte-identical results to just starting the
    // daemon once. Blocking (potentially minutes on a fresh build) — the
    // operator explicitly asked for `--fix`.
    let has_python_venv_warn = checks
        .iter()
        .any(|c| matches!(c, CheckResult::Warn(msg) if msg.contains("Python/MPS venv:")));
    if has_python_venv_warn {
        if !fixed_any {
            println!("\nFixing issues...");
            fixed_any = true;
        }
        fix_python_venv();
    }
}

/// Re-run the Python/MPS sidecar's eager venv bootstrap (epic #3524 slice 5).
///
/// Why: `PythonEmbedderCheck` warns on a missing/stale/corrupt venv but never
/// touches the filesystem itself (doctor checks stay read-only). `--fix`
/// performs the actual repair by calling the same `ensure_venv_eager` the
/// daemon calls at `start` — this rebuilds from scratch when the `.ready`
/// recheck fails, or completes a first-time bootstrap when the venv never
/// existed.
/// What: runs on a blocking thread (uv/python subprocess calls are
/// synchronous); reports success/failure. A failure here is informational
/// only — it does not fail `doctor --fix` overall, since the daemon's own
/// runtime fallback (epic #3524 slice 5, `FallbackEmbedderAdapter`) means a
/// broken Python venv degrades to the Rust ort path rather than blocking
/// search entirely.
/// Test: `cargo test --workspace` — exercised indirectly; the bootstrap
/// itself is covered by `trusty-embedderd-py`'s own test suite.
fn fix_python_venv() {
    println!("  Rebuilding Python/MPS embedder venv (this may take a few minutes)...");
    match tokio::task::block_in_place(trusty_embedderd_py::ensure_venv_eager) {
        Ok(layout) => println!(
            "  {} Python/MPS venv ready at {}",
            "✓".green(),
            layout.venv_dir.display()
        ),
        Err(e) => println!("  {} Python/MPS venv rebuild failed: {e:#}", "✗".red()),
    }
}

/// Install the newsyslog config + daily rotation LaunchAgent for `stderr.log`.
///
/// Why: the doctor's log-rotation check warns when the launchd-managed
/// `stderr.log` has no size cap; `--fix` repairs it without requiring sudo by
/// installing a user-level newsyslog config and a `LaunchAgent` that runs
/// `newsyslog` daily (see `commands::log_rotation`).
/// What: on macOS, calls `install_rotation()` and reports success/failure.
/// On other platforms it is a no-op (the rotation check is always Ok there).
/// Test: `trusty-search doctor --fix` on macOS prints the install confirmation
/// and a follow-up `doctor` reports the rotation check as OK.
fn fix_log_rotation() {
    #[cfg(target_os = "macos")]
    {
        match crate::commands::log_rotation::install_rotation() {
            Ok(()) => {
                let conf = crate::commands::log_rotation::newsyslog_conf_path()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default();
                println!(
                    "  {} Installed log rotation for stderr.log (1 MB × 7 archives, daily check)",
                    "✓".green()
                );
                println!("    {} {}", "·".dimmed(), conf.dimmed());
            }
            Err(e) => println!("  {} Could not install log rotation: {e}", "✗".red()),
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        println!(
            "  {} Log rotation is managed by the platform service manager — nothing to do",
            "·".dimmed()
        );
    }
}

/// Print the trailing summary line(s): green "all good", or a count of
/// warnings/errors plus the hint to re-run with `--fix`.
fn print_summary(errors: usize, warnings: usize, fix: bool) {
    println!();
    if errors == 0 && warnings == 0 {
        println!("{}", "Everything looks good!".green().bold());
        return;
    }
    if errors > 0 || warnings > 0 {
        println!(
            "Issues found: {} warning{}, {} error{}",
            warnings,
            if warnings == 1 { "" } else { "s" },
            errors,
            if errors == 1 { "" } else { "s" }
        );
    }
    if !fix {
        println!(
            "Run {} to attempt automatic repair.",
            "trusty-search doctor --fix".cyan()
        );
    }
}
