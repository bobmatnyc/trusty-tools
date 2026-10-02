//! Tests for [`super`] — the launchd-plist credential guard (#8236).
//!
//! Every fixture value here is an obvious fake. Nothing in this file is, or
//! has ever been, a live credential.

use super::*;

/// A fake OpenRouter-shaped key. Not a credential; the `FAKE` run is the point.
const FAKE_API_KEY: &str = "sk-or-v1-0000000000000000000000000000FAKE";

/// A fake Telegram bot-token-shaped value, for the no-vendor-prefix path.
const FAKE_BOT_TOKEN: &str = "1234567890:AAFAKEFAKEFAKEFAKEFAKEFAKEFAKEFAKEFAKE";

/// Assert a rendered error or finding never carries the fixture's value.
///
/// Why: every error arm here reports a file the scan could not read, and the
/// unreadable region is exactly the region that may hold the credential.
fn assert_value_never_echoed(text: &str) {
    assert!(
        !text.contains(FAKE_API_KEY),
        "output must name the key, never the value: {text}"
    );
    assert!(
        !text.contains("sk-or-"),
        "output must not carry a credential prefix: {text}"
    );
}

/// A generated unit carrying one credential entry among ordinary tunables.
fn plist_with_credential() -> String {
    format!(
        "<plist version=\"1.0\">\n<dict>\n  \
         <key>Label</key>\n  <string>com.trusty.mpm</string>\n  \
         <key>EnvironmentVariables</key>\n  <dict>\n    \
         <key>PATH</key>\n    <string>/usr/bin</string>\n    \
         <key>OPENROUTER_API_KEY</key>\n    <string>{FAKE_API_KEY}</string>\n    \
         <key>RUST_LOG</key>\n    <string>info</string>\n  \
         </dict>\n</dict>\n</plist>\n"
    )
}

/// Why: the two credentials #8236 found, plus the families around them, must
/// all read as credential keys or the guard protects nothing.
/// Test: this test.
#[test]
fn credential_keys_are_detected() {
    for key in [
        "OPENROUTER_API_KEY",
        "TELEGRAM_BOT_TOKEN",
        "ANTHROPIC_API_KEY",
        "GITHUB_TOKEN",
        "GH_TOKEN",
        "SLACK_BOT_TOKEN",
        "AWS_SECRET_ACCESS_KEY",
        "AWS_SESSION_TOKEN",
        "DB_PASSWORD",
        "SSH_PASSPHRASE",
        "TOKEN",
        "openrouter_api_key",
    ] {
        assert!(is_credential_env_key(key), "{key} must read as credential");
    }
}

/// Why: the plist keys this workspace's own units legitimately carry must
/// survive. A guard that strips `TRUSTY_MEMORY_LIMIT_MB` breaks the daemon it
/// was meant to protect, and a broken guard gets disabled.
/// Test: this test.
#[test]
fn tunable_keys_are_not_credentials() {
    for key in [
        "PATH",
        "RUST_LOG",
        "HF_HOME",
        "FASTEMBED_CACHE_DIR",
        "TRUSTY_MEMORY_LIMIT_MB",
        "TRUSTY_MAX_BATCH_SIZE",
        "TRUSTY_MPM_SUPERVISOR_INTERVAL",
        "MAX_TOKENS",
        "TRUSTY_TOKEN_BUDGET",
        "",
    ] {
        assert!(
            !is_credential_env_key(key),
            "{key} must not read as credential"
        );
    }
}

/// Why: a key naming a PATH to a credential, or an identifier beside one,
/// holds no secret. Stripping `TRUSTY_BUGREPORT_GH_APP_KEY_FILE` would break
/// bug reporting and protect nothing.
/// Test: this test.
#[test]
fn credential_reference_keys_are_not_credentials() {
    for key in [
        "TRUSTY_BUGREPORT_GH_APP_KEY_FILE",
        "AWS_ACCESS_KEY_ID",
        "OPENROUTER_API_KEY_PATH",
        "GITHUB_TOKEN_ENV",
        "SLACK_TOKEN_NAME",
    ] {
        assert!(
            !is_credential_env_key(key),
            "{key} points at a credential; it is not one"
        );
    }
}

/// Why: a `ProgramArguments` entry has no key, so the vendor prefix is the
/// only signal left.
/// Test: this test.
#[test]
fn credential_values_are_detected_by_prefix() {
    for value in [
        FAKE_API_KEY,
        "ghp_0000000000000000000000000000000FAKE",
        "xoxb-0000000000-0000000000-000000FAKE",
        "AKIA0000000000000FAKE",
    ] {
        assert!(
            looks_like_credential_value(value),
            "a vendor-prefixed value must read as a credential"
        );
    }
}

/// Why: the second credential in #8236 carried no vendor prefix at all, so a
/// prefix table alone would have missed exactly half the report.
/// Test: this test.
#[test]
fn credential_values_are_detected_for_telegram_shape() {
    assert!(looks_like_credential_value(FAKE_BOT_TOKEN));
}

/// Why: the argv check runs over every generated unit's `ProgramArguments`, so
/// a false positive there refuses a working install.
/// Test: this test.
#[test]
fn ordinary_arguments_are_not_credential_values() {
    for value in [
        "daemon",
        "--addr",
        "127.0.0.1:7880",
        "/Users/x/.cargo/bin/trusty-search",
        "sk-",
        "12:34",
        "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin",
    ] {
        assert!(
            !looks_like_credential_value(value),
            "{value} must not read as a credential"
        );
    }
}

/// Why: the renderer's guard must drop the credential and NOTHING else —
/// #4868 proved that silently dropping an unanticipated tunable re-arms a
/// production incident.
/// Test: this test.
#[test]
fn strip_credential_env_removes_only_the_credential_pair() {
    let mut pairs = vec![
        ("PATH".to_string(), "/usr/bin".to_string()),
        ("OPENROUTER_API_KEY".to_string(), FAKE_API_KEY.to_string()),
        ("RUST_LOG".to_string(), "info".to_string()),
        ("TELEGRAM_BOT_TOKEN".to_string(), FAKE_BOT_TOKEN.to_string()),
    ];
    let removed = strip_credential_env(&mut pairs);
    assert_eq!(removed, vec!["OPENROUTER_API_KEY", "TELEGRAM_BOT_TOKEN"]);
    assert_eq!(
        pairs,
        vec![
            ("PATH".to_string(), "/usr/bin".to_string()),
            ("RUST_LOG".to_string(), "info".to_string()),
        ]
    );
    for key in &removed {
        assert!(
            !key.contains("FAKE"),
            "a finding must name the key, never the value"
        );
    }
}

/// Why: the guard runs on every render, so the clean path has to be free.
/// Test: this test.
#[test]
fn strip_credential_env_is_a_noop_when_clean() {
    let mut pairs = vec![("PATH".to_string(), "/usr/bin".to_string())];
    assert!(strip_credential_env(&mut pairs).is_empty());
    assert_eq!(pairs.len(), 1);
}

/// Why: the read-only half `tm doctor` calls must name the key it found.
/// Test: this test.
#[test]
fn plist_credential_env_keys_names_the_key() {
    let keys = plist_credential_env_keys(&plist_with_credential()).expect("parses");
    assert_eq!(keys, vec!["OPENROUTER_API_KEY"]);
}

/// Why: remediation rewrites a live plist. Dropping a neighbouring tunable, or
/// leaving the value behind, both fail the operator differently.
/// Test: this test.
#[test]
fn scrub_removes_the_credential_entry_and_keeps_the_rest() {
    let scrubbed = scrub_plist_credential_env(&plist_with_credential()).expect("parses");
    assert_eq!(scrubbed.keys, vec!["OPENROUTER_API_KEY"]);
    assert!(
        !scrubbed.xml.contains(FAKE_API_KEY),
        "the value must be gone"
    );
    assert!(!scrubbed.xml.contains("OPENROUTER_API_KEY"));
    assert!(scrubbed.xml.contains("<key>PATH</key>"));
    assert!(scrubbed.xml.contains("<key>RUST_LOG</key>"));
    assert!(scrubbed.xml.contains("<string>info</string>"));
    assert!(
        !scrubbed.xml.contains("\n\n"),
        "the deletion must not leave a blank line: {}",
        scrubbed.xml
    );
}

/// Why: `--fix` must not rewrite a file it has nothing to change in.
/// Test: this test.
#[test]
fn scrub_is_byte_identical_when_clean() {
    let xml = "<plist version=\"1.0\">\n<dict>\n  <key>EnvironmentVariables</key>\n  \
               <dict>\n    <key>PATH</key>\n    <string>/usr/bin</string>\n  \
               </dict>\n</dict>\n</plist>\n";
    let scrubbed = scrub_plist_credential_env(xml).expect("parses");
    assert!(scrubbed.keys.is_empty());
    assert_eq!(scrubbed.xml, xml);
}

/// Why: a `<key>TOKEN</key>` in `KeepAlive` or any other dict is not an
/// environment variable, and deleting it would corrupt the unit.
/// Test: this test.
#[test]
fn scrub_ignores_keys_outside_the_environment_dict() {
    let xml = "<plist version=\"1.0\">\n<dict>\n  <key>KeepAlive</key>\n  \
               <dict>\n    <key>SuccessfulExit</key>\n    <false/>\n  </dict>\n  \
               <key>Password</key>\n  <string>not-an-env-var</string>\n</dict>\n</plist>\n";
    let scrubbed = scrub_plist_credential_env(xml).expect("parses");
    assert!(scrubbed.keys.is_empty());
    assert_eq!(scrubbed.xml, xml);
}

/// Why: a hand-edited plist can carry a non-string value. Deleting the key
/// without its value would leave the dict malformed.
/// Test: this test.
#[test]
fn scrub_removes_a_self_closing_value() {
    let xml = "<key>EnvironmentVariables</key>\n<dict>\n<key>API_KEY</key>\n<true/>\n\
               <key>PATH</key>\n<string>/usr/bin</string>\n</dict>\n";
    let scrubbed = scrub_plist_credential_env(xml).expect("parses");
    assert_eq!(scrubbed.keys, vec!["API_KEY"]);
    assert!(!scrubbed.xml.contains("<true/>"));
    assert!(scrubbed.xml.contains("<key>PATH</key>"));
}

/// Why: "I could not read the dict" must never render as "this host is clean".
/// Test: this test.
#[test]
fn scrub_reports_an_unterminated_environment_dict() {
    let xml =
        "<key>EnvironmentVariables</key>\n<dict>\n<key>PATH</key>\n<string>/usr/bin</string>\n";
    let err = scrub_plist_credential_env(xml).expect_err("an unterminated dict is an error");
    assert!(err.reason.contains("unterminated"), "was: {}", err.reason);
    assert!(
        !format!("{err}").contains("/usr/bin"),
        "an error must not echo plist content"
    );
}

/// Why: the same false-negative risk, one element down — a key whose value
/// element is missing means the pairing could not be resolved.
/// Test: this test.
#[test]
fn scrub_reports_a_key_with_no_value_element() {
    let xml = "<key>EnvironmentVariables</key>\n<dict>\n<key>OPENROUTER_API_KEY</key>\n</dict>\n";
    let err = scrub_plist_credential_env(xml).expect_err("a dangling key is an error");
    assert!(
        err.reason.contains("no value element"),
        "was: {}",
        err.reason
    );
}

/// Why: the third false-negative arm — `EnvironmentVariables` announced and no
/// `<dict>` anywhere after it. Returning "clean" here would pass a document
/// whose environment block the scan never located.
/// Test: this test.
#[test]
fn scrub_reports_environment_variables_with_no_dict() {
    let xml = "<plist version=\"1.0\">\n<key>EnvironmentVariables</key>\n  \
               <string>truncated</string>\n</plist>\n";
    let err = scrub_plist_credential_env(xml).expect_err("a dictless environment is an error");
    assert!(
        err.reason.contains("not followed by a <dict>"),
        "was: {}",
        err.reason
    );
}

/// Why: a `<key>` that never closes means the scan cannot even read the name
/// of the variable, let alone decide whether it is a credential.
/// Test: this test.
#[test]
fn scrub_reports_an_unterminated_key() {
    let xml = "<key>EnvironmentVariables</key>\n<dict>\n<key>OPENROUTER_API_KEY\n\
               <string>x</string>\n</dict>\n";
    let err = scrub_plist_credential_env(xml).expect_err("an unterminated key is an error");
    assert!(
        err.reason.contains("<key> is unterminated"),
        "was: {}",
        err.reason
    );
    assert_value_never_echoed(&format!("{err}"));
}

/// Why: the last arm of the pairing walk — a value element that opens and
/// never closes inside the dict. The credential is in that unterminated
/// element, so a silent pass is the exact false negative this module exists to
/// prevent.
/// Test: this test.
#[test]
fn scrub_reports_a_value_element_with_no_closing_tag() {
    let xml = format!(
        "<key>EnvironmentVariables</key>\n<dict>\n<key>OPENROUTER_API_KEY</key>\n\
         <string>{FAKE_API_KEY}\n</dict>\n"
    );
    let err = scrub_plist_credential_env(&xml).expect_err("an unterminated value is an error");
    assert!(
        err.reason.contains("no value element"),
        "was: {}",
        err.reason
    );
    assert_value_never_echoed(&format!("{err}"));
}

/// Why: a unit with no environment at all is the common case and must parse.
/// Test: this test.
#[test]
fn scrub_accepts_a_plist_with_no_environment_dict() {
    let xml = "<plist version=\"1.0\">\n<dict>\n  <key>Label</key>\n  \
               <string>com.trusty.mpm</string>\n</dict>\n</plist>\n";
    let scrubbed = scrub_plist_credential_env(xml).expect("parses");
    assert!(scrubbed.keys.is_empty());
    assert_eq!(scrubbed.xml, xml);
}

/// Why (#8236 item 3): detection is registry-driven FIRST. A name the resolver
/// can route must never depend on the suffix heuristic agreeing with it.
/// Test: this test.
#[test]
fn every_registry_name_is_detected_as_a_credential() {
    for (provider, var) in crate::credential_registry::REGISTRY {
        assert!(
            is_credential_env_key(var),
            "registered credential {var} (provider {provider}) is not detected"
        );
    }
}

/// Why (#8236 item 4): a binary plist read as text finds no `<key>` and the
/// scanner would call the host CLEAN. The magic is what stops that.
/// Test: this test.
#[test]
fn a_binary_plist_is_detected_by_magic() {
    let mut bytes = b"bplist00".to_vec();
    bytes.extend_from_slice(&[0xd1, 0x01, 0x02, 0x5f]);
    assert!(is_binary_plist(&bytes));
}

/// Why: the XML path must not be diverted into the binary arm.
/// Test: this test.
#[test]
fn an_xml_plist_is_not_mistaken_for_a_binary_one() {
    assert!(!is_binary_plist(plist_with_credential().as_bytes()));
    assert!(!is_binary_plist(b""));
}

/// Why (#8236 item 2): `--fix` migrates the value, so the parser now hands one
/// back. Item 9 requires that value be unprintable everywhere it travels.
/// Test: this test.
#[test]
fn entries_carry_the_value_only_inside_the_wrapper() {
    let xml = plist_with_credential();
    let entries = credential_entries(&xml).expect("parses");

    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].key, "OPENROUTER_API_KEY");
    assert_eq!(entries[0].value.expose(), FAKE_API_KEY);
    assert_value_never_echoed(&format!("{:?}", entries[0]));
}

/// Why (#8236 item 9): a `{:?}` of anything holding a value is a log line away
/// from disclosure.
/// Test: this test.
#[test]
fn plist_secret_never_renders_its_value() {
    let secret = PlistSecret::new(FAKE_API_KEY);
    assert_value_never_echoed(&format!("{secret:?} {secret}"));
    assert_eq!(format!("{secret}"), "<redacted>");
    assert!(!secret.is_empty());
}

/// Why: a clean unit must produce no entries at all, so `--fix` plans nothing.
/// Test: this test.
#[test]
fn entries_are_empty_for_a_clean_plist() {
    let xml = "<plist version=\"1.0\">\n<dict>\n  <key>EnvironmentVariables</key>\n  \
               <dict>\n    <key>PATH</key>\n    <string>/usr/bin</string>\n  \
               </dict>\n</dict>\n</plist>\n";
    assert!(credential_entries(xml).expect("parses").is_empty());
}

/// Why (#8236 item 2): a key whose value was NOT confirmed into the store must
/// survive the rewrite — stripping it would disable the feature it configures.
/// Test: this test.
#[test]
fn scrub_plist_keys_removes_only_the_named_key() {
    let xml = format!(
        "<plist version=\"1.0\">\n<dict>\n  \
         <key>EnvironmentVariables</key>\n  <dict>\n    \
         <key>OPENROUTER_API_KEY</key>\n    <string>{FAKE_API_KEY}</string>\n    \
         <key>AWS_SECRET_ACCESS_KEY</key>\n    <string>unmapped-fake</string>\n  \
         </dict>\n</dict>\n</plist>\n"
    );

    let scrubbed = scrub_plist_keys(&xml, &["OPENROUTER_API_KEY".to_string()]).expect("parses");

    assert_eq!(scrubbed.keys, vec!["OPENROUTER_API_KEY".to_string()]);
    assert!(!scrubbed.xml.contains("OPENROUTER_API_KEY"));
    assert!(
        scrubbed.xml.contains("AWS_SECRET_ACCESS_KEY"),
        "an unmigrated key was stripped: {}",
        scrubbed.xml
    );
    assert_value_never_echoed(&scrubbed.xml);
}

/// Why: the no-op case must not rewrite a single byte, so a failed migration
/// leaves the file provably untouched.
/// Test: this test.
#[test]
fn scrub_plist_keys_with_no_names_is_byte_identical() {
    let xml = plist_with_credential();
    let scrubbed = scrub_plist_keys(&xml, &[]).expect("parses");
    assert!(scrubbed.keys.is_empty());
    assert_eq!(scrubbed.xml, xml);
}
