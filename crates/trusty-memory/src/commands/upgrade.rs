//! Handler for `trusty-memory upgrade` — interactive CLI upgrade subcommand.
//!
//! Why: Operators running `trusty-memory upgrade` should be able to check
//! whether a new version is available and, with explicit confirmation, install
//! it and restart the daemon — all from a single CLI call. This surface is
//! distinct from the background update-notice printed at startup: it is
//! interactive, explicit, and drives the full install + restart pipeline.
//!
//! What: `handle_upgrade` implements two modes:
//!   - `--check`: run a fresh crates.io check and print current vs available
//!     version, then exit. No install.
//!   - (default): same check; if already up-to-date, say so and exit. If an
//!     update is available, prompt "Update … Y→? [y/N]" (or skip prompt with
//!     `--yes`); on confirm, delegate to
//!     `trusty_common::update::upgrade_and_restart`.
//!
//! The daemon self-exit path (launchd respawn) is handled inside
//! `upgrade_and_restart` — this file only deals with user interaction.
//!
//! Test: `cargo run -p trusty-memory -- upgrade --check` against a running
//! daemon verifies the version-check path without triggering an install.

use anyhow::Result;
use trusty_common::update::{check_crates_io, upgrade_and_restart, UpdateInfo};

/// Handle `trusty-memory upgrade [--check] [--yes]`.
///
/// Why: Gives operators a single command to go from "I wonder if I'm up to
/// date" through install and daemon restart, with explicit confirmation at
/// every step.
///
/// What:
///   - Performs a fresh crates.io check (bypassing the 24-h cache).
///   - In `--check` mode: prints current and available versions, then exits.
///   - Otherwise: prompts for confirmation (unless `--yes`) and calls
///     `upgrade_and_restart` on confirm. When the daemon is launchd-supervised,
///     `upgrade_and_restart` calls `std::process::exit(1)` after a successful
///     install + health-gate, so this function may not return. When no
///     supervisor is detected, it returns `Ok(Some(hint))` with a message
///     telling the operator to restart the daemon manually.
///
/// Test: manual validation via `trusty-memory upgrade --check` and
/// `trusty-memory upgrade --yes` from a shell. The unit tests in
/// `trusty-common` cover the underlying primitives.
pub async fn handle_upgrade(check_only: bool, yes: bool) -> Result<()> {
    let crate_name = env!("CARGO_PKG_NAME");
    let current = env!("CARGO_PKG_VERSION");

    eprintln!("Checking for updates to {crate_name} (current: {current})…");

    let info: Option<UpdateInfo> = check_crates_io(crate_name, current).await;

    match info {
        None => {
            eprintln!("{crate_name} {current} is already up to date.");
        }
        Some(ref info) => {
            eprintln!(
                "Update available: {crate_name} {} (you have {})",
                info.latest, info.current
            );
        }
    }

    if check_only {
        return Ok(());
    }

    let Some(info) = info else {
        return Ok(());
    };

    // Confirm unless --yes was passed.
    if !yes {
        eprint!(
            "Update {crate_name} {} → {}? [y/N] ",
            info.current, info.latest
        );
        // Flush is best-effort — if the terminal is non-interactive this may
        // be silently ignored.
        use std::io::Write as _;
        let _ = std::io::stderr().flush();

        let mut line = String::new();
        std::io::stdin()
            .read_line(&mut line)
            .map_err(|e| anyhow::anyhow!("failed to read confirmation: {e}"))?;

        let trimmed = line.trim().to_ascii_lowercase();
        if trimmed != "y" && trimmed != "yes" {
            eprintln!("Upgrade cancelled.");
            return Ok(());
        }
    }

    // Delegate to the shared install + health-gate + restart-or-hint helper.
    // Note: when the process is launchd-supervised this call may not return —
    // it calls std::process::exit(1) after a successful install+health-gate so
    // launchd's KeepAlive::OnSuccess respawns the new binary.
    match upgrade_and_restart(crate_name, crate_name).await {
        Ok(Some(hint)) => {
            eprintln!("{hint}");
        }
        Ok(None) => {
            // upgrade_and_restart returned Ok(None) only when there is nothing
            // to do (should not happen here since we already have info, but
            // guard for safety).
        }
        Err(e) => {
            return Err(anyhow::anyhow!("upgrade failed: {e}"));
        }
    }

    Ok(())
}
