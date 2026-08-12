//! `taudit install` refuses, from the outside.
//!
//! Why: every other test of this path calls the library. This one runs the
//! binary a recipient would run, because the guarantee #5495 owes is about what
//! the program does, not what a function returns — an exit code of 0 with an
//! empty `tools/` directory would satisfy every unit test in the crate and still
//! be the exact degraded success (#5454) this work exists to prevent.
//!
//! What: each test drives a real `taudit install` over a throwaway working
//! directory and asserts the same three things — a non-zero exit, an empty
//! `tools/` area, and no version record. The network-reaching cases are
//! `#[ignore]`d; `cargo test -p trusty-audit -- --include-ignored` runs them.
//!
//! What this file deliberately does NOT cover: a successful download. Proving
//! that offline needs a fixture release server, which
//! `trusty-installer`'s own `download::pinned::tests` already stands up and
//! drives through the same entry point this crate calls. Duplicating it here
//! would test `trusty-installer`, not this client.
//! Test: this is the test.

use std::path::{Path, PathBuf};
use std::process::Command;

const TAUDIT: &str = env!("CARGO_BIN_EXE_taudit");

/// A config pinning versions no release will ever carry.
const UNPUBLISHABLE: &str = r#"
openrouter_key = "sk-or-v1-not-a-real-key"
instructions = "Assess the last 52 weeks."

[tools]
tga = "0.0.0-never-published"
trusty-analyze = "0.0.0-never-published"
trusty-review = "0.0.0-never-published"
"#;

struct Engagement {
    _tmp: tempfile::TempDir,
    work: PathBuf,
    config: PathBuf,
}

impl Engagement {
    /// Lay out a package directory with `config`, if any, beside a work root.
    fn new(config: Option<&str>) -> Self {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = tmp.path().join("trusty-audit-work");
        let config_path = tmp.path().join("engagement.toml");
        if let Some(text) = config {
            std::fs::write(&config_path, text).expect("write engagement config");
        }
        Self {
            _tmp: tmp,
            work,
            config: config_path,
        }
    }

    fn run(&self, verb: &str) -> (String, String, Option<i32>) {
        let out = Command::new(TAUDIT)
            .args([
                "--work-dir",
                self.work.to_str().expect("utf-8 path"),
                "--config",
                self.config.to_str().expect("utf-8 path"),
                verb,
            ])
            .output()
            .unwrap_or_else(|e| panic!("taudit did not start: {e}"));
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code(),
        )
    }

    /// The state a refusal must leave behind: nothing.
    fn assert_nothing_was_installed(&self) {
        let tools = self.work.join("tools");
        if tools.exists() {
            let left: Vec<PathBuf> = std::fs::read_dir(&tools)
                .expect("read tools/")
                .map(|e| e.expect("entry").path())
                .collect();
            assert!(left.is_empty(), "the refusal left files behind: {left:?}");
        }
        let record = self.work.join("state/tool-versions.toml");
        assert!(
            !record.exists(),
            "the refusal recorded a version at {}",
            record.display()
        );
    }
}

fn assert_refused(stderr: &str, code: Option<i32>) {
    assert_eq!(code, Some(1), "expected a non-zero exit; stderr: {stderr}");
    assert!(!stderr.trim().is_empty(), "a refusal must say why");
}

#[test]
fn installing_with_no_engagement_config_refuses() {
    let e = Engagement::new(None);
    let (stdout, stderr, code) = e.run("install");
    assert_refused(&stderr, code);
    assert!(
        stdout.is_empty(),
        "a refusal must print no result: {stdout}"
    );
    assert!(
        stderr.contains("engagement.toml"),
        "the refusal must name the file it wanted: {stderr}"
    );
    e.assert_nothing_was_installed();
}

#[test]
fn installing_from_a_config_with_no_pins_refuses() {
    let e = Engagement::new(Some(
        "openrouter_key = \"sk-or-v1-x\"\ninstructions = \"assess\"\n",
    ));
    let (_, stderr, code) = e.run("install");
    assert_refused(&stderr, code);
    assert!(
        stderr.contains("engagement config"),
        "the refusal must name the schema that did not match: {stderr}"
    );
    e.assert_nothing_was_installed();
}

#[test]
fn installing_from_a_config_pinning_only_two_tools_refuses() {
    let two_of_three = UNPUBLISHABLE.replace("trusty-review = \"0.0.0-never-published\"\n", "");
    let e = Engagement::new(Some(&two_of_three));
    let (_, stderr, code) = e.run("install");
    assert_refused(&stderr, code);
    e.assert_nothing_was_installed();
}

/// A tool present but not placed by this client is never shown with a version.
#[test]
fn tools_reports_an_unrecorded_binary_as_unverified() {
    let e = Engagement::new(Some(UNPUBLISHABLE));
    e.run("workdir");
    std::fs::write(e.work.join("tools/tga"), b"#!/bin/sh\n").expect("plant a binary");

    let (stdout, _, code) = e.run("tools");
    assert_eq!(code, Some(0));
    assert!(stdout.contains("UNVERIFIED"), "{stdout}");
    assert!(stdout.contains("MISSING"), "{stdout}");
}

/// The whole download path against the real release host.
#[test]
#[ignore = "reaches the GitHub release API; run with --include-ignored"]
fn an_unpublished_pin_refuses_against_the_real_release_host() {
    let e = Engagement::new(Some(UNPUBLISHABLE));
    let (stdout, stderr, code) = e.run("install");
    assert_refused(&stderr, code);
    assert!(
        stdout.is_empty(),
        "a refusal must print no result: {stdout}"
    );
    assert!(
        stderr.contains("0.0.0-never-published"),
        "the refusal must name the pin it could not satisfy: {stderr}"
    );
    assert!(
        stderr.contains("nothing was installed"),
        "the refusal must state what happened to the install directory: {stderr}"
    );
    e.assert_nothing_was_installed();
}

/// The egress-blocked case, forced: a config whose pins resolve against a host
/// that does not answer. It is the same refusal a recipient behind a proxy that
/// blocks binary downloads gets, and it is a known v1 risk rather than a solved
/// problem — see the `tools` module docs.
#[test]
#[ignore = "attempts a network connection; run with --include-ignored"]
fn a_blocked_download_host_refuses_rather_than_proceeding() {
    let e = Engagement::new(Some(UNPUBLISHABLE));
    // 203.0.113.0/24 is TEST-NET-3 (RFC 5737): reserved for documentation and
    // guaranteed not to route, so this stands in for a blocked egress path
    // without depending on the host's own firewall.
    let (_, stderr, code) = Command::new(TAUDIT)
        .args([
            "--work-dir",
            e.work.to_str().expect("utf-8 path"),
            "--config",
            e.config.to_str().expect("utf-8 path"),
            "install",
        ])
        .env("HTTPS_PROXY", "http://203.0.113.1:9")
        .env("HTTP_PROXY", "http://203.0.113.1:9")
        .output()
        .map(|out| {
            (
                String::from_utf8_lossy(&out.stdout).into_owned(),
                String::from_utf8_lossy(&out.stderr).into_owned(),
                out.status.code(),
            )
        })
        .expect("taudit did not start");

    assert_refused(&stderr, code);
    e.assert_nothing_was_installed();
    assert!(
        Path::new(&e.work).join("tools").exists(),
        "the working directory is still created; only the install refused"
    );
}
