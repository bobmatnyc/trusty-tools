//! Handler for `trusty-memory service` (macOS launchd integration).
//!
//! Why: launchd is the canonical way to keep a long-lived foreground daemon
//! alive on macOS — it survives logout, restarts on crash, and integrates with
//! `launchctl` for diagnostics. Wrapping the plist mechanics in `service`
//! subcommands keeps users from having to hand-edit XML. This mirrors the
//! pattern used by `trusty-search service`, sharing the
//! [`trusty_common::launchd`] implementation so the two tools cannot drift.
//! What: macOS routes to `service_install` / `service_start` / `service_stop`
//! / `service_logs`. Non-macOS prints a "not supported" error and exits 1.
//! Test: on Linux, every action returns Err with the platform message; on
//! macOS, `service install` writes the plist without loading it, `start`
//! bootstraps it, `stop` boots it out, and `logs` tails the log files.

use anyhow::Result;
use clap::Subcommand;
#[cfg(target_os = "macos")]
use colored::Colorize;

/// Subcommands for `trusty-memory service` (macOS launchd integration).
///
/// Why: the four lifecycle actions (install, start, stop, logs) are the
/// minimum surface needed to manage a launchd-backed daemon without
/// hand-editing plists or shelling out to `launchctl` directly.
/// What: a clap-derived enum dispatched by [`handle_service`].
/// Test: clap's `--help` enumerates all four; integration via
/// `cargo run -p trusty-memory -- service --help`.
#[derive(Debug, Clone, Subcommand)]
pub enum ServiceAction {
    /// Install the LaunchAgent plist (does not load it).
    Install,
    /// Install and load the LaunchAgent (start the daemon).
    Start,
    /// Unload the LaunchAgent (stop the daemon).
    Stop,
    /// Tail the launchd stdout / stderr logs.
    Logs,
}

/// Reverse-DNS label for the LaunchAgent.
///
/// Why: launchd identifies agents by their `Label`, which must also be the
/// plist filename's stem. Centralising the constant keeps install / start /
/// stop in lockstep.
/// What: `com.trusty.memory` — matches the naming convention used by
/// `trusty-search` (`com.trusty.trusty-search`) and follows reverse-DNS.
/// Test: covered indirectly by `service install` integration runs.
#[cfg(target_os = "macos")]
pub const LAUNCHD_LABEL: &str = "com.trusty.memory";

/// Dispatch a `trusty-memory service <action>` invocation.
///
/// Why: the binary's `main.rs` should not contain `#[cfg]` blocks — it
/// always calls this function and lets the module decide what is and isn't
/// supported on the current platform.
/// What: on macOS, dispatches to the per-action helper. On every other
/// platform, returns an error with a friendly message pointing operators to
/// their native service manager.
/// Test: on Linux CI, asserts the Err message contains "not supported".
pub fn handle_service(action: &ServiceAction) -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        match action {
            ServiceAction::Install => service_install(),
            ServiceAction::Start => service_start(),
            ServiceAction::Stop => service_stop(),
            ServiceAction::Logs => service_logs(),
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = action;
        anyhow::bail!(
            "`trusty-memory service` is not supported on this platform — \
             use your distro's service manager (systemd, OpenRC, etc.) directly."
        );
    }
}

/// Resolve the log directory for the launchd-managed daemon.
///
/// Why: launchd writes `stdout` and `stderr` to files we declare in the
/// plist, and they need a real directory before the daemon can start.
/// Centralising the path keeps install / logs in agreement.
/// What: `<data_dir>/trusty-memory/logs`, where `<data_dir>` comes from
/// `dirs::data_dir()` (`~/Library/Application Support` on macOS). Creates
/// the directory if it does not already exist.
/// Test: covered indirectly by `service install` integration runs.
#[cfg(target_os = "macos")]
pub(crate) fn launchd_log_dir() -> Result<std::path::PathBuf> {
    let data =
        dirs::data_dir().ok_or_else(|| anyhow::anyhow!("could not resolve user data directory"))?;
    let dir = data.join("trusty-memory").join("logs");
    std::fs::create_dir_all(&dir)
        .map_err(|e| anyhow::anyhow!("create log dir {}: {e}", dir.display()))?;
    Ok(dir)
}

/// Build the shared `LaunchdConfig` describing the trusty-memory agent.
///
/// Why: install / start / stop all need the same plist label, log paths,
/// and arg vector. Building it in one place keeps them in sync and lets the
/// shared [`trusty_common::launchd`] module own the XML rendering and the
/// `launchctl` glue.
///
/// 🔴 The args MUST invoke `serve --foreground` rather than bare `serve`.
/// Plain `serve` self-spawns a detached child and exits 0 (matching
/// `trusty-search start`'s background-mode behaviour), which launchd
/// interprets as "service stopped" — it then re-launches the agent in a
/// tight loop, creating orphan daemon processes and breaking auto-restart
/// on reboot (issue #132). `--foreground` keeps the HTTP daemon in this
/// process so launchd supervises the actual daemon PID and `KeepAlive`
/// works correctly.
///
/// What: assembles a [`trusty_common::launchd::LaunchdConfig`] pointing at
/// the current binary with `serve --foreground` so launchd supervises the
/// daemon process directly; uses `KeepAlive::OnSuccess` so a clean shutdown
/// does not crash-loop. Also injects `FASTEMBED_CACHE_DIR=$HOME/.cache/fastembed`
/// so the embedder model download does not try to write into launchd's
/// read-only sandbox `TMPDIR` (GH #58).
/// Test: `build_launchd_config_uses_canonical_shape` asserts the
/// `--foreground` flag is present (issue #132 regression guard);
/// `build_launchd_config_sets_fastembed_cache_dir` asserts the env var is
/// wired in. End-to-end exercised via `service install` / `service start`.
#[cfg(target_os = "macos")]
pub(crate) fn build_launchd_config(
    exe: std::path::PathBuf,
    log_dir: std::path::PathBuf,
) -> trusty_common::launchd::LaunchdConfig {
    use trusty_common::launchd::{KeepAlive, LaunchdConfig};
    LaunchdConfig {
        label: LAUNCHD_LABEL.to_string(),
        exe_path: exe,
        args: vec!["serve".to_string(), "--foreground".to_string()],
        log_dir,
        keep_alive: KeepAlive::OnSuccess,
        throttle_interval: 10,
        env_vars: fastembed_env_vars(),
    }
}

/// Build the env var list embedded into the LaunchAgent plist.
///
/// Why: launchd's per-agent `TMPDIR` is a sandboxed `/var/folders/.../T/`
/// path that is **read-only** for the agent's UID. fastembed's default
/// model retrieval path is derived from that `TMPDIR`, so the first
/// `TextEmbedding::try_new` call fails with `EROFS (os error 30)` and the
/// daemon never reaches a ready state (GH #58). Pinning the fastembed cache
/// to a writable user-owned directory in the plist solves the problem for
/// every daemon start. Both `FASTEMBED_CACHE_DIR` and `FASTEMBED_CACHE_PATH`
/// are emitted so the daemon agrees with both fastembed's native env
/// (`FASTEMBED_CACHE_DIR`) and the alternative name documented in our
/// install flow / accepted by `resolve_fastembed_cache_dir` (GH #62).
/// What: returns `[("FASTEMBED_CACHE_DIR", "$HOME/.cache/fastembed"),
/// ("FASTEMBED_CACHE_PATH", "$HOME/.cache/fastembed")]`, expanding `$HOME`
/// from the install-time user. If `HOME` is unset (very unusual), returns
/// an empty list — `resolve_fastembed_cache_dir` will then fall back to
/// its own logic at daemon startup.
/// Test: `build_launchd_config_sets_fastembed_cache_dir` covers the happy
/// path for both env var names.
#[cfg(target_os = "macos")]
fn fastembed_env_vars() -> Vec<(String, String)> {
    if let Some(home) = dirs::home_dir() {
        let cache = home.join(".cache").join("fastembed");
        let value = cache.to_string_lossy().into_owned();
        return vec![
            ("FASTEMBED_CACHE_DIR".to_string(), value.clone()),
            ("FASTEMBED_CACHE_PATH".to_string(), value),
        ];
    }
    Vec::new()
}

#[cfg(target_os = "macos")]
fn current_exe() -> Result<std::path::PathBuf> {
    std::env::current_exe().map_err(|e| anyhow::anyhow!("could not resolve current exe: {e}"))
}

/// `service install` — write the plist without loading it.
///
/// Why: operators sometimes want to inspect or hand-edit the plist before
/// launchd takes ownership. Splitting "install" from "start" gives them that
/// window without forcing a stop-start dance.
/// What: resolves the binary path and log directory, then calls
/// `LaunchdConfig::install()` which writes `~/Library/LaunchAgents/<label>.plist`
/// and creates the log directory. Does not call `bootstrap`.
/// Test: integration via `cargo run -p trusty-memory -- service install`.
#[cfg(target_os = "macos")]
fn service_install() -> Result<()> {
    let exe = current_exe()?;
    let log_dir = launchd_log_dir()?;
    let cfg = build_launchd_config(exe, log_dir.clone());
    let plist_path = cfg.plist_path()?;
    cfg.install()?;
    println!(
        "{} Wrote LaunchAgent plist: {}",
        "✓".green(),
        plist_path.display()
    );
    ensure_fastembed_cache_dir();
    println!(
        "  Logs:    {}\n  Start:   {}",
        log_dir.display().to_string().dimmed(),
        "trusty-memory service start".cyan(),
    );
    Ok(())
}

/// Ensure the fastembed cache directory exists at install time.
///
/// Why: GH #62 — the launchd plist now pins `FASTEMBED_CACHE_PATH` to
/// `$HOME/.cache/fastembed`, but if that directory does not yet exist the
/// daemon's first `TextEmbedding::try_new` will still trip over fastembed's
/// cache-creation path under launchd's restricted environment. Creating the
/// directory up-front (cheap, no network) guarantees the env var resolves
/// to a writable path on the very first daemon start. A full model pre-warm
/// is performed by `trusty-memory setup`; here we only do the minimum
/// (mkdir -p) so `service install` stays fast and side-effect-light.
/// What: best-effort `create_dir_all` against `$HOME/.cache/fastembed`.
/// Failures are logged to stdout as a hint but do not abort install.
/// Test: side-effecting; covered manually via `trusty-memory service install`.
#[cfg(target_os = "macos")]
fn ensure_fastembed_cache_dir() {
    let Some(home) = dirs::home_dir() else {
        return;
    };
    let cache = home.join(".cache").join("fastembed");
    match std::fs::create_dir_all(&cache) {
        Ok(()) => println!(
            "{} fastembed cache dir ready at {}",
            "✓".green(),
            cache.display().to_string().dimmed()
        ),
        Err(e) => eprintln!(
            "  {} could not pre-create {} ({e}); daemon will retry on first request.",
            "·".dimmed(),
            cache.display()
        ),
    }
}

/// `service start` — install the plist (if needed) and bootstrap the agent.
///
/// Why: the common "I want it running" path should be one command, not two.
/// `install` + `bootstrap` is idempotent under the shared launchd module
/// (bootstrap calls bootout first), so calling start repeatedly is safe.
/// What: writes the plist via `install()`, then loads it into the user's
/// `gui/<uid>` domain via `bootstrap()`. The agent will start immediately
/// and restart on non-zero exits per `KeepAlive::OnSuccess`.
/// Test: integration via `cargo run -p trusty-memory -- service start`.
#[cfg(target_os = "macos")]
fn service_start() -> Result<()> {
    let exe = current_exe()?;
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
    println!(
        "  Logs:    {}\n  Stop:    {}",
        log_dir.display().to_string().dimmed(),
        "trusty-memory service stop".cyan(),
    );
    Ok(())
}

/// `service stop` — boot out the agent (stop and unload).
///
/// Why: operators need a friendly counterpart to `start` that does not
/// require remembering the full `launchctl bootout gui/<uid>/<label>`
/// invocation. The shared launchd module treats "not loaded" as success, so
/// calling stop on an unloaded agent is also a no-op.
/// What: builds the same config used by `start`, then calls `bootout()`.
/// Leaves the plist file in place — re-`start` will reload it.
/// Test: integration via `cargo run -p trusty-memory -- service stop`.
#[cfg(target_os = "macos")]
fn service_stop() -> Result<()> {
    let exe = current_exe()?;
    let log_dir = launchd_log_dir()?;
    let cfg = build_launchd_config(exe, log_dir);
    cfg.bootout()?;
    println!(
        "{} Unloaded {} (plist file preserved at {}).",
        "✓".green(),
        LAUNCHD_LABEL,
        cfg.plist_path()?.display().to_string().dimmed()
    );
    Ok(())
}

/// `service logs` — tail the launchd stdout/stderr log files.
///
/// Why: launchd routes the daemon's stdout/stderr to plain files; a friendly
/// `tail -F` wrapper avoids forcing operators to remember the path.
/// What: resolves the log directory and execs `tail -F <stdout> <stderr>`.
/// Emits a hint when neither file exists yet (daemon never started).
/// Test: side-effecting; covered manually via
/// `cargo run -p trusty-memory -- service logs`.
#[cfg(target_os = "macos")]
fn service_logs() -> Result<()> {
    let log_dir = launchd_log_dir()?;
    let stdout = log_dir.join("stdout.log");
    let stderr = log_dir.join("stderr.log");
    if !stdout.exists() && !stderr.exists() {
        eprintln!(
            "{} No logs at {} yet — start the service first ({}).",
            "·".dimmed(),
            log_dir.display(),
            "trusty-memory service start".cyan()
        );
        return Ok(());
    }
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
    use super::*;

    /// Why: on non-macOS platforms, every `service` action must surface a
    /// clear, actionable error instead of silently succeeding or panicking.
    /// What: invokes `handle_service` with each action and asserts the Err
    /// message contains the "not supported" sentinel.
    /// Test: macOS skips this (the actions perform real `launchctl` work).
    #[cfg(not(target_os = "macos"))]
    #[test]
    fn handle_service_errors_on_unsupported_platform() {
        for action in [
            ServiceAction::Install,
            ServiceAction::Start,
            ServiceAction::Stop,
            ServiceAction::Logs,
        ] {
            let err = handle_service(&action).expect_err("must fail on non-macOS");
            let msg = format!("{err}");
            assert!(
                msg.contains("not supported"),
                "expected platform error, got: {msg}"
            );
        }
    }

    /// Why: the LaunchdConfig we hand to `trusty_common::launchd` must always
    /// describe the canonical trusty-memory agent (label, args, restart
    /// policy). Drift here corrupts every plist that the binary writes.
    /// Issue #132 specifically required that the args invoke
    /// `serve --foreground` — plain `serve` self-spawns and exits 0, which
    /// launchd interprets as "service stopped" and re-launches in a tight
    /// loop. This assertion is the regression guard.
    /// What: builds the config with dummy paths and asserts the
    /// load-bearing fields, including the `--foreground` flag.
    /// Test: pure construction, no fs side effects.
    #[cfg(target_os = "macos")]
    #[test]
    fn build_launchd_config_uses_canonical_shape() {
        use std::path::PathBuf;
        use trusty_common::launchd::KeepAlive;

        let cfg = build_launchd_config(
            PathBuf::from("/usr/local/bin/trusty-memory"),
            PathBuf::from("/tmp/trusty-memory/logs"),
        );
        assert_eq!(cfg.label, LAUNCHD_LABEL);
        assert_eq!(
            cfg.args,
            vec!["serve".to_string(), "--foreground".to_string()],
            "launchd plist must invoke `serve --foreground` (issue #132) so \
             launchd supervises the daemon PID directly instead of \
             re-launching the self-spawning parent on every exit"
        );
        assert_eq!(cfg.keep_alive, KeepAlive::OnSuccess);
        assert_eq!(cfg.throttle_interval, 10);
        // env_vars is allowed to be empty only on hosts without a HOME
        // (extremely rare); on developer/CI machines HOME is always set
        // and FASTEMBED_CACHE_DIR must be wired in.
        if dirs::home_dir().is_some() {
            assert!(
                cfg.env_vars.iter().any(|(k, _)| k == "FASTEMBED_CACHE_DIR"),
                "FASTEMBED_CACHE_DIR must be present in the LaunchAgent plist (GH #58)"
            );
        }
    }

    /// Why: GH #58 — launchd's read-only `TMPDIR` breaks fastembed's first
    /// model download. The plist installer is the single source of truth
    /// for the daemon's runtime environment, so the env var must be set
    /// there. Asserting on `build_launchd_config` (not just
    /// `fastembed_env_vars`) catches regressions where someone strips the
    /// env list when refactoring the config builder.
    /// What: builds the config with dummy paths and asserts the env var is
    /// present and points under `$HOME/.cache/fastembed`.
    /// Test: pure construction, no fs side effects.
    #[cfg(target_os = "macos")]
    #[test]
    fn build_launchd_config_sets_fastembed_cache_dir() {
        use std::path::PathBuf;

        let cfg = build_launchd_config(
            PathBuf::from("/usr/local/bin/trusty-memory"),
            PathBuf::from("/tmp/trusty-memory/logs"),
        );
        if let Some(home) = dirs::home_dir() {
            let expected = home
                .join(".cache")
                .join("fastembed")
                .to_string_lossy()
                .into_owned();
            let dir_value = cfg
                .env_vars
                .iter()
                .find(|(k, _)| k == "FASTEMBED_CACHE_DIR")
                .map(|(_, v)| v.clone())
                .expect("FASTEMBED_CACHE_DIR must be present");
            assert_eq!(dir_value, expected);
            // GH #62: also assert FASTEMBED_CACHE_PATH is present and
            // points to the same path. Both names exist because fastembed
            // reads `FASTEMBED_CACHE_DIR` natively, while
            // `resolve_fastembed_cache_dir` (and our docs) prefer the
            // `FASTEMBED_CACHE_PATH` alias.
            let path_value = cfg
                .env_vars
                .iter()
                .find(|(k, _)| k == "FASTEMBED_CACHE_PATH")
                .map(|(_, v)| v.clone())
                .expect("FASTEMBED_CACHE_PATH must be present (GH #62)");
            assert_eq!(path_value, expected);
        }
    }
}
