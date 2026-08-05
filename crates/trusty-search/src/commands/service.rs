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
    Install {
        /// Generate a unit that suppresses the daemon's auto-discovery scan.
        ///
        /// #4823: without this there was no supported way to make
        /// `--no-auto-discover` durable — hand-editing the plist worked until
        /// the next `service install` regenerated it. The flag is written into
        /// the unit's `ProgramArguments`, and a subsequent `service install`
        /// preserves it unless `--auto-discover` is passed.
        #[arg(long, conflicts_with = "auto_discover")]
        no_auto_discover: bool,

        /// Re-enable auto-discovery, dropping a suppression the installed unit
        /// carried (#4823). Required to turn the scan back on, so it is never
        /// re-enabled as a silent side effect of reinstalling.
        #[arg(long)]
        auto_discover: bool,
    },
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
pub(crate) const LAUNCHD_LABEL: &str = "com.trusty.trusty-search";

/// Dispatch a `trusty-search service <action>` invocation.
pub fn handle_service(action: &ServiceAction) -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        match action {
            ServiceAction::Install {
                no_auto_discover,
                auto_discover,
            } => service_install(*no_auto_discover, *auto_discover),
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
///
/// #4823: `service install` is normally run from a plain shell exporting none
/// of the `TRUSTY_*` tunables, so reading only the process env blanked every
/// tunable the installed unit already carried. Values now carry forward from
/// `existing` when the process env does not supply them. `TRUSTY_NO_AUTO_DISCOVER`
/// is deliberately never emitted here — see
/// [`crate::commands::service_unit::resolve_persisted_env`].
///
/// What: always emits an `HF_HOME` entry resolved at install time, plus the
/// `PERSISTED_ENV_VARS` resolved from the process env with the installed unit
/// as fallback. The env lookup is injected so the assembly is testable without
/// mutating a shared process env from a parallel test binary.
/// Test: `launchd_env_pairs_never_carries_no_auto_discover`,
/// `launchd_env_pairs_carries_forward_installed_tunables`; the resolution
/// rules themselves are covered by `service_unit::resolve_persisted_env_*`.
#[cfg(target_os = "macos")]
fn launchd_env_vars(
    existing: Option<&crate::commands::service_unit::InstalledUnit>,
) -> Vec<(String, String)> {
    launchd_env_pairs(dirs::home_dir(), |key| std::env::var(key).ok(), existing)
}

/// Pure assembly behind [`launchd_env_vars`] — see its docs.
#[cfg(target_os = "macos")]
fn launchd_env_pairs(
    home: Option<std::path::PathBuf>,
    lookup: impl Fn(&str) -> Option<String>,
    existing: Option<&crate::commands::service_unit::InstalledUnit>,
) -> Vec<(String, String)> {
    use crate::commands::service_unit::resolve_persisted_env;

    let mut pairs: Vec<(String, String)> = Vec::new();

    // Always pin HF_HOME to $HOME/.cache/huggingface resolved at install time.
    if let Some(home) = home {
        let hf_home = home.join(".cache").join("huggingface");
        pairs.push(("HF_HOME".to_string(), hf_home.display().to_string()));
    }

    // Operator tunables: process env wins, installed unit is the fallback.
    pairs.extend(resolve_persisted_env(lookup, existing));

    pairs
}

/// Read the currently-installed LaunchAgent plist, if there is one.
///
/// Why (#4823): regeneration must know what the unit on disk already says
/// before overwriting it, or deliberate operator configuration is discarded.
/// What: reads `~/Library/LaunchAgents/<LAUNCHD_LABEL>.plist` and parses its
/// `ProgramArguments` / `EnvironmentVariables`. Any failure (no home dir, no
/// file, unreadable) yields `None` — "nothing to preserve", which is exactly
/// the pre-#4823 behaviour, so a broken read can never make install fail.
/// Test: filesystem-dependent; the parsing it delegates to is covered by
/// `service_unit::parse_installed_unit_*`.
#[cfg(target_os = "macos")]
fn installed_unit() -> Option<crate::commands::service_unit::InstalledUnit> {
    let path = dirs::home_dir()?
        .join("Library")
        .join("LaunchAgents")
        .join(format!("{LAUNCHD_LABEL}.plist"));
    let xml = std::fs::read_to_string(path).ok()?;
    Some(crate::commands::service_unit::parse_installed_unit(&xml))
}

/// Build the shared `LaunchdConfig` describing the trusty-search agent.
///
/// Why: install/uninstall/status all need the same plist label, log paths,
/// and env-var set. Building it in one place keeps them in sync.
///
/// 🔴 fd limits: macOS launchd's default soft fd ceiling for user agents is
/// 256. trusty-search warm-boots one index per registered directory, each
/// holding multiple on-disk segment files plus sockets and log descriptors;
/// at ~450 indexes (observed during the 0.34.0 release install, issue #2947)
/// the process hit EMFILE mid warm-boot and needed a manual local plist
/// patch to recover. The generated plist always sets both
/// `SoftResourceLimits` and `HardResourceLimits` to
/// [`trusty_common::launchd::LAUNCHD_FD_LIMIT`] (8192), matching the
/// trusty-memory convention, so the limit is permanent and survives
/// `service install` regeneration instead of requiring hand-patching.
///
/// 🔴 restart policy: [`KeepAlive::Always`], not `OnSuccess`. Under
/// `OnSuccess` (`SuccessfulExit: false`) launchd restarts the daemon *only*
/// after a non-zero exit, so any clean exit — a plain SIGTERM, an orderly
/// drain, `trusty-search stop` — left search down indefinitely with no
/// recovery and no alarm (issue #4113: a 2026-07-27 outage ran from 13:27:22Z
/// until an operator hand-ran `launchctl kickstart`). Every consumer silently
/// degraded rather than erroring. `Always` is a strict superset of the old
/// policy — the non-zero-exit restarts it already performed still happen —
/// so the only behavioural delta is that a clean exit now comes back too.
/// Deliberate "stop it and leave it stopped" stays expressible through
/// launchd's own unload path (`launchctl bootout gui/$(id -u)/<label>`, or
/// `trusty-search service uninstall`), which removes the job entirely and
/// therefore outranks any `KeepAlive` setting.
///
/// 🔴 auto-discovery (#4823): the suppression travels as a
/// `ProgramArguments` flag, never as a `TRUSTY_NO_AUTO_DISCOVER`
/// `EnvironmentVariables` entry. The daemon declares `--no-auto-discover` as
/// a clap `bool` with `env = "TRUSTY_NO_AUTO_DISCOVER"`, so an env value is
/// parsed rather than merely detected; emitting a value the parser rejects
/// aborts startup and launchd then throttle-loops a daemon that never comes
/// up. `ProgramArguments` has no such failure mode and is legible to a human
/// reading the plist.
///
/// What: assembles a [`trusty_common::launchd::LaunchdConfig`] using
/// `start --foreground` as the entry point (plus `--no-auto-discover` when
/// `suppress_auto_discover`) and `KeepAlive::Always` so a clean shutdown is
/// recovered from automatically.
/// Test: `build_launchd_config_sets_fd_limit` and
/// `build_launchd_config_plist_includes_fd_limit` assert the fd ceiling is
/// wired into both the config struct and the rendered plist XML (issue
/// #2947 regression guard); `build_launchd_config_keeps_alive_after_clean_exit`
/// and `build_launchd_config_plist_has_unconditional_keepalive` pin the #4113
/// restart policy; `build_launchd_config_plist_carries_no_auto_discover_arg`,
/// `build_launchd_config_omits_no_auto_discover_by_default` and
/// `launchd_env_vars_never_carries_no_auto_discover` pin #4823. Also exercised
/// via service install/uninstall.
#[cfg(target_os = "macos")]
fn build_launchd_config(
    exe: std::path::PathBuf,
    log_dir: std::path::PathBuf,
    suppress_auto_discover: bool,
    existing: Option<&crate::commands::service_unit::InstalledUnit>,
) -> trusty_common::launchd::LaunchdConfig {
    use crate::commands::service_unit::NO_AUTO_DISCOVER_ARG;
    use trusty_common::launchd::{KeepAlive, LaunchdConfig, LAUNCHD_FD_LIMIT};

    let mut args = vec!["start".to_string(), "--foreground".to_string()];
    // #4823: express the operator's auto-discovery choice as a CLI flag so it
    // survives regeneration and cannot carry an unparseable env value.
    if suppress_auto_discover {
        args.push(NO_AUTO_DISCOVER_ARG.to_string());
    }

    LaunchdConfig {
        label: LAUNCHD_LABEL.to_string(),
        exe_path: exe,
        args,
        log_dir,
        // #4113: unconditional restart — `OnSuccess` left the daemon down
        // permanently after any clean (exit 0) shutdown.
        keep_alive: KeepAlive::Always,
        throttle_interval: 30,
        env_vars: launchd_env_vars(existing),
        // Fix fd exhaustion during large-fleet warm-boot (issue #2947):
        // raise both soft and hard limits to 8192 so the daemon can hold
        // thousands of open index files before hitting EMFILE.
        fd_limit: Some(LAUNCHD_FD_LIMIT),
    }
}

/// Report whether the trusty-search LaunchAgent is currently loaded.
///
/// Why: #4113 moved the agent to `KeepAlive::Always`, so a launchd-supervised
/// daemon comes back roughly `throttle_interval` seconds after
/// `trusty-search stop`'s SIGTERM. `stop` uses this to tell the operator that
/// its stop is temporary and to name the command that is not.
/// What: builds a label-only [`trusty_common::launchd::LaunchdConfig`] — every
/// other field is inert for `is_loaded`, which only runs
/// `launchctl print gui/<uid>/<label>` — and returns its `is_loaded()`. Never
/// errors: an unqueryable launchd reads as "not loaded" so `stop` stays quiet
/// rather than printing a hint it cannot justify.
/// Test: side-effecting `launchctl` call; the message it gates is covered by
/// `stop::tests::launchd_restart_notice_*`.
#[cfg(target_os = "macos")]
pub(crate) fn launchd_agent_loaded() -> bool {
    use trusty_common::launchd::{KeepAlive, LaunchdConfig};
    LaunchdConfig {
        label: LAUNCHD_LABEL.to_string(),
        exe_path: std::path::PathBuf::new(),
        args: Vec::new(),
        log_dir: std::path::PathBuf::new(),
        keep_alive: KeepAlive::Always,
        throttle_interval: 0,
        env_vars: Vec::new(),
        fd_limit: None,
    }
    .is_loaded()
}

/// Generate, write, and load the LaunchAgent.
///
/// Why (#4823): regeneration is what `service install` is *for*, so it must
/// not be the thing that discards deliberate operator configuration. The
/// installed unit is read first and its auto-discovery setting carried
/// forward unless this invocation asks otherwise; every outcome is announced
/// so a capability change is never silent.
/// What: resolves the auto-discovery decision, prints it, renders the plist
/// (preserving known `EnvironmentVariables` the old unit carried), installs
/// and bootstraps it, then installs log rotation.
/// Test: side-effecting install; the decision and rendering it depends on are
/// covered by `service_unit::resolve_auto_discover_*` and
/// `build_launchd_config_plist_carries_no_auto_discover_arg`.
#[cfg(target_os = "macos")]
fn service_install(request_off: bool, request_on: bool) -> Result<()> {
    use crate::commands::service_unit::{resolve_auto_discover, AutoDiscover};

    let exe = std::env::current_exe()
        .map_err(|e| anyhow::anyhow!("could not resolve current exe: {e}"))?;
    let log_dir = launchd_log_dir()?;

    // #4823: read the unit we are about to overwrite so its settings survive.
    let existing = installed_unit();
    let decision = resolve_auto_discover(request_off, request_on, existing.as_ref());
    match decision {
        AutoDiscover::Suppress { preserved: true } => println!(
            "{} Preserved auto-discovery suppression from the installed unit \
             — pass {} to re-enable it.",
            "·".dimmed(),
            "--auto-discover".cyan()
        ),
        AutoDiscover::Suppress { preserved: false } => println!(
            "{} Auto-discovery disabled: the unit runs {}",
            "·".dimmed(),
            "start --foreground --no-auto-discover".cyan()
        ),
        AutoDiscover::Enable { dropped: true } => println!(
            "{} Dropped the installed unit's auto-discovery suppression — the \
             daemon will scan its configured scan_paths again.",
            "⚠".yellow()
        ),
        AutoDiscover::Enable { dropped: false } => {}
    }

    let cfg = build_launchd_config(
        exe,
        log_dir.clone(),
        decision.suppressed(),
        existing.as_ref(),
    );
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
    // Uninstall only needs the label to locate and bootout the plist, so the
    // auto-discovery and preservation inputs are inert here.
    let cfg = build_launchd_config(exe, log_dir, false, None);
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

#[cfg(test)]
mod tests {
    // `super::*` (in particular `build_launchd_config`) is only referenced by
    // the macOS-only tests below; on other platforms the whole module body
    // compiles out to nothing, so gate the import to avoid an
    // `unused_imports` -D warnings failure on non-macOS CI runners (#2947
    // follow-up).
    #[cfg(target_os = "macos")]
    use super::*;

    /// Why: the LaunchdConfig handed to `trusty_common::launchd` must always
    /// carry the fd-limit fix — dropping it silently reintroduces the
    /// large-fleet warm-boot EMFILE crash (issue #2947).
    /// What: builds the config with dummy paths and asserts `fd_limit` is
    /// `Some(LAUNCHD_FD_LIMIT)`.
    /// Test: pure construction, no fs side effects.
    #[cfg(target_os = "macos")]
    #[test]
    fn build_launchd_config_sets_fd_limit() {
        use std::path::PathBuf;
        use trusty_common::launchd::LAUNCHD_FD_LIMIT;

        let cfg = build_launchd_config(
            PathBuf::from("/usr/local/bin/trusty-search"),
            PathBuf::from("/tmp/trusty-search/logs"),
            false,
            None,
        );
        assert_eq!(
            cfg.fd_limit,
            Some(LAUNCHD_FD_LIMIT),
            "fd_limit must be Some(LAUNCHD_FD_LIMIT) so the generated plist \
             raises both soft and hard NumberOfFiles limits to \
             {LAUNCHD_FD_LIMIT}, preventing EMFILE during large-fleet \
             warm-boot (issue #2947)"
        );
    }

    /// Why: the generated plist XML (what launchd actually reads from disk)
    /// must contain both resource-limit dicts with the canonical fd value —
    /// asserting on `render_plist()` output catches regressions where the
    /// config struct is correct but the renderer drops the dicts.
    /// What: renders the plist with a dummy exe/log dir and checks that the
    /// `SoftResourceLimits` / `HardResourceLimits` / `NumberOfFiles` keys
    /// appear with the right integer value.
    /// Test: pure string generation, no fs side effects.
    #[cfg(target_os = "macos")]
    #[test]
    fn build_launchd_config_plist_includes_fd_limit() {
        use std::path::PathBuf;
        use trusty_common::launchd::LAUNCHD_FD_LIMIT;

        let cfg = build_launchd_config(
            PathBuf::from("/usr/local/bin/trusty-search"),
            PathBuf::from("/tmp/trusty-search/logs"),
            false,
            None,
        );
        let xml = cfg.render_plist().expect("render_plist must succeed");

        assert!(
            xml.contains("<key>SoftResourceLimits</key>"),
            "plist must contain SoftResourceLimits to raise the fd ceiling"
        );
        assert!(
            xml.contains("<key>HardResourceLimits</key>"),
            "plist must contain HardResourceLimits so the soft limit is not \
             clamped below it"
        );
        let fd_str = format!("<integer>{LAUNCHD_FD_LIMIT}</integer>");
        assert!(
            xml.contains(&fd_str),
            "plist NumberOfFiles must equal {LAUNCHD_FD_LIMIT}, got xml: {xml}"
        );
    }

    /// Why: issue #4113 — `KeepAlive::OnSuccess` (`SuccessfulExit: false`)
    /// restarts the daemon only after a NON-zero exit, so a clean SIGTERM /
    /// orderly drain left search down indefinitely with no recovery and no
    /// alarm. Reverting this field to `OnSuccess` silently reintroduces a
    /// permanent-outage class of bug that no other test would catch.
    /// What: builds the config with dummy paths and asserts `keep_alive` is
    /// `KeepAlive::Always`.
    /// Test: pure construction, no fs side effects.
    #[cfg(target_os = "macos")]
    #[test]
    fn build_launchd_config_keeps_alive_after_clean_exit() {
        use std::path::PathBuf;
        use trusty_common::launchd::KeepAlive;

        let cfg = build_launchd_config(
            PathBuf::from("/usr/local/bin/trusty-search"),
            PathBuf::from("/tmp/trusty-search/logs"),
            false,
            None,
        );
        assert_eq!(
            cfg.keep_alive,
            KeepAlive::Always,
            "keep_alive must be KeepAlive::Always so launchd restarts the \
             daemon after a CLEAN (exit 0) shutdown too — KeepAlive::OnSuccess \
             leaves it down permanently (issue #4113). Deliberate stops go \
             through `launchctl bootout` / `trusty-search service uninstall`."
        );
    }

    /// Why: the config struct can be right while the renderer emits the wrong
    /// plist fragment — launchd reads the XML, not the struct. This asserts on
    /// what actually lands in `~/Library/LaunchAgents`.
    /// What: renders the plist and requires an unconditional
    /// `<key>KeepAlive</key><true/>` with no `SuccessfulExit` dictionary
    /// anywhere in the document.
    /// Test: pure string generation, no fs side effects.
    #[cfg(target_os = "macos")]
    #[test]
    fn build_launchd_config_plist_has_unconditional_keepalive() {
        use std::path::PathBuf;

        let cfg = build_launchd_config(
            PathBuf::from("/usr/local/bin/trusty-search"),
            PathBuf::from("/tmp/trusty-search/logs"),
            false,
            None,
        );
        let xml = cfg.render_plist().expect("render_plist must succeed");

        assert!(
            xml.contains("<key>KeepAlive</key>\n  <true/>"),
            "plist must set KeepAlive unconditionally true (issue #4113), got \
             xml: {xml}"
        );
        assert!(
            !xml.contains("SuccessfulExit"),
            "plist must NOT carry a SuccessfulExit dict — that is the \
             restart-only-on-failure policy #4113 removed, got xml: {xml}"
        );
    }

    /// Why (issue #4823): the whole point of the fix is that the operator's
    /// auto-discovery suppression reaches the file launchd actually reads.
    /// Asserting on `render_plist()` rather than the config struct is what
    /// catches the #4709 failure mode where the struct is right and the
    /// renderer drops the fragment.
    /// What: builds a suppressing config and requires `--no-auto-discover` to
    /// appear inside `ProgramArguments`, after `start --foreground`.
    /// Test: pure string generation, no fs side effects.
    #[cfg(target_os = "macos")]
    #[test]
    fn build_launchd_config_plist_carries_no_auto_discover_arg() {
        use std::path::PathBuf;

        let cfg = build_launchd_config(
            PathBuf::from("/usr/local/bin/trusty-search"),
            PathBuf::from("/tmp/trusty-search/logs"),
            true,
            None,
        );
        assert_eq!(
            cfg.args,
            vec!["start", "--foreground", "--no-auto-discover"],
            "the suppression must travel as a CLI arg (issue #4823)"
        );

        let xml = cfg.render_plist().expect("render_plist must succeed");
        let args_section = xml
            .split("<key>ProgramArguments</key>")
            .nth(1)
            .and_then(|rest| rest.split("</array>").next())
            .unwrap_or_default();
        assert!(
            args_section.contains("<string>--no-auto-discover</string>"),
            "ProgramArguments must carry --no-auto-discover so the setting \
             survives regeneration (issue #4823), got xml: {xml}"
        );
        // The #4709 restart policy must survive alongside the new arg.
        assert!(
            xml.contains("<key>KeepAlive</key>\n  <true/>"),
            "KeepAlive must stay unconditionally true (issue #4113/#4709) when \
             auto-discovery is suppressed, got xml: {xml}"
        );
    }

    /// Why: the default install must be byte-identical to pre-#4823 behaviour
    /// — adding the flag unconditionally would disable discovery for everyone.
    /// What: a non-suppressing config must not mention the flag anywhere.
    /// Test: pure string generation, no fs side effects.
    #[cfg(target_os = "macos")]
    #[test]
    fn build_launchd_config_omits_no_auto_discover_by_default() {
        use std::path::PathBuf;

        let cfg = build_launchd_config(
            PathBuf::from("/usr/local/bin/trusty-search"),
            PathBuf::from("/tmp/trusty-search/logs"),
            false,
            None,
        );
        assert_eq!(cfg.args, vec!["start", "--foreground"]);
        let xml = cfg.render_plist().expect("render_plist must succeed");
        assert!(
            !xml.contains("no-auto-discover"),
            "default install must not suppress auto-discovery, got xml: {xml}"
        );
    }

    /// Why (issue #4823 comment — the trap): the reporter's `daemon.env` holds
    /// `TRUSTY_NO_AUTO_DISCOVER=1`. Copying that into the plist's
    /// `EnvironmentVariables` is what made the daemon refuse to boot, because
    /// clap parses the env value through `FromStr<bool>`. The generated plist
    /// must never carry that key at all, whatever the process env or the
    /// installed unit says.
    /// What: drives the assembly with a lookup that returns `1` for the var AND
    /// an installed unit carrying `TRUSTY_NO_AUTO_DISCOVER=1`, then requires
    /// both the pairs and the rendered XML to be free of the key.
    /// Test: pure — the env lookup is injected, so no process env is mutated.
    #[cfg(target_os = "macos")]
    #[test]
    fn launchd_env_pairs_never_carries_no_auto_discover() {
        use crate::commands::service_unit::{parse_installed_unit, NO_AUTO_DISCOVER_ENV};
        use std::path::PathBuf;

        let existing = parse_installed_unit(
            "<key>EnvironmentVariables</key><dict>\
             <key>TRUSTY_NO_AUTO_DISCOVER</key><string>1</string></dict>",
        );
        assert!(
            existing.suppresses_auto_discover(),
            "fixture must represent the hand-edited unit"
        );

        let pairs = launchd_env_pairs(
            Some(PathBuf::from("/Users/x")),
            |k| (k == NO_AUTO_DISCOVER_ENV).then(|| "1".to_string()),
            Some(&existing),
        );
        assert!(
            !pairs.iter().any(|(k, _)| k == NO_AUTO_DISCOVER_ENV),
            "launchd_env_pairs must never emit {NO_AUTO_DISCOVER_ENV}, got {pairs:?}"
        );

        // The renderer is the thing launchd reads — assert there too. The
        // suppression must survive as a CLI arg while the env key stays absent.
        let mut cfg = build_launchd_config(
            PathBuf::from("/usr/local/bin/trusty-search"),
            PathBuf::from("/tmp/trusty-search/logs"),
            true,
            Some(&existing),
        );
        cfg.env_vars = pairs;
        let xml = cfg.render_plist().expect("render_plist must succeed");
        assert!(
            !xml.contains(NO_AUTO_DISCOVER_ENV),
            "the rendered plist must never carry {NO_AUTO_DISCOVER_ENV} — a \
             value clap rejects aborts daemon startup (issue #4823), got xml: {xml}"
        );
        assert!(
            xml.contains("<string>--no-auto-discover</string>"),
            "the suppression must still be preserved, as a CLI arg, got xml: {xml}"
        );
    }

    /// Why (issue #4823, generalised): `service install` runs from a shell that
    /// exports nothing, so reading only the process env blanked tunables the
    /// installed unit carried — silently unpinning e.g. `TRUSTY_DEVICE=cpu`.
    /// What: with an empty env lookup, the installed unit's value survives; the
    /// unconditional `HF_HOME` pin (#86) is still emitted alongside it.
    /// Test: pure — the env lookup is injected, so no process env is mutated.
    #[cfg(target_os = "macos")]
    #[test]
    fn launchd_env_pairs_carries_forward_installed_tunables() {
        use crate::commands::service_unit::parse_installed_unit;
        use std::path::PathBuf;

        let existing = parse_installed_unit(
            "<key>EnvironmentVariables</key><dict>\
             <key>TRUSTY_BM25_CORPUS_CAP</key><string>4242</string></dict>",
        );
        let pairs = launchd_env_pairs(Some(PathBuf::from("/Users/x")), |_| None, Some(&existing));
        assert!(
            pairs
                .iter()
                .any(|(k, v)| k == "TRUSTY_BM25_CORPUS_CAP" && v == "4242"),
            "an installed unit's tunable must survive regeneration (issue \
             #4823), got {pairs:?}"
        );
        assert!(
            pairs
                .iter()
                .any(|(k, v)| k == "HF_HOME" && v == "/Users/x/.cache/huggingface"),
            "the #86 HF_HOME pin must still be emitted, got {pairs:?}"
        );
    }
}
