//! Unit tests for the #8878 presence-key Keychain floor (`pm_guard_presence_key.rs`).

use super::*;

/// Writes naming the key item, in every spelling the floor folds.
pub(crate) const NAMED: &[&str] = &[
    "security add-generic-password -s trusty-mpm.presence -a grant-hmac-v1 -w 0123 -A",
    "security add-generic-password -U -s trusty-mpm.presence -a grant-hmac-v1 -w k",
    "security delete-generic-password -s trusty-mpm.presence",
    "security delete-generic-password -a grant-hmac-v1",
    "security set-generic-password-partition-list -s trusty-mpm.presence -S apple-tool: -k pw",
    "/usr/bin/security add-generic-password -s \"trusty-mpm\".presence -a x -w y",
    "SECURITY ADD-GENERIC-PASSWORD -s TRUSTY-MPM.PRESENCE -a x -w y",
    "security add-generic-password -s trusty-mpm.pres''ence -a x -w y",
    "security add-generic-password -s trusty-mpm\\.presence -a x -w y",
    "security -q add-generic-password -strusty-mpm.presence -agrant-hmac-v1 -wk",
    "sh -c 'security delete-generic-password -s trusty-mpm.presence'",
    "echo $(security delete-generic-password -s trusty-mpm.presence)",
    "echo trusty-mpm.presence | xargs security delete-generic-password -s",
    "python3 -c 'import subprocess; subprocess.run([\"security\", \
         \"delete-generic-password\", \"-s\", \"trusty-mpm.presence\"])'",
    "python3 -c 'import keyring; keyring.set_password(\"trusty-mpm.presence\", \
         \"grant-hmac-v1\", \"k\" * 32)'",
    "python3 -c 'import keyring; keyring.delete_password(\"trusty-mpm.presence\", \"x\")'",
    "swift -e 'SecItemDelete([kSecAttrService: \"trusty-mpm.presence\"] as CFDictionary)'",
];

/// `security` writes whose selector words are chosen at run time.
pub(crate) const RUN_TIME: &[&str] = &[
    "security add-generic-password -s \"$SVC\" -a x -w y",
    "security delete-generic-password -s trusty-mpm.$(echo presence)",
    "security delete-generic-password -s `cat svc`",
    "security add-generic-password $ARGS",
    "security delete-generic-password -a me ${REST}",
    "bash -c \"security delete-generic-password -s $S\"",
];

/// `security` writes with no literal `-s` or `-a`.
pub(crate) const NO_SELECTOR: &[&str] = &[
    "security delete-generic-password",
    "security delete-generic-password -l label",
    "security set-generic-password-partition-list -S apple: -k pw",
    "security add-generic-password -w x",
    "security delete-generic-password -As foo",
];

/// Other items, reads and mentions: these pass.
pub(crate) const OTHERS: &[&str] = &[
    "security add-generic-password -s gh:github.com -a masa -w \"$TOKEN\" -U",
    "security add-generic-password -s other -a me -X \"$HEX\"",
    "security delete-generic-password -s other-service -a me",
    "security find-generic-password -s trusty-mpm.presence >/dev/null 2>&1",
    "grep -rn trusty-mpm.presence docs/",
    "grep -rn delete-generic-password crates",
    "cargo test -p trusty-mpm --no-fail-fast presence",
    "tm presence local",
];

/// Assert every command is denied; a failure lists every row that passed.
fn assert_denied(commands: &[&str]) {
    let wrong: Vec<&str> = commands
        .iter()
        .copied()
        .filter(|c| !evaluate_presence_key_write(c).is_some_and(|r| r.contains("#8878")))
        .collect();
    assert!(
        wrong.is_empty(),
        "{} rows not denied: {wrong:#?}",
        wrong.len()
    );
}

/// Assert no command is denied; a failure lists every denied row.
fn assert_passed(commands: &[&str]) {
    let wrong: Vec<(&str, String)> = commands
        .iter()
        .filter_map(|c| evaluate_presence_key_write(c).map(|r| (*c, r)))
        .collect();
    assert!(wrong.is_empty(), "{} rows denied: {wrong:#?}", wrong.len());
}

/// #8878 critic HIGH: adding, deleting or re-permissioning the key item is
/// denied in every spelling the credential-print reading folds — quotes,
/// backslashes, case, glued options, wrappers, argv lists, API calls.
#[test]
fn every_spelling_naming_the_key_item_is_denied_8878() {
    assert_denied(NAMED);
}

/// A `security` write whose selector words the shell chooses at run time
/// can reach the item without naming it.
#[test]
fn a_run_time_word_in_a_security_write_is_denied_8878() {
    assert_denied(RUN_TIME);
}

/// A `security` write with no literal `-s` or `-a` matches or creates an
/// item the guard cannot tell from the key; a getopt cluster is not read
/// as a selector.
#[test]
fn a_security_write_with_no_literal_selector_is_denied_8878() {
    assert_denied(NO_SELECTOR);
}

/// Other items, reads and mentions pass: a password value from a variable,
/// a delete of another service, an existence check, a search.
#[test]
fn other_keychain_items_and_reads_pass_8878() {
    assert_passed(OTHERS);
}
