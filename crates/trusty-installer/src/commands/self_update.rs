//! `trusty-installer self-update` — update the installer itself.
//!
//! Why: The installer binary manages the whole stack but has no automated path
//! to update itself; this command closes that gap.
//!
//! What: Attempts a prebuilt download for "trusty-installer" into the current
//! binary's parent directory. On success prints a restart hint (does NOT re-exec).
//! On fallback, attempts `cargo install trusty-installer --locked` if cargo is on
//! PATH; otherwise prints a manual install hint. Before either attempt, the target
//! directory is probed for writability — a non-writable dir aborts immediately
//! with an actionable error (no misleading "updated" report when the binary on
//! PATH was never replaced).
//!
//! Test: `tests` covers writability detection, the Outcome→message mapping,
//! and the cargo-fallback location-divergence warning as pure functions; the live
//! network + cargo paths are side-effecting and gated behind `#[ignore]`.

use std::path::{Path, PathBuf};

use crate::commands::progress_ui::narrator;
use crate::download::{self, Outcome};

/// Handle `trusty-installer self-update`.
///
/// Why: Provides a zero-friction update path for the installer binary itself,
/// leveraging the same prebuilt-first strategy as `tctl install`.
///
/// What: Resolves the install directory from `current_exe()` parent (or the
/// default install dir as fallback), probes the directory for writability and
/// aborts with an actionable error if it is not writable, then calls
/// `try_install_prebuilt("trusty-installer", &dir)` and either prints the
/// restart hint or falls back to cargo with a truthful install-location message.
///
/// Test: `tests::outcome_installed_message`, `tests::nonwritable_dir_is_detected`,
///       `tests::cargo_fallback_different_dir_message`.
pub fn run(json: bool) -> i32 {
    let narr = narrator(json);
    let install_dir = resolve_install_dir();

    // Up-front writability check (approach a): probe before any download so we
    // never report "updated" when the target directory cannot be written to.
    if !is_dir_writable(&install_dir) {
        let _ = narr.error(&unwritable_dir_error(&install_dir));
        return 1;
    }

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
            run_cargo_fallback(json, &install_dir)
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
fn resolve_install_dir() -> PathBuf {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            if parent != Path::new("") {
                return parent.to_owned();
            }
        }
    }
    download::default_install_dir().unwrap_or_else(|| PathBuf::from("/usr/local/bin"))
}

/// Probe whether a directory is writable by creating and immediately removing a temp file.
///
/// Why: Mode-bit checks alone are unreliable (ACLs, immutable flags, cross-user
/// ownership). A create-and-remove probe is the portable, non-destructive way to
/// confirm actual write access on both macOS and Linux without leaving any trace.
///
/// What: Uses `tempfile::Builder::new().tempfile_in(dir)` — the temp file is
/// removed on drop if the call succeeds. Returns `true` iff the probe succeeds.
///
/// Test: `tests::writable_temp_dir_is_detected`, `tests::nonwritable_dir_is_detected`.
pub fn is_dir_writable(dir: &Path) -> bool {
    // tempfile_in creates a file and registers it for removal on drop — no litter.
    tempfile::Builder::new().tempfile_in(dir).is_ok()
}

/// Format the error message for a non-writable install directory.
///
/// Why: Separating message text from the I/O path lets unit tests verify the
/// string without needing a real non-writable directory on the filesystem.
///
/// What: Returns an actionable error string naming `dir` and suggesting two fixes.
///
/// Test: `tests::unwritable_dir_error_names_path`.
pub fn unwritable_dir_error(dir: &Path) -> String {
    format!(
        "self-update aborted: install directory `{}` is not writable.\n\
         Fix: run with elevated permissions (e.g. `sudo tctl self-update`), or\n\
         install manually: `cargo install trusty-installer --locked`",
        dir.display()
    )
}

/// Resolve the directory where `cargo install` places binaries.
///
/// Why: Needed to compare with the current binary location so the cargo fallback
/// can warn when it installs to a different directory than the original binary,
/// which would leave the binary on PATH unmodified (the misleading-success bug).
///
/// What: Returns `$CARGO_HOME/bin` when `CARGO_HOME` is set, otherwise
/// `~/.cargo/bin` via `dirs::home_dir()`. Returns `None` if the home directory
/// cannot be determined.
///
/// Test: `tests::cargo_install_dir_ends_in_bin`.
pub fn cargo_install_dir() -> Option<PathBuf> {
    if let Ok(cargo_home) = std::env::var("CARGO_HOME") {
        return Some(PathBuf::from(cargo_home).join("bin"));
    }
    dirs::home_dir().map(|h| h.join(".cargo").join("bin"))
}

/// Format the post-cargo-install success message, warning when the install
/// location differs from the original binary's directory.
///
/// Why: Truthful messaging — if cargo installs to `~/.cargo/bin` but the binary
/// the user normally runs lives at `/usr/local/bin`, they must know the in-PATH
/// binary was NOT replaced and what they need to do about it.
///
/// What: If `cargo_dir` is `Some` and differs from `original_dir`, returns a
/// warning message naming both paths. If they are the same, or `cargo_dir` is
/// `None`, returns the plain "Restart to apply" message.
///
/// Test: `tests::cargo_updated_same_dir`, `tests::cargo_updated_different_dir`,
///       `tests::cargo_updated_none_dir`.
pub fn cargo_updated_message(cargo_dir: Option<&Path>, original_dir: &Path) -> String {
    match cargo_dir {
        Some(cd) if cd != original_dir => format!(
            "trusty-installer updated via cargo to `{}`.\n\
             WARNING: the binary at `{}` was NOT replaced.\n\
             The update takes effect only if `{}` appears on your PATH \
             before `{}`.",
            cd.display(),
            original_dir.display(),
            cd.display(),
            original_dir.display()
        ),
        _ => "trusty-installer updated via cargo. Restart to apply.".to_owned(),
    }
}

/// Fall back to `cargo install trusty-installer --locked` when the prebuilt path fails.
///
/// Why: Cargo is the universal fallback; if it is present the user always has a
/// path to get the latest installer regardless of platform or network issues.
///
/// What: Checks for `cargo` on PATH; if present runs
/// `trusty_common::update::perform_upgrade("trusty-installer")` and
/// `verify_installed_binary`; then prints the actual install location with a
/// warning when it differs from `original_dir`. Prints the manual-install hint
/// if cargo is absent.
///
/// Test: `tests::no_cargo_hint_contains_install_cmd`.
fn run_cargo_fallback(json: bool, original_dir: &Path) -> i32 {
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
                    let cargo_dir = cargo_install_dir();
                    let msg = cargo_updated_message(cargo_dir.as_deref(), original_dir);
                    let _ = narr.info(&msg);
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
/// Test: `tests::no_cargo_hint_contains_install_cmd`.
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

    /// Why: A writable directory must pass the probe so the up-front check in
    ///      `run()` does not block installs into user-owned directories.
    /// What: Creates a temp dir (always writable by the process owner), calls
    ///       `is_dir_writable`, asserts true.
    /// Test: This is the test.
    #[test]
    fn writable_temp_dir_is_detected() {
        let dir = tempfile::tempdir().unwrap();
        assert!(is_dir_writable(dir.path()), "temp dir should be writable");
    }

    /// Why: A non-writable directory must be detected before any download attempt,
    ///      preventing the misleading-success bug where the binary on PATH was not
    ///      replaced but "updated" was reported anyway.
    /// What: Creates a temp dir, removes write permission (0o555), calls
    ///       `is_dir_writable`, asserts false. Restores permissions before asserting
    ///       so temp-dir cleanup succeeds.
    /// Test: This is the test. Unix-only — Windows read-only-dir semantics differ.
    #[test]
    #[cfg(unix)]
    fn nonwritable_dir_is_detected() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let ro = std::fs::Permissions::from_mode(0o555);
        std::fs::set_permissions(dir.path(), ro).unwrap();
        let writable = is_dir_writable(dir.path());
        // Restore before asserting so tempdir cleanup does not fail.
        let rw = std::fs::Permissions::from_mode(0o755);
        std::fs::set_permissions(dir.path(), rw).unwrap();
        assert!(!writable, "read-only dir must not be detected as writable");
    }

    /// Why: The unwritable-dir error must name the directory so the user knows
    ///      exactly which path requires elevated permissions.
    /// What: Calls `unwritable_dir_error` with a synthetic path; asserts both the
    ///       path and a fix hint appear.
    /// Test: This is the test.
    #[test]
    fn unwritable_dir_error_names_path() {
        let path = PathBuf::from("/usr/local/bin");
        let msg = unwritable_dir_error(&path);
        assert!(msg.contains("/usr/local/bin"), "error must name the dir");
        assert!(msg.contains("cargo install"), "error must suggest fix");
    }

    /// Why: When cargo installs to a DIFFERENT directory than the original binary,
    ///      the success message must warn the user that the binary they normally
    ///      run was NOT replaced (core of the misleading-success fix).
    /// What: Calls `cargo_updated_message` with two different paths; asserts both
    ///       appear and the WARNING keyword + "NOT replaced" phrase are present.
    /// Test: This is the test.
    #[test]
    fn cargo_updated_different_dir() {
        let original = PathBuf::from("/usr/local/bin");
        let cargo = PathBuf::from("/home/user/.cargo/bin");
        let msg = cargo_updated_message(Some(&cargo), &original);
        assert!(msg.contains("/usr/local/bin"), "must name original dir");
        assert!(msg.contains("/home/user/.cargo/bin"), "must name cargo dir");
        assert!(msg.contains("WARNING"), "must include WARNING");
        assert!(msg.contains("NOT replaced"), "must state what was not done");
    }

    /// Why: When cargo installs to the SAME directory as the original binary, no
    ///      warning is needed — the update is truthfully complete.
    /// What: Calls `cargo_updated_message` with identical paths; asserts no WARNING.
    /// Test: This is the test.
    #[test]
    fn cargo_updated_same_dir() {
        let dir = PathBuf::from("/usr/local/bin");
        let msg = cargo_updated_message(Some(&dir), &dir);
        assert!(!msg.contains("WARNING"), "same dir must not warn");
        assert!(
            msg.contains("Restart to apply"),
            "must contain restart hint"
        );
    }

    /// Why: When `cargo_install_dir()` returns `None` (no home dir), the fallback
    ///      message must be the plain success message, not a spurious warning.
    /// What: Calls `cargo_updated_message(None, ...)` and asserts no WARNING.
    /// Test: This is the test.
    #[test]
    fn cargo_updated_none_dir() {
        let original = PathBuf::from("/usr/local/bin");
        let msg = cargo_updated_message(None, &original);
        assert!(!msg.contains("WARNING"), "None cargo dir must not warn");
        assert!(msg.contains("Restart to apply"));
    }

    /// Why: `cargo_install_dir()` must return a path ending in `bin`, consistent
    ///      with where `cargo install` places binaries, so the comparison in
    ///      `cargo_updated_message` is meaningful.
    /// What: Calls `cargo_install_dir()` and asserts the last component is `bin`.
    /// Test: This is the test.
    #[test]
    fn cargo_install_dir_ends_in_bin() {
        if let Some(p) = cargo_install_dir() {
            assert_eq!(
                p.file_name().and_then(|n| n.to_str()),
                Some("bin"),
                "cargo install dir must end in `bin`, got `{}`",
                p.display()
            );
        }
        // If None (no home dir), there is nothing to assert.
    }
}
