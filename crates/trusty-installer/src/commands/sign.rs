//! `tctl sign <target>` — standalone Developer-ID signing entry point (#2558).
//!
//! Why: `tctl install` already signs every installed trusty binary through
//! its install step (`macos_signing::install_step`, #11587), but an
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
//! requested target. Prints cert-setup guidance and exits 3 when no Developer
//! ID certificate is available, and exits 1 when signing or verification
//! fails (#11683); prints a success note and returns 0 otherwise. On non-macOS: prints
//! a no-op notice and returns 0 (codesign/TCC are Apple-specific).
//!
//! Test: `tests::run_unknown_target_is_error` covers the shared error path
//! cross-platform (unknown target name); `tests::run_known_target_is_noop_on_non_macos`
//! (compiled and run only on non-macOS hosts — this workspace's CI runs on
//! `ubuntu-latest`) covers the no-op path end to end; the macOS signing path
//! itself is side-effecting (real `codesign`) and validated manually.

use std::path::PathBuf;

use super::macos_signing;

/// Exit code for any signing failure other than a missing certificate.
///
/// Why: #11683 — the signed-install scripts must fail hard on these.
pub const EXIT_SIGN_FAILED: i32 = 1;

/// Exit code for an unknown signing target.
pub const EXIT_UNKNOWN_TARGET: i32 = 2;

/// Exit code when no Developer ID Application certificate is available.
///
/// Why: #11683 — the signed-install scripts soft-pass this case alone, so it
/// needs a code no other failure shares.
pub const EXIT_NO_CERTIFICATE: i32 = 3;

/// Map a `sign_set_strict` error to the `tctl sign` process exit code.
///
/// Why: #11683 — the scripts tell "no certificate" apart from every other
/// failure by exit code only.
/// What: `NoCertificate` gives [`EXIT_NO_CERTIFICATE`]; every other variant
/// gives [`EXIT_SIGN_FAILED`].
/// Test: `tests::no_certificate_has_distinct_exit_code`,
/// `tests::sign_failure_exit_code_is_not_no_certificate`.
pub fn sign_error_exit_code(err: &macos_signing::SignSetError) -> i32 {
    match err {
        macos_signing::SignSetError::NoCertificate => EXIT_NO_CERTIFICATE,
        _ => EXIT_SIGN_FAILED,
    }
}

/// Handle `tctl sign <target>` (target: `trusty-search`, `trusty-mpm`,
/// `trusty-memory` / `trusty-analyze` — owner ruling 2026-08-06 — or
/// `trusty-assistants` — #4277, renamed in #11632 — or `trusty-secrets` —
/// #11587).
///
/// Why: The CLI-facing entry point for standalone Developer-ID signing,
/// separate from the fail-soft hooks `commands::install` runs automatically.
///
/// What: Validates `target` against `macos_signing::binaries_for_set`
/// (non-empty = known), resolves `dir` (or the default cargo bin dir), and on
/// macOS calls `sign_set_strict`. `verbose` (#2939) is forwarded to the macOS
/// path, printing the raw identity-probe output and selected identity to
/// stderr — a diagnostic aid for "no identity found" failures despite a valid
/// keychain cert. Returns the process exit code: `0` on success,
/// [`EXIT_NO_CERTIFICATE`] (`3`) when no Developer ID certificate is
/// available, [`EXIT_SIGN_FAILED`] (`1`) on any other signing, verification
/// or record-refresh failure (#11587: including a signature that reads back
/// ad-hoc or with the wrong Team ID), and [`EXIT_UNKNOWN_TARGET`] (`2`) for an
/// unknown target. On non-macOS a known target is always a `0` no-op.
///
/// Test: `tests::run_unknown_target_is_error`,
/// `tests::no_certificate_has_distinct_exit_code`.
pub fn run(target: &str, dir: Option<PathBuf>, json: bool, verbose: bool) -> i32 {
    if macos_signing::binaries_for_set(target).is_empty() {
        eprintln!(
            // #11587: the target list comes from the signing table.
            "tctl sign: unknown target '{target}' (expected one of: {})",
            macos_signing::signable_set_names().join(", ")
        );
        return EXIT_UNKNOWN_TARGET;
    }

    #[cfg(target_os = "macos")]
    {
        run_macos(target, dir, json, verbose)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (dir, json, verbose);
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
/// What: Resolves the install dir, calls `sign_set_strict` (forwarding
/// `verbose`), and maps the result to an exit code + human message
/// (cert-setup guidance on failure, success note otherwise).
///
/// Test: Side-effecting (real `codesign`); not invoked in the test suite.
#[cfg(target_os = "macos")]
fn run_macos(target: &str, dir: Option<PathBuf>, json: bool, verbose: bool) -> i32 {
    let install_dir = dir.unwrap_or_else(default_bin_dir);

    match macos_signing::sign_set_strict(&install_dir, target, verbose) {
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
            // #10607: signing rewrites the bytes the record's digests name.
            if let Err(why) = super::deployment_record::refresh_digests(target) {
                eprintln!("tctl sign: could not update the install record: {why}");
                return EXIT_SIGN_FAILED;
            }
            0
        }
        // PR #2657 review (MEDIUM): match on the typed `SignSetError` variant
        // instead of substring-matching `e.to_string()` — brittle against any
        // future wording change to the error message.
        // #11683: the exit code comes from `sign_error_exit_code`, so no cert
        // (3) stays distinct from every other failure (1).
        Err(e) => {
            if !json {
                if matches!(e, macos_signing::SignSetError::NoCertificate) {
                    eprintln!("{}", macos_signing::cert_setup_guidance(target));
                } else {
                    eprintln!("tctl sign: {e}");
                }
            }
            sign_error_exit_code(&e)
        }
    }
}

/// Default binary directory: `$CARGO_HOME/bin`, falling back to `~/.cargo/bin`.
///
/// Why: `cargo install` writes here; `tctl sign` (run standalone, without a
/// preceding `tctl install`) needs the same default so it finds the binaries.
///
/// What: #4964 — delegates to the shared
/// [`trusty_common::bin_resolve::canonical_bin_dir`] rather than restating the
/// `CARGO_HOME` read, so `tctl sign`'s default and every other cargo-bin-dir
/// resolution in the workspace cannot drift apart.
///
/// Test: no test in this crate — a two-line delegation that only supplies the
/// last-resort `/usr/local/bin`. The rule it delegates to is covered in
/// trusty-common by the `canonical_bin_dir_from_*` tests, which the pointer
/// lint cannot verify from here because it scopes citations to the citing
/// file's own crate.
#[cfg(target_os = "macos")]
fn default_bin_dir() -> PathBuf {
    trusty_common::bin_resolve::canonical_bin_dir()
        .unwrap_or_else(|| PathBuf::from("/usr/local/bin"))
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
        let code = run("not-a-real-target", None, true, false);
        assert_eq!(code, EXIT_UNKNOWN_TARGET);
        assert_eq!(code, 2);
    }

    /// Why: #11683 — the signed-install scripts soft-pass a missing cert by
    /// exit code alone, so the code is a contract.
    /// What: `NoCertificate` maps to 3, and 3 is not the generic failure code.
    /// Test: This is the test.
    #[test]
    fn no_certificate_has_distinct_exit_code() {
        let code = sign_error_exit_code(&macos_signing::SignSetError::NoCertificate);
        assert_eq!(code, 3);
        assert_ne!(code, EXIT_SIGN_FAILED);
        assert_ne!(code, EXIT_UNKNOWN_TARGET);
    }

    /// Why: #11683 — a codesign error or a wrong-Team-ID signature must not
    /// read as "no cert", or the scripts would soft-pass it.
    /// What: `Sign` and `NoBinariesFound` map to 1.
    /// Test: This is the test.
    #[test]
    fn sign_failure_exit_code_is_not_no_certificate() {
        let sign = macos_signing::SignSetError::Sign(anyhow::anyhow!("wrong Team ID"));
        assert_eq!(sign_error_exit_code(&sign), 1);
        let missing = macos_signing::SignSetError::NoBinariesFound {
            set: "trusty-memory".to_owned(),
            dir: "/nonexistent".to_owned(),
        };
        assert_eq!(sign_error_exit_code(&missing), 1);
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
        let code = run("trusty-search", None, false, false);
        assert_eq!(code, 0);
    }
}
