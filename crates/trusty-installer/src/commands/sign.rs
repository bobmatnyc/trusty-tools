//! `tctl sign <target>` — standalone Developer-ID signing entry point (#2558).
//!
//! Why: `tctl install` already signs `trusty-search`/`trusty-embedderd` and
//! (since #2558's scope extension) `trusty-mpm` as a fail-soft post-install
//! hook (`macos_signing::post_install_search` / `post_install_mpm`), but an
//! operator who just built a binary from local source (`cargo install --path`)
//! or wants to (re-)sign an already-installed binary without a full `tctl
//! install` run needs a direct, HARD-failing entry point — this is that
//! command. `scripts/install-trusty-search-signed.sh` shells out to it instead
//! of duplicating the `codesign`/`security find-identity` invocations in bash,
//! which is exactly the drift #2558 exists to close.
//!
//! What: Resolves the install directory (defaults to `$CARGO_HOME/bin` /
//! `~/.cargo/bin`, since this command targets `cargo install`-produced
//! binaries), then delegates to `macos_signing::sign_set_strict` for the
//! requested target. Prints cert-setup guidance and returns a non-zero exit
//! code when no Developer ID certificate is available or signing/verification
//! fails; prints a success note and returns 0 otherwise. On non-macOS: prints
//! a no-op notice and returns 0 (codesign/TCC are Apple-specific).
//!
//! Test: `tests::run_unknown_target_is_error` covers the shared error path
//! cross-platform (unknown target name); `tests::run_known_target_is_noop_on_non_macos`
//! (compiled and run only on non-macOS hosts — this workspace's CI runs on
//! `ubuntu-latest`) covers the no-op path end to end; the macOS signing path
//! itself is side-effecting (real `codesign`) and validated manually.

use std::path::PathBuf;

use super::macos_signing;

/// Handle `tctl sign <target>` (target: `trusty-search` or `trusty-mpm`).
///
/// Why: The CLI-facing entry point for standalone Developer-ID signing,
/// separate from the fail-soft hooks `commands::install` runs automatically.
///
/// What: Validates `target` against `macos_signing::binaries_for_set`
/// (non-empty = known), resolves `dir` (or the default cargo bin dir), and on
/// macOS calls `sign_set_strict`. Returns the process exit code: `0` on
/// success, `1` on a signing/verification/no-cert failure, `2` for an unknown
/// target. On non-macOS this is always a `0` no-op.
///
/// Test: `tests::run_unknown_target_is_error`.
pub fn run(target: &str, dir: Option<PathBuf>, json: bool) -> i32 {
    if macos_signing::binaries_for_set(target).is_empty() {
        eprintln!(
            "tctl sign: unknown target '{target}' (expected 'trusty-search' or 'trusty-mpm')"
        );
        return 2;
    }

    #[cfg(target_os = "macos")]
    {
        run_macos(target, dir, json)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (dir, json);
        eprintln!("tctl sign: macOS-only (codesign/TCC are Apple-specific) — no-op.");
        0
    }
}

/// The macOS signing path, separated so it can hold `#[cfg(target_os =
/// "macos")]` cleanly without splitting `run`'s argument validation.
///
/// Why: Keeps the always-available "unknown target" check in `run` shared
/// across platforms while isolating the real `codesign` interaction.
///
/// What: Resolves the install dir, calls `sign_set_strict`, and maps the
/// result to an exit code + human message (cert-setup guidance on failure,
/// success note otherwise).
///
/// Test: Side-effecting (real `codesign`); not invoked in the test suite.
#[cfg(target_os = "macos")]
fn run_macos(target: &str, dir: Option<PathBuf>, json: bool) -> i32 {
    let install_dir = dir.unwrap_or_else(default_bin_dir);

    match macos_signing::sign_set_strict(&install_dir, target) {
        Ok(signed) => {
            if !json {
                for path in &signed {
                    eprintln!("tctl sign: signed and verified {}", path.display());
                }
                eprintln!(
                    "{target}: signed with Developer ID. The macOS grant will persist across \
                     all future reinstalls."
                );
            }
            0
        }
        // PR #2657 review (MEDIUM): match on the typed `SignSetError` variant
        // instead of substring-matching `e.to_string()` — brittle against any
        // future wording change to the error message.
        Err(macos_signing::SignSetError::NoCertificate) => {
            if !json {
                eprintln!("{}", macos_signing::cert_setup_guidance(target));
            }
            1
        }
        Err(e) => {
            if !json {
                eprintln!("tctl sign: {e}");
            }
            1
        }
    }
}

/// Default binary directory: `$CARGO_HOME/bin`, falling back to `~/.cargo/bin`.
///
/// Why: `cargo install` writes here; `tctl sign` (run standalone, without a
/// preceding `tctl install`) needs the same default so it finds the binaries.
///
/// What: Reads `CARGO_HOME`; joins `bin`, or falls back to the home directory.
///
/// Test: Not unit-tested directly (thin env/home-dir read); the equivalent
/// logic in `commands::install::cargo_bin_dir_from_env` is tested there.
#[cfg(target_os = "macos")]
fn default_bin_dir() -> PathBuf {
    match std::env::var("CARGO_HOME") {
        Ok(home) if !home.is_empty() => PathBuf::from(home).join("bin"),
        _ => dirs::home_dir()
            .map(|h| h.join(".cargo").join("bin"))
            .unwrap_or_else(|| PathBuf::from("/usr/local/bin")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: An unrecognised target must be a clean, cross-platform error (exit
    /// 2), not a signing attempt.
    /// What: Calls `run` with a bogus target; asserts exit 2.
    /// Test: This is the test.
    #[test]
    fn run_unknown_target_is_error() {
        let code = run("not-a-real-target", None, true);
        assert_eq!(code, 2);
    }

    /// Why: PR #2657 review (LOW) — the non-macOS no-op path (codesign/TCC are
    /// Apple-specific) must be exercised end to end, not just asserted by
    /// inspection. This workspace's CI runs on `ubuntu-latest`, so this test
    /// actually executes there; on a macOS dev machine it is compiled out
    /// (`run` takes the `run_macos` branch instead, which is validated
    /// manually — see the module doc comment).
    /// What: A known target (`trusty-search`) on a non-macOS host returns exit
    /// 0 without ever reaching the macOS-only `codesign` invocation (that code
    /// path is not even compiled into this binary on non-macOS).
    /// Test: This is the test.
    #[cfg(not(target_os = "macos"))]
    #[test]
    fn run_known_target_is_noop_on_non_macos() {
        let code = run("trusty-search", None, false);
        assert_eq!(code, 0);
    }
}
