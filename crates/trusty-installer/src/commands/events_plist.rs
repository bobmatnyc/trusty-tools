//! The launchd plist `tctl` writes for the trusty-events bus daemon (#10042).
//!
//! Why: ADR-0065 runs `trusty-events serve` as its own launchd-supervised
//! daemon. The shared daemons (search, memory, console) each write their own
//! plist with `<binary> service install`; trusty-events ships no `service`
//! subcommand, so `tctl` owns its plist, the way it owns the trusty-mpm
//! supervisor's ([`super::plist_bootstrap`]).
//!
//! What: [`EVENTS_PLIST_TEMPLATE`] and [`fill_events_template`] render the
//! plist; [`install_events_plist_for`] writes it under
//! `~/Library/LaunchAgents/` and bootstraps it through an injected
//! [`super::plist_bootstrap::LaunchctlPort`], so a test never reaches the live `gui/<uid>` domain
//! (#3551). The job runs `trusty-events serve`, restarts it only after an
//! unsuccessful exit (`KeepAlive` → `SuccessfulExit: false`) no sooner than
//! every 10 s (`ThrottleInterval`), and carries no socket, port or address:
//! the daemon binds its Unix socket itself and never TCP (ADR-0032). Exit 1 —
//! another instance holds the socket, a log write failed, the drain timed out,
//! or accept failed 64 times — is therefore retried; a clean SIGTERM exit 0 is
//! not.
//!
//! Test: `events_plist_tests.rs` — the golden render, the KeepAlive and
//! ThrottleInterval keys, the no-TCP rule, and the write-and-bootstrap path
//! against a stub `launchctl`.

use std::path::{Path, PathBuf};

use super::plist_bootstrap::SupervisorTarget;

/// The plist template, with `__PLIST_LABEL__`, `__EVENTS_BINARY_PATH__`,
/// `__HOME__` and `__EXIT_TIMEOUT__` placeholders.
///
/// Why: the installer has no workspace at runtime, so the template is embedded.
/// Its keys and layout follow `trusty_common::launchd::LaunchdConfig::render_plist`,
/// which writes the console's plist, so the two units read alike; the
/// differences are the `KeepAlive` form and the absent `PATH` (the bus runs no
/// child process).
/// What: the plist XML. `ExitTimeOut` is filled from
/// `trusty_common::shutdown::TERMINATION_GRACE_SECS`, as the shared renderer
/// does (#4393).
/// Test: `events_plist_matches_the_golden_file`,
/// `events_plist_keeps_alive_only_after_an_unsuccessful_exit`,
/// `events_plist_carries_no_tcp_endpoint`.
pub const EVENTS_PLIST_TEMPLATE: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>__PLIST_LABEL__</string>
  <key>ProgramArguments</key>
  <array>
    <string>__EVENTS_BINARY_PATH__</string>
    <string>serve</string>
  </array>
  <key>KeepAlive</key>
  <dict>
    <key>SuccessfulExit</key>
    <false/>
  </dict>
  <key>RunAtLoad</key>
  <true/>
  <key>ThrottleInterval</key>
  <integer>10</integer>
  <key>ExitTimeOut</key>
  <integer>__EXIT_TIMEOUT__</integer>
  <key>StandardOutPath</key>
  <string>__HOME__/.trusty-events/logs/stdout.log</string>
  <key>StandardErrorPath</key>
  <string>__HOME__/.trusty-events/logs/stderr.log</string>
</dict>
</plist>
"#;

/// Whether `tctl` writes this member's launchd plist itself.
///
/// Why: `service_bootstrap` shells out to `<binary> service install` for the
/// shared daemons; a member without that subcommand needs this branch instead,
/// or its install reports "skipped" and `tctl start` fails on a missing plist.
/// What: `true` for `trusty-events` only.
/// Test: `installer_owns_only_the_events_plist`.
pub fn installer_owns_plist(binary: &str) -> bool {
    binary == trusty_common::control_bus::EVENTS_SERVICE
}

/// The launchd label of the trusty-events job.
///
/// What: [`super::plist_label::plist_label_for`] for the bus member, so the
/// plist's `Label`, `tctl start|stop` and `tctl status` name one label.
/// Test: `events_label_is_the_conventional_label`.
pub fn events_label() -> String {
    super::plist_label::plist_label_for(trusty_common::control_bus::EVENTS_SERVICE)
}

/// Fill [`EVENTS_PLIST_TEMPLATE`] for a home directory and binary path.
///
/// Why: a pure function, so the golden test needs no filesystem or launchd.
/// What: XML-escapes both paths, then replaces every placeholder.
/// Test: `events_plist_matches_the_golden_file`,
/// `events_plist_escapes_the_paths_it_embeds`.
pub fn fill_events_template(home: &str, events_path: &str) -> String {
    let filled = EVENTS_PLIST_TEMPLATE
        .replace("__PLIST_LABEL__", &xml_escape(&events_label()))
        .replace("__EVENTS_BINARY_PATH__", &xml_escape(events_path))
        .replace("__HOME__", &xml_escape(home))
        .replace(
            "__EXIT_TIMEOUT__",
            &trusty_common::shutdown::TERMINATION_GRACE_SECS.to_string(),
        );
    debug_assert!(!filled.contains("__"), "unfilled placeholder in the plist");
    filled
}

/// Escape the five XML special characters in a plist `<string>` value.
fn xml_escape(raw: &str) -> String {
    raw.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// Write the trusty-events plist and bootstrap it.
///
/// Why: `tctl install trusty-events` and `tctl start trusty-events` on a host
/// with no plist both land here, so the unit exists before launchd is asked to
/// run it.
/// What: fills the template for `target.home` and `events_path`, creates
/// `~/.trusty-events/logs` and `~/Library/LaunchAgents`, writes
/// `<label>.plist`, then boots the label out (ignored: it may not be loaded)
/// and bootstraps it into `target.domain`. Returns the plist path.
///
/// # Errors
/// A non-UTF-8 path, a directory or file that cannot be written, or a failed
/// `launchctl bootstrap`.
/// Test: `install_events_plist_for_writes_and_bootstraps_through_the_stub`,
/// `install_events_plist_for_surfaces_a_bootstrap_failure`.
pub fn install_events_plist_for(
    target: &SupervisorTarget<'_>,
    events_path: &Path,
) -> anyhow::Result<PathBuf> {
    use anyhow::Context as _;

    let home = target
        .home
        .to_str()
        .context("home directory path is not valid UTF-8")?;
    let exe = events_path
        .to_str()
        .context("trusty-events binary path is not valid UTF-8")?;
    let content = fill_events_template(home, exe);

    let log_dir = target.home.join(".trusty-events").join("logs");
    let agents_dir = target.home.join("Library").join("LaunchAgents");
    let plist_path = agents_dir.join(format!("{}.plist", events_label()));
    std::fs::create_dir_all(&log_dir)
        .with_context(|| format!("creating log directory {}", log_dir.display()))?;
    std::fs::create_dir_all(&agents_dir)
        .with_context(|| format!("creating LaunchAgents dir {}", agents_dir.display()))?;
    std::fs::write(&plist_path, content)
        .with_context(|| format!("writing plist to {}", plist_path.display()))?;

    target.launchctl.bootout(&target.domain, &plist_path);
    target
        .launchctl
        .bootstrap(&target.domain, &plist_path)
        .map_err(|stderr| anyhow::anyhow!("launchctl bootstrap failed: {stderr}"))?;
    Ok(plist_path)
}

/// Production entry point: write and bootstrap the plist for the live user.
///
/// Why / What: [`install_events_plist_for`] against the real home directory,
/// `gui/<uid>` and the real `launchctl`. On other platforms launchd does not
/// exist, so it refuses.
/// Test: not exercised directly — it is the live target #3551 keeps tests
/// away from; its logic is [`install_events_plist_for`].
pub fn install_events_plist(events_path: &Path) -> anyhow::Result<()> {
    #[cfg(target_os = "macos")]
    {
        let launchctl = super::plist_bootstrap::RealLaunchctl;
        let target = SupervisorTarget::production(&launchctl)?;
        install_events_plist_for(&target, events_path).map(|_| ())
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = events_path;
        anyhow::bail!("the trusty-events launchd plist is macOS-only")
    }
}

#[cfg(test)]
#[path = "events_plist_tests.rs"]
mod tests;
