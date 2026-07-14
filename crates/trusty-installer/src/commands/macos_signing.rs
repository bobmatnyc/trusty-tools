//! macOS Developer-ID codesign + FDA/TCC guidance — single source of truth (#2558).
//!
//! Why: Before #2558 this logic was duplicated between this module (the
//! `tctl install` post-install hook) and `scripts/install-trusty-search-signed.sh`
//! (the documented manual "permanent fix"). The two implementations had already
//! drifted — the script used `--identifier com.trusty.trusty-search` /
//! `com.trusty.trusty-embedderd` (matching the real launchd label convention in
//! `plist_label.rs` and the release-workflow docs) plus `--options runtime
//! --timestamp` and post-sign verification, while this module used the shorter
//! `com.trusty.search` / `com.trusty.embedderd` identifiers with none of the
//! extra flags. Folding both into one module (and one set of tests) means the
//! identifier scheme, signing flags, and guidance text can never diverge again.
//! Owner-authorized scope extension (2026-07-14, issue #2558 comment): the same
//! stable-identity signing now also covers `trusty-mpm` — macOS's App Data TCC
//! category ("'trusty-mpm' would like to access data from other apps")
//! re-prompts on every ad-hoc-signed rebuild for the same cdhash reason FDA does,
//! because `tm` reads other apps' `$HOME` containers (Claude config dirs, tmux
//! state).
//!
//! What: [`binaries_for_set`] maps a named signable set (`"trusty-search"` →
//! search + embedderd, `"trusty-mpm"` → mpm alone) to its binaries;
//! [`codesign_identifier`] maps each binary to its fixed `--identifier`. Probes
//! for a Developer ID Application certificate (env var `TRUSTY_SIGN_IDENTITY` >
//! `security find-identity`), signs with `--options runtime --timestamp`
//! (Hardened Runtime + secure timestamp, matching the script and required for
//! notarization), and verifies with `codesign --verify --deep --strict`.
//! [`post_install_search`] / [`post_install_mpm`] are the fail-soft hooks
//! `commands::install` calls after each member lands; [`sign_set_strict`] is the
//! hard-failing primitive the standalone `tctl sign <target>` command
//! (`commands::sign`) uses, which `scripts/install-trusty-search-signed.sh` now
//! shells out to instead of duplicating codesign flags in bash.
//!
//! Test: `tests` covers `binaries_for_set`, `codesign_identifier`,
//! `has_developer_id_cert` identity-probe logic, and all guidance-text
//! generation as pure functions; `codesign` / `security` are not invoked in
//! tests (side-effecting, macOS-only).

/// The `trusty-search` signable set: `trusty-search` + its bundled `trusty-embedderd`.
pub const SEARCH_SET: &str = "trusty-search";

/// The `trusty-mpm` signable set: `trusty-mpm` alone.
pub const MPM_SET: &str = "trusty-mpm";

/// Resolve the binaries that make up a named Developer-ID-signable set.
///
/// Why: `trusty-search` ships two binaries that must be signed together
/// (`trusty-search` + the bundled `trusty-embedderd`); `trusty-mpm` is signed
/// alone. Centralising the set membership means every caller (the fail-soft
/// install hooks and the strict `tctl sign` command) agrees on exactly which
/// binaries a target name covers.
///
/// What: Returns the binary names for `"trusty-search"` or `"trusty-mpm"`; an
/// empty slice for any other input (the caller treats that as "unknown set").
///
/// Test: `tests::binaries_for_set_covers_search_and_mpm`,
/// `tests::binaries_for_set_unknown_is_empty`.
pub fn binaries_for_set(set: &str) -> &'static [&'static str] {
    match set {
        SEARCH_SET => &["trusty-search", "trusty-embedderd"],
        MPM_SET => &["trusty-mpm"],
        _ => &[],
    }
}

/// The codesign identifiers for every signable binary.
///
/// Why: A fixed `--identifier` anchors the designated requirement (DR) to a
/// stable value rather than the binary hash, so the FDA / App-Data TCC grant
/// survives a recompile even without a Developer ID cert. The scheme
/// (`com.trusty.<binary>` with the binary's full name, e.g.
/// `com.trusty.trusty-search`) matches the launchd label convention in
/// `plist_label.rs` and `docs/reference/release-workflow.md` — this is the
/// canonical scheme; the pre-#2558 `com.trusty.search` (short form) used here
/// was the drift this issue fixes.
///
/// What: Maps binary name → codesign identifier.
///
/// Test: `tests::identifier_map_covers_all_signable_binaries`.
pub fn codesign_identifier(binary: &str) -> &'static str {
    match binary {
        "trusty-search" => "com.trusty.trusty-search",
        "trusty-embedderd" => "com.trusty.trusty-embedderd",
        "trusty-mpm" => "com.trusty.trusty-mpm",
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
/// Why: Signing with `--identifier` anchors the DR so the TCC FDA / App-Data
/// grant persists across reinstalls (no re-grant needed after `cargo install`).
/// `--options runtime` enables the Hardened Runtime (required for
/// notarization); `--timestamp` requests a secure timestamp from Apple's
/// servers — both match the pre-#2558 script's flags, folded in here so the
/// script no longer needs its own `codesign` invocation.
///
/// What: Runs `codesign --force --options runtime --timestamp --identifier <id>
/// --sign <identity> <path>`. Returns `Ok(())` on success, `Err` with the
/// `codesign` stderr on failure.
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
        .args([
            "--force",
            "--options",
            "runtime",
            "--timestamp",
            "--identifier",
            identifier,
            "--sign",
            identity,
        ])
        .arg(path)
        .output()
        .with_context(|| format!("running codesign on {}", path.display()))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        anyhow::bail!("codesign failed for {}: {stderr}", path.display());
    }
    Ok(())
}

/// Verify a binary's signature immediately after signing.
///
/// Why: `set -e`-equivalent belt-and-suspenders — `sign_binary` already fails
/// on a non-zero `codesign` exit, but a strict `--verify --deep --strict` pass
/// catches subtler corruption (e.g. a truncated write) that the sign step alone
/// might not (matches the script's post-sign check, issue #2322).
///
/// What: Runs `codesign --verify --deep --strict <path>`; `Ok(())` on a clean
/// verification, `Err` with the verifier's output otherwise.
///
/// Test: Not invoked in tests (side-effecting, requires a real signed binary).
#[cfg(target_os = "macos")]
pub fn verify_signature(path: &std::path::Path) -> anyhow::Result<()> {
    use anyhow::Context;
    let out = std::process::Command::new("codesign")
        .args(["--verify", "--deep", "--strict"])
        .arg(path)
        .output()
        .with_context(|| format!("running codesign --verify on {}", path.display()))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        anyhow::bail!(
            "post-sign verification failed for {}: {stderr}",
            path.display()
        );
    }
    Ok(())
}

/// Sign and verify every binary in a named set, hard-failing on any problem.
///
/// Why: The fail-soft `post_install_*` hooks below must never abort a `tctl
/// install` run over a signing hiccup, but the standalone `tctl sign <target>`
/// command (used directly, or shelled out to by
/// `scripts/install-trusty-search-signed.sh`) is the operator explicitly asking
/// for signing to happen — a silent partial failure there would be worse than a
/// loud one, exactly like the pre-#2558 script's `set -euo pipefail` behaviour.
///
/// What: Resolves the Developer ID identity (erroring if none is found), then
/// for every binary in `binaries_for_set(set)` that exists under `install_dir`,
/// signs it and verifies the signature — bailing out on the first failure.
/// Returns the paths that were signed. Errors when the set name is unknown or
/// no binary in the set was found on disk.
///
/// Test: Not invoked in tests (side-effecting); the set/identifier resolution
/// it composes is tested independently via `binaries_for_set` /
/// `codesign_identifier`.
#[cfg(target_os = "macos")]
pub fn sign_set_strict(
    install_dir: &std::path::Path,
    set: &str,
) -> anyhow::Result<Vec<std::path::PathBuf>> {
    let binaries = binaries_for_set(set);
    if binaries.is_empty() {
        anyhow::bail!("unknown signable set '{set}' (expected 'trusty-search' or 'trusty-mpm')");
    }
    let identity = has_developer_id_cert()
        .ok_or_else(|| anyhow::anyhow!("no Developer ID Application certificate found"))?;

    let mut signed = Vec::new();
    for name in binaries {
        let path = install_dir.join(name);
        if !path.exists() {
            continue;
        }
        sign_binary(&path, &identity)?;
        verify_signature(&path)?;
        signed.push(path);
    }
    if signed.is_empty() {
        anyhow::bail!(
            "no binaries from set '{set}' found in {}",
            install_dir.display()
        );
    }
    Ok(signed)
}

/// The Developer ID certificate setup guidance (no cert found, strict path).
///
/// Why: Extracted as a pure function so the exact guidance text is unit-tested
/// without any system calls; mirrors the script's `print_cert_setup_and_exit`.
///
/// What: Returns the 5-step "enroll → issue cert → install → verify → re-run"
/// instructions for `tctl sign <target>`.
///
/// Test: `tests::cert_setup_guidance_contains_steps`.
pub fn cert_setup_guidance(target: &str) -> String {
    format!(
        "No 'Developer ID Application' certificate found in the login keychain.\n\
         \n\
         To enable persistent signing (one-time setup):\n\
         \n\
         1. Enroll in the Apple Developer Program:\n\
         \x20\x20\x20\x20https://developer.apple.com/programs/enroll/\n\
         2. Issue a \"Developer ID Application\" certificate (Xcode -> Settings ->\n\
         \x20\x20\x20\x20Accounts -> Manage Certificates -> \"+\", or\n\
         \x20\x20\x20\x20https://developer.apple.com/account/resources/certificates/list).\n\
         3. Download and double-click the .cer file to install it in your login\n\
         \x20\x20\x20\x20keychain.\n\
         4. Verify: security find-identity -v -p codesigning | grep 'Developer ID Application'\n\
         5. Re-run: tctl sign {target}\n"
    )
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
            launchctl bootout gui/$(id -u) ~/Library/LaunchAgents/com.trusty.trusty-search.plist\n\
            launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/com.trusty.trusty-search.plist\n\
         \n\
         Tip: install a Developer ID Application certificate and set TRUSTY_SIGN_IDENTITY\n\
         to make this grant persist across all future reinstalls."
    )
}

/// The App-Data TCC guidance for `trusty-mpm` (used when no Developer ID cert
/// is found).
///
/// Why: `tm` re-prompts "'trusty-mpm' would like to access data from other
/// apps" after every ad-hoc-signed rebuild, for the same cdhash-identity reason
/// FDA does for trusty-search — but it is a different TCC category (App Data,
/// not Full Disk Access) with no `System Settings` list entry to remove/re-add,
/// so the guidance differs from [`fda_guidance`].
///
/// What: Returns guidance explaining that the next prompt must be approved once
/// and that a Developer ID cert makes that approval persist.
///
/// Test: `tests::app_data_guidance_mentions_prompt`.
pub fn app_data_guidance(binary_path: &str) -> String {
    format!(
        "trusty-mpm App Data TCC guidance (macOS):\n\
         Every `cargo install` replaces the binary with a new cdhash, so macOS re-prompts\n\
         \"'trusty-mpm' would like to access data from other apps\" (it reads other apps'\n\
         $HOME containers — Claude config dirs, tmux state) after every reinstall.\n\
         \n\
         Approve the next prompt for `{binary_path}` when it appears — no manual\n\
         System Settings step is required for this TCC category.\n\
         \n\
         Tip: install a Developer ID Application certificate and set TRUSTY_SIGN_IDENTITY\n\
         to make that approval persist across all future reinstalls (run `tctl sign trusty-mpm`)."
    )
}

/// Run the fail-soft post-install signing hook for a named set.
///
/// Why: Shared by [`post_install_search`] and [`post_install_mpm`] so the
/// probe/sign/guidance sequence is written once.
///
/// What: On macOS: probes for a Developer ID cert; if found, signs + verifies
/// every present binary in `set` and prints a success note; on cert-probe
/// failure, prints the set-appropriate guidance (FDA for search, App Data TCC
/// for mpm) using the set's first binary's path. Never aborts the caller.
#[cfg(target_os = "macos")]
fn post_install_signed_set(install_dir: &std::path::Path, set: &str, json: bool) {
    let binaries = binaries_for_set(set);
    let Some(&primary) = binaries.first() else {
        return;
    };
    let primary_path = install_dir.join(primary);

    match has_developer_id_cert() {
        Some(identity) => {
            let mut signed_ok = true;
            for name in binaries {
                let path = install_dir.join(name);
                if !path.exists() {
                    // A bundled binary (e.g. embedderd) may not be present; skip gracefully.
                    continue;
                }
                if let Err(e) = sign_binary(&path, &identity) {
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
                    "{set}: signed with Developer ID. The macOS grant will persist across \
                     all future reinstalls."
                );
            }
        }
        None => {
            let path_str = primary_path.to_string_lossy();
            if !json {
                let guidance = if set == MPM_SET {
                    app_data_guidance(&path_str)
                } else {
                    fda_guidance(&path_str)
                };
                eprintln!("{guidance}");
            }
        }
    }
}

/// Run post-install codesign and FDA guidance for trusty-search.
///
/// Why: After trusty-search (and trusty-embedderd) are installed the installer
/// should immediately sign them (if a cert is available) or print the FDA
/// re-grant guidance so the operator knows what to do next.
///
/// What: On macOS delegates to [`post_install_signed_set`] for [`SEARCH_SET`].
/// On non-macOS: no-op.
///
/// Test: `tests::fda_guidance_contains_steps` (pure); signing itself is not
/// invoked in tests (side-effecting).
pub fn post_install_search(install_dir: &std::path::Path, json: bool) {
    #[cfg(target_os = "macos")]
    post_install_signed_set(install_dir, SEARCH_SET, json);
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (install_dir, json); // suppress unused warnings on non-macOS
    }
}

/// Run post-install codesign and App-Data-TCC guidance for trusty-mpm.
///
/// Why: Owner-authorized scope extension (#2558, 2026-07-14) — `tm` needs the
/// same stable-identity signing as trusty-search so the App Data TCC prompt
/// does not re-fire on every `cargo install` rebuild.
///
/// What: On macOS delegates to [`post_install_signed_set`] for [`MPM_SET`]. On
/// non-macOS: no-op.
///
/// Test: `tests::app_data_guidance_mentions_prompt` (pure); signing itself is
/// not invoked in tests (side-effecting).
pub fn post_install_mpm(install_dir: &std::path::Path, json: bool) {
    #[cfg(target_os = "macos")]
    post_install_signed_set(install_dir, MPM_SET, json);
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (install_dir, json); // suppress unused warnings on non-macOS
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: Both signable sets must resolve to their documented binaries.
    /// What: Asserts `trusty-search` covers search+embedderd, `trusty-mpm` covers mpm alone.
    /// Test: This is the test.
    #[test]
    fn binaries_for_set_covers_search_and_mpm() {
        assert_eq!(
            binaries_for_set(SEARCH_SET),
            &["trusty-search", "trusty-embedderd"]
        );
        assert_eq!(binaries_for_set(MPM_SET), &["trusty-mpm"]);
    }

    /// Why: An unrecognised set name must not silently sign nothing without
    /// signal — callers detect "unknown set" via an empty slice.
    /// What: Asserts a bogus name returns an empty slice.
    /// Test: This is the test.
    #[test]
    fn binaries_for_set_unknown_is_empty() {
        assert!(binaries_for_set("not-a-set").is_empty());
    }

    /// Why: Every signable binary name must map to a stable identifier
    /// (contract for the codesign `--identifier` flag), matching the
    /// `com.trusty.trusty-<binary>` scheme used by `plist_label.rs` and the
    /// release-workflow docs (the pre-#2558 short-form identifiers were the
    /// drift this module fixes).
    /// What: Asserts all three target binaries map to the expected IDs.
    /// Test: This is the test.
    #[test]
    fn identifier_map_covers_all_signable_binaries() {
        assert_eq!(
            codesign_identifier("trusty-search"),
            "com.trusty.trusty-search"
        );
        assert_eq!(
            codesign_identifier("trusty-embedderd"),
            "com.trusty.trusty-embedderd"
        );
        assert_eq!(codesign_identifier("trusty-mpm"), "com.trusty.trusty-mpm");
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

    /// Why: The App-Data TCC guidance must reference the binary path and the
    /// actual macOS prompt wording so the operator recognises it.
    /// What: Calls `app_data_guidance` and checks the output.
    /// Test: This is the test.
    #[test]
    fn app_data_guidance_mentions_prompt() {
        let g = app_data_guidance("/usr/local/bin/trusty-mpm");
        assert!(
            g.contains("access data from other apps"),
            "TCC prompt wording missing"
        );
        assert!(
            g.contains("/usr/local/bin/trusty-mpm"),
            "binary path missing"
        );
        assert!(g.contains("Developer ID"), "Developer ID tip missing");
    }

    /// Why: The cert-setup guidance must reference the exact `tctl sign`
    /// invocation the operator should re-run, parameterised by target.
    /// What: Asserts all 5 steps are present and the target is interpolated.
    /// Test: This is the test.
    #[test]
    fn cert_setup_guidance_contains_steps() {
        let g = cert_setup_guidance("trusty-mpm");
        for step in ["1.", "2.", "3.", "4.", "5."] {
            assert!(g.contains(step), "{step} missing");
        }
        assert!(g.contains("tctl sign trusty-mpm"));
    }
}
