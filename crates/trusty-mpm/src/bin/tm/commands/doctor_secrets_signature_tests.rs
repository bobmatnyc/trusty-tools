//! Tests for the `secrets_signature` doctor row (#11580).

use std::collections::VecDeque;
use std::ffi::OsStr;
use std::path::Path;

use super::*;

const NEW: &str = "1111111111111111111111111111111111111111";
const OLD: &str = "2222222222222222222222222222222222222222";
const BIN: &str = "/opt/tools/bin/trusty-secrets";

/// One dumped generic-password item, shaped like `security dump-keychain -a`.
fn item(service: &str, apps: &str) -> String {
    format!(
        "keychain: \"/var/keychains/login.keychain-db\"\n\
         version: 512\nclass: \"genp\"\nattributes:\n\
         \x20   \"acct\"<blob>=\"v1\"\n\
         \x20   \"svce\"<blob>=\"{service}\"\n\
         access: 1 entries\n    entry 0:\n        authorizations (1): decrypt\n{apps}"
    )
}

fn app(cdhash: &str) -> String {
    format!(
        "        applications (1):\n            0: {BIN} (status -67068)\n\
         \x20               requirement: cdhash H\"{cdhash}\"\n"
    )
}

/// A runner answering each call from a script, recording the programs run.
struct Scripted {
    answers: VecDeque<Run>,
    calls: Vec<String>,
}

impl Scripted {
    fn new(answers: Vec<Run>) -> Self {
        Self {
            answers: answers.into(),
            calls: Vec::new(),
        }
    }
}

impl ProbeRunner for Scripted {
    fn run(&mut self, program: &str, _args: &[&OsStr]) -> Run {
        self.calls.push(program.to_string());
        self.answers
            .pop_front()
            .unwrap_or_else(|| Run::Error("unscripted call".into()))
    }
}

fn ok(stdout: &str, stderr: &str) -> Run {
    Run::Exited {
        success: true,
        code: Some(0),
        stdout: stdout.into(),
        stderr: stderr.into(),
    }
}

fn codesign_ok(cdhash: &str) -> Run {
    ok(
        "",
        &format!("Executable={BIN}\nCDHash={cdhash}\nSignature=adhoc\n"),
    )
}

fn row(answers: Vec<Run>) -> DoctorCheck {
    signature_row_with(Some(Path::new(BIN)), &mut Scripted::new(answers))
}

#[test]
fn grade_items_trusts_the_current_cdhash_and_skips_foreign_items() {
    let dump = format!(
        "{}{}",
        item("trusty-secrets.vault-blob", &app(NEW)),
        item("com.example.other", "")
    );
    let got = grade_items(&dump, NEW, Path::new(BIN));
    assert_eq!(
        got,
        vec![("trusty-secrets.vault-blob".to_string(), ItemTrust::Trusted)]
    );
}

#[test]
fn grade_items_flags_an_older_cdhash_as_a_prompt() {
    let dump = item("trusty/acme/repo", &app(OLD));
    let got = grade_items(&dump, NEW, Path::new(BIN));
    assert_eq!(
        got,
        vec![("trusty/acme/repo".to_string(), ItemTrust::Prompt)]
    );
}

#[test]
fn grade_items_treats_an_any_app_acl_as_trusted() {
    let dump = item(
        "trusty-secrets.vault-blob",
        "        applications: <null>\n",
    );
    let got = grade_items(&dump, NEW, Path::new(BIN));
    assert_eq!(got[0].1, ItemTrust::Trusted);
}

// #11580: the prompt path — a reinstall re-signed the binary.
#[test]
fn signature_row_warns_on_drift_after_a_reinstall() {
    let dump = item("trusty-secrets.vault-blob", &app(OLD));
    let check = row(vec![ok(&dump, ""), codesign_ok(NEW)]);
    assert_eq!(check.status, CheckStatus::Warn, "{}", check.message);
    assert!(
        check.message.contains("signature drift"),
        "{}",
        check.message
    );
    assert!(
        check.message.contains("tm secrets unlock"),
        "{}",
        check.message
    );
}

// #11580: the denied path — no ACL entry trusts this binary at all.
#[test]
fn signature_row_fails_when_the_acl_denies_the_binary() {
    let apps = "        applications (1):\n            0: /usr/bin/other (OK)\n";
    let dump = item("trusty-secrets.vault-blob", apps);
    let check = row(vec![ok(&dump, ""), codesign_ok(NEW)]);
    assert_eq!(check.status, CheckStatus::Fail, "{}", check.message);
    assert!(check.message.contains("access denied"), "{}", check.message);
}

#[test]
fn signature_row_is_ok_when_every_item_trusts_the_current_cdhash() {
    let dump = item("trusty-secrets.vault-blob", &app(NEW));
    let check = row(vec![ok(&dump, ""), codesign_ok(NEW)]);
    assert_eq!(check.status, CheckStatus::Ok, "{}", check.message);
    assert!(check.message.contains("1 item(s)"), "{}", check.message);
}

#[test]
fn signature_row_is_ok_with_no_trusty_items() {
    let check = row(vec![
        ok(&item("com.example.other", ""), ""),
        codesign_ok(NEW),
    ]);
    assert_eq!(check.status, CheckStatus::Ok, "{}", check.message);
}

#[test]
fn signature_row_spawns_nothing_without_the_binary() {
    let mut runner = Scripted::new(vec![]);
    let check = signature_row_with(None, &mut runner);
    assert_eq!(check.status, CheckStatus::Ok);
    assert!(runner.calls.is_empty());
}

#[test]
fn signature_row_is_ok_without_a_macos_keychain() {
    let check = row(vec![Run::NotFound]);
    assert_eq!(check.status, CheckStatus::Ok, "{}", check.message);
}

// #11580 Fail-Open Check: a refused or hung probe is never OK.
#[test]
fn signature_row_is_unknown_when_a_probe_fails() {
    let refused = Run::Exited {
        success: false,
        code: Some(51),
        stdout: String::new(),
        stderr: "User interaction is not allowed.\n".into(),
    };
    for answers in [
        vec![refused],
        vec![Run::TimedOut],
        vec![ok("", ""), Run::Error("boom".into())],
        vec![ok("", ""), ok("", "code object is not signed at all\n")],
    ] {
        let check = row(answers);
        assert_eq!(check.status, CheckStatus::Unknown, "{}", check.message);
    }
}

#[test]
fn find_on_path_finds_a_file_and_ignores_a_missing_path() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("trusty-secrets"), "x").unwrap();
    let path_var = std::env::join_paths([dir.path()]).unwrap();
    assert_eq!(
        find_on_path("trusty-secrets", Some(&path_var)),
        Some(dir.path().join("trusty-secrets"))
    );
    assert_eq!(find_on_path("trusty-secrets", None), None);
}
