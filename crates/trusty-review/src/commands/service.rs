//! Handler for `trusty-review service` (macOS launchd integration).
//!
//! Why: `stable_set()` in trusty-installer marks trusty-review
//! `ManageStrategy::Launchd`, and `tctl start trusty-review` calls
//! `launchctl bootstrap … com.trusty.trusty-review.plist` — but until this
//! module existed no code path in the repository ever WROTE that plist, so a
//! fresh-machine `tctl start` hard-failed for the review daemon (#2557). This
//! subcommand supplies the missing launchd install/uninstall/status/logs
//! mechanics, mirroring the established `trusty-search` / `trusty-analyze`
//! `service` pattern so `tctl install` (#2556) and `tctl start` have a real
//! plist to bootstrap.
//! What: on macOS routes `ServiceAction` to a single launchd operation via the
//! shared `trusty_common::launchd` module; the agent runs `trusty-review serve`
//! (the long-lived HTTP webhook daemon). On non-macOS the entry point returns a
//! clear error rather than emitting confusing plist output.
//! Test: `service_label_matches_tctl_convention` and `serve_args_are_serve`
//! (macOS-only, pure) pin the load-bearing label + args; the install/uninstall
//! paths are side-effecting `launchctl` calls exercised manually (never in the
//! test suite, per the hermetic-test rule).

use anyhow::Result;
use clap::Subcommand;

/// Subcommand actions for `trusty-review service`.
///
/// Why: launchd is the canonical way to keep the long-lived review webhook
/// daemon alive on macOS; wrapping the plist mechanics in `service`
/// subcommands keeps operators from hand-editing XML and gives `tctl` a stable
/// `trusty-review service install` hook to call.
/// What: each variant maps to one launchd operation (or `tail -F` for Logs).
/// Test: `cargo run -p trusty-review -- service --help` lists the four actions;
/// on Linux any action returns Err with the platform message.
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
/// `plist_label::plist_label_for("trusty-review")` (which derives
/// `com.trusty.trusty-review` by convention) or `tctl start`/`tctl stop` would
/// target a launchd job that `service install` never created. Pinning the
/// constant here and asserting it in a test guards that cross-crate contract.
/// What: the `Label` key value and the `<label>.plist` base name.
/// Test: `service_label_matches_tctl_convention`.
#[cfg(target_os = "macos")]
pub const LAUNCHD_LABEL: &str = "com.trusty.trusty-review";

/// Dispatch a `trusty-review service <action>` invocation.
///
/// Why: launchd is macOS-specific; on other platforms we return a clear error
/// rather than emitting confusing plist / launchctl failures.
/// What: macOS routes to install / uninstall / status / logs. Non-macOS bails
/// with a platform message.
/// Test: on Linux every action returns Err (see module doc); the macOS paths
/// are side-effecting and validated manually.
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
            "`trusty-review service` is only supported on macOS — \
             use your distro's service manager (systemd, OpenRC, etc.) directly."
        );
    }
}

/// The daemon serve args embedded in the launchd plist.
///
/// Why: the launchd agent must start the long-lived HTTP webhook server, which
/// is `trusty-review serve`; centralising the args keeps install and the label
/// contract in one testable place.
/// What: returns `["serve"]`.
/// Test: `serve_args_are_serve`.
#[cfg(target_os = "macos")]
fn serve_args() -> Vec<String> {
    vec!["serve".to_string()]
}

/// Resolve the log directory for the review launchd agent.
///
/// Why: align with the other trusty-* daemons by writing logs under
/// `~/.trusty-review/logs/` (matches `trusty-analyze`'s convention).
/// What: returns `~/.trusty-review/logs`, creating it on demand.
/// Test: side-effecting; exercised transitively by `service install`.
#[cfg(target_os = "macos")]
fn launchd_log_dir() -> Result<std::path::PathBuf> {
    let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("could not resolve $HOME"))?;
    let dir = home.join(".trusty-review").join("logs");
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Build the shared `LaunchdConfig` for the review daemon.
///
/// Why: install / uninstall / status all need the same label, exe path, args,
/// and log directory; building it once keeps them in agreement.
/// What: resolves the current executable and log dir and returns a
/// `LaunchdConfig` that runs `trusty-review serve`, kept alive always with a
/// 10-second restart throttle and a seeded daemon `PATH` (#1298).
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
        "[ok] trusty-review service installed and started ({LAUNCHD_LABEL} loaded into {domain})."
    );
    println!(
        "  Logs:    {}\n  Status:  trusty-review service status",
        cfg.log_dir.display()
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
            "[ok] trusty-review service uninstalled ({} removed).",
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
        eprintln!("  Install with: trusty-review service install");
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
    // Defer to `tail -F` for a familiar follow-mode experience.
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
    /// `com.trusty.trusty-review` in `plist_label_for` and bootstraps THAT
    /// plist; a drift here silently breaks `tctl start trusty-review`.
    /// What: asserts the constant equals the convention-derived label.
    /// Test: this is the test.
    #[test]
    fn service_label_matches_tctl_convention() {
        assert_eq!(LAUNCHD_LABEL, "com.trusty.trusty-review");
    }

    /// Why: the launchd agent must start the HTTP webhook daemon, i.e.
    /// `trusty-review serve`; a wrong arg would load a plist that runs the
    /// wrong (or no) subcommand.
    /// What: asserts the embedded args are exactly `["serve"]`.
    /// Test: this is the test.
    #[test]
    fn serve_args_are_serve() {
        assert_eq!(serve_args(), vec!["serve".to_string()]);
    }
}
