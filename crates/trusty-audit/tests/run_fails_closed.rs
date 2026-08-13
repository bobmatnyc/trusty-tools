//! `taudit run` refuses, and reports partial failure, from the outside.
//!
//! Why: #5555's highest-risk shape is a sweep that fails partway and still
//! leaves the client reporting success. The library returns `Ok` for a partial
//! sweep on purpose — the per-repo failures are data a front end renders — so
//! the guarantee that a shell sees `$? != 0` lives in the binary, and only a
//! test of the binary can prove it. Every in-module test would pass against a
//! `main.rs` that ignored the status entirely.
//!
//! What: a throwaway work directory with stub binaries standing in for the
//! pinned triple, driven through the real `taudit run`. No network, so nothing
//! here is `#[ignore]`d.
//! Test: this is the test.

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;

const TAUDIT: &str = env!("CARGO_BIN_EXE_taudit");

const CONFIG: &str = r#"
openrouter_key = "sk-or-v1-not-a-real-key"
instructions = "Assess the last 52 weeks."

[tools]
tga = "2.9.4"
trusty-analyze = "0.9.2"
trusty-review = "0.15.1"
"#;

struct Engagement {
    _tmp: tempfile::TempDir,
    work: PathBuf,
    config: PathBuf,
}

impl Engagement {
    fn new() -> Self {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = tmp.path().join("trusty-audit-work");
        let config = tmp.path().join("engagement.toml");
        std::fs::write(&config, CONFIG).expect("write engagement config");
        for area in ["tools", "repos", "extract", "state", "out", "logs"] {
            std::fs::create_dir_all(work.join(area)).expect("mkdir area");
        }
        Self {
            _tmp: tmp,
            work,
            config,
        }
    }

    /// Stand in for a verified install: three executables plus the record that
    /// says this client placed them.
    fn install_stubs(&self, script: &str) {
        for name in ["tga", "trusty-analyze", "trusty-review"] {
            let path = self.work.join("tools").join(name);
            std::fs::write(&path, script).expect("stub");
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        }
        let record = format!(
            "[[tools]]\ncrate_name = \"tga\"\nversion = \"2.9.4\"\nbinary = \"{d}/tga\"\n\
             [[tools]]\ncrate_name = \"trusty-analyze\"\nversion = \"0.9.2\"\nbinary = \"{d}/a\"\n\
             [[tools]]\ncrate_name = \"trusty-review\"\nversion = \"0.15.1\"\nbinary = \"{d}/r\"\n",
            d = self.work.join("tools").display()
        );
        std::fs::write(self.work.join("state/tool-versions.toml"), record).expect("record");
    }

    fn select(&self, entries: &[(&str, &str)]) {
        let mut text = String::new();
        for (name, path) in entries {
            text.push_str(&format!(
                "[[repositories]]\nname = \"{name}\"\npath = \"{path}\"\n\n"
            ));
        }
        std::fs::write(self.work.join("state/selected-repos.toml"), text).expect("selection");
    }

    fn checkout(&self, name: &str) {
        std::fs::create_dir_all(self.work.join("repos").join(name)).expect("mkdir repo");
    }

    fn run(&self) -> (String, String, Option<i32>) {
        let out = Command::new(TAUDIT)
            .args(["run", "--work-dir"])
            .arg(&self.work)
            .arg("--config")
            .arg(&self.config)
            .output()
            .expect("taudit runs");
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code(),
        )
    }

    fn progress(&self) -> String {
        std::fs::read_to_string(self.work.join("state/run-progress.toml")).unwrap_or_default()
    }
}

fn is_empty(dir: &Path) -> bool {
    std::fs::read_dir(dir)
        .map(|mut d| d.next().is_none())
        .unwrap_or(true)
}

/// Without the pinned triple there is no run and no fallback to `PATH`.
#[test]
fn a_run_without_the_pinned_tools_refuses() {
    let e = Engagement::new();
    e.checkout("acme-api");
    e.select(&[("acme-api", "repos/acme-api")]);

    let (_, stderr, code) = e.run();
    assert_ne!(code, Some(0), "{stderr}");
    assert!(stderr.contains("trusty-audit install"), "{stderr}");
    assert!(e.progress().is_empty(), "no run may be recorded");
    assert!(is_empty(&e.work.join("out")));
}

/// Nothing selected is a refusal, never a zero-repository success.
#[test]
fn a_run_with_nothing_selected_refuses() {
    let e = Engagement::new();
    e.install_stubs("#!/bin/sh\nexit 0\n");

    let (_, stderr, code) = e.run();
    assert_ne!(code, Some(0), "{stderr}");
    assert!(stderr.contains("nothing to audit"), "{stderr}");
    assert!(e.progress().is_empty());
}

/// The guard this file exists for: one repository of two fails, the sweep
/// completes, and the process still does not exit 0.
#[test]
fn a_partial_sweep_exits_non_zero_and_names_the_failure() {
    let e = Engagement::new();
    // Succeed for acme-api, fail for acme-web — the config path names the repo.
    e.install_stubs("#!/bin/sh\ncase \"$*\" in *acme-web*) exit 4;; esac\nexit 0\n");
    e.checkout("acme-api");
    e.checkout("acme-web");
    e.select(&[
        ("acme-api", "repos/acme-api"),
        ("acme-web", "repos/acme-web"),
    ]);

    let (stdout, stderr, code) = e.run();
    assert_eq!(code, Some(1), "stdout: {stdout}\nstderr: {stderr}");
    assert!(stdout.contains("PARTIAL"), "{stdout}");
    assert!(stdout.contains("FAILED  acme-web"), "{stdout}");
    assert!(stdout.contains("ok      acme-api"), "{stdout}");

    let progress = e.progress();
    assert!(progress.contains("Partial"), "{progress}");
}

/// Every repository failing is distinct from some of them failing.
#[test]
fn a_total_failure_is_reported_as_one() {
    let e = Engagement::new();
    e.install_stubs("#!/bin/sh\nexit 5\n");
    e.checkout("acme-api");
    e.select(&[("acme-api", "repos/acme-api")]);

    let (stdout, _, code) = e.run();
    assert_eq!(code, Some(1));
    assert!(stdout.contains("no repository was audited"), "{stdout}");
    assert!(!stdout.contains("PARTIAL"), "{stdout}");
    assert!(e.progress().contains("AllFailed"), "{}", e.progress());
}

/// The clean path, so the non-zero exits above are not vacuous.
#[test]
fn a_clean_sweep_exits_zero() {
    let e = Engagement::new();
    e.install_stubs("#!/bin/sh\nexit 0\n");
    e.checkout("acme-api");
    e.select(&[("acme-api", "repos/acme-api")]);

    let (stdout, stderr, code) = e.run();
    assert_eq!(code, Some(0), "stdout: {stdout}\nstderr: {stderr}");
    assert!(stdout.contains("Audited 1 repository"), "{stdout}");
}
