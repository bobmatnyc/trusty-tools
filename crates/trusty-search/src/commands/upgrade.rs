//! Handler for `trusty-search upgrade` — interactive CLI upgrade subcommand.
//!
//! Why: Operators running `trusty-search upgrade` should be able to check
//! whether a new version is available and, with explicit confirmation, install
//! it and restart the daemon — all from a single CLI call.
//!
//! What: `handle_upgrade` implements two modes:
//!   - `--check`: run a fresh crates.io check and print current vs available
//!     version, then exit. No install.
//!   - (default): same check; if already up-to-date, say so and exit. If an
//!     update is available, prompt "Update … Y→? [y/N]" (or skip with
//!     `--yes`); on confirm, delegate to
//!     `trusty_common::update::upgrade_and_restart`.
//!
//! The daemon self-exit path (launchd respawn) is handled inside
//! `upgrade_and_restart` — this file only deals with user interaction.
//!
//! Test: `cargo run -p trusty-search -- upgrade --check` against a running
//! daemon verifies the version-check path without triggering an install.

use anyhow::Result;
use trusty_common::update::{check_crates_io, upgrade_and_restart, UpdateInfo};

/// Handle `trusty-search upgrade [--check] [--yes]`.
///
/// Why: Gives operators a single command to go from "I wonder if I'm up to
/// date" through install and daemon restart.
///
/// What: Performs a fresh crates.io check (bypassing the 24-h cache).
/// In `--check` mode: prints current and available versions, then exits.
/// Otherwise: prompts for confirmation (unless `--yes`) and calls
/// `upgrade_and_restart` on confirm. When the daemon is launchd-supervised,
/// `upgrade_and_restart` calls `std::process::exit(1)` after a successful
/// install + health-gate, so this function may not return on that path.
/// When no supervisor is detected, it returns `Ok` with a hint to restart.
///
/// Test: manual validation via `trusty-search upgrade --check` and
/// `trusty-search upgrade --yes` from a shell.
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
    // When the process is launchd-supervised this call may not return — it
    // calls std::process::exit(1) after a successful install + health-gate so
    // launchd's KeepAlive::OnSuccess respawns the new binary.
    match upgrade_and_restart(crate_name, crate_name).await {
        Ok(Some(hint)) => {
            eprintln!("{hint}");
        }
        Ok(None) => {}
        Err(e) => {
            return Err(anyhow::anyhow!("upgrade failed: {e}"));
        }
    }

    Ok(())
}
