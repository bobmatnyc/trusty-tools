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
trusty-search = "0.47.0"
trusty-analyze = "0.9.2"
trusty-review = "0.15.1"
"#;

/// The directory name `--work-dir` is spelled with, relative to the engagement.
const WORK_DIR_NAME: &str = "trusty-audit-work";

struct Engagement {
    tmp: tempfile::TempDir,
    work: PathBuf,
    config: PathBuf,
}

impl Engagement {
    fn new() -> Self {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = tmp.path().join(WORK_DIR_NAME);
        let config = tmp.path().join("engagement.toml");
        std::fs::write(&config, CONFIG).expect("write engagement config");
        for area in ["tools", "repos", "extract", "state", "out", "logs"] {
            std::fs::create_dir_all(work.join(area)).expect("mkdir area");
        }
        Self { tmp, work, config }
    }

    /// A stub `tga` that writes the manifest a real one would — a zero exit
    /// with no manifest is a FAILURE, so a test expecting success needs this.
    fn manifest_writer(exit_for: Option<&str>) -> String {
        let fail = match exit_for {
            Some(pattern) => format!("case \"$*\" in *{pattern}*) exit 4;; esac\n"),
            None => String::new(),
        };
        format!(
            "#!/bin/sh\n{fail}out=\"\"\nwhile [ $# -gt 0 ]; do\n  \
             case \"$1\" in --output) out=\"$2\"; shift;; esac\n  shift\ndone\n\
             mkdir -p \"$out\"\n\
             printf '[report]\\ntitle = \"Acme\"\\n\\n[[repositories]]\\n\
             name = \"acme\"\\npath = \"/r\"\\n' > \"$out/manifest.toml\"\nexit 0\n"
        )
    }

    /// Stand in for a verified install: four executables plus the record that
    /// says this client placed them.
    fn install_stubs(&self, script: &str) {
        for name in ["tga", "trusty-search", "trusty-analyze", "trusty-review"] {
            let path = self.work.join("tools").join(name);
            std::fs::write(&path, script).expect("stub");
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        }
        let record = format!(
            "[[tools]]\ncrate_name = \"tga\"\nversion = \"2.9.4\"\nbinary = \"{d}/tga\"\n\
             [[tools]]\ncrate_name = \"trusty-search\"\nversion = \"0.47.0\"\nbinary = \"{d}/s\"\n\
             [[tools]]\ncrate_name = \"trusty-analyze\"\nversion = \"0.9.2\"\nbinary = \"{d}/a\"\n\
             [[tools]]\ncrate_name = \"trusty-review\"\nversion = \"0.15.1\"\nbinary = \"{d}/r\"\n",
            d = self.work.join("tools").display()
        );
        std::fs::write(self.work.join("state/tool-versions.toml"), record).expect("record");
    }

    fn select(&self, entries: &[(&str, &str)]) {
        // `count` first: the reader uses it to detect a truncated write.
        let mut text = format!("count = {}\n\n", entries.len());
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
        let mut command = Command::new(TAUDIT);
        command
            .args(["run", "--work-dir"])
            .arg(&self.work)
            .arg("--config")
            .arg(&self.config);
        report(&mut command)
    }

    /// The same run with the flags spelled RELATIVE, from a shell sitting in
    /// the engagement directory — the shape #5672 broke.
    ///
    /// The child's cwd is set here rather than the test process's: mutating
    /// the process cwd would race every other test in this binary.
    fn run_relative(&self) -> (String, String, Option<i32>) {
        let mut command = Command::new(TAUDIT);
        command
            .args([
                "run",
                "--work-dir",
                WORK_DIR_NAME,
                "--config",
                "engagement.toml",
            ])
            .current_dir(self.tmp.path());
        report(&mut command)
    }

    fn progress(&self) -> String {
        std::fs::read_to_string(self.work.join("state/run-progress.toml")).unwrap_or_default()
    }
}

/// Run one `taudit` and split its result into stdout, stderr and status.
fn report(command: &mut Command) -> (String, String, Option<i32>) {
    let out = command.output().expect("taudit runs");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code(),
    )
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
    e.install_stubs(&Engagement::manifest_writer(Some("acme-web")));
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

/// A zero exit that produced no manifest is a failure, from the outside.
#[test]
fn a_child_that_exits_zero_having_written_nothing_exits_non_zero() {
    let e = Engagement::new();
    e.install_stubs("#!/bin/sh\nexit 0\n");
    e.checkout("acme-api");
    e.select(&[("acme-api", "repos/acme-api")]);

    let (stdout, _, code) = e.run();
    assert_eq!(code, Some(1), "{stdout}");
    assert!(stdout.contains("wrote no manifest"), "{stdout}");
}

/// The clean path, so the non-zero exits above are not vacuous.
#[test]
fn a_clean_sweep_exits_zero() {
    let e = Engagement::new();
    e.install_stubs(&Engagement::manifest_writer(None));
    e.checkout("acme-api");
    e.select(&[("acme-api", "repos/acme-api")]);

    let (stdout, stderr, code) = e.run();
    assert_eq!(code, Some(0), "stdout: {stdout}\nstderr: {stderr}");
    assert!(stdout.contains("Audited 1 repository"), "{stdout}");
}

/// #5672: the same clean sweep, driven with a RELATIVE `--work-dir`.
///
/// Against `origin/main` this fails. `spawn_tga` makes the work-dir root the
/// child's working directory while naming the tool by a path relative to the
/// PARENT's, so the child looked for `trusty-audit-work/trusty-audit-work/
/// tools/tga`, the spawn returned `os error 2`, and the sweep exited 1 with
/// "`tga audit` could not be started".
#[test]
fn a_clean_sweep_with_a_relative_work_dir_exits_zero() {
    let e = Engagement::new();
    e.install_stubs(&Engagement::manifest_writer(None));
    e.checkout("acme-api");
    e.select(&[("acme-api", "repos/acme-api")]);

    let (stdout, stderr, code) = e.run_relative();
    assert!(
        !stdout.contains("could not be started"),
        "the child never started — the tool path was relative to its own cwd:\n{stdout}"
    );
    assert_eq!(code, Some(0), "stdout: {stdout}\nstderr: {stderr}");
    assert!(stdout.contains("Audited 1 repository"), "{stdout}");
}
