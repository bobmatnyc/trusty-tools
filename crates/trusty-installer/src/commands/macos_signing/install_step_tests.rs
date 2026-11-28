//! Tests for the install sign step (#11587).
//!
//! Every `codesign` here is a fake shell script; no test runs the real
//! `codesign` or `security`, or reads the Keychain (Architect KEYCHAIN HOLD).

use super::super::{binaries_for_set, codesign_identifier, set_of, BROWSER_SET};
use super::*;

/// The identity every signing test asks for (Team ID `ABCDE12345`).
const IDENTITY: &str = "Developer ID Application: Test Owner (ABCDE12345)";

/// `codesign -dvv` stderr for an ad-hoc, linker-signed binary (real shape).
const ADHOC: &str = "Identifier=trusty_x-bfc09c7c0fda3530\n\
    CodeDirectory v=20400 size=1 flags=0x20002(adhoc,linker-signed) hashes=1+0 location=embedded\n\
    Signature=adhoc\nTeamIdentifier=not set\n";

/// The rest of a Developer ID `codesign -dvv` summary for team `ABCDE12345`;
/// the fake prints the `Identifier=` line in front of it.
const DEVELOPER_ID: &str =
    "CodeDirectory v=20500 size=1 flags=0x10000(runtime) hashes=1+2 location=embedded\n\
    Authority=Developer ID Application: Test Owner (ABCDE12345)\n\
    Authority=Developer ID Certification Authority\nAuthority=Apple Root CA\n\
    TeamIdentifier=ABCDE12345\n";

/// A stateful fake `codesign` and its call log.
#[cfg(unix)]
struct Fake {
    _dir: tempfile::TempDir,
    codesign: std::path::PathBuf,
    log: std::path::PathBuf,
}

#[cfg(unix)]
impl Fake {
    /// Every argument line the fake was called with.
    fn calls(&self) -> Vec<String> {
        std::fs::read_to_string(&self.log)
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    /// The sign calls (`--force …`) only.
    fn sign_calls(&self) -> Vec<String> {
        self.calls()
            .into_iter()
            .filter(|c| c.starts_with("--force"))
            .collect()
    }
}

/// A fake `codesign`: a sign call writes the `--identifier` value to
/// `<path>.signed` (or fails when the path ends in `fail`); `-dvv` prints a
/// Developer ID summary with that identifier once the marker exists, else an
/// ad-hoc one; `--verify` passes. Every call is logged.
#[cfg(unix)]
fn fake(fail: &str) -> Fake {
    let dir = tempfile::tempdir().expect("tempdir");
    let (good, adhoc, log) = (
        dir.path().join("good.txt"),
        dir.path().join("adhoc.txt"),
        dir.path().join("calls.log"),
    );
    std::fs::write(&good, DEVELOPER_ID).expect("write fixture");
    std::fs::write(&adhoc, ADHOC).expect("write fixture");
    let codesign = dir.path().join("codesign");
    crate::commands::test_support::write_exec_script(
        &codesign,
        &format!(
            "#!/bin/sh\necho \"$*\" >> '{log}'\nlast=''; id=''; prev=''\n\
             for a in \"$@\"; do\n  if [ \"$prev\" = --identifier ]; then id=\"$a\"; fi\n  \
             prev=\"$a\"; last=\"$a\"\ndone\n\
             case \"$1\" in\n  -dvv)\n    if [ -f \"$last.signed\" ]; then\n      \
             echo \"Identifier=$(cat \"$last.signed\")\" >&2; cat '{good}' >&2\n    \
             else cat '{adhoc}' >&2; fi\n    exit 0 ;;\n  --verify) exit 0 ;;\nesac\n\
             case \"$last\" in *{fail}) echo errSecInternalComponent >&2; exit 1 ;; esac\n\
             printf '%s' \"$id\" > \"$last.signed\"\nexit 0\n",
            log = log.display(),
            good = good.display(),
            adhoc = adhoc.display(),
        ),
    );
    Fake {
        _dir: dir,
        codesign,
        log,
    }
}

/// A bin dir holding a placeholder file per name.
#[cfg(unix)]
fn bin_dir(names: &[&str]) -> tempfile::TempDir {
    let bin = tempfile::tempdir().expect("tempdir");
    for name in names {
        std::fs::write(bin.path().join(name), b"placeholder").expect("write bin");
    }
    bin
}

/// Why (#11587): the install step signs by name from one list; the required
/// Keychain readers (Architect, 05:30Z) must all be on it, and nothing else.
/// What: every required binary and every cheap extra has a set; unrelated
/// names do not; both browser binaries carry `com.trusty.browser`, the value
/// trusty-browser's own designated requirement pins.
/// Test: This is the test.
#[test]
fn signable_names_only_table_binaries() {
    let all = all_signable_binaries();
    for b in [
        "trusty-secrets",
        "trusty-memory",
        "trusty-browser",
        "trusty-browser-mcp",
        "tm",
        "trusty-mpm",
        "trusty-console",
        "trusty-review",
        "trusty-installer",
        "tctl",
    ] {
        assert!(all.contains(&b), "{b} is not signed at install");
        assert!(set_of(b).is_some(), "{b}");
    }
    for b in ["tga", "trusty-events", "trusty-secrets.signed", "ls"] {
        assert_eq!(set_of(b), None, "{b}");
    }
    assert_eq!(
        binaries_for_set(BROWSER_SET),
        vec!["trusty-browser", "trusty-browser-mcp"]
    );
    assert_eq!(codesign_identifier("trusty-browser"), "com.trusty.browser");
    assert_eq!(
        codesign_identifier("trusty-browser-mcp"),
        "com.trusty.browser"
    );
}

/// Why (#11587): per-binary sign invocation and per-binary report. One
/// binary's failure must not stop the next, and each must carry its own
/// identifier and Hardened Runtime policy.
/// What: signs `trusty-secrets` and `trusty-browser-mcp` (whose sign call
/// fails) and skips an absent and a non-table name; asserts one outcome each
/// for the two present table binaries, the identifiers written, and the
/// automatic policy (`--options runtime` for the browser set only).
/// Test: This is the test.
#[cfg(unix)]
#[test]
fn sign_binaries_reports_each_binary_on_its_own() {
    let f = fake("trusty-browser-mcp");
    let bin = bin_dir(&["trusty-secrets", "trusty-browser-mcp", "tga"]);
    let names = [
        "trusty-secrets",
        "trusty-browser-mcp",
        "trusty-memory",
        "tga",
    ];
    let out = sign_binaries_with(f.codesign.as_os_str(), bin.path(), &names, Some(IDENTITY));

    let got: Vec<(&str, SignStatus)> = out.iter().map(|o| (o.binary.as_str(), o.status)).collect();
    assert_eq!(
        got,
        vec![
            ("trusty-secrets", SignStatus::Signed),
            ("trusty-browser-mcp", SignStatus::Failed),
        ]
    );
    assert!(out[0].is_signed());
    assert!(!out[1].is_signed());
    assert!(
        out[1].detail.contains("codesign failed"),
        "{}",
        out[1].detail
    );
    let marker = std::fs::read_to_string(bin.path().join("trusty-secrets.signed"))
        .expect("trusty-secrets was signed");
    assert_eq!(marker, "com.trusty.trusty-secrets");

    let signs = f.sign_calls();
    assert_eq!(signs.len(), 2, "{signs:?}");
    assert!(signs[0].contains("--identifier com.trusty.trusty-secrets"));
    assert!(
        signs[0].contains(&format!("--sign {IDENTITY}")),
        "{}",
        signs[0]
    );
    assert!(!signs[0].contains("--options runtime"), "{}", signs[0]);
    assert!(signs[1].contains("--identifier com.trusty.browser"));
    assert!(signs[1].contains("--options runtime"), "{}", signs[1]);
}

/// Why (#11587): no identity configured is a reported outcome, never a
/// silent skip and never a sign attempt with an empty identity.
/// What: two ad-hoc binaries and no identity give two `NoIdentity` outcomes
/// and no sign call.
/// Test: This is the test.
#[cfg(unix)]
#[test]
fn sign_binaries_without_an_identity_signs_nothing() {
    let f = fake("never-matches");
    let bin = bin_dir(&["trusty-secrets", "trusty-memory"]);
    let names = ["trusty-secrets", "trusty-memory"];
    let out = sign_binaries_with(f.codesign.as_os_str(), bin.path(), &names, None);
    assert_eq!(out.len(), 2);
    assert!(out.iter().all(|o| o.status == SignStatus::NoIdentity));
    assert!(f.sign_calls().is_empty(), "{:?}", f.calls());
}

/// Why (#11587): a binary already signed with its identifier and team is left
/// alone (no churn, no key prompt); one with another identifier or another
/// team is re-signed, and the wrong team then fails the attestation.
/// What: `trusty-secrets` pre-signed correctly is `AlreadySigned` with or
/// without an identity; `tm` pre-signed with a wrong identifier is re-signed;
/// an identity for team `ZZZZZ99999` re-signs and reports `Failed`.
/// Test: This is the test.
#[cfg(unix)]
#[test]
fn sign_binaries_leaves_an_already_signed_binary_alone() {
    let f = fake("never-matches");
    let bin = bin_dir(&["trusty-secrets", "tm"]);
    let mark = |name: &str, id: &str| {
        std::fs::write(bin.path().join(format!("{name}.signed")), id).expect("marker")
    };
    mark("trusty-secrets", "com.trusty.trusty-secrets");
    mark("tm", "com.trusty.wrong");
    let names = ["trusty-secrets", "tm"];
    let out = sign_binaries_with(f.codesign.as_os_str(), bin.path(), &names, Some(IDENTITY));
    assert_eq!(out[0].status, SignStatus::AlreadySigned);
    assert_eq!(out[1].status, SignStatus::Signed);
    assert_eq!(f.sign_calls().len(), 1, "only tm is re-signed");

    let none = sign_binaries_with(f.codesign.as_os_str(), bin.path(), &names[..1], None);
    assert_eq!(none[0].status, SignStatus::AlreadySigned);

    let other_team = "Developer ID Application: Someone Else (ZZZZZ99999)";
    let out = sign_binaries_with(
        f.codesign.as_os_str(),
        bin.path(),
        &names[..1],
        Some(other_team),
    );
    assert_eq!(out[0].status, SignStatus::Failed);
    assert!(out[0].detail.contains("ZZZZZ99999"), "{}", out[0].detail);
}

/// Why (#11587): `tctl install` signs each member as it lands, then sweeps the
/// directory for binaries other installs placed (trusty-secrets,
/// trusty-browser); the sweep must not sign a member's binary twice.
/// What: `sign_member` for the mpm set, then `sweep`; asserts four outcomes in
/// order and four sign calls.
/// Test: This is the test.
#[cfg(unix)]
#[test]
fn install_signer_sweep_skips_what_a_member_signed() {
    let f = fake("never-matches");
    let bin = bin_dir(&["tm", "trusty-mpm", "trusty-secrets", "trusty-browser"]);
    let mut signer = InstallSigner::with_codesign(&f.codesign, Some(IDENTITY));
    let member: Vec<String> = vec!["tm".into(), "trusty-mpm".into()];
    signer.sign_member(&bin.path().join("tm"), &member);
    signer.sweep(bin.path());
    let out = signer.into_outcomes();
    let names: Vec<&str> = out.iter().map(|o| o.binary.as_str()).collect();
    assert_eq!(
        names,
        ["tm", "trusty-mpm", "trusty-secrets", "trusty-browser"]
    );
    assert!(out.iter().all(BinarySigning::is_signed));
    assert_eq!(f.sign_calls().len(), 4, "{:?}", f.calls());
}

/// Why (#11587): off macOS the step must do nothing and report nothing.
/// What: an inert signer (no `codesign`) over a dir holding a signable binary
/// yields no outcome; off macOS, `probe()` is that inert signer. On macOS
/// `probe()` is not called: it would run `security`.
/// Test: This is the test.
#[cfg(unix)]
#[test]
fn an_inert_install_signer_reports_nothing() {
    let bin = bin_dir(&["trusty-secrets"]);
    let mut inert = InstallSigner::default();
    inert.sweep(bin.path());
    assert!(inert.into_outcomes().is_empty());
    #[cfg(not(target_os = "macos"))]
    {
        let mut probed = InstallSigner::probe();
        probed.sweep(bin.path());
        assert!(probed.into_outcomes().is_empty());
    }
}

/// Why (#11587): Fail-Open Check. Only an attested outcome may read as signed.
/// What: `NoIdentity` and `Failed` lines say `NOT signed` and are not
/// `is_signed`; `Signed` and `AlreadySigned` lines never say `NOT`;
/// `report_lines` asks for the tip only when an identity was missing.
/// Test: This is the test.
#[test]
fn install_step_lines_never_call_an_unsigned_binary_signed() {
    let at = |status| BinarySigning {
        binary: "trusty-browser".to_owned(),
        path: std::path::PathBuf::from("/bin/trusty-browser"),
        status,
        detail: "boom".to_owned(),
    };
    for status in [SignStatus::NoIdentity, SignStatus::Failed] {
        let o = at(status);
        assert!(!o.is_signed());
        assert!(o.line().contains("NOT signed"), "{}", o.line());
    }
    for status in [SignStatus::Signed, SignStatus::AlreadySigned] {
        let o = at(status);
        assert!(o.is_signed());
        assert!(!o.line().contains("NOT"), "{}", o.line());
    }
    assert!(report_lines(&[at(SignStatus::NoIdentity)]).1);
    assert!(!report_lines(&[at(SignStatus::Failed), at(SignStatus::Signed)]).1);
}
