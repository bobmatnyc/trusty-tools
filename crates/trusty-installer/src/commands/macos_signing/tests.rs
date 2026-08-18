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

/// Why (#4277): `trusty-agents` is its own signable set, distinct from
/// `trusty-mpm` — see [`AGENTS_SET`]'s doc for why it isn't folded in.
/// What: Asserts `AGENTS_SET` resolves to exactly `["tagent"]`.
/// Test: This is the test.
#[test]
fn binaries_for_set_covers_agents() {
    assert_eq!(binaries_for_set(AGENTS_SET), vec!["tagent"]);
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
    assert_eq!(codesign_identifier("tagent"), "com.trusty.tagent");
}

/// Why: PR #2657 review MEDIUM — with set membership and identifier now
/// derived from one table, this regression-guards that every binary
/// produced by `binaries_for_set` for EVERY known set has a real
/// (non-fallback) identifier, i.e. the two views of the table can never
/// silently disagree.
///
/// The set list used to be the hardcoded array `[SEARCH_SET, MPM_SET,
/// AGENTS_SET]`, which made this vacuous for exactly the case it was meant to
/// catch: a set added to the table but not to the array was never iterated, so
/// the test passed without checking it. It now reads the set names out of
/// `SIGNABLE_BINARIES` itself, so a new row is covered the moment it lands.
///
/// What: For every distinct set name appearing in `SIGNABLE_BINARIES`, asserts
/// every member binary's identifier is not the `"com.trusty.unknown"` fallback.
/// Test: This is the test.
#[test]
fn every_set_member_has_a_real_identifier() {
    for set in declared_sets() {
        for binary in binaries_for_set(set) {
            assert_ne!(
                codesign_identifier(binary),
                "com.trusty.unknown",
                "{binary} in set {set} has no real identifier"
            );
        }
    }
}

/// Every distinct set name declared in `SIGNABLE_BINARIES`, in table order.
///
/// Why: Several guards below must cover EVERY set, and a hardcoded list is the
/// vacuity trap described on `every_set_member_has_a_real_identifier` — a set
/// the list forgets is a set nothing checks. Deriving the list from the table
/// makes forgetting impossible.
/// What: Walks `SIGNABLE_BINARIES`, collecting each set name the first time it
/// appears.
/// Test: Used by `every_set_member_has_a_real_identifier`,
/// `every_declared_set_is_a_named_constant`,
/// `signing_persistence_tip_names_every_signable_set`,
/// `every_sign_target_arg_resolves_to_a_real_set`.
fn declared_sets() -> Vec<&'static str> {
    let mut sets: Vec<&'static str> = Vec::new();
    for (_, set, _) in SIGNABLE_BINARIES {
        if !sets.contains(set) {
            sets.push(set);
        }
    }
    sets
}

/// Why: A typo in a table row's set field (`"trusty-momery"`) silently creates
/// an extra "set" that no `tctl sign` target and no post-install hook can ever
/// name, so the binary is never signed and nothing reports it. Pinning the
/// declared sets against the exported constants catches that at compile-and-
/// test time rather than on someone's machine months later.
/// What: Asserts the distinct set names in `SIGNABLE_BINARIES` are exactly the
/// exported set constants, in table order.
/// Test: This is the test.
#[test]
fn every_declared_set_is_a_named_constant() {
    assert_eq!(
        declared_sets(),
        vec![SEARCH_SET, MPM_SET, AGENTS_SET, MEMORY_SET, ANALYZE_SET]
    );
}

/// Why: This table decides which binaries get a stable macOS designated
/// requirement and which stay ad-hoc, losing their TCC grant on every
/// `cargo install`. Every prior gap in it (#2721 `tm`, #2951 the GUI, #4277
/// `tagent`, and `trusty-memory`/`trusty-analyze` under the 2026-08-06 owner
/// ruling) was a silent
/// omission, never a wrong value — so the guard that matters is one that fails
/// when a row goes MISSING, which a per-binary spot check cannot do. Pinning
/// the whole table is deliberate brittleness: changing it should require
/// stating the change here.
/// What: Asserts `SIGNABLE_BINARIES` equals the expected `(binary, set,
/// identifier)` triples exactly, including order.
/// Test: This is the test.
#[test]
fn signable_binaries_table_is_pinned() {
    assert_eq!(
        SIGNABLE_BINARIES,
        &[
            ("trusty-search", SEARCH_SET, "com.trusty.trusty-search"),
            (
                "trusty-embedderd",
                SEARCH_SET,
                "com.trusty.trusty-embedderd"
            ),
            ("trusty-mpm", MPM_SET, "com.trusty.trusty-mpm"),
            ("tm", MPM_SET, "com.trusty.tm"),
            ("trusty-mpm-gui", MPM_SET, "com.trusty.trusty-mpm.gui"),
            ("tagent", AGENTS_SET, "com.trusty.tagent"),
            ("trusty-memory", MEMORY_SET, "com.trusty.trusty-memory"),
            (
                "trusty-memory-mcp-bridge",
                MEMORY_SET,
                "com.trusty.trusty-memory-mcp-bridge"
            ),
            ("trusty-analyze", ANALYZE_SET, "com.trusty.trusty-analyze"),
        ]
    );
}

/// Why (owner ruling 2026-08-06): `cargo install --path crates/trusty-memory`
/// installs more than one binary and #2721 is the recorded lesson that signing
/// only the primary one leaves the rest ad-hoc while the prompt keeps
/// recurring. Order matters for the same reason it does for `MPM_SET`:
/// `first()` is the binary any guidance text names.
/// What: Asserts `MEMORY_SET` resolves to `trusty-memory` and
/// `trusty-memory-mcp-bridge` in that order, each with its canonical
/// `com.trusty.<binary>` identifier. #5329 dropped the third entry,
/// `trusty-bm25-daemon` — that binary is no longer built or installed.
/// Test: This is the test.
#[test]
fn binaries_for_set_covers_memory() {
    assert_eq!(
        binaries_for_set(MEMORY_SET),
        vec!["trusty-memory", "trusty-memory-mcp-bridge"]
    );
    assert_eq!(
        codesign_identifier("trusty-memory"),
        "com.trusty.trusty-memory"
    );
    assert_eq!(
        codesign_identifier("trusty-memory-mcp-bridge"),
        "com.trusty.trusty-memory-mcp-bridge"
    );
}

/// Why: `SignTargetArg` — not `binaries_for_set` — is the real gate on
/// `tctl sign <target>`: clap rejects any value the enum does not list before
/// `commands::sign::run` is ever called. A set added to `SIGNABLE_BINARIES`
/// without a matching variant is therefore signable in principle and
/// unreachable in practice, which is a silent no-op rather than an error.
/// A variant's `as_set_name()` is only HALF the CLI contract, and the half
/// that does not gate anything. What clap actually accepts on the command line
/// is the `#[value(name = …)]` attribute, and nothing forces the two to agree:
/// a variant whose attribute reads `"trusty-memroy"` while `as_set_name()`
/// returns `"trusty-memory"` passes a check that only reads the latter, yet
/// `tctl sign trusty-memory` is rejected by clap and `tctl sign trusty-memroy`
/// resolves to a set that exists. That is the same vacuity this test was added
/// to close, one level up — so both strings are pinned against each other.
///
/// What: For every `SignTargetArg` variant, asserts (a) its clap value name is
/// byte-identical to its `as_set_name()`, and (b) that name resolves to a
/// non-empty set; then asserts the variants and the table's declared sets are
/// the same collection, so neither side can carry an entry the other lacks.
/// Test: This is the test.
#[test]
fn every_sign_target_arg_resolves_to_a_real_set() {
    use crate::cli::SignTargetArg;
    use clap::ValueEnum;

    let mut from_cli: Vec<&'static str> = Vec::new();
    for variant in SignTargetArg::value_variants() {
        let set = variant.as_set_name();

        // (a) The string clap ACCEPTS must be the string we RESOLVE. Without
        // this, a typo'd `#[value(name = …)]` is invisible to every other
        // assertion here.
        let possible = variant
            .to_possible_value()
            .expect("every SignTargetArg variant must be a selectable clap value");
        assert_eq!(
            possible.get_name(),
            set,
            "SignTargetArg::{variant:?} accepts `{}` on the command line but resolves to set \
             `{set}` — `tctl sign {set}` would be rejected by clap",
            possible.get_name()
        );

        // (b) …and that string must name a set that has binaries.
        assert!(
            !binaries_for_set(set).is_empty(),
            "`tctl sign {set}` names a set with no binaries in SIGNABLE_BINARIES"
        );
        from_cli.push(set);
    }

    let mut declared = declared_sets();
    from_cli.sort_unstable();
    declared.sort_unstable();
    assert_eq!(
        from_cli, declared,
        "every SIGNABLE_BINARIES set needs a SignTargetArg variant and vice versa"
    );
}

/// Why: The tip names the valid `tctl sign` targets in prose, and prose goes
/// stale — it still said `<trusty-search|trusty-mpm>` long after #4277 shipped
/// `trusty-agents`. Checking it against the table turns a hand-maintained
/// string into one that cannot silently fall behind.
///
/// This parses the `<a|b|c>` group rather than asking whether the tip
/// `contains` each set name. A substring check passes spuriously on any
/// prefix-shaped name — a future `trusty-mem` set would be "found" inside the
/// existing `trusty-memory` — and it is blind in the other direction, saying
/// nothing when the tip advertises a target that is not a set at all.
///
/// What: Extracts the `<…>` group, splits it on `|`, and asserts the
/// advertised targets are exactly the sets declared in `SIGNABLE_BINARIES`.
/// Test: This is the test.
#[test]
fn signing_persistence_tip_names_every_signable_set() {
    let tip = signing_persistence_tip();
    let open = tip
        .find('<')
        .expect("the tip must list its targets in a `<a|b>` group");
    let close = open
        + tip[open..]
            .find('>')
            .expect("the tip's `<` must be closed by a `>`");

    let mut advertised: Vec<&str> = tip[open + 1..close].split('|').collect();
    let mut declared = declared_sets();
    advertised.sort_unstable();
    declared.sort_unstable();
    assert_eq!(
        advertised, declared,
        "signing_persistence_tip advertises {advertised:?} but SIGNABLE_BINARIES declares \
         {declared:?}: {tip}"
    );
}

/// Why: The PM decision (PR #2657 review HIGH) is a precise per-set,
/// per-context split — pin the truth table so a future edit cannot
/// silently flip either preserved pre-PR behavior. #4277 added `AGENTS_SET`
/// to the always-hardened side (no dylib-loading concern, same as MPM_SET).
/// What: explicit=true is always hardened (all sets); explicit=false is
/// hardened only for MPM_SET and AGENTS_SET. `MEMORY_SET` sits on the
/// SEARCH_SET side (owner ruling 2026-08-06): `trusty-memory` links
/// `ort`/`fastembed` via `trusty-common`'s `memory-core`, so it carries the
/// same unverified ONNX-dylib-under-library-validation exposure that keeps
/// trusty-search off automatic Hardened Runtime.
/// Test: This is the test.
#[test]
fn hardened_runtime_policy() {
    assert!(use_hardened_runtime(SEARCH_SET, true));
    assert!(use_hardened_runtime(MPM_SET, true));
    assert!(use_hardened_runtime(AGENTS_SET, true));
    assert!(use_hardened_runtime(MEMORY_SET, true));
    assert!(use_hardened_runtime(ANALYZE_SET, true));
    assert!(!use_hardened_runtime(SEARCH_SET, false));
    assert!(use_hardened_runtime(MPM_SET, false));
    assert!(use_hardened_runtime(AGENTS_SET, false));
    assert!(!use_hardened_runtime(MEMORY_SET, false));
    assert!(!use_hardened_runtime(ANALYZE_SET, false));
}

/// Why (owner ruling 2026-08-06): `trusty-analyze` produces exactly ONE
/// binary. The crate's only other target is the `trusty_analyze` library, so
/// there is no bundled sibling to forget the way `tm` was forgotten in #2721 —
/// this pins that the set stays a single entry rather than silently acquiring
/// a second one.
/// What: Asserts `ANALYZE_SET` resolves to `["trusty-analyze"]` with the
/// canonical `com.trusty.<binary>` identifier.
/// Test: This is the test.
#[test]
fn binaries_for_set_covers_analyze() {
    assert_eq!(binaries_for_set(ANALYZE_SET), vec!["trusty-analyze"]);
    assert_eq!(
        codesign_identifier("trusty-analyze"),
        "com.trusty.trusty-analyze"
    );
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

/// Why (#3846): a first install has no prior FDA entry to remove — the
/// fresh-install variant must NOT tell the operator to remove anything, only
/// to grant FDA once, and must stay terse (≤3 lines) with a pointer to the
/// full doc instead of inlining every step.
/// What: Calls `fda_guidance` with `existed_before = false` and checks the
/// output has no remove/re-add language, references the binary path, and
/// points at the doc anchor.
/// Test: This is the test.
#[test]
fn fda_guidance_fresh_install_has_no_remove_step() {
    let guidance = fda_guidance("/usr/local/bin/trusty-search", false);
    assert!(
        !guidance.to_lowercase().contains("remove"),
        "fresh-install guidance must not mention removing a prior entry: {guidance}"
    );
    assert!(
        guidance.contains("/usr/local/bin/trusty-search"),
        "binary path not in guidance"
    );
    assert!(
        guidance.contains("Full Disk Access"),
        "FDA label not in guidance"
    );
    assert!(
        guidance.contains("docs/reference/release-workflow.md"),
        "must point at the full-detail doc"
    );
    assert!(
        guidance.lines().count() <= 3,
        "fresh-install guidance must stay terse (<=3 lines): {guidance}"
    );
}

/// Why (#3846): replacing an existing binary DOES invalidate its FDA
/// grant (cdhash changed), so the reinstall variant must retain the
/// remove-then-re-add + daemon-restart guidance the fresh variant omits.
/// What: Calls `fda_guidance` with `existed_before = true` and checks for
/// remove/re-add language, the binary path, and terseness.
/// Test: This is the test.
#[test]
fn fda_guidance_reinstall_has_remove_and_readd() {
    let guidance = fda_guidance("/usr/local/bin/trusty-search", true);
    assert!(
        guidance.to_lowercase().contains("re-grant") || guidance.to_lowercase().contains("remove"),
        "reinstall guidance must mention re-granting/removing the stale entry: {guidance}"
    );
    assert!(
        guidance.contains("/usr/local/bin/trusty-search"),
        "binary path not in guidance"
    );
    assert!(
        guidance.contains("docs/reference/release-workflow.md"),
        "must point at the full-detail doc"
    );
    assert!(
        guidance.lines().count() <= 3,
        "reinstall guidance must stay terse (<=3 lines): {guidance}"
    );
}

/// Why (#3846): the preamble must not claim `cargo install` specifically —
/// this install path is prebuilt-tarball-based via `tctl`, and the guidance
/// text must be installation-method-agnostic.
/// What: Asserts neither guidance variant mentions `cargo install`.
/// Test: This is the test.
#[test]
fn fda_guidance_is_install_method_agnostic() {
    for existed_before in [false, true] {
        let guidance = fda_guidance("/usr/local/bin/trusty-search", existed_before);
        assert!(
            !guidance.contains("cargo install"),
            "guidance must not name a specific install method: {guidance}"
        );
    }
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
    // but safe to call in Rust 1.94 (not yet stabilized as unsafe).
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
/// found. Covers both malformed shapes: zero quotes (takes the `let-else
/// continue` path when `find('"')` fails) and exactly one quote (takes the
/// `last > first` false branch, since `find` and `rfind` return the same
/// index) — code-critic review on PR #2963 flagged the single-quote branch as
/// untested.
/// What: Feeds a zero-quote line, a one-quote (truncated) line, then a
/// well-formed line; asserts the well-formed identity is still returned.
/// Test: This is the test.
#[test]
fn parse_developer_id_identity_skips_malformed_line() {
    let fixture = "1) F03283D9CB41F7F084FFB01636A9AED54C8FB362 Developer ID Application: no quotes here\n\
         2) B4C5D6E7F8A9B4C5D6E7F8A9B4C5D6E7F8A9B4C5 \"Developer ID Application: truncated\n\
         3) A1B2C3D4E5F6A1B2C3D4E5F6A1B2C3D4E5F6A1B2 \"Developer ID Application: Bob Matsuoka (4JH68XUHC5)\"\n";
    let identity = parse_developer_id_identity(fixture).expect("must find the well-formed line");
    assert_eq!(
        identity,
        "Developer ID Application: Bob Matsuoka (4JH68XUHC5)"
    );
}

/// Why (#3846): the Developer ID / `TRUSTY_SIGN_IDENTITY` tip moved OUT of
/// the per-component guidance into [`signing_persistence_tip`] so it prints
/// once per run, not once per component — `fda_guidance` must no longer
/// embed it.
/// What: Asserts neither variant of `fda_guidance` mentions Developer ID,
/// while `signing_persistence_tip` itself does.
/// Test: This is the test.
#[test]
fn fda_guidance_no_longer_embeds_signing_tip() {
    for existed_before in [false, true] {
        let g = fda_guidance("/some/path/trusty-search", existed_before);
        assert!(
            !g.contains("Developer ID"),
            "the once-per-run signing tip must not be embedded per-component: {g}"
        );
    }
}

/// Why: The App-Data TCC guidance must reference the binary path and the
/// actual macOS prompt wording so the operator recognises it, in BOTH the
/// fresh-install and reinstall variants, and must stay terse and
/// method-agnostic (no "cargo install") like [`fda_guidance`].
/// What: Calls `app_data_guidance` with both `existed_before` values and
/// checks the output.
/// Test: This is the test.
#[test]
fn app_data_guidance_fresh_and_reinstall_variants() {
    for existed_before in [false, true] {
        let g = app_data_guidance("/usr/local/bin/trusty-mpm", existed_before);
        assert!(
            g.contains("access data from other apps"),
            "TCC prompt wording missing: {g}"
        );
        assert!(
            g.contains("/usr/local/bin/trusty-mpm"),
            "binary path missing: {g}"
        );
        assert!(
            !g.contains("Developer ID"),
            "signing tip must not be embedded per-component: {g}"
        );
        assert!(
            !g.contains("cargo install"),
            "guidance must not name a specific install method: {g}"
        );
        assert!(
            g.contains("docs/reference/release-workflow.md"),
            "must point at the full-detail doc: {g}"
        );
        assert!(
            g.lines().count() <= 3,
            "guidance must stay terse (<=3 lines): {g}"
        );
    }
}

/// Why (#3846): the fresh-install variant additionally must not carry any
/// reinstall-specific wording ("reinstalling replaced").
/// What: Asserts the `existed_before = false` variant has no reinstall
/// language.
/// Test: This is the test.
#[test]
fn app_data_guidance_fresh_install_has_no_reinstall_wording() {
    let g = app_data_guidance("/usr/local/bin/trusty-mpm", false);
    assert!(
        !g.to_lowercase().contains("reinstall"),
        "fresh-install guidance must not mention reinstalling: {g}"
    );
}

/// Why (#3846): the signing tip must exist exactly once, be found via
/// [`signing_persistence_tip`], and mention both the env-var override and the
/// `tctl sign` invocation so it stands alone as a complete pointer.
/// What: Asserts the returned string mentions `TRUSTY_SIGN_IDENTITY` and
/// `tctl sign`.
/// Test: This is the test.
#[test]
fn signing_persistence_tip_mentions_sign_identity_and_tctl_sign() {
    let tip = signing_persistence_tip();
    assert!(tip.contains("TRUSTY_SIGN_IDENTITY"));
    assert!(tip.contains("tctl sign"));
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
