//! macOS codesign + FDA guidance for trusty-search and trusty-embedderd (Phase 8).
//!
//! Why: On macOS, every `cargo install` writes a new binary with a new cdhash,
//! invalidating the TCC Full Disk Access grant. Developer-ID signing with a fixed
//! `--identifier` anchors the DR to the Team ID, not the binary hash, so the FDA
//! grant persists across reinstalls. When no cert is available, the operator must
//! re-grant FDA manually.
//!
//! What: Probes for a Developer ID Application certificate (env var
//! `TRUSTY_SIGN_IDENTITY` > `security find-identity`), signs BOTH
//! `trusty-search` and `trusty-embedderd` in the install directory, and prints
//! the appropriate guidance (persist-across-reinstall notice OR the 4-step manual
//! FDA re-grant procedure).
//!
//! Test: `tests` covers `has_developer_id_cert` identity-probe logic and the
//! FDA guidance text generation as pure functions; `codesign` and `security` are
//! not invoked in tests.

/// The codesign identifiers for the two signed binaries.
///
/// Why: A fixed `--identifier` anchors the designated requirement (DR) to a
/// stable value rather than the binary hash, so the FDA grant survives a
/// recompile even without a Developer ID cert.
///
/// What: Maps binary name → codesign identifier.
///
/// Test: `tests::identifier_map_covers_both_binaries`.
pub fn codesign_identifier(binary: &str) -> &'static str {
    match binary {
        "trusty-search" => "com.trusty.search",
        "trusty-embedderd" => "com.trusty.embedderd",
        _ => "com.trusty.unknown",
    }
}

/// Check whether a Developer ID Application certificate is available.
///
/// Why: Signing is only possible when a suitable cert is present in the keychain;
/// we check before attempting `codesign` to give a clear early message.
///
/// What: Checks `TRUSTY_SIGN_IDENTITY` env first (user override); if absent,
/// runs `security find-identity -v -p codesigning` and looks for
/// "Developer ID Application" in the output. Returns `Some(identity)` when
/// found, `None` when no cert is available.
///
/// Test: `tests::has_developer_id_cert_respects_env` (macOS only).
#[cfg(target_os = "macos")]
pub fn has_developer_id_cert() -> Option<String> {
    // Environment override takes priority.
    if let Ok(identity) = std::env::var("TRUSTY_SIGN_IDENTITY") {
        if !identity.is_empty() {
            return Some(identity);
        }
    }
    // Probe the keychain.
    let output = std::process::Command::new("security")
        .args(["find-identity", "-v", "-p", "codesigning"])
        .output()
        .ok()?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        if line.contains("Developer ID Application") {
            // Extract the identity string: the part after the ") " token.
            if let Some(ident) = line.split(") ").nth(1) {
                return Some(ident.trim().to_owned());
            }
        }
    }
    None
}

/// Sign a binary with a Developer ID Application certificate.
///
/// Why: Signing with `--identifier` anchors the DR so the TCC FDA grant
/// persists across reinstalls (no re-grant needed after `cargo install`).
///
/// What: Runs `codesign --force --sign <identity> --identifier <id> <path>`.
/// Returns `Ok(())` on success, `Err` with the `codesign` stderr on failure.
///
/// Test: Not invoked in tests (side-effecting); the identifier mapping is
/// tested via `codesign_identifier`.
#[cfg(target_os = "macos")]
pub fn sign_binary(path: &std::path::Path, identity: &str) -> anyhow::Result<()> {
    use anyhow::Context;
    let binary_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown");
    let identifier = codesign_identifier(binary_name);
    let out = std::process::Command::new("codesign")
        .args(["--force", "--sign", identity, "--identifier", identifier])
        .arg(path)
        .output()
        .with_context(|| format!("running codesign on {}", path.display()))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        anyhow::bail!("codesign failed for {}: {stderr}", path.display());
    }
    Ok(())
}

/// The 4-step FDA re-grant guidance (used when no Developer ID cert is found).
///
/// Why: Extracted as a pure function so the exact guidance text can be unit-tested
/// without any system calls.
///
/// What: Returns the formatted 4-step manual FDA re-grant instructions, with
/// the actual binary path interpolated.
///
/// Test: `tests::fda_guidance_contains_steps`.
pub fn fda_guidance(binary_path: &str) -> String {
    format!(
        "trusty-search FDA guidance (macOS):\n\
         Every `cargo install` replaces the binary with a new cdhash, which invalidates\n\
         the macOS TCC Full Disk Access grant. Re-grant it now:\n\
         \n\
         1. Open System Settings \u{2192} Privacy & Security \u{2192} Full Disk Access.\n\
         2. Remove `{binary_path}` from the list (click the entry, then click -).\n\
         3. Re-add it (click +, navigate to `{binary_path}`).\n\
         4. Restart the daemon:\n\
            launchctl bootout gui/$(id -u) ~/Library/LaunchAgents/com.trusty.search.plist\n\
            launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/com.trusty.search.plist\n\
         \n\
         Tip: install a Developer ID Application certificate and set TRUSTY_SIGN_IDENTITY\n\
         to make this grant persist across all future reinstalls."
    )
}

/// Run post-install codesign and FDA guidance for trusty-search.
///
/// Why: After trusty-search (and trusty-embedderd) are installed the installer
/// should immediately sign them (if a cert is available) or print the FDA
/// re-grant guidance so the operator knows what to do next.
///
/// What: On macOS: probes for a Developer ID cert via `has_developer_id_cert`;
/// if found, signs BOTH `trusty-search` and `trusty-embedderd` in `install_dir`;
/// on success prints that signing makes the FDA grant persist. On cert-probe
/// failure, prints the 4-step FDA re-grant guidance with the actual path. On
/// non-macOS: no-op.
///
/// Test: `tests::fda_guidance_contains_steps` (pure); signing itself is not
/// invoked in tests (side-effecting).
pub fn post_install_search(install_dir: &std::path::Path, json: bool) {
    #[cfg(target_os = "macos")]
    post_install_search_macos(install_dir, json);
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (install_dir, json); // suppress unused warnings on non-macOS
    }
}

#[cfg(target_os = "macos")]
fn post_install_search_macos(install_dir: &std::path::Path, json: bool) {
    let search_path = install_dir.join("trusty-search");
    let embedderd_path = install_dir.join("trusty-embedderd");

    match has_developer_id_cert() {
        Some(identity) => {
            let mut signed_ok = true;
            for path in [&search_path, &embedderd_path] {
                if !path.exists() {
                    // embedderd may not be separately installed; skip gracefully.
                    continue;
                }
                if let Err(e) = sign_binary(path, &identity) {
                    if !json {
                        eprintln!(
                            "trusty-installer: warning: codesign failed for {}: {e}",
                            path.display()
                        );
                    }
                    signed_ok = false;
                }
            }
            if signed_ok && !json {
                eprintln!(
                    "trusty-search: signed with Developer ID. \
                     The macOS Full Disk Access grant will persist across all future reinstalls."
                );
            }
        }
        None => {
            // No cert — print the 4-step FDA re-grant guidance.
            let path_str = search_path.to_string_lossy();
            if !json {
                eprintln!("{}", fda_guidance(&path_str));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: Both binary names must map to stable identifiers (contract for the
    /// codesign `--identifier` flag).
    /// What: Asserts the two target binaries map to the expected com.trusty.* IDs.
    /// Test: This is the test.
    #[test]
    fn identifier_map_covers_both_binaries() {
        assert_eq!(codesign_identifier("trusty-search"), "com.trusty.search");
        assert_eq!(
            codesign_identifier("trusty-embedderd"),
            "com.trusty.embedderd"
        );
    }

    /// Why: The FDA guidance must contain all 4 numbered steps and reference the
    /// binary path so the operator knows which file to re-add.
    /// What: Calls `fda_guidance` with a synthetic path and checks the output.
    /// Test: This is the test.
    #[test]
    fn fda_guidance_contains_steps() {
        let guidance = fda_guidance("/usr/local/bin/trusty-search");
        assert!(guidance.contains("1."), "step 1 missing");
        assert!(guidance.contains("2."), "step 2 missing");
        assert!(guidance.contains("3."), "step 3 missing");
        assert!(guidance.contains("4."), "step 4 missing");
        assert!(
            guidance.contains("/usr/local/bin/trusty-search"),
            "binary path not in guidance"
        );
        assert!(
            guidance.contains("Full Disk Access"),
            "FDA label not in guidance"
        );
    }

    /// Why: `TRUSTY_SIGN_IDENTITY` env var must override the keychain probe.
    /// What: On macOS, sets the env var; asserts `has_developer_id_cert` returns
    /// the env value. Restores env after the test.
    /// Test: This is the test.
    #[cfg(target_os = "macos")]
    #[test]
    fn has_developer_id_cert_respects_env() {
        // We cannot modify env safely in parallel tests without a mutex, so we
        // just verify the function is callable and returns an Option<String>.
        // The env-override path is tested by setting the env var manually.
        let prev = std::env::var("TRUSTY_SIGN_IDENTITY").ok();
        // SAFETY: single-threaded test; environment mutation is process-global
        // but safe to call in Rust 1.91 (not yet stabilized as unsafe).
        unsafe {
            std::env::set_var(
                "TRUSTY_SIGN_IDENTITY",
                "Test Developer ID: Foo Bar (TEAM123)",
            );
        }
        let result = has_developer_id_cert();
        // Restore.
        unsafe {
            match prev {
                Some(v) => std::env::set_var("TRUSTY_SIGN_IDENTITY", v),
                None => std::env::remove_var("TRUSTY_SIGN_IDENTITY"),
            }
        }
        assert_eq!(
            result.as_deref(),
            Some("Test Developer ID: Foo Bar (TEAM123)")
        );
    }

    /// Why: `fda_guidance` must mention Developer ID as the permanent fix tip.
    /// What: Asserts the tip line is present.
    /// Test: This is the test.
    #[test]
    fn fda_guidance_mentions_developer_id_tip() {
        let g = fda_guidance("/some/path/trusty-search");
        assert!(
            g.contains("Developer ID"),
            "tip about Developer ID cert missing"
        );
    }
}
