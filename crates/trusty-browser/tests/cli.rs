//! The `trusty-browser` binary against Python's verbs, output and exit codes.
//!
//! Every run sets `HOME` to a temp directory and `--state` to a temp state;
//! no verb here reaches launchd or the Keychain.

mod common;

use std::io::{Read, Write};
use std::os::unix::net::UnixListener;
use std::path::Path;
use std::process::{Command, Output, Stdio};

use trusty_browser::cli::RELOAD_NOTICE;
use trusty_browser::cli_operator::{grant_id_ok, operator_frame, profile_id_ok};

fn run(home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_trusty-browser"))
        .args(args)
        .env("HOME", home)
        .stdin(Stdio::null())
        .output()
        .expect("run")
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

#[test]
fn cli_verbs_match_python_names_and_exit_codes() {
    let home = common::state_dir();
    let help = text(&run(home.path(), &["--help"]).stdout);
    for verb in [
        "serve",
        "revoke",
        "extension-path",
        "install-host",
        "launchd-plist",
        "install-service",
        "uninstall-service",
        "grants",
        "revoke-grant",
        "revoke-grants",
        "pairings",
        "unpair",
        "native-host",
        "migrate-keychain",
    ] {
        assert!(
            help.contains(&format!("  {verb} ")),
            "{verb} missing:\n{help}"
        );
    }
    assert_eq!(run(home.path(), &[]).status.code(), Some(2));
    assert_eq!(run(home.path(), &["bogus"]).status.code(), Some(2));
    assert_eq!(run(home.path(), &["revoke-grant"]).status.code(), Some(2));
    assert_eq!(
        run(home.path(), &["grants", "extra"]).status.code(),
        Some(2)
    );
    assert_eq!(
        run(home.path(), &["migrate-keychain"]).status.code(),
        Some(2)
    );
}

#[test]
fn operator_verbs_validate_like_python() {
    let home = common::state_dir();
    let st = home.path().join("state");
    let st = st.to_str().expect("utf8");
    for (args, msg) in [
        (
            vec!["revoke-grant", "g-12"],
            "A grant id is g- followed by 12 lower-case hex characters\n",
        ),
        (
            vec!["unpair", "nope"],
            "A profile id is a lower-case UUID v4\n",
        ),
        (
            vec!["grants"],
            "The Trusty Browser service is not reachable on operator.sock\n",
        ),
        (
            vec!["revoke-grant", "g-0123456789ab"],
            "The Trusty Browser service is not reachable on operator.sock\n",
        ),
        (vec!["revoke", "BAD"], "InvalidEnrollment\n"),
    ] {
        let mut all = vec!["--state", st];
        all.extend(&args);
        let out = run(home.path(), &all);
        assert_eq!(
            (out.status.code(), text(&out.stderr)),
            (Some(1), msg.to_owned()),
            "{args:?}"
        );
        assert!(out.stdout.is_empty());
    }
    assert!(grant_id_ok("g-0123456789ab") && !grant_id_ok("g-0123456789AB"));
    assert!(profile_id_ok("73670307-8744-4354-b70b-4ec41884ea1f"));
    assert!(!profile_id_ok("73670307-8744-5354-b70b-4ec41884ea1f"));
    assert!(!profile_id_ok("73670307-8744-4354-c70b-4ec41884ea1f"));
    let frame = operator_frame("revoke-grant", Some("g-0123456789ab")).expect("frame");
    assert_eq!(
        frame.dumps_spaced().expect("dump"),
        r#"{"op": "revoke_grant", "grant": "g-0123456789ab"}"#
    );
}

/// Serve one frame on `operator.sock`; return the request body.
fn serve_once(sock: &Path, reply: &'static str) -> std::thread::JoinHandle<String> {
    let listener = UnixListener::bind(sock).expect("bind");
    std::thread::spawn(move || {
        let (mut s, _) = listener.accept().expect("accept");
        let mut head = [0u8; 4];
        s.read_exact(&mut head).expect("head");
        let mut body = vec![0u8; u32::from_ne_bytes(head) as usize];
        s.read_exact(&mut body).expect("body");
        s.write_all(&(reply.len() as u32).to_ne_bytes()).expect("w");
        s.write_all(reply.as_bytes()).expect("w");
        String::from_utf8(body).expect("utf8")
    })
}

#[test]
fn operator_call_prints_the_reply_and_exit_code() {
    let home = common::state_dir();
    let st = home.path().join("s");
    std::fs::create_dir(&st).expect("mkdir");
    let sock = st.join("operator.sock");
    let stp = st.to_str().expect("utf8");
    let server = serve_once(&sock, r#"{"ok": true, "pairings": [{"email": "bêta"}]}"#);
    let out = run(home.path(), &["--state", stp, "pairings"]);
    assert_eq!(server.join().expect("join"), r#"{"op": "pairings"}"#);
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(
        text(&out.stdout),
        "{\"ok\":true,\"pairings\":[{\"email\":\"b\\u00eata\"}]}\n"
    );
    std::fs::remove_file(&sock).expect("rm");
    let server = serve_once(&sock, r#"{"ok": false, "error": "UnknownGrant"}"#);
    let out = run(
        home.path(),
        &["--state", stp, "revoke-grant", "g-0123456789ab"],
    );
    server.join().expect("join");
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        text(&out.stdout),
        "{\"ok\":false,\"error\":\"UnknownGrant\"}\n"
    );
}

#[test]
fn extension_path_prints_path_and_notice() {
    let home = common::state_dir();
    let st = home.path().join("state");
    let out = run(
        home.path(),
        &["--state", st.to_str().expect("utf8"), "extension-path"],
    );
    assert_eq!(out.status.code(), Some(0));
    let real = std::fs::canonicalize(&st).expect("canon").join("extension");
    assert_eq!(text(&out.stdout), format!("{}\n", real.display()));
    assert_eq!(text(&out.stderr), format!("{RELOAD_NOTICE}\n"));
    assert_eq!(
        RELOAD_NOTICE,
        "Reload Trusty Browser in each Chrome profile (chrome://extensions) to load the new version."
    );
}

#[test]
fn default_state_and_install_host_live_under_home() {
    let home = common::state_dir();
    // A test build refuses the `install-host` verb (#11440), so this drives the
    // library call the verb makes, with the binary as the registered exe.
    let real = std::fs::canonicalize(home.path()).expect("canon");
    let exe = std::fs::canonicalize(env!("CARGO_BIN_EXE_trusty-browser")).expect("exe");
    let dir = trusty_browser::install_host::default_directory(home.path()).expect("dir");
    let manifest = trusty_browser::install_host::install(&real.join(".trusty/browser"), &dir, &exe)
        .expect("install");
    let hosts = if cfg!(target_os = "macos") {
        "Library/Application Support/Google/Chrome/NativeMessagingHosts"
    } else {
        ".config/google-chrome/NativeMessagingHosts"
    };
    // Python prints the unresolved home-relative manifest path.
    assert_eq!(
        manifest,
        home.path().join(hosts).join("com.trusty.browser.json")
    );
    let launcher =
        std::fs::read_to_string(real.join(".trusty/browser/chrome-native-host")).expect("launcher");
    assert!(launcher.starts_with(&format!(
        "#!/bin/sh\nexec {} native-host --socket ",
        exe.display()
    )));
    let plist = text(&run(home.path(), &["launchd-plist"]).stdout);
    assert!(plist.contains(&format!(
        "<string>{}/.trusty/browser</string>",
        real.display()
    )));
}

#[test]
fn revoke_offline_without_a_route_is_a_no_op() {
    let home = common::state_dir();
    let st = home.path().join("state");
    let out = run(
        home.path(),
        &["--state", st.to_str().expect("utf8"), "revoke", "wf-1"],
    );
    assert_eq!(
        (out.status.code(), text(&out.stderr)),
        (Some(0), String::new())
    );
    assert!(st.join("registry.sqlite3").is_file());
}

/// A test build carries the seams, so it never registers itself with Chrome.
#[test]
fn a_test_build_refuses_to_register() {
    let home = common::state_dir();
    // `install-service` shares the guard; it is not run here because a
    // regression would reach the real `/bin/launchctl`.
    let out = run(home.path(), &["install-host"]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(text(&out.stderr), "test build; refusing to register\n");
    assert!(!home.path().join(".trusty").exists());
    assert!(!home.path().join("Library").exists());
}

/// `serve` says on stderr that the test seams are active.
#[test]
fn serve_with_test_seams_prints_a_banner() {
    let home = common::state_dir();
    let st = home.path().join("state");
    let mut child = Command::new(env!("CARGO_BIN_EXE_trusty-browser"))
        .args(["--state", st.to_str().expect("utf8"), "serve"])
        .env("HOME", home.path())
        .env("TRUSTY_BROWSER_TEST_SEAMS", "1")
        .stdin(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn");
    let mut err = child.stderr.take().expect("stderr");
    let line = std::thread::spawn(move || {
        let mut buf = [0u8; 256];
        let n = err.read(&mut buf).unwrap_or(0);
        String::from_utf8_lossy(&buf[..n]).into_owned()
    });
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !line.is_finished() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let _ = child.kill();
    let _ = child.wait();
    let got = line.join().expect("reader");
    assert!(
        got.starts_with(trusty_browser::cli::TEST_SEAMS_BANNER),
        "stderr: {got:?}"
    );
}
