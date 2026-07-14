//! Handler for `trusty-console service` (macOS launchd integration).
//!
//! Why: `stable_set()` in trusty-installer marks trusty-console
//! `ManageStrategy::Launchd`, so `tctl start trusty-console` calls
//! `launchctl bootstrap … com.trusty.trusty-console.plist` — but no code path
//! in the repository ever WROTE that plist, so a fresh-machine `tctl start`
//! hard-failed for the console daemon (#2557). This subcommand supplies the
//! missing launchd install/uninstall/status/logs mechanics, mirroring the
//! `trusty-search` / `trusty-analyze` `service` pattern.
//!
//! Note (design decision, umbrella #2555 open question): whether the console
//! SHOULD be launchd-managed or process-managed (like trusty-mpm's own
//! `start`/`stop` verbs) is an owner decision left open. The console exposes a
//! real long-lived HTTP daemon (`trusty-console serve`, graceful SIGTERM
//! shutdown), which launchd supervises correctly, and `stable_set()` already
//! classifies it `Launchd`; this module makes that existing classification
//! FUNCTION rather than deciding it is the right lifecycle model. If the owner
//! later chooses process-management, the console would need its own
//! `start`/`stop` verbs and the stable-set strategy would flip to `OwnVerb`.
//!
//! What: on macOS routes `ServiceAction` to a single launchd operation via the
//! shared `trusty_common::launchd` module; the agent runs `trusty-console
//! serve` (the dashboard HTTP daemon). On non-macOS the entry point returns a
//! clear error.
//! Test: `service_label_matches_tctl_convention` and `serve_args_are_serve`
//! (macOS-only, pure) pin the load-bearing label + args; install/uninstall are
//! side-effecting `launchctl` calls exercised manually (never in tests).

use anyhow::Result;
use clap::Subcommand;

/// Subcommand actions for `trusty-console service`.
///
/// Why: launchd keeps the long-lived console dashboard daemon alive on macOS;
/// wrapping the plist mechanics in `service` subcommands gives `tctl` a stable
/// `trusty-console service install` hook and spares operators hand-editing XML.
/// What: each variant maps to one launchd operation (or `tail -F` for Logs).
/// Test: `cargo run -p trusty-console -- service --help` lists the four
/// actions; on Linux any action returns Err with the platform message.
#[derive(Debug, Clone, Subcommand)]
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

/// Reverse-DNS label for the LaunchAgent.
///
/// Why: this MUST equal `trusty_installer`'s
/// `plist_label::plist_label_for("trusty-console")` (`com.trusty.trusty-console`
/// by convention) or `tctl start`/`tctl stop` would target a launchd job that
/// `service install` never created.
/// What: the `Label` key value and the `<label>.plist` base name.
/// Test: `service_label_matches_tctl_convention`.
#[cfg(target_os = "macos")]
pub const LAUNCHD_LABEL: &str = "com.trusty.trusty-console";

/// Dispatch a `trusty-console service <action>` invocation.
///
/// Why: launchd is macOS-specific; on other platforms we return a clear error.
/// What: macOS routes to install / uninstall / status / logs. Non-macOS bails.
/// Test: on Linux every action returns Err; the macOS paths are side-effecting
/// and validated manually.
pub fn run_service_action(action: &ServiceAction) -> Result<()> {
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
            "`trusty-console service` is only supported on macOS — \
             use your distro's service manager (systemd, OpenRC, etc.) directly."
        );
    }
}

/// The daemon serve args embedded in the launchd plist.
///
/// Why: the launchd agent must start the dashboard HTTP daemon, which is
/// `trusty-console serve`.
/// What: returns `["serve"]`.
/// Test: `serve_args_are_serve`.
#[cfg(target_os = "macos")]
fn serve_args() -> Vec<String> {
    vec!["serve".to_string()]
}

/// Resolve the log directory for the console launchd agent.
///
/// Why: align with the other trusty-* daemons (`~/.trusty-<name>/logs`).
/// What: returns `~/.trusty-console/logs`, creating it on demand.
/// Test: side-effecting; exercised transitively by `service install`.
#[cfg(target_os = "macos")]
fn launchd_log_dir() -> Result<std::path::PathBuf> {
    let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("could not resolve $HOME"))?;
    let dir = home.join(".trusty-console").join("logs");
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Build the shared `LaunchdConfig` for the console daemon.
///
/// Why: install / uninstall / status all need the same label, exe path, args,
/// and log directory; building it once keeps them in agreement.
/// What: resolves the current executable and log dir and returns a
/// `LaunchdConfig` that runs `trusty-console serve`, kept alive always with a
/// 10-second restart throttle and a seeded daemon `PATH` (#1298 — the console
/// shells out to `tailscale`).
/// Test: side-effecting resolution; exercised transitively by every macOS
/// `service` subcommand.
#[cfg(target_os = "macos")]
fn launchd_config() -> Result<trusty_common::launchd::LaunchdConfig> {
    use trusty_common::launchd::{KeepAlive, LaunchdConfig};

    let exe = std::env::current_exe()
        .map_err(|e| anyhow::anyhow!("could not resolve current exe: {e}"))?;
    let log_dir = launchd_log_dir()?;
    Ok(LaunchdConfig {
        label: LAUNCHD_LABEL.to_string(),
        exe_path: exe,
        args: serve_args(),
        log_dir,
        keep_alive: KeepAlive::Always,
        throttle_interval: 10,
        env_vars: Vec::new(),
        fd_limit: None,
    }
    .with_daemon_path())
}

#[cfg(target_os = "macos")]
fn service_install() -> Result<()> {
    let cfg = launchd_config()?;
    cfg.install()
        .map_err(|e| anyhow::anyhow!("install LaunchAgent plist: {e}"))?;
    let plist_path = cfg.plist_path()?;
    println!("[ok] Wrote LaunchAgent plist: {}", plist_path.display());

    cfg.bootstrap()
        .map_err(|e| anyhow::anyhow!("launchctl bootstrap: {e}"))?;
    let domain = format!("gui/{}", trusty_common::launchd::current_uid());
    println!(
        "[ok] trusty-console service installed and started ({LAUNCHD_LABEL} loaded into {domain})."
    );
    println!(
        "  Logs:    {}\n  Status:  trusty-console service status",
        cfg.log_dir.display()
    );
    Ok(())
}

#[cfg(target_os = "macos")]
fn service_uninstall() -> Result<()> {
    let cfg = launchd_config()?;
    let plist_path = cfg.plist_path()?;
    if plist_path.exists() {
        let _ = cfg.bootout();
        std::fs::remove_file(&plist_path)
            .map_err(|e| anyhow::anyhow!("remove {}: {e}", plist_path.display()))?;
        println!(
            "[ok] trusty-console service uninstalled ({} removed).",
            plist_path.display()
        );
    } else {
        println!(
            "[skip] {} not installed — nothing to do",
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
        Ok(())
    } else {
        eprintln!("  Install with: trusty-console service install");
        anyhow::bail!(
            "{target} is not loaded ({})",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
}

#[cfg(target_os = "macos")]
fn service_logs() -> Result<()> {
    let log_dir = launchd_log_dir()?;
    let stdout_log = log_dir.join("stdout.log");
    let stderr_log = log_dir.join("stderr.log");
    if !stdout_log.exists() && !stderr_log.exists() {
        eprintln!(
            "[skip] No logs at {} yet — start the service first.",
            log_dir.display()
        );
        return Ok(());
    }
    let status = std::process::Command::new("tail")
        .arg("-F")
        .arg(&stdout_log)
        .arg(&stderr_log)
        .status()
        .map_err(|e| anyhow::anyhow!("tail failed: {e}"))?;
    if !status.success() {
        anyhow::bail!("tail exited with {status}");
    }
    Ok(())
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    /// Why: the label is a cross-crate contract — `tctl` derives
    /// `com.trusty.trusty-console` in `plist_label_for` and bootstraps THAT
    /// plist; drift here silently breaks `tctl start trusty-console`.
    /// What: asserts the constant equals the convention-derived label.
    /// Test: this is the test.
    #[test]
    fn service_label_matches_tctl_convention() {
        assert_eq!(LAUNCHD_LABEL, "com.trusty.trusty-console");
    }

    /// Why: the launchd agent must start the dashboard HTTP daemon, i.e.
    /// `trusty-console serve`.
    /// What: asserts the embedded args are exactly `["serve"]`.
    /// Test: this is the test.
    #[test]
    fn serve_args_are_serve() {
        assert_eq!(serve_args(), vec!["serve".to_string()]);
    }
}
