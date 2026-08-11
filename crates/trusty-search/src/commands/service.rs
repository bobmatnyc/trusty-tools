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

        /// Reload the agent even when the rendered unit is unchanged.
        ///
        /// #4868: install skips the reload when the plist is byte-identical and
        /// the label is loaded, which is what stops a re-run costing a restart.
        /// That is wrong after the BINARY changed — `make deploy` installs a new
        /// executable behind an identical plist, and without this flag launchd
        /// keeps running the old image. Deploy paths pass `--force`; operators
        /// re-running install do not need it.
        #[arg(long)]
        force: bool,
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
///
/// #4868: this was the literal `"com.trusty.trusty-search"`, which is not the
/// label launchd has loaded — the live unit is `com.trusty.search`. So
/// `service install` wrote and bootstrapped a SECOND unit, evicted nothing, and
/// left #4868's own `ExitTimeOut` plist fix in a file launchd never reads.
/// Re-exported from the canonical registry rather than restated, because
/// correcting the literal is what was already done for #2827 and the defect
/// came back elsewhere.
#[cfg(target_os = "macos")]
pub(crate) const LAUNCHD_LABEL: &str = trusty_common::launchd_labels::SEARCH;

/// Dispatch a `trusty-search service <action>` invocation.
pub fn handle_service(action: &ServiceAction) -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        match action {
            ServiceAction::Install {
                no_auto_discover,
                auto_discover,
                force,
            } => service_install(*no_auto_discover, *auto_discover, *force),
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
/// #4868: install now overwrites the LIVE plist rather than a differently-named
/// one, so anything this function fails to reproduce is DESTROYED rather than
/// merely absent from a file nobody read. The named-tunable allowlist was not
/// enough — see
/// [`crate::commands::service_unit::resolve_persisted_env`], which now carries
/// forward every key the installed unit had.
///
/// #4829: the unit also carries `RUST_LOG`, defaulting to `info`. Without it
/// launchd exec'd the daemon with `RUST_LOG` unset, `tracing_init` fell back to
/// `"warn"`, and every `tracing::info!` the daemon writes about its own boot was
/// dropped. An installed-unit or shell value still wins — the default is only
/// applied when neither supplies one.
///
/// What: always emits an `HF_HOME` entry resolved at install time, plus every
/// env var the installed unit carried, with the process env taking precedence,
/// plus a `RUST_LOG` fallback. Keys this function computes itself (`HF_HOME`)
/// are never carried forward, so a stale value cannot outrank the freshly
/// resolved one. The env lookup is injected so the assembly is testable without
/// mutating a shared process env from a parallel test binary.
/// Test: `launchd_env_pairs_never_carries_no_auto_discover`,
/// `launchd_env_pairs_carries_forward_installed_tunables`,
/// `launchd_env_pairs_carries_forward_unanticipated_keys`,
/// `launchd_env_pairs_defaults_rust_log_to_info`,
/// `launchd_env_pairs_keeps_an_operator_rust_log`; the resolution rules
/// themselves are covered by `service_unit::resolve_persisted_env_*`.
#[cfg(target_os = "macos")]
fn launchd_env_vars(
    existing: Option<&crate::commands::service_unit::InstalledUnit>,
) -> Vec<(String, String)> {
    launchd_env_pairs(dirs::home_dir(), |key| std::env::var(key).ok(), existing)
}

/// Env keys the generated template resolves itself, so they are never carried
/// forward from an installed unit.
///
/// `HF_HOME` is recomputed at install time (#86) and `PATH` is seeded by
/// `with_daemon_path` (#1298); a stale value for either must not win.
#[cfg(target_os = "macos")]
const TEMPLATE_OWNED_ENV: &[&str] = &["HF_HOME", "PATH"];

/// Tracing filter env var the daemon reads through `trusty_common::init_tracing`.
#[cfg(target_os = "macos")]
const RUST_LOG_ENV: &str = "RUST_LOG";

/// Verbosity the generated unit falls back to when nothing else supplies one.
///
/// Why (#4829): launchd re-spawns the daemon with no shell environment, so
/// `RUST_LOG` arrived unset and `trusty_common::tracing_init` fell back to
/// `"warn"` — every `tracing::info!` line the daemon writes to diagnose itself
/// was dropped, including the two that confirm auto-discovery suppression. A
/// daemon that logs nothing at INFO cannot be diagnosed from its own logs.
/// What: `"info"`, emitted only as a default — see [`launchd_env_pairs`].
#[cfg(target_os = "macos")]
const DEFAULT_LAUNCHD_RUST_LOG: &str = "info";

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

    // Operator tunables and everything else the unit carried: process env wins,
    // installed unit is the fallback (#4868).
    pairs.extend(resolve_persisted_env(&lookup, existing, TEMPLATE_OWNED_ENV));

    // #4829: give the unit a RUST_LOG so the daemon logs at INFO under launchd.
    // Lowest precedence of the three sources — a value the installed unit
    // already carried, or one exported in the installing shell, is picked up by
    // `resolve_persisted_env` above and is left alone here.
    if !pairs.iter().any(|(k, _)| k == RUST_LOG_ENV) {
        let level = lookup(RUST_LOG_ENV).unwrap_or_else(|| DEFAULT_LAUNCHD_RUST_LOG.to_string());
        pairs.push((RUST_LOG_ENV.to_string(), level));
    }

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
///
/// #4868: reading only `<LAUNCHD_LABEL>.plist` defeats #4823 precisely on the
/// migration path this issue introduces. On the host the whole premise
/// describes — one whose live unit carries the LEGACY name — the canonical file
/// does not exist yet, so the read returned `None`, every preserved tunable
/// (`TRUSTY_NO_AUTO_DISCOVER`, `TRUSTY_DEVICE`, `TRUSTY_BM25_CORPUS_CAP`) was
/// silently dropped, and eviction then deleted the legacy plist that held the
/// only record. The legacy plists are therefore consulted as a fallback, and
/// this must run BEFORE eviction.
///
/// What: reads `~/Library/LaunchAgents/<LAUNCHD_LABEL>.plist`; when absent,
/// falls back to the first present legacy plist for this label, in registry
/// order (newest alias first). Parses `ProgramArguments` /
/// `EnvironmentVariables`. Any failure yields `None` — "nothing to preserve" —
/// so a broken read can never make install fail.
/// Test: `installed_unit_paths_prefers_canonical_then_legacy`; the parsing it
/// delegates to is covered by `service_unit::parse_installed_unit_*`.
#[cfg(target_os = "macos")]
fn installed_unit() -> Option<crate::commands::service_unit::InstalledUnit> {
    let home = dirs::home_dir()?;
    for path in installed_unit_paths(&home) {
        if let Ok(xml) = std::fs::read_to_string(&path) {
            return Some(crate::commands::service_unit::parse_installed_unit(&xml));
        }
    }
    None
}

/// Candidate plist paths to read operator configuration from, best first.
///
/// Why: kept pure and separate from the filesystem read so the ordering — the
/// thing #4868 got wrong — is testable without a real `~/Library/LaunchAgents`.
/// What: the canonical `<label>.plist`, then each legacy alias's plist in
/// registry order.
/// Test: `installed_unit_paths_prefers_canonical_then_legacy`.
#[cfg(target_os = "macos")]
fn installed_unit_paths(home: &std::path::Path) -> Vec<std::path::PathBuf> {
    let agents = home.join("Library").join("LaunchAgents");
    std::iter::once(LAUNCHD_LABEL)
        .chain(
            trusty_common::launchd_labels::legacy_labels_for(LAUNCHD_LABEL)
                .iter()
                .copied(),
        )
        .map(|label| agents.join(format!("{label}.plist")))
        .collect()
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
        // #4868: the live unit carries one; regenerating without it would
        // silently change the daemon's working directory on upgrade.
        working_directory: existing.and_then(|u| u.working_directory.clone()),
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
        working_directory: None,
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
fn service_install(request_off: bool, request_on: bool, force: bool) -> Result<()> {
    use crate::commands::service_unit::{resolve_auto_discover, AutoDiscover};
    use trusty_common::launchd_activate::Activation;

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
    let domain = format!("gui/{}", trusty_common::launchd::current_uid());

    // #4868: activate through the label-correct path — evict the labels earlier
    // installs registered (`com.trusty.trusty-search`, `com.bobmatnyc.trusty-search`)
    // so this install cannot leave a second daemon fighting for :7878 and the
    // index locks (#2938), reload only when the unit actually changed, and roll
    // back rather than leave the service down if the bootstrap fails.
    let outcome = cfg.install_and_activate_forced(
        trusty_common::launchd_labels::legacy_labels_for(LAUNCHD_LABEL),
        force,
    )?;
    for label in outcome.evicted() {
        println!(
            "{} Evicted the stale LaunchAgent {} — it named the same daemon \
             under an old label.",
            "⚠".yellow(),
            label.cyan()
        );
    }
    match outcome {
        Activation::AlreadyCurrent { .. } => println!(
            "{} {} is already loaded in {} with this exact unit — left running.",
            "·".dimmed(),
            LAUNCHD_LABEL,
            domain
        ),
        Activation::Activated { .. } => {
            println!(
                "{} Wrote LaunchAgent plist: {}",
                "✓".green(),
                plist_path.display()
            );
            println!(
                "{} Loaded {} into {} — daemon will start automatically.",
                "✓".green(),
                LAUNCHD_LABEL,
                domain
            );
        }
    }

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
#[path = "service_tests.rs"]
mod tests;
