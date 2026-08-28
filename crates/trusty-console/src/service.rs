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
/// Why: this MUST equal the label `tctl start`/`tctl stop` targets, or those
/// commands drive a launchd job that `service install` never created. Making
/// both read the same registry constant is what turns "must equal" from a
/// comment into a fact.
///
/// #4868: was the literal `"com.trusty.trusty-console"` while the unit launchd
/// actually has loaded is `com.trusty.console`, so `service status` queried a
/// label that does not exist — the same divergence that broke trusty-search.
/// What: the `Label` key value and the `<label>.plist` base name.
/// Test: `service_label_matches_tctl_convention`.
#[cfg(target_os = "macos")]
pub const LAUNCHD_LABEL: &str = trusty_common::launchd_labels::CONSOLE;

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
        working_directory: None,
    }
    .with_daemon_path())
}

/// Install the LaunchAgent and load it.
///
/// #4868: console's label genuinely CHANGES with this fix
/// (`com.trusty.trusty-console` → `com.trusty.console`), which makes eviction
/// mandatory rather than tidy. A bare `install()` + `bootstrap()` on a host that
/// ran the older installer would leave the old unit loaded AND add the new one —
/// two console daemons on one port, the exact #2938 condition. Routing through
/// `install_and_activate` boots the old label out first, skips the reload when
/// nothing changed, and rolls back rather than leaving the dashboard down.
#[cfg(target_os = "macos")]
fn service_install() -> Result<()> {
    let cfg = launchd_config()?;
    let plist_path = cfg.plist_path()?;
    let outcome = cfg
        .install_and_activate(trusty_common::launchd_labels::legacy_labels_for(
            LAUNCHD_LABEL,
        ))
        .map_err(|e| anyhow::anyhow!("install LaunchAgent: {e}"))?;
    for label in outcome.evicted() {
        println!(
            "[warn] Evicted the stale LaunchAgent {label} — it named this daemon under an old label."
        );
    }
    let domain = format!("gui/{}", trusty_common::launchd::current_uid());
    if matches!(
        outcome,
        trusty_common::launchd_activate::Activation::AlreadyCurrent { .. }
    ) {
        println!(
            "[ok] {LAUNCHD_LABEL} is already loaded in {domain} with this exact unit — left running."
        );
        println!(
            "  Logs:    {}\n  Status:  trusty-console service status",
            cfg.log_dir.display()
        );
        return Ok(());
    }
    println!("[ok] Wrote LaunchAgent plist: {}", plist_path.display());
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
    // #4868: a host that never ran the migrating install still has the unit
    // under its old label. Removing only the canonical plist printed "nothing
    // to do" while leaving that one loaded.
    for label in cfg.evict_legacy(trusty_common::launchd_labels::legacy_labels_for(
        LAUNCHD_LABEL,
    )) {
        println!("[ok] Unloaded and removed the stale LaunchAgent {label}");
    }
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

    /// Why: the label is a cross-crate contract — `tctl` resolves it through
    /// `plist_label_for` and bootstraps THAT plist, so drift silently breaks
    /// `tctl start trusty-console`. #4868: asserting against a re-typed literal
    /// is what made the old version of this test agree with the wrong answer
    /// (`com.trusty.trusty-console`, while launchd has `com.trusty.console`);
    /// it now asserts against the registry both sides read.
    /// What: the constant equals the canonical registry label, and is NOT the
    /// pre-#4868 full-name form.
    /// Test: this is the test.
    #[test]
    fn service_label_matches_tctl_convention() {
        assert_eq!(LAUNCHD_LABEL, trusty_common::launchd_labels::CONSOLE);
        assert_ne!(
            LAUNCHD_LABEL, "com.trusty.trusty-console",
            "the full-name form is a legacy alias, not a unit launchd has"
        );
    }

    /// Why: the launchd agent must start the dashboard HTTP daemon, i.e.
    /// `trusty-console serve`.
    /// What: asserts the embedded args are exactly `["serve"]`.
    /// Test: this is the test.
    #[test]
    fn serve_args_are_serve() {
        assert_eq!(serve_args(), vec!["serve".to_string()]);
    }

    /// Cross-crate port-uniqueness contract (#2566, extended by #2573).
    ///
    /// Why: trusty-review's original `DEFAULT_PORT` (7880) silently collided
    /// with trusty-mpm's live `DEFAULT_DAEMON_ADDR`, crash-looping a launchd
    /// agent on install. This mirrors that fix's guard for the console's own
    /// default (`crate::DEFAULT_PORT`), pointer-commented to each sibling's
    /// real source constant, so a future edit here that reintroduces a
    /// collision fails this test instead of shipping a crash-loop. #2573
    /// extended this table to also cover trusty-embedderd's `--http` mode
    /// default, which the original table omitted because it is a manual/
    /// dev-run listener rather than a `tctl`-managed daemon.
    /// What: asserts `crate::DEFAULT_PORT` is absent from the known-sibling
    /// ports list.
    /// Test: this is the test.
    #[test]
    fn default_port_does_not_collide_with_known_siblings() {
        // (binary, port, source-of-truth pointer)
        //
        // #6277 / #6287 / #6286: trusty-review, trusty-analyze and trusty-memory
        // have NO ROW. None binds a TCP port any more — all three serve a Unix
        // socket (ADR-0032), so 7891, 7879 and 7070 are not reserved by
        // anything and listing them would forbid a future daemon a free port.
        let known_siblings: &[(&str, u16, &str)] = &[
            (
                "trusty-search",
                7878,
                "trusty-search/src/service/constants.rs::DEFAULT_PORT",
            ),
            (
                "trusty-mpm",
                7880,
                "trusty-mpm/src/core/discovery.rs::DEFAULT_DAEMON_ADDR",
            ),
            (
                "trusty-embedderd",
                7890,
                "trusty-embedderd/src/lib.rs::Args::http_addr (--http default_value, manual/dev-run only)",
            ),
            (
                // #3331: trusty-agents joined the proxied-sibling set; its API
                // server default port must not collide with the console's.
                "trusty-agents",
                8080,
                "trusty-agents/src/runtime/mode_dispatch.rs (--port default 8080)",
            ),
            // #6288: trusty-mpm's supervisor has NO ROW. It stopped binding
            // 7881 for a `/metrics` + `/health` listener nothing read and
            // publishes to `~/.trusty-mpm/supervisor-metrics.json` instead, so
            // 7881 is not reserved by anything and listing it would forbid a
            // future daemon a free port. #3364 (below) is why the row existed.
            (
                // #3364: trusty-code's own default HTTP port, which previously
                // reused 7881 and collided with the supervisor's listener,
                // since retired.
                "trusty-code",
                7882,
                "trusty-code/src/serve/mod.rs::DEFAULT_HTTP_PORT",
            ),
        ];
        for (binary, port, source) in known_siblings {
            assert_ne!(
                crate::DEFAULT_PORT,
                *port,
                "trusty-console DEFAULT_PORT {} collides with {binary}'s {port} ({source})",
                crate::DEFAULT_PORT
            );
        }
    }
}
