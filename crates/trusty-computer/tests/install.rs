//! Phase 6: `install-service`, `uninstall-service` and signing of both binaries (#11460).
//!
//! Every path is under a temp HOME; `launchctl` and `codesign` are recording
//! fakes and the CuaDriver version is a closure, so no test reaches launchd,
//! the Keychain or cua-driver. Ported from trusty-browser `tests/install.rs`.

use std::cell::RefCell;
use std::fs;
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use trusty_computer::codesign::{self, Codesign, Signer, Signing};
use trusty_computer::launchd::{self, Launchctl, Layout, Outcome};
use trusty_computer::service_cli;

/// Records every call; `print` answers with the configured outcome.
struct FakeCtl {
    print: Outcome,
    calls: RefCell<Vec<Vec<String>>>,
}

impl FakeCtl {
    fn new(code: i32, stdout: &str) -> FakeCtl {
        FakeCtl {
            print: Outcome {
                code,
                stdout: stdout.to_owned(),
                stderr: String::new(),
            },
            calls: RefCell::new(Vec::new()),
        }
    }
    fn verbs(&self) -> Vec<String> {
        self.calls.borrow().iter().map(|c| c[0].clone()).collect()
    }
}

impl Launchctl for FakeCtl {
    fn run(&self, args: &[&str]) -> io::Result<Outcome> {
        self.calls
            .borrow_mut()
            .push(args.iter().map(|s| s.to_string()).collect());
        Ok(if args[0] == "print" {
            self.print.clone()
        } else {
            Outcome::default()
        })
    }
}

/// `verify` answers from a queue (the last answer repeats); `sign` is fixed.
struct FakeSign {
    verify: RefCell<Vec<bool>>,
    sign: Result<(), String>,
    listing: String,
    calls: RefCell<Vec<String>>,
}

impl FakeSign {
    fn new(verify: &[bool], sign: Result<(), String>, listing: &str) -> FakeSign {
        FakeSign {
            verify: RefCell::new(verify.to_vec()),
            sign,
            listing: listing.to_owned(),
            calls: RefCell::new(Vec::new()),
        }
    }
}

impl Codesign for FakeSign {
    fn verify(&self, path: &Path) -> io::Result<bool> {
        self.calls
            .borrow_mut()
            .push(format!("verify {}", path.display()));
        let mut q = self.verify.borrow_mut();
        Ok(if q.len() > 1 { q.remove(0) } else { q[0] })
    }
    fn sign(&self, path: &Path, identity: &str) -> io::Result<Result<(), String>> {
        self.calls
            .borrow_mut()
            .push(format!("sign {identity} {}", path.display()));
        Ok(self.sign.clone())
    }
    fn identities(&self) -> io::Result<String> {
        self.calls.borrow_mut().push("identities".to_owned());
        Ok(self.listing.clone())
    }
}

const ONE_ID: &str = "  1) 0123456789ABCDEF0123456789ABCDEF01234567 \"Developer ID Application: Test Owner (4JH68XUHC5)\"\n     1 valid identities found\n";
const ID: &str = "Developer ID Application: Test Owner (4JH68XUHC5)";

fn driver_ok() -> Result<String, String> {
    Ok("cua-driver 0.23.2".to_owned())
}

fn home() -> tempfile::TempDir {
    tempfile::tempdir().expect("tempdir")
}

/// `home/bin/trusty-computer` and its `trusty-computer-mcp` sibling.
fn layout(home: &Path) -> Layout {
    let bin = home.join("bin");
    fs::create_dir_all(&bin).expect("bin");
    fs::write(bin.join("trusty-computer"), b"exe").expect("exe");
    fs::write(bin.join("trusty-computer-mcp"), b"mcp").expect("mcp");
    Layout {
        home: home.to_path_buf(),
        state: home.join(".trusty/computer"),
        exe: bin.join("trusty-computer"),
        uid: 501,
    }
}

fn signer(tool: &FakeSign) -> Signer<'_> {
    Signer {
        signing: Signing::Sign(None),
        tool,
    }
}

fn plist_path(home: &Path) -> PathBuf {
    home.join("Library/LaunchAgents/com.trusty.computer.plist")
}

fn install(l: &Layout, ctl: &FakeCtl, s: &Signer<'_>) -> Result<(), launchd::LaunchdError> {
    launchd::manage(l, true, ctl, s, &driver_ok)
}

/// No plist and no launchctl call beyond `print`.
fn assert_nothing_registered(l: &Layout, ctl: &FakeCtl) {
    assert!(!plist_path(&l.home).exists(), "plist written");
    assert_eq!(ctl.verbs(), vec!["print"]);
}

#[test]
fn install_service_writes_files_and_bootstraps() {
    let dir = home();
    let l = layout(dir.path());
    let ctl = FakeCtl::new(113, "");
    let tool = FakeSign::new(&[true], Ok(()), "");
    install(&l, &ctl, &signer(&tool)).expect("install");
    let plist = plist_path(dir.path());
    assert_eq!(
        *ctl.calls.borrow(),
        vec![
            vec!["print".to_owned(), "gui/501/com.trusty.computer".to_owned()],
            vec![
                "bootstrap".to_owned(),
                "gui/501".to_owned(),
                plist.display().to_string()
            ],
        ]
    );
    let text = fs::read_to_string(&plist).expect("plist");
    assert_eq!(text, launchd::plist(&l.exe, &l.state).expect("render"));
    let argv = format!(
        "\t\t<string>{}</string>\n\t\t<string>broker</string>\n\t\t<string>--state</string>\n\t\t<string>{}</string>\n",
        l.exe.display(),
        l.state.display()
    );
    assert!(text.contains(&argv), "{text}");
    assert!(text.contains("<key>KeepAlive</key>\n\t<true/>"));
    assert_eq!(fs::metadata(&plist).expect("m").mode() & 0o777, 0o600);
    assert_eq!(fs::metadata(&l.state).expect("m").mode() & 0o777, 0o700);
    // Loaded with our plist: a second install is a no-op that does not bootstrap.
    let ctl = FakeCtl::new(0, &format!("\tpath = {}\n", plist.display()));
    install(&l, &ctl, &signer(&tool)).expect("reinstall");
    assert_eq!(ctl.verbs(), vec!["print"]);
}

#[test]
fn uninstall_service_removes_the_job() {
    let dir = home();
    let l = layout(dir.path());
    let tool = FakeSign::new(&[true], Ok(()), "");
    install(&l, &FakeCtl::new(113, ""), &signer(&tool)).expect("install");
    let plist = plist_path(dir.path());
    let ctl = FakeCtl::new(0, &format!("\tpath = {}\n", plist.display()));
    launchd::manage(&l, false, &ctl, &signer(&tool), &driver_ok).expect("uninstall");
    assert_eq!(
        *ctl.calls.borrow(),
        vec![
            vec!["print".to_owned(), "gui/501/com.trusty.computer".to_owned()],
            vec![
                "bootout".to_owned(),
                "gui/501/com.trusty.computer".to_owned()
            ],
        ]
    );
    assert!(!plist.exists());
    // Nothing loaded, nothing on disk: uninstall is a no-op.
    let ctl = FakeCtl::new(113, "");
    launchd::manage(&l, false, &ctl, &signer(&tool), &driver_ok).expect("again");
    assert_eq!(ctl.verbs(), vec!["print"]);
}

#[test]
fn install_service_refuses_a_job_owned_elsewhere() {
    let dir = home();
    let l = layout(dir.path());
    let tool = FakeSign::new(&[true], Ok(()), "");
    let ctl = FakeCtl::new(0, "\tpath = /elsewhere/com.trusty.computer.plist\n");
    let err = install(&l, &ctl, &signer(&tool)).expect_err("refused");
    assert_eq!(
        err.to_string(),
        "A different launchd job owns com.trusty.computer"
    );
    assert_nothing_registered(&l, &ctl);
    // A plist we did not write is never changed or removed.
    let plist = plist_path(dir.path());
    fs::create_dir_all(plist.parent().expect("parent")).expect("mkdir");
    fs::write(&plist, "foreign").expect("w");
    for verb in [true, false] {
        let ctl = FakeCtl::new(113, "");
        let err = launchd::manage(&l, verb, &ctl, &signer(&tool), &driver_ok).expect_err("refused");
        assert!(err.to_string().starts_with("Refusing to change"), "{err}");
        assert!(ctl.verbs().is_empty());
    }
    assert_eq!(fs::read_to_string(&plist).expect("r"), "foreign");
}

#[test]
fn auto_detect_needs_exactly_one_team_identity() {
    assert_eq!(codesign::detect_identity(ONE_ID).as_deref(), Ok(ID));
    let other = "  1) AA \"Developer ID Application: X (ZZZZZZZZZZ)\"\n  2) BB \"Apple Development: Y (4JH68XUHC5)\"\n";
    assert!(
        codesign::detect_identity(other)
            .expect_err("none")
            .starts_with("No Developer ID Application identity for team 4JH68XUHC5")
    );
    let two = format!("{ONE_ID}  2) CC \"Developer ID Application: Other (4JH68XUHC5)\"\n");
    assert!(
        codesign::detect_identity(&two)
            .expect_err("two")
            .starts_with("More than one")
    );
}

#[test]
fn signing_constants_name_the_computer_identity() {
    assert_eq!(codesign::IDENTIFIER, "com.trusty.computer");
    assert_eq!(codesign::TEAM, "4JH68XUHC5");
    assert_eq!(
        codesign::REQUIREMENT,
        "identifier \"com.trusty.computer\" and anchor apple generic and certificate leaf[subject.OU] = \"4JH68XUHC5\""
    );
    assert_eq!(codesign::IDENTITY_ENV, "TRUSTY_COMPUTER_SIGN_IDENTITY");
}

#[test]
fn install_service_signs_an_unsigned_binary_before_writing() {
    let dir = home();
    let l = layout(dir.path());
    let tool = FakeSign::new(&[false, true], Ok(()), ONE_ID);
    install(&l, &FakeCtl::new(113, ""), &signer(&tool)).expect("install");
    let exe = l.exe.display();
    assert_eq!(
        *tool.calls.borrow(),
        vec![
            format!("verify {exe}"),
            "identities".to_owned(),
            format!("sign {ID} {exe}"),
            format!("verify {exe}"),
            format!("verify {}", codesign::mcp_sibling(&l.exe).display()),
        ]
    );
    assert!(plist_path(dir.path()).is_file());
}

#[test]
fn install_service_uses_the_given_identity_and_skips_a_signed_binary() {
    let dir = home();
    let l = layout(dir.path());
    let tool = FakeSign::new(&[false, true], Ok(()), "");
    let s = Signer {
        signing: Signing::Sign(Some("Developer ID Application: Given (4JH68XUHC5)".into())),
        tool: &tool,
    };
    install(&l, &FakeCtl::new(113, ""), &s).expect("install");
    assert_eq!(
        tool.calls.borrow()[1],
        format!(
            "sign Developer ID Application: Given (4JH68XUHC5) {}",
            l.exe.display()
        )
    );
    // Already satisfying the requirement: verified once, never re-signed.
    let tool = FakeSign::new(&[true], Err("must not sign".into()), "");
    let ctl = FakeCtl::new(
        0,
        &format!("\tpath = {}\n", plist_path(dir.path()).display()),
    );
    install(&l, &ctl, &signer(&tool)).expect("reinstall");
    assert_eq!(
        *tool.calls.borrow(),
        vec![
            format!("verify {}", l.exe.display()),
            format!("verify {}", codesign::mcp_sibling(&l.exe).display()),
        ]
    );
}

#[test]
fn install_service_writes_nothing_when_signing_fails() {
    let dir = home();
    let l = layout(dir.path());
    let tool = FakeSign::new(&[false], Err("errSecInternalComponent".into()), ONE_ID);
    let ctl = FakeCtl::new(113, "");
    let err = install(&l, &ctl, &signer(&tool)).expect_err("refused");
    assert_eq!(
        err.to_string(),
        format!(
            "codesign failed for {}: errSecInternalComponent",
            l.exe.display()
        )
    );
    assert_nothing_registered(&l, &ctl);
}

#[test]
fn install_service_writes_nothing_when_verify_fails_after_signing() {
    let dir = home();
    let l = layout(dir.path());
    let tool = FakeSign::new(&[false, false], Ok(()), ONE_ID);
    let ctl = FakeCtl::new(113, "");
    let err = install(&l, &ctl, &signer(&tool)).expect_err("refused");
    assert!(
        err.to_string()
            .contains("does not satisfy the signing requirement"),
        "{err}"
    );
    assert_nothing_registered(&l, &ctl);
}

#[test]
fn install_service_refuses_without_an_identity() {
    let dir = home();
    let l = layout(dir.path());
    let tool = FakeSign::new(&[false], Ok(()), "     0 valid identities found\n");
    let ctl = FakeCtl::new(113, "");
    let err = install(&l, &ctl, &signer(&tool)).expect_err("refused");
    assert!(err.to_string().contains("use --no-sign"), "{err}");
    assert!(!tool.calls.borrow().iter().any(|c| c.starts_with("sign")));
    assert_nothing_registered(&l, &ctl);
}

#[test]
fn install_service_no_sign_registers_without_codesign() {
    let dir = home();
    let l = layout(dir.path());
    let tool = FakeSign::new(&[false], Err("must not sign".into()), "");
    let s = Signer {
        signing: Signing::Skip,
        tool: &tool,
    };
    install(&l, &FakeCtl::new(113, ""), &s).expect("install");
    assert!(tool.calls.borrow().is_empty());
    assert!(plist_path(dir.path()).is_file());
    assert!(codesign::NO_SIGN_WARNING.starts_with("warning: --no-sign"));
}

#[test]
fn install_service_signs_the_mcp_sibling() {
    let dir = home();
    let l = layout(dir.path());
    let mcp = codesign::mcp_sibling(&l.exe);
    assert_eq!(mcp, dir.path().join("bin/trusty-computer-mcp"));
    let tool = FakeSign::new(&[false, true, false, true], Ok(()), ONE_ID);
    install(&l, &FakeCtl::new(113, ""), &signer(&tool)).expect("install");
    let (exe, mcp) = (l.exe.display(), mcp.display());
    assert_eq!(
        *tool.calls.borrow(),
        vec![
            format!("verify {exe}"),
            "identities".to_owned(),
            format!("sign {ID} {exe}"),
            format!("verify {exe}"),
            format!("verify {mcp}"),
            "identities".to_owned(),
            format!("sign {ID} {mcp}"),
            format!("verify {mcp}"),
        ]
    );
    assert!(plist_path(dir.path()).is_file());
}

#[test]
fn install_service_writes_nothing_when_the_sibling_fails_to_sign() {
    let dir = home();
    let l = layout(dir.path());
    // The main binary signs and verifies; the sibling never verifies.
    let tool = FakeSign::new(&[false, true, false, false], Ok(()), ONE_ID);
    let ctl = FakeCtl::new(113, "");
    let err = install(&l, &ctl, &signer(&tool)).expect_err("refused");
    let mcp = codesign::mcp_sibling(&l.exe);
    assert!(
        err.to_string().starts_with(&format!(
            "{} does not satisfy the signing requirement",
            mcp.display()
        )),
        "{err}"
    );
    assert_nothing_registered(&l, &ctl);
}

#[test]
fn install_service_refuses_a_missing_mcp_sibling() {
    let dir = home();
    let l = layout(dir.path());
    let mcp = codesign::mcp_sibling(&l.exe);
    fs::remove_file(&mcp).expect("rm");
    let tool = FakeSign::new(&[true], Ok(()), ONE_ID);
    let ctl = FakeCtl::new(113, "");
    let err = install(&l, &ctl, &signer(&tool)).expect_err("refused");
    assert!(
        err.to_string()
            .starts_with(&format!("{} is missing", mcp.display())),
        "{err}"
    );
    assert!(tool.calls.borrow().is_empty());
    assert_nothing_registered(&l, &ctl);
}

#[test]
fn install_service_refuses_the_wrong_cua_driver() {
    let dir = home();
    let l = layout(dir.path());
    let tool = FakeSign::new(&[true], Ok(()), "");
    for version in [
        Ok("cua-driver 0.24.0".to_owned()),
        Err("No such file or directory".to_owned()),
    ] {
        let ctl = FakeCtl::new(113, "");
        let v = version.clone();
        let err = launchd::manage(&l, true, &ctl, &signer(&tool), &move || v.clone())
            .expect_err("refused");
        assert!(err.to_string().contains("CuaDriver"), "{err}");
        assert_nothing_registered(&l, &ctl);
    }
}

#[test]
fn test_seams_build_refuses_to_register() {
    let err = launchd::refuse_test_build(true).expect_err("refused");
    assert!(err.to_string().contains("test-seams"), "{err}");
    launchd::refuse_test_build(false).expect("release build registers");
}

#[test]
fn signing_flag_beats_env_and_no_sign_skips() {
    let given = service_cli::ServiceArgs {
        sign_identity: Some("flag".into()),
        ..Default::default()
    };
    assert_eq!(
        service_cli::signing(&given, Some("env".into())),
        Signing::Sign(Some("flag".into()))
    );
    let bare = service_cli::ServiceArgs::default();
    assert_eq!(
        service_cli::signing(&bare, Some("env".into())),
        Signing::Sign(Some("env".into()))
    );
    assert_eq!(
        service_cli::signing(&bare, Some(String::new())),
        Signing::Sign(None)
    );
    let skip = service_cli::ServiceArgs {
        no_sign: true,
        ..Default::default()
    };
    assert_eq!(
        service_cli::signing(&skip, Some("env".into())),
        Signing::Skip
    );
}

#[test]
fn service_args_parse_and_refuse_conflicts() {
    let a: Vec<String> = ["--state", "/s", "--sign-identity", "X"]
        .map(String::from)
        .to_vec();
    let p = service_cli::parse(&a).expect("parse");
    assert_eq!(p.state.as_deref(), Some(Path::new("/s")));
    assert_eq!(p.sign_identity.as_deref(), Some("X"));
    let bad: Vec<String> = ["--no-sign", "--sign-identity", "X"]
        .map(String::from)
        .to_vec();
    assert!(
        service_cli::parse(&bad)
            .expect_err("conflict")
            .contains("conflict")
    );
    assert!(service_cli::parse(&["--bogus".to_owned()]).is_err());
}
