//! Handler for `trusty-analyzer service` (macOS launchd integration).
//!
//! Mirrors the trusty-search service layout: a thin platform-gated dispatcher
//! that maps each `ServiceAction` to a single launchd operation via the shared
//! `trusty_common::launchd` module. On non-macOS targets the entry point
//! prints a clear message and exits 1.

use anyhow::Result;
use colored::Colorize;

/// Reverse-DNS label for the LaunchAgent. Used as the plist filename and the
/// `Label` key — both must match for `launchctl` lookups to work.
#[cfg(target_os = "macos")]
const LAUNCHD_LABEL: &str = "com.trusty.analyze";

/// Subcommand actions for `trusty-analyzer service`.
///
/// Why: launchd is the canonical way to keep a long-lived foreground service
/// alive on macOS — wrapping plist mechanics in `service` subcommands keeps
/// users from having to hand-edit XML.
/// What: each variant maps to one launchd operation (or `tail -F` for Logs).
/// Test: `cargo run -- service --help` lists the four actions; on Linux,
/// any action prints "not supported" and exits 1.
#[derive(Debug, Clone)]
pub enum ServiceAction {
    /// Install the LaunchAgent plist and load it.
    Install,
    /// Unload the LaunchAgent and remove the plist.
    Uninstall,
    /// Show launchd status for the agent.
    Status,
    /// Tail the launchd stdout / stderr logs.
    Logs,
}

/// Dispatch a `trusty-analyzer service <action>` invocation.
///
/// Why: launchd is macOS-specific; on other platforms we exit cleanly with a
/// clear message rather than emitting confusing plist errors.
/// What: macOS routes to `service_install` / `service_uninstall` /
/// `service_status` / `service_logs`. Non-macOS prints "not supported" and
/// exits 1.
/// Test: on Linux, every action exits 1 with the platform message;
/// on macOS, `service status` runs `launchctl print` without crashing.
pub fn run_service_action(action: ServiceAction) -> Result<()> {
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
        eprintln!(
            "{} `trusty-analyze service` is not supported on this platform — \
             use your distro's service manager (systemd, OpenRC, etc.) directly.",
            "✗".red()
        );
        std::process::exit(1);
    }
}

/// Resolve the log directory for the analyzer launchd agent.
///
/// Why: align with the other trusty-* daemons by writing logs under
/// `~/.trusty-analyze/logs/` instead of `~/Library/Logs/`. Easier to find
/// and matches the convention shared across the workspace.
/// What: returns `~/.trusty-analyze/logs`, creating it on demand.
/// Test: covered transitively by `setup daemon`.
#[cfg(target_os = "macos")]
fn launchd_log_dir() -> Result<std::path::PathBuf> {
    let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("could not resolve $HOME"))?;
    let dir = home.join(".trusty-analyze").join("logs");
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Build the shared `LaunchdConfig` for this daemon.
///
/// Why: every `service` action needs the same plist label, executable path,
/// args, and log directory. Centralising the construction keeps install,
/// uninstall, status, and logs in agreement.
/// What: resolves the current executable, computes the log dir, and returns
/// a `LaunchdConfig` configured for an always-on agent that runs
/// `trusty-analyze serve` with a 10-second restart throttle.
/// Test: exercised transitively by every macOS `service` subcommand.
#[cfg(target_os = "macos")]
fn launchd_config() -> Result<trusty_common::launchd::LaunchdConfig> {
    use trusty_common::launchd::{KeepAlive, LaunchdConfig};

    let exe = std::env::current_exe()
        .map_err(|e| anyhow::anyhow!("could not resolve current exe: {e}"))?;
    let log_dir = launchd_log_dir()?;
    Ok(LaunchdConfig {
        label: LAUNCHD_LABEL.to_string(),
        exe_path: exe,
        args: vec!["serve".to_string()],
        log_dir,
        keep_alive: KeepAlive::Always,
        throttle_interval: 10,
        env_vars: Vec::new(),
        // Why: the analyze daemon opens one redb file + per-index chunk
        // caches but does not need elevated fd limits; None delegates to the
        // OS default, matching the trusty-search pattern.
        fd_limit: None,
    })
}

/// Resolve the LaunchAgent plist path.
///
/// Why: `setup daemon` needs to check whether the plist already exists before
/// deciding whether to install or simply reload it.
/// What: returns `~/Library/LaunchAgents/com.trusty.analyze.plist` via the
/// shared `LaunchdConfig::plist_path` helper.
/// Test: covered transitively by `setup daemon`.
#[cfg(target_os = "macos")]
pub fn launchd_plist_path() -> Result<std::path::PathBuf> {
    launchd_config()?.plist_path()
}

/// Install and start the launchd LaunchAgent.
///
/// Why: exposed so the `setup daemon` subcommand can install the background
/// service without re-implementing the plist mechanics.
/// What: writes the plist via `LaunchdConfig::install`, then `bootstrap`s it
/// into the current user's GUI domain via the shared helper.
/// Test: `setup daemon` on macOS installs the plist and the daemon answers
/// `/health`; on Linux `setup daemon` skips this path with a clear message.
#[cfg(target_os = "macos")]
pub fn service_install() -> Result<()> {
    let cfg = launchd_config()?;
    cfg.install()
        .map_err(|e| anyhow::anyhow!("install LaunchAgent plist: {e}"))?;
    let plist_path = cfg.plist_path()?;
    println!(
        "{} Wrote LaunchAgent plist: {}",
        "✓".green(),
        plist_path.display()
    );

    cfg.bootstrap()
        .map_err(|e| anyhow::anyhow!("launchctl bootstrap: {e}"))?;

    let domain = format!("gui/{}", trusty_common::launchd::current_uid());
    println!(
        "{} trusty-analyze service installed and started ({} loaded into {}).",
        "✓".green(),
        LAUNCHD_LABEL,
        domain
    );
    println!(
        "  Logs:    {}\n  Status:  {}",
        cfg.log_dir.display().to_string().dimmed(),
        "trusty-analyze service status".cyan(),
    );
    Ok(())
}

#[cfg(target_os = "macos")]
fn service_uninstall() -> Result<()> {
    let cfg = launchd_config()?;
    let plist_path = cfg.plist_path()?;
    if plist_path.exists() {
        // bootout is best-effort: a not-loaded agent is fine here.
        let _ = cfg.bootout();
        std::fs::remove_file(&plist_path)
            .map_err(|e| anyhow::anyhow!("remove {}: {e}", plist_path.display()))?;
        println!(
            "{} trusty-analyze service uninstalled ({} removed).",
            "✓".green(),
            plist_path.display()
        );
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
        eprintln!(
            "{} {} is not loaded ({})",
            "✗".red(),
            target,
            String::from_utf8_lossy(&output.stderr).trim()
        );
        eprintln!(
            "  Install with: {}",
            "trusty-analyze service install".cyan()
        );
        std::process::exit(1);
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn service_logs() -> Result<()> {
    use std::os::unix::process::CommandExt;
    let log_dir = launchd_log_dir()?;
    // trusty_common's LaunchdConfig writes separate stdout/stderr files; tail
    // both so users see the full picture in one stream.
    let stdout_log = log_dir.join("stdout.log");
    let stderr_log = log_dir.join("stderr.log");
    if !stdout_log.exists() && !stderr_log.exists() {
        eprintln!(
            "{} No logs at {} yet — start the service first.",
            "·".dimmed(),
            log_dir.display()
        );
        return Ok(());
    }
    // Replace the current process with `tail -F` so the user gets a familiar
    // follow-mode experience and we don't have to re-implement log rotation.
    let err = std::process::Command::new("tail")
        .arg("-F")
        .arg(&stdout_log)
        .arg(&stderr_log)
        .exec();
    Err(anyhow::anyhow!("exec tail failed: {err}"))
}
