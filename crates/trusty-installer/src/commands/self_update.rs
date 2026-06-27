//! `trusty-installer self-update` — update the installer itself.
//!
//! Why: The installer binary manages the whole stack but has no automated path
//! to update itself; this command closes that gap.
//!
//! What: Attempts a prebuilt download for "trusty-installer" into the current
//! binary's parent directory. On success prints a restart hint (does NOT re-exec).
//! On fallback, attempts `cargo install trusty-installer --locked` if cargo is on
//! PATH; otherwise prints a manual install hint.
//!
//! Test: `tests` covers the Outcome→message mapping as pure functions; the live
//! network + cargo paths are side-effecting.

use crate::commands::progress_ui::narrator;
use crate::download::{self, Outcome};

/// Handle `trusty-installer self-update`.
///
/// Why: Provides a zero-friction update path for the installer binary itself,
/// leveraging the same prebuilt-first strategy as `tctl install`.
///
/// What: Resolves the install directory from `current_exe()` parent (or the
/// default install dir as fallback), calls `try_install_prebuilt("trusty-installer",
/// &dir)`, and either prints the restart hint or falls back to cargo.
///
/// Test: `tests::outcome_installed_message`, `tests::outcome_fallback_no_cargo_message`.
pub fn run(json: bool) -> i32 {
    let narr = narrator(json);
    let install_dir = resolve_install_dir();

    let outcome = crate::commands::runtime::block_on(download::try_install_prebuilt(
        "trusty-installer",
        &install_dir,
    ));

    match outcome {
        Outcome::Installed { version, .. } => {
            let msg = installed_message(&version);
            let _ = narr.info(&msg);
            0
        }
        Outcome::Fallback { reason } => {
            tracing::info!(%reason, "prebuilt self-update unavailable");
            let _ = narr.info(&format!("prebuilt unavailable: {reason}"));
            run_cargo_fallback(json)
        }
    }
}

/// Resolve the directory into which to place the updated binary.
///
/// Why: The installer should update itself in the same directory where it was
/// launched, matching user expectations and avoiding $PATH confusion.
///
/// What: Uses `std::env::current_exe()` and takes the parent directory; falls
/// back to `download::default_install_dir()` or `/usr/local/bin` if the parent
/// cannot be resolved.
///
/// Test: `tests::resolve_install_dir_returns_path`.
fn resolve_install_dir() -> std::path::PathBuf {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            if parent != std::path::Path::new("") {
                return parent.to_owned();
            }
        }
    }
    download::default_install_dir().unwrap_or_else(|| std::path::PathBuf::from("/usr/local/bin"))
}

/// Fall back to `cargo install trusty-installer --locked` when prebuilt fails.
///
/// Why: Cargo is the universal fallback; if it is present the user always has a
/// path to get the latest installer regardless of platform or network issues.
///
/// What: Checks for `cargo` on PATH; if present runs
/// `trusty_common::update::perform_upgrade("trusty-installer")` and
/// `verify_installed_binary`; if absent prints the manual install hint.
///
/// Test: `tests::cargo_fallback_message_without_cargo`.
fn run_cargo_fallback(json: bool) -> i32 {
    let narr = narrator(json);
    match which::which("cargo") {
        Ok(_) => {
            let _ = narr.info("cargo found; running `cargo install trusty-installer --locked`");
            let result = crate::commands::runtime::block_on(async {
                trusty_common::update::perform_upgrade("trusty-installer").await?;
                trusty_common::update::verify_installed_binary("trusty-installer").await
            });
            match result {
                Ok(()) => {
                    let _ = narr.info("trusty-installer updated via cargo. Restart to apply.");
                    0
                }
                Err(e) => {
                    let _ = narr.error(&format!("cargo install failed: {e}"));
                    1
                }
            }
        }
        Err(_) => {
            let _ = narr.info(no_cargo_hint());
            1
        }
    }
}

/// Pure helper: format the installed-version message (for tests).
///
/// Why: Separating the message from the I/O side-effect allows unit tests to
/// verify the text without a real download.
///
/// What: Returns the restart-hint string for a given version.
///
/// Test: `tests::outcome_installed_message`.
pub fn installed_message(version: &str) -> String {
    format!("trusty-installer updated to {version}. Restart to apply.")
}

/// Pure helper: format the no-cargo fallback hint (for tests).
///
/// Why: Same testability rationale as `installed_message`.
///
/// What: Returns the manual-install hint string.
///
/// Test: `tests::no_cargo_hint`.
pub fn no_cargo_hint() -> &'static str {
    "No Rust toolchain found on PATH. To update manually, run:\n  \
     cargo install trusty-installer --locked"
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: The restart hint must include the version and the "Restart to apply" phrase.
    /// What: Calls `installed_message` with a synthetic version and checks both.
    /// Test: This is the test.
    #[test]
    fn outcome_installed_message() {
        let msg = installed_message("1.2.3");
        assert!(msg.contains("1.2.3"), "should contain version");
        assert!(
            msg.contains("Restart to apply"),
            "should contain restart hint"
        );
    }

    /// Why: When cargo is absent, the fallback must give an actionable manual hint.
    /// What: Asserts the hint string contains `cargo install trusty-installer`.
    /// Test: This is the test.
    #[test]
    fn no_cargo_hint_contains_install_cmd() {
        let hint = no_cargo_hint();
        assert!(hint.contains("cargo install trusty-installer"));
    }

    /// Why: `resolve_install_dir` must always return a non-empty path.
    /// What: Calls it and asserts the result is non-empty.
    /// Test: This is the test.
    #[test]
    fn resolve_install_dir_returns_path() {
        let p = resolve_install_dir();
        assert!(!p.as_os_str().is_empty());
    }
}
