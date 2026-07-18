use super::*;

/// Why: Both signable sets must resolve to their documented binaries. The
/// `trusty-mpm` set MUST include `tm` (#2721) — omitting it left the primary
/// `tm` binary ad-hoc and the App-Data TCC prompt kept recurring. Order
/// matters: `trusty-mpm` first so it stays the guidance/prompt "primary".
/// `trusty-mpm-gui` (#2951) joined the set after `tm` for the same reason.
/// What: Asserts `trusty-search` covers search+embedderd, `trusty-mpm` covers
/// `trusty-mpm`, `tm`, and `trusty-mpm-gui` in that order.
/// Test: This is the test.
#[test]
fn binaries_for_set_covers_search_and_mpm() {
    assert_eq!(
        binaries_for_set(SEARCH_SET),
        vec!["trusty-search", "trusty-embedderd"]
    );
    assert_eq!(
        binaries_for_set(MPM_SET),
        vec!["trusty-mpm", "tm", "trusty-mpm-gui"]
    );
}

/// Why: An unrecognised set name must not silently sign nothing without
/// signal — callers detect "unknown set" via an empty result.
/// What: Asserts a bogus name returns an empty `Vec`.
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
/// What: Asserts all five target binaries map to the expected IDs,
/// including the `tm` binary (`com.trusty.tm`, #2721) and `trusty-mpm-gui`
/// (`com.trusty.trusty-mpm.gui`, #2951) that share the trusty-mpm set.
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
    assert_eq!(codesign_identifier("tm"), "com.trusty.tm");
    assert_eq!(
        codesign_identifier("trusty-mpm-gui"),
        "com.trusty.trusty-mpm.gui"
    );
}

/// Why: PR #2657 review MEDIUM — with set membership and identifier now
/// derived from one table, this regression-guards that every binary
/// produced by `binaries_for_set` for EVERY known set has a real
/// (non-fallback) identifier, i.e. the two views of the table can never
/// silently disagree.
/// What: For each of `SEARCH_SET`/`MPM_SET`, asserts every member binary's
/// identifier is not the `"com.trusty.unknown"` fallback.
/// Test: This is the test.
#[test]
fn every_set_member_has_a_real_identifier() {
    for set in [SEARCH_SET, MPM_SET] {
        for binary in binaries_for_set(set) {
            assert_ne!(
                codesign_identifier(binary),
                "com.trusty.unknown",
                "{binary} in set {set} has no real identifier"
            );
        }
    }
}

/// Why: The PM decision (PR #2657 review HIGH) is a precise per-set,
/// per-context split — pin the truth table so a future edit cannot
/// silently flip either preserved pre-PR behavior.
/// What: explicit=true is always hardened (both sets); explicit=false is
/// hardened only for MPM_SET.
/// Test: This is the test.
#[test]
fn hardened_runtime_policy() {
    assert!(use_hardened_runtime(SEARCH_SET, true));
    assert!(use_hardened_runtime(MPM_SET, true));
    assert!(!use_hardened_runtime(SEARCH_SET, false));
    assert!(use_hardened_runtime(MPM_SET, false));
}

/// Why: The migration notice must actually fire when the on-disk
/// identifier differs from the canonical one — this is the #2657 review
/// HIGH fix; a silent no-op here would reproduce #873.
/// What: Asserts the pre-#2558 short-form identifier triggers a notice
/// naming both the old and new values.
/// Test: This is the test.
#[test]
fn identifier_migration_notice_warns_on_change() {
    let notice = identifier_migration_notice("trusty-search", "com.trusty.search")
        .expect("must warn on a real change");
    assert!(notice.contains("com.trusty.search"));
    assert!(notice.contains("com.trusty.trusty-search"));
    assert!(notice.contains("Full Disk Access"));
}

/// Why: The notice must NOT fire for a fresh install (no prior identifier)
/// or when re-signing with the identifier unchanged — false-positive
/// alarms would train operators to ignore the real warning.
/// What: Asserts `None` for an empty old identifier and for a match.
/// Test: This is the test.
#[test]
fn identifier_migration_notice_silent_when_unchanged_or_absent() {
    assert!(identifier_migration_notice("trusty-search", "").is_none());
    assert!(identifier_migration_notice("trusty-search", "com.trusty.trusty-search").is_none());
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

/// Why: THE #2937/#2939 regression test. A realistic multi-line `security
/// find-identity -v -p codesigning` fixture (including the header line
/// and the trailing "N valid identities found" footer the real tool
/// emits) must yield the clean quoted identity string — SHA1 fingerprint
/// and quotes stripped — not the garbage `line.split(") ").nth(1)`
/// previously passed straight to `codesign --sign`.
/// What: Asserts the parsed identity exactly matches the quoted common
/// name, with no leading fingerprint or quote characters.
/// Test: This is the test.
#[test]
fn parse_developer_id_identity_strips_fingerprint_and_quotes() {
    let fixture = "Policy: Codesigning\n\
         Matching identities\n\
         1) F03283D9CB41F7F084FFB01636A9AED54C8FB362 \"Developer ID Application: Bob Matsuoka (4JH68XUHC5)\"\n\
         2) A1B2C3D4E5F6A1B2C3D4E5F6A1B2C3D4E5F6A1B2 \"Apple Development: Bob Matsuoka (4JH68XUHC5)\"\n\
            2 identities found\n\
         \n\
         Valid identities only\n\
         1) F03283D9CB41F7F084FFB01636A9AED54C8FB362 \"Developer ID Application: Bob Matsuoka (4JH68XUHC5)\"\n\
            1 valid identities found\n";
    let identity = parse_developer_id_identity(fixture).expect("must find an identity");
    assert_eq!(
        identity,
        "Developer ID Application: Bob Matsuoka (4JH68XUHC5)"
    );
    assert!(
        !identity.contains("F03283D9"),
        "fingerprint must not leak into the parsed identity"
    );
    assert!(
        !identity.starts_with('"') && !identity.ends_with('"'),
        "quotes must be stripped from the parsed identity"
    );
}

/// Why: A probe with no Developer ID Application line (e.g. only an Apple
/// Development cert, or an empty keychain) must return `None`, not panic
/// or return a garbage substring.
/// What: Asserts `None` for output containing no matching line, and for
/// the "0 valid identities found" empty-keychain case.
/// Test: This is the test.
#[test]
fn parse_developer_id_identity_no_match() {
    let no_dev_id = "1) A1B2C3D4E5F6A1B2C3D4E5F6A1B2C3D4E5F6A1B2 \"Apple Development: Bob Matsuoka (4JH68XUHC5)\"\n\
            1 identities found\n";
    assert!(parse_developer_id_identity(no_dev_id).is_none());

    let empty = "0 valid identities found\n";
    assert!(parse_developer_id_identity(empty).is_none());

    assert!(parse_developer_id_identity("").is_none());
}

/// Why: A "Developer ID Application" line with fewer than two quote
/// characters (malformed / truncated output) must be skipped rather than
/// aborting the entire probe — a later, well-formed line should still be
/// found.
/// What: Feeds a malformed line followed by a well-formed one; asserts
/// the well-formed identity is still returned.
/// Test: This is the test.
#[test]
fn parse_developer_id_identity_skips_malformed_line() {
    let fixture = "1) F03283D9CB41F7F084FFB01636A9AED54C8FB362 Developer ID Application: no quotes here\n\
         2) A1B2C3D4E5F6A1B2C3D4E5F6A1B2C3D4E5F6A1B2 \"Developer ID Application: Bob Matsuoka (4JH68XUHC5)\"\n";
    let identity = parse_developer_id_identity(fixture).expect("must find the well-formed line");
    assert_eq!(
        identity,
        "Developer ID Application: Bob Matsuoka (4JH68XUHC5)"
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

/// Why: `commands::sign::run_macos` matches on `SignSetError` variants
/// (PR #2657 review MEDIUM fix, replacing string-matching); the variants'
/// `Display` text must stay distinguishable from each other for any
/// fallback logging path that does print the message.
/// What: Asserts each variant's rendered message is non-empty and unique.
/// Test: This is the test.
#[test]
fn sign_set_error_display_is_distinct_per_variant() {
    let variants: Vec<String> = vec![
        SignSetError::UnknownSet("bogus".to_owned()).to_string(),
        SignSetError::NoCertificate.to_string(),
        SignSetError::NoBinariesFound {
            set: "trusty-search".to_owned(),
            dir: "/tmp".to_owned(),
        }
        .to_string(),
        SignSetError::Sign(anyhow::anyhow!("boom")).to_string(),
    ];
    for v in &variants {
        assert!(!v.is_empty());
    }
    let unique: std::collections::HashSet<&String> = variants.iter().collect();
    assert_eq!(unique.len(), variants.len(), "variant messages collided");
}
