//! Tests for keeping a gh token in the keyring, never in `hosts.yml` (#10037).
//!
//! Why: D6e forbids a plaintext token in a tm `hosts.yml`; every failure arm
//! must leave the file without a new token and never half-written.
//! What: temp dirs and [`TableKeyring`], an in-memory keyring that also
//! answers `gh auth token` lookups the way the machine-global keyring does.
//! No test touches a real keyring, `gh` or the network. Every token is a
//! `tok-` fake.
//! Test: this file IS the test module.

use std::cell::RefCell;
use std::path::Path;

use super::*;
use crate::core::gh_account_proof::GhTokenProbe;

/// An in-memory gh keyring: `(host, login) → token`, or a refusal.
///
/// What: `store` records the entry unless built with [`TableKeyring::failing`];
/// as a [`GhTokenProbe`] it answers any dir, since gh's keyring is
/// machine-global, not per config dir.
#[derive(Default)]
pub(crate) struct TableKeyring {
    entries: RefCell<Vec<(String, String, String)>>,
    fail: Option<String>,
}

impl TableKeyring {
    /// A keyring whose every write is refused with `reason`.
    pub(crate) fn failing(reason: &str) -> Self {
        Self {
            fail: Some(reason.to_string()),
            ..Self::default()
        }
    }

    /// The token stored for `login` on `host`, if any.
    pub(crate) fn token(&self, host: &str, login: &str) -> Option<String> {
        self.entries
            .borrow()
            .iter()
            .find(|(h, l, _)| h == host && l.eq_ignore_ascii_case(login))
            .map(|(_, _, t)| t.clone())
    }
}

impl GhKeyringWriter for TableKeyring {
    fn store(&self, host: &str, login: &str, token: &str) -> Result<(), String> {
        if let Some(reason) = &self.fail {
            return Err(reason.clone());
        }
        let mut entries = self.entries.borrow_mut();
        entries.retain(|(h, l, _)| !(h == host && l == login));
        entries.push((host.to_string(), login.to_string(), token.to_string()));
        Ok(())
    }
}

impl GhTokenProbe for TableKeyring {
    fn token(&self, _dir: &Path, host: &str, login: &str) -> Result<String, String> {
        TableKeyring::token(self, host, login).ok_or_else(|| "no keyring entry".to_string())
    }
}

/// A legacy #8914 `hosts.yml`: a github.com token at both levels, plus a GHES
/// host whose token tm has not migrated.
const LEGACY: &str = "\
github.com:
    users:
        octo-pinned:
            oauth_token: tok-legacy
    user: octo-pinned
    oauth_token: tok-legacy
    git_protocol: https
ghe.example.com:
    users:
        octo-pinned:
            oauth_token: tok-ghes
    user: octo-pinned
";

/// 🔴 #10037: the token reaches the keyring; a fresh `hosts.yml` names the
/// login and holds no token.
#[test]
fn a_stored_token_reaches_the_keyring_and_never_hosts_yml() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let keyring = TableKeyring::default();
    store_in_keyring(tmp.path(), "github.com", "octo-new", "tok-in", &keyring)
        .expect("a working keyring stores the token");
    assert_eq!(
        keyring.token("github.com", "octo-new").as_deref(),
        Some("tok-in")
    );
    let text = std::fs::read_to_string(tmp.path().join("hosts.yml")).expect("hosts.yml");
    assert!(!text.contains("tok-"), "hosts.yml holds a token: {text}");
    assert_eq!(hosts_yml_holds_token(&text), Ok(false));
    let doc: serde_yaml::Value = serde_yaml::from_str(&text).expect("yaml");
    assert_eq!(doc["github.com"]["user"].as_str(), Some("octo-new"));
    assert!(
        doc["github.com"]["users"].get("octo-new").is_some(),
        "{text}"
    );
}

/// 🔴 #10037 migration: a legacy github.com token moves to the keyring and
/// leaves the file; the GHES host's token and every other key are kept.
#[test]
fn a_legacy_hosts_yml_token_is_migrated_and_other_hosts_kept() {
    let tmp = tempfile::tempdir().expect("tempdir");
    std::fs::write(tmp.path().join("hosts.yml"), LEGACY).expect("hosts.yml");
    let keyring = TableKeyring::default();
    store_in_keyring(
        tmp.path(),
        "github.com",
        "octo-pinned",
        "tok-legacy",
        &keyring,
    )
    .expect("migration succeeds");
    assert_eq!(
        keyring.token("github.com", "octo-pinned").as_deref(),
        Some("tok-legacy")
    );
    let text = std::fs::read_to_string(tmp.path().join("hosts.yml")).expect("hosts.yml");
    assert!(!text.contains("tok-legacy"), "{text}");
    let doc: serde_yaml::Value = serde_yaml::from_str(&text).expect("yaml");
    assert_eq!(doc["github.com"]["git_protocol"].as_str(), Some("https"));
    assert_eq!(
        doc["ghe.example.com"]["users"]["octo-pinned"]["oauth_token"].as_str(),
        Some("tok-ghes"),
        "a host tm did not migrate keeps its token"
    );
}

/// 🔴 FAIL-OPEN CHECK (#10037): a keyring write failure returns `Err` and
/// never falls back to `hosts.yml` — a missing file stays missing, and a
/// legacy file is kept byte-for-byte rather than losing its working token.
#[test]
fn a_keyring_failure_writes_no_token_to_hosts_yml() {
    let keyring = TableKeyring::failing("the keychain is locked");
    let fresh = tempfile::tempdir().expect("tempdir");
    let err = store_in_keyring(fresh.path(), "github.com", "octo-new", "tok-in", &keyring)
        .expect_err("a refused keyring write must refuse");
    assert!(err.contains("locked") && !err.contains("tok-"), "{err}");
    assert!(
        !fresh.path().join("hosts.yml").exists(),
        "hosts.yml was written"
    );

    let legacy = tempfile::tempdir().expect("tempdir");
    std::fs::write(legacy.path().join("hosts.yml"), LEGACY).expect("hosts.yml");
    store_in_keyring(
        legacy.path(),
        "github.com",
        "octo-pinned",
        "tok-new",
        &keyring,
    )
    .expect_err("a refused keyring write must refuse");
    let kept = std::fs::read_to_string(legacy.path().join("hosts.yml")).expect("hosts.yml");
    assert_eq!(
        kept, LEGACY,
        "a failed migration must keep the file as it was"
    );
    assert!(
        !kept.contains("tok-new"),
        "the new token fell back to hosts.yml"
    );
}

/// 🔴 FAIL-OPEN CHECK (#10037): a `hosts.yml` rewrite that fails after the
/// keyring write returns `Err`, leaves the file exactly as it was, and leaves
/// no staging file behind. The target here is a symlink, which the atomic
/// writer refuses (#8236).
#[cfg(unix)]
#[test]
fn a_failed_hosts_yml_rewrite_leaves_the_file_whole() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let real = tmp.path().join("real-hosts.yml");
    std::fs::write(&real, LEGACY).expect("real");
    let dir = tmp.path().join("acct");
    std::fs::create_dir_all(&dir).expect("dir");
    std::os::unix::fs::symlink(&real, dir.join("hosts.yml")).expect("symlink");
    let keyring = TableKeyring::default();
    let err = store_in_keyring(&dir, "github.com", "octo-pinned", "tok-legacy", &keyring)
        .expect_err("a failed rewrite must refuse");
    assert!(
        err.contains("cannot write") && !err.contains("tok-"),
        "{err}"
    );
    assert_eq!(std::fs::read_to_string(&real).expect("real"), LEGACY);
    let names: Vec<_> = std::fs::read_dir(&dir)
        .expect("ls")
        .map(|e| e.expect("entry").file_name())
        .collect();
    assert_eq!(
        names,
        vec![std::ffi::OsString::from("hosts.yml")],
        "a staging file was left"
    );
}

/// FAIL-OPEN CHECK: a `hosts.yml` that is not a gh mapping is refused before
/// the keyring is written, and kept.
#[test]
fn a_hosts_yml_that_is_not_a_mapping_is_refused_before_the_keyring() {
    let tmp = tempfile::tempdir().expect("tempdir");
    std::fs::write(tmp.path().join("hosts.yml"), "- a list\n").expect("hosts.yml");
    let keyring = TableKeyring::default();
    let err = store_in_keyring(tmp.path(), "github.com", "octo-new", "tok-in", &keyring)
        .expect_err("a non-mapping hosts.yml must be refused");
    assert!(err.contains("not a gh hosts.yml mapping"), "{err}");
    assert!(
        keyring.token("github.com", "octo-new").is_none(),
        "the keyring was written"
    );
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("hosts.yml")).expect("hosts.yml"),
        "- a list\n"
    );
}

/// The token detector sees a host-level and a per-user token, not a bare login.
#[test]
fn holds_token_sees_host_and_user_tokens() {
    assert_eq!(hosts_yml_holds_token(LEGACY), Ok(true));
    assert_eq!(
        hosts_yml_holds_token("h:\n    users:\n        a:\n            oauth_token: tok-x\n"),
        Ok(true)
    );
    assert_eq!(
        hosts_yml_holds_token("github.com:\n    users:\n        a:\n    user: a\n"),
        Ok(false)
    );
    assert!(hosts_yml_holds_token(":: [").is_err());
    assert!(hosts_yml_holds_token("- tok-x\n").is_err(), "a list is not clean");
    assert_eq!(hosts_yml_holds_token(""), Ok(false));
}

/// The doctor scan names token-holding and unparsable files and skips clean
/// ones; a missing `gh-accounts` dir is clean.
#[test]
fn plaintext_token_files_names_holding_and_unreadable_files() {
    let tmp = tempfile::tempdir().expect("tempdir");
    assert_eq!(plaintext_token_files(tmp.path()), Ok((vec![], vec![])));
    let root = tmp.path().join("gh-accounts");
    for (login, text) in [
        ("legacy", LEGACY),
        (
            "clean",
            "github.com:\n    users:\n        clean:\n    user: clean\n",
        ),
        ("broken", ":: ["),
    ] {
        std::fs::create_dir_all(root.join(login)).expect("dir");
        std::fs::write(root.join(login).join("hosts.yml"), text).expect("hosts.yml");
    }
    std::fs::create_dir_all(root.join("empty")).expect("dir");
    let (holding, unreadable) = plaintext_token_files(tmp.path()).expect("scan");
    assert_eq!(holding, vec![root.join("legacy").join("hosts.yml")]);
    assert_eq!(unreadable, vec![root.join("broken").join("hosts.yml")]);
}

/// The production writer refuses in a unit-test build, so no test reaches the
/// real keyring.
#[test]
fn the_system_keyring_is_never_written_by_a_test() {
    let err = SystemGhKeyring
        .store("github.com", "octo-new", "tok-in")
        .expect_err("a test build never writes the keyring");
    assert!(err.contains("never writes the keyring"), "{err}");
}

/// FAIL-OPEN CHECK: a value that could end its quotes or a `security -i` line
/// is refused, and the refusal never quotes it.
#[test]
fn quoted_refuses_a_value_that_could_escape_its_quotes() {
    assert_eq!(quoted("token", "tok-ok_1").as_deref(), Ok("'tok-ok_1'"));
    for bad in ["", "tok 2", "tok'x", "tok\"x", "tok\\x", "tok\nx"] {
        let err = quoted("token", bad).expect_err(bad);
        assert!(bad.is_empty() || !err.contains(bad), "{err}");
    }
}
