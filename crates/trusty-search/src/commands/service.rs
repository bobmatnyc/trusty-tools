//! Handler for `trusty-search service` (macOS launchd integration).
//!
//! Why: launchd is the canonical way to keep a long-lived foreground service
//! alive on macOS — it survives logout, restarts on crash, and integrates with
//! `launchctl` for diagnostics. Wrapping the plist mechanics in `service`
//! subcommands keeps users from having to hand-edit XML.
//! What: macOS routes to `service_install` / `service_uninstall` /
//! `service_status` / `service_logs`. Non-macOS prints "not supported" and
//! exits 1.
//! Test: on Linux, every action returns Err with the platform message;
//! on macOS, `service status` runs `launchctl list` without crashing.

use anyhow::Result;
use clap::Subcommand;
#[cfg(target_os = "macos")]
use colored::Colorize;

/// Subcommands for `trusty-search service` (macOS launchd integration).
#[derive(Debug, Clone, Subcommand)]
pub enum ServiceAction {
    /// Install the LaunchAgent plist and load it
    Install,
    /// Unload the LaunchAgent and remove the plist
    Uninstall,
    /// Show launchd status for the agent
    Status,
    /// Tail the launchd stdout / stderr logs
    Logs,
}

/// Reverse-DNS label for the LaunchAgent. Used as the plist filename and the
/// `Label` key — both must match for `launchctl` lookups to work.
#[cfg(target_os = "macos")]
const LAUNCHD_LABEL: &str = "com.trusty.trusty-search";

/// Dispatch a `trusty-search service <action>` invocation.
pub fn handle_service(action: &ServiceAction) -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        match action {
            ServiceAction::Install => service_install(),
            ServiceAction::Uninstall => service_uninstall(),
            ServiceAction::Status => service_status(),
            ServiceAction::Logs => service_logs(),
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = action;
        anyhow::bail!(
            "`trusty-search service` is not supported on this platform — \
             use your distro's service manager (systemd, OpenRC, etc.) directly."
        );
    }
}

#[cfg(target_os = "macos")]
fn launchd_log_dir() -> Result<std::path::PathBuf> {
    let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("could not resolve $HOME"))?;
    let dir = home.join("Library").join("Logs").join("trusty-search");
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Build the environment-variable pairs for the launchd plist.
///
/// Why: launchd re-spawns the daemon without the user's shell environment.
/// Embedding env vars directly in the plist provides a belt-and-suspenders
/// guarantee for operator tunables, and pins `HF_HOME` to the user's standard
/// Hugging Face cache directory so fastembed-rs never inherits a non-standard
/// or read-only `HF_HOME` that was set in an earlier shell session (fixes #86).
/// What: always emits an `HF_HOME` entry resolved at install time, plus any
/// `PERSISTED_ENV_VARS` that are currently set.
/// Test: covered indirectly by service install integration runs.
#[cfg(target_os = "macos")]
fn launchd_env_vars() -> Vec<(String, String)> {
    use crate::service::PERSISTED_ENV_VARS;

    let mut pairs: Vec<(String, String)> = Vec::new();

    // Always pin HF_HOME to $HOME/.cache/huggingface resolved at install time.
    if let Some(home) = dirs::home_dir() {
        let hf_home = home.join(".cache").join("huggingface");
        pairs.push(("HF_HOME".to_string(), hf_home.display().to_string()));
    }

    // Append operator tunables (TRUSTY_* vars) that are currently set.
    for key in PERSISTED_ENV_VARS {
        if let Ok(val) = std::env::var(key) {
            pairs.push((key.to_string(), val));
        }
    }

    pairs
}

/// Build the shared `LaunchdConfig` describing the trusty-search agent.
///
/// Why: install/uninstall/status all need the same plist label, log paths,
/// and env-var set. Building it in one place keeps them in sync.
/// What: assembles a [`trusty_common::launchd::LaunchdConfig`] using
/// `start --foreground` as the entry point and `KeepAlive::OnSuccess` so the
/// daemon's idempotent `start` exit isn't crash-looped.
/// Test: exercised via service install/uninstall.
#[cfg(target_os = "macos")]
fn build_launchd_config(
    exe: std::path::PathBuf,
    log_dir: std::path::PathBuf,
) -> trusty_common::launchd::LaunchdConfig {
    use trusty_common::launchd::{KeepAlive, LaunchdConfig};
    LaunchdConfig {
        label: LAUNCHD_LABEL.to_string(),
        exe_path: exe,
        args: vec!["start".to_string(), "--foreground".to_string()],
        log_dir,
        keep_alive: KeepAlive::OnSuccess,
        throttle_interval: 30,
        env_vars: launchd_env_vars(),
        fd_limit: None,
    }
}

#[cfg(target_os = "macos")]
fn service_install() -> Result<()> {
    let exe = std::env::current_exe()
        .map_err(|e| anyhow::anyhow!("could not resolve current exe: {e}"))?;
    let log_dir = launchd_log_dir()?;
    let cfg = build_launchd_config(exe, log_dir.clone());
    let plist_path = cfg.plist_path()?;
    cfg.install()?;
    println!(
        "{} Wrote LaunchAgent plist: {}",
        "✓".green(),
        plist_path.display()
    );

    cfg.bootstrap()?;
    let domain = format!("gui/{}", trusty_common::launchd::current_uid());
    println!(
        "{} Loaded {} into {} — daemon will start automatically.",
        "✓".green(),
        LAUNCHD_LABEL,
        domain
    );

    // Issue #127: install log rotation for the launchd-managed stderr.log so
    // it never grows unbounded. Non-fatal — a failure here still leaves a
    // working service; `trusty-search doctor --fix` can install it later.
    match crate::commands::log_rotation::install_rotation() {
        Ok(()) => println!(
            "{} Installed stderr.log rotation (1 MB × 7 archives, daily check)",
            "✓".green()
        ),
        Err(e) => eprintln!(
            "{} Could not install log rotation ({e}) — run `trusty-search doctor --fix` later",
            "⚠".yellow()
        ),
    }

    println!(
        "  Logs:    {}\n  Status:  {}",
        log_dir.display().to_string().dimmed(),
        "trusty-search service status".cyan(),
    );
    Ok(())
}

#[cfg(target_os = "macos")]
fn service_uninstall() -> Result<()> {
    let exe = std::env::current_exe()
        .map_err(|e| anyhow::anyhow!("could not resolve current exe: {e}"))?;
    let log_dir = launchd_log_dir()?;
    let cfg = build_launchd_config(exe, log_dir);
    let plist_path = cfg.plist_path()?;
    let uid = trusty_common::launchd::current_uid();
    let domain = format!("gui/{uid}");
    if plist_path.exists() {
        let _ = cfg.bootout();
        std::fs::remove_file(&plist_path)
            .map_err(|e| anyhow::anyhow!("remove {}: {e}", plist_path.display()))?;
        println!(
            "{} Unloaded and removed {}",
            "✓".green(),
            plist_path.display()
        );

        // Issue #127: also tear down the log-rotation LaunchAgent + config so
        // an uninstall leaves no orphaned launchd job behind.
        if let Ok(rot_plist) = crate::commands::log_rotation::rotation_plist_path() {
            if rot_plist.exists() {
                let _ = std::process::Command::new("launchctl")
                    .args(["bootout", &domain])
                    .arg(&rot_plist)
                    .status();
                let _ = std::fs::remove_file(&rot_plist);
            }
        }
        if let Ok(conf) = crate::commands::log_rotation::newsyslog_conf_path() {
            let _ = std::fs::remove_file(&conf);
        }
    } else {
        println!(
            "{} {} not installed — nothing to do",
            "·".dimmed(),
            plist_path.display()
        );
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn service_status() -> Result<()> {
    let uid = trusty_common::launchd::current_uid();
    let target = format!("gui/{uid}/{LAUNCHD_LABEL}");
    let output = std::process::Command::new("launchctl")
        .args(["print", &target])
        .output()
        .map_err(|e| anyhow::anyhow!("launchctl print failed: {e}"))?;
    if output.status.success() {
        println!("{}", String::from_utf8_lossy(&output.stdout));
    } else {
        // `launchctl print` exits non-zero when the service isn't loaded.
        // Print the install hint before bailing so the user sees both lines.
        eprintln!("  Install with: trusty-search service install");
        anyhow::bail!(
            "{} is not loaded ({})",
            target,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn service_logs() -> Result<()> {
    let log_dir = launchd_log_dir()?;
    let stdout = log_dir.join("stdout.log");
    let stderr = log_dir.join("stderr.log");
    if !stdout.exists() && !stderr.exists() {
        eprintln!(
            "{} No logs at {} yet — start the service first.",
            "·".dimmed(),
            log_dir.display()
        );
        return Ok(());
    }
    // Defer to `tail -F` so the user gets a familiar follow-mode experience
    // and we don't have to re-implement log rotation handling.
    let status = std::process::Command::new("tail")
        .arg("-F")
        .arg(&stdout)
        .arg(&stderr)
        .status()
        .map_err(|e| anyhow::anyhow!("tail failed: {e}"))?;
    if !status.success() {
        anyhow::bail!("tail exited with {status}");
    }
    Ok(())
}
