//! Tests for the launchd-unit half of `service_unit.rs`.
//!
//! Why: kept in its own sibling file so `service_unit.rs` stays under the
//! 500-SLOC production cap (`scripts/check_line_cap.sh`) — see the split note
//! in that file's module doc.
//!
//! Gated to macOS for the same reason the code under test is — see the module
//! docs. The logic is pure string handling, so these would run anywhere; the
//! items they exercise simply do not exist on other targets.

use super::*;

fn sample_plist() -> &'static str {
    "<plist version=\"1.0\">\n<dict>\n\
     <key>Label</key>\n<string>com.trusty.trusty-search</string>\n\
     <key>ProgramArguments</key>\n<array>\n\
     <string>/usr/local/bin/trusty-search</string>\n\
     <string>start</string>\n<string>--foreground</string>\n\
     <string>--no-auto-discover</string>\n</array>\n\
     <key>SoftResourceLimits</key>\n<dict>\n\
     <key>NumberOfFiles</key>\n<integer>8192</integer>\n</dict>\n\
     <key>EnvironmentVariables</key>\n<dict>\n\
     <key>HF_HOME</key>\n<string>/Users/x/.cache/huggingface</string>\n\
     <key>TRUSTY_DEVICE</key>\n<string>cpu</string>\n\
     <key>TRUSTY_NO_AUTO_DISCOVER</key>\n<string>1</string>\n\
     </dict>\n</dict>\n</plist>\n"
}

/// Why: preservation is only possible if we can actually read the unit on
/// disk, including past an intervening nested `<dict>` (the fd-limit dicts
/// sit between `ProgramArguments` and `EnvironmentVariables`).
/// What: parses a representative generated plist and asserts both sections.
/// Test: this function.
#[test]
fn parse_installed_unit_reads_args_and_env() {
    let unit = parse_installed_unit(sample_plist());
    assert_eq!(
        unit.args,
        vec![
            "/usr/local/bin/trusty-search",
            "start",
            "--foreground",
            "--no-auto-discover"
        ]
    );
    assert_eq!(unit.env_value("TRUSTY_DEVICE"), Some("cpu"));
    assert_eq!(unit.env_value("TRUSTY_NO_AUTO_DISCOVER"), Some("1"));
    assert_eq!(unit.env_value("NOPE"), None);
}

/// Why: a hand-made or truncated plist must degrade to "no operator
/// intent", never panic or index out of bounds mid-install.
/// What: parses documents with each section missing and a malformed one.
/// Test: this function.
#[test]
fn parse_installed_unit_tolerates_missing_sections() {
    assert_eq!(parse_installed_unit(""), InstalledUnit::default());
    assert_eq!(
        parse_installed_unit("<dict><key>Label</key><string>x</string></dict>"),
        InstalledUnit::default()
    );
    let truncated = "<key>ProgramArguments</key><array><string>start";
    assert!(parse_installed_unit(truncated).args.is_empty());
}

/// Why: `render_plist` XML-escapes every value, so a round trip must
/// unescape or a path containing `&` comes back corrupted.
/// What: asserts all five entity forms are reversed.
/// Test: this function.
#[test]
fn parse_installed_unit_unescapes_xml() {
    let xml = "<key>EnvironmentVariables</key><dict>\
               <key>TRUSTY_DEVICE</key><string>a&amp;b&lt;c&gt;d&quot;e&apos;f</string>\
               </dict>";
    assert_eq!(
        parse_installed_unit(xml).env_value("TRUSTY_DEVICE"),
        Some("a&b<c>d\"e'f")
    );
}

/// Why: this is the representation `service install` now generates; failing
/// to recognise it would make the setting non-durable across two installs.
/// What: asserts detection of the bare flag and the `=value` form.
/// Test: this function.
#[test]
fn suppresses_auto_discover_via_arg() {
    let unit = InstalledUnit {
        args: vec!["start".into(), NO_AUTO_DISCOVER_ARG.into()],
        env: vec![],
        working_directory: None,
    };
    assert!(unit.suppresses_auto_discover());

    let unit = InstalledUnit {
        args: vec![format!("{NO_AUTO_DISCOVER_ARG}=1")],
        env: vec![],
        working_directory: None,
    };
    assert!(unit.suppresses_auto_discover());
}

/// Why: the legacy hand-made unit and the 2026-08-04 hand-edit expressed
/// the suppression as `TRUSTY_NO_AUTO_DISCOVER=1` in `EnvironmentVariables`
/// — exactly the value clap used to reject. Regeneration must recognise it.
/// What: asserts env-based detection for both `1` and `true`.
/// Test: this function.
#[test]
fn suppresses_auto_discover_via_env() {
    for raw in ["1", "true"] {
        let unit = InstalledUnit {
            args: vec!["start".into()],
            env: vec![(NO_AUTO_DISCOVER_ENV.into(), raw.into())],
            working_directory: None,
        };
        assert!(
            unit.suppresses_auto_discover(),
            "env value {raw:?} must read as suppressed"
        );
    }
    assert!(parse_installed_unit(sample_plist()).suppresses_auto_discover());
}

/// Why: an operator who wrote an explicit falsey value meant "scan" —
/// reading mere presence as suppression would invert their intent.
/// What: asserts falsey arg and env spellings read as not suppressed.
/// Test: this function.
#[test]
fn suppresses_auto_discover_respects_explicit_false() {
    let unit = InstalledUnit {
        args: vec![format!("{NO_AUTO_DISCOVER_ARG}=false")],
        env: vec![],
        working_directory: None,
    };
    assert!(!unit.suppresses_auto_discover());

    let unit = InstalledUnit {
        args: vec![],
        env: vec![(NO_AUTO_DISCOVER_ENV.into(), "0".into())],
        working_directory: None,
    };
    assert!(!unit.suppresses_auto_discover());
    assert!(!InstalledUnit::default().suppresses_auto_discover());
}

/// Why: this is the #4823 defect itself — regenerating over a unit that
/// suppressed auto-discovery must not re-enable it.
/// What: no flags + a suppressing unit → `Suppress { preserved: true }`.
/// Test: this function.
#[test]
fn resolve_auto_discover_preserves_existing_suppression() {
    let existing = parse_installed_unit(sample_plist());
    assert_eq!(
        resolve_auto_discover(false, false, Some(&existing)),
        AutoDiscover::Suppress { preserved: true }
    );
}

/// Why: an operator must be able to express the setting on a fresh machine
/// where no unit exists yet.
/// What: `--no-auto-discover` with no installed unit → suppress, not
/// flagged as preserved (it came from this invocation).
/// Test: this function.
#[test]
fn resolve_auto_discover_explicit_request() {
    assert_eq!(
        resolve_auto_discover(true, false, None),
        AutoDiscover::Suppress { preserved: false }
    );
    let existing = parse_installed_unit(sample_plist());
    assert_eq!(
        resolve_auto_discover(true, false, Some(&existing)),
        AutoDiscover::Suppress { preserved: false }
    );
}

/// Why: preservation must be escapable, and turning a suppression off is
/// a capability change the caller has to be able to announce.
/// What: `--auto-discover` over a suppressing unit → `Enable { dropped }`.
/// Test: this function.
#[test]
fn resolve_auto_discover_explicit_reenable_reports_drop() {
    let existing = parse_installed_unit(sample_plist());
    assert_eq!(
        resolve_auto_discover(false, true, Some(&existing)),
        AutoDiscover::Enable { dropped: true }
    );
    assert_eq!(
        resolve_auto_discover(false, true, None),
        AutoDiscover::Enable { dropped: false }
    );
}

/// Why: the fix must not change behaviour for the overwhelmingly common
/// case — a plain install with auto-discovery left on.
/// What: no flags, no installed unit → `Enable { dropped: false }`.
/// Test: this function.
#[test]
fn resolve_auto_discover_defaults_to_enabled() {
    let decision = resolve_auto_discover(false, false, None);
    assert_eq!(decision, AutoDiscover::Enable { dropped: false });
    assert!(!decision.suppressed());
    assert!(resolve_auto_discover(true, false, None).suppressed());
}

/// Why: an operator exporting a tunable before `service install` must still
/// win over whatever the old unit said.
/// What: process env value beats the installed unit's value.
/// Test: this function.
#[test]
fn resolve_persisted_env_prefers_process_env() {
    let existing = parse_installed_unit(sample_plist());
    // `HF_HOME` is template-owned, so the caller passes it here exactly as
    // `launchd_env_pairs` does — a stale value must not outrank the freshly
    // resolved one (#4868).
    let pairs = resolve_persisted_env(
        |k| (k == "TRUSTY_DEVICE").then(|| "metal".to_string()),
        Some(&existing),
        &["HF_HOME"],
    );
    assert_eq!(
        pairs,
        vec![("TRUSTY_DEVICE".to_string(), "metal".to_string())]
    );
}

/// Why (#4823 generalised): `service install` normally runs from a shell
/// exporting nothing, which previously blanked every tunable the unit
/// carried — including the `TRUSTY_DEVICE=cpu` pin that keeps Apple Silicon
/// off CoreML.
/// What: with an empty process env, the installed unit's value survives.
/// Test: this function.
#[test]
fn resolve_persisted_env_carries_forward_installed_values() {
    let existing = parse_installed_unit(sample_plist());
    let pairs = resolve_persisted_env(|_| None, Some(&existing), &["HF_HOME"]);
    assert_eq!(
        pairs,
        vec![("TRUSTY_DEVICE".to_string(), "cpu".to_string())]
    );
    assert!(resolve_persisted_env(|_| None, None, &["HF_HOME"]).is_empty());
}

/// Why (#4868 review): an allowlist keeps losing the NEXT hand-set var. The
/// live unit carried five keys no allowlist named, including
/// `TRUSTY_WARMBOOT_INDEX_TIMEOUT_SECS` — the hand-patch from an incident
/// where a restart cost a 200k-chunk index. Once install started overwriting
/// the LIVE plist, dropping a key stopped being harmless and became data
/// loss.
/// What: a key the code has never heard of survives, and a template-owned
/// key is NOT carried forward so a stale value cannot outrank a fresh one.
/// Test: this function.
#[test]
fn resolve_persisted_env_carries_forward_unknown_keys() {
    let existing = parse_installed_unit(
        "<key>EnvironmentVariables</key><dict>\
         <key>HF_HOME</key><string>/stale/hf</string>\
         <key>TRUSTY_WARMBOOT_INDEX_TIMEOUT_SECS</key><string>60</string>\
         <key>SOME_KEY_NOBODY_ANTICIPATED</key><string>keepme</string>\
         </dict>",
    );
    let pairs = resolve_persisted_env(|_| None, Some(&existing), &["HF_HOME"]);
    let has = |k: &str, v: &str| pairs.iter().any(|(a, b)| a == k && b == v);

    assert!(
        has("TRUSTY_WARMBOOT_INDEX_TIMEOUT_SECS", "60"),
        "the incident hand-patch must survive regeneration, got {pairs:?}"
    );
    assert!(
        has("SOME_KEY_NOBODY_ANTICIPATED", "keepme"),
        "an unanticipated key must survive too, got {pairs:?}"
    );
    assert!(
        !pairs.iter().any(|(k, _)| k == "HF_HOME"),
        "a template-owned key must not be carried forward — the caller \
         recomputes it, got {pairs:?}"
    );
}

/// Why (#4868 review): the live unit sets a `WorkingDirectory` and the
/// generated template never emitted the key, so regeneration silently
/// changed the daemon's working directory once install began overwriting
/// the live plist.
/// What: the key is read from a top-level scalar, and its absence is `None`.
/// Test: this function.
#[test]
fn parse_installed_unit_reads_working_directory() {
    let unit = parse_installed_unit(
        "<key>WorkingDirectory</key>\n<string>/Users/x/work</string>\n\
         <key>EnvironmentVariables</key>\n<dict>\n</dict>\n",
    );
    assert_eq!(unit.working_directory.as_deref(), Some("/Users/x/work"));

    assert_eq!(
        parse_installed_unit(sample_plist()).working_directory,
        None,
        "a unit without the key must not invent one"
    );
}

/// Why: this is the trap in the #4823 comment. The installed unit carries
/// `TRUSTY_NO_AUTO_DISCOVER=1`; copying that into the regenerated plist is
/// what crashed the daemon on the next launchd restart. The suppression
/// travels as a `ProgramArguments` flag instead, so no value that a clap
/// parser could reject is ever written to `EnvironmentVariables`.
/// What: asserts the key is absent from the resolved pairs even when both
/// the process env and the installed unit supply it.
/// Test: this function.
#[test]
fn resolve_persisted_env_never_emits_no_auto_discover() {
    let existing = parse_installed_unit(sample_plist());
    let pairs = resolve_persisted_env(
        |k| (k == NO_AUTO_DISCOVER_ENV).then(|| "1".to_string()),
        Some(&existing),
        &[],
    );
    assert!(
        !pairs.iter().any(|(k, _)| k == NO_AUTO_DISCOVER_ENV),
        "{NO_AUTO_DISCOVER_ENV} must never be emitted into a generated \
         plist — a rejected value aborts daemon startup (#4823); got {pairs:?}"
    );
}
