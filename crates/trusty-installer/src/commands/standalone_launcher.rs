//! Take the `trusty-embedderd-py` launcher binary over from a standalone
//! `trusty-embedderd-py` install during `cargo install` (#9974).
//!
//! Why: trusty-embedderd-py 0.2.0 is library-only; trusty-search ships the
//! `trusty-embedderd-py` launcher binary instead (#9468, owner ruling D94). A
//! machine that ran `cargo install trusty-embedderd-py` 0.1.x still has cargo
//! install metadata naming that package as the binary's owner, and cargo
//! refuses to install a binary another package owns unless `--force` is given.
//! `--force` (Architect ruling, option b) replaces an earlier
//! uninstall-then-install: the uninstall ran outside the #5777 ownership guard,
//! so a failed install afterwards lost the launcher for good.
//!
//! What: asks `cargo install --list` whether the standalone package owns the
//! launcher, and when it does, runs the crate's install with `--force`
//! through the guarded `trusty_common::update::perform_upgrade*_with_force`.
//! tctl never runs `cargo uninstall`. Every other install is unchanged:
//! `cargo install <crate> --locked`, no `--force`.
//!
//! Test: `standalone_launcher_tests.rs`.

use std::process::Stdio;

/// The package that owned the launcher binary before 0.2.0.
pub(crate) const STANDALONE_PACKAGE: &str = "trusty-embedderd-py";

/// The launcher binary whose ownership moved to trusty-search.
pub(crate) const LAUNCHER_BIN: &str = "trusty-embedderd-py";

/// Whether cargo's install metadata records the standalone launcher package.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LauncherRecord {
    /// The standalone package is recorded as the owner of the launcher bin.
    Recorded,
    /// No such record, or the metadata could not be listed.
    NotRecorded,
}

/// Runs cargo for one install. Tests inject a recording fake.
pub(crate) trait CargoRunner {
    /// The stdout of `cargo install --list`.
    async fn list_installed(&self) -> anyhow::Result<String>;
    /// The guarded `cargo install <crate_name> --locked`, plus `--force` when
    /// `force` is set.
    async fn install(&self, crate_name: &str, force: bool) -> anyhow::Result<()>;
}

/// Whether `cargo install --list` output records the standalone package as
/// the owner of the launcher binary.
///
/// Why: `--force` must only be added when cargo would otherwise refuse, and
/// only for the one package D94 retired.
/// What: a package line (`<name> v<version>[ (<source>)]:`) starts a block of
/// indented binary lines. Returns true when the block whose name is exactly
/// `trusty-embedderd-py` lists the launcher, after stripping the platform's
/// `EXE_SUFFIX` (cargo records `trusty-embedderd-py.exe` on Windows).
/// Test: `record_with_standalone_launcher_forces_the_install`,
/// `no_record_installs_without_force`,
/// `real_cargo_force_install_drops_the_standalone_record` (ignored).
pub(crate) fn launcher_recorded_in_list(list: &str) -> bool {
    let mut in_standalone = false;
    for line in list.lines() {
        if line.starts_with(char::is_whitespace) {
            let bin = line.trim();
            let bin = bin
                .strip_suffix(std::env::consts::EXE_SUFFIX)
                .unwrap_or(bin);
            if in_standalone && bin == LAUNCHER_BIN {
                return true;
            }
        } else {
            in_standalone = line.split_whitespace().next() == Some(STANDALONE_PACKAGE);
        }
    }
    false
}

/// Whether `crate_name` ships the launcher binary and so may take it over.
///
/// What: per the shared `installed_binaries` table, never for the standalone
/// package itself.
/// Test: `crate_without_the_launcher_never_lists_or_forces`.
fn ships_launcher(crate_name: &str) -> bool {
    crate_name != STANDALONE_PACKAGE
        && trusty_common::bin_resolve::installed_binaries(crate_name)
            .iter()
            .any(|b| b == LAUNCHER_BIN)
}

/// Ask cargo whether the standalone launcher is recorded.
///
/// Why: `cargo install --list` reads the metadata of the same install root
/// the following `cargo install` uses — same environment, so the same
/// `CARGO_INSTALL_ROOT`, `[install] root` config and `CARGO_HOME` precedence —
/// so tctl never resolves that root itself. A failed listing (including
/// malformed metadata, which cargo refuses to parse) is treated as "no
/// record" with a logged warning, not as an error: the install that follows
/// reads the same metadata and fails loudly with cargo's own error, so
/// nothing is silently skipped, and an error here would block an install
/// cargo itself accepts.
/// What: `Recorded` when [`launcher_recorded_in_list`] matches; otherwise
/// `NotRecorded`.
/// Test: `failed_listing_installs_without_force`.
async fn read_record<R: CargoRunner>(runner: &R) -> LauncherRecord {
    match runner.list_installed().await {
        Ok(list) if launcher_recorded_in_list(&list) => LauncherRecord::Recorded,
        Ok(_) => LauncherRecord::NotRecorded,
        Err(e) => {
            tracing::warn!(error = %e, "`cargo install --list` failed; assuming no standalone {STANDALONE_PACKAGE} install");
            LauncherRecord::NotRecorded
        }
    }
}

/// Install `crate_name`, adding `--force` only to take the launcher over.
///
/// Why: one decision point for every tctl cargo fallback, so neither path
/// can drop or widen `--force`. A record whose binary file is missing needs
/// nothing extra: `--force` skips cargo's ownership check and rewrites the
/// metadata either way.
/// What: lists installs only when `crate_name` ships the launcher; forces
/// the guarded install only when the standalone record exists. No other
/// cargo command runs, so a failed install leaves every binary where the
/// #5777 guard restored it.
/// Test: `record_with_standalone_launcher_forces_the_install`,
/// `no_record_installs_without_force`,
/// `failed_install_keeps_the_launcher`,
/// `crate_without_the_launcher_never_lists_or_forces`.
///
/// # Errors
/// The install's failure.
pub(crate) async fn install_crate<R: CargoRunner>(
    crate_name: &str,
    runner: &R,
) -> anyhow::Result<()> {
    let record = if ships_launcher(crate_name) {
        read_record(runner).await
    } else {
        LauncherRecord::NotRecorded
    };
    let force = record == LauncherRecord::Recorded;
    if force {
        tracing::info!(crate_name, "standalone {STANDALONE_PACKAGE} owns {LAUNCHER_BIN}; installing with --force to take it over");
    }
    runner.install(crate_name, force).await
}

/// The real cargo, spawned as a child process.
pub(crate) struct SystemCargo {
    /// Capture the install's stdio instead of inheriting it, for callers
    /// driving a live terminal display (#3830).
    pub(crate) captured: bool,
}

impl CargoRunner for SystemCargo {
    async fn list_installed(&self) -> anyhow::Result<String> {
        let out = tokio::process::Command::new("cargo")
            .args(["install", "--list"])
            .stdin(Stdio::null())
            .output()
            .await
            .map_err(|e| anyhow::anyhow!("could not run `cargo install --list`: {e}"))?;
        if !out.status.success() {
            anyhow::bail!(
                "`cargo install --list` exited with {}: {}",
                out.status,
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    async fn install(&self, crate_name: &str, force: bool) -> anyhow::Result<()> {
        // #9974: --force still goes through the #5777 ownership guard.
        if self.captured {
            trusty_common::update::perform_upgrade_captured_with_force(crate_name, force).await
        } else {
            trusty_common::update::perform_upgrade_with_force(crate_name, force).await
        }
    }
}

/// `cargo install <crate_name> --locked` for the tctl cargo fallbacks,
/// taking the launcher over from a standalone install when needed (#9974).
///
/// Why: `tctl install` and `tctl upgrade` share this one entry, so neither
/// can skip the takeover decision.
/// What: [`install_crate`] with [`SystemCargo`].
/// Test: `cargo_fallbacks_route_through_the_takeover_decision`; the decision
/// is covered by [`install_crate`]'s tests.
///
/// # Errors
/// The install's failure.
pub(crate) async fn cargo_install(crate_name: &str, captured: bool) -> anyhow::Result<()> {
    install_crate(crate_name, &SystemCargo { captured }).await
}

#[cfg(test)]
#[path = "standalone_launcher_tests.rs"]
pub(crate) mod tests;
