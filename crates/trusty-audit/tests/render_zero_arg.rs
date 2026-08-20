//! `trusty-audit render`, with nothing after it, in an unzipped package (#6080).
//!
//! Why: the recipient of a finished audit is given ONE command. Every unit test
//! for the re-render names its source, its output and its renderer, so all three
//! defaults were provable only by driving the real binary with an argv of one
//! word and a real working directory — which is exactly the invocation that did
//! not work: the source defaulted to `~/.trusty-tools/trusty-audit/work`, empty
//! on a recipient's machine, while they stood in the package.
//!
//! What: an unzipped package on disk — `engagement.toml` at its root, one
//! `reports/<stem>/manifest.toml` under it — a stub `trusty-review` on `PATH`,
//! and `HOME` pointed at a throwaway directory so nothing reads or writes the
//! developer's own working directory.
//! Test: this is the test.

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;

const TAUDIT: &str = env!("CARGO_BIN_EXE_taudit");

/// The config the handoff package leaves at the root of the unzipped tree.
const CONFIG: &str = r#"
openrouter_key = "sk-or-v1-not-a-real-key"
instructions = "Assess the last 52 weeks."

[tools]
tga = "2.9.4"
trusty-search = "0.47.0"
trusty-analyze = "0.9.2"
trusty-review = "0.15.1"
"#;

/// A manifest naming a checkout that is not on this machine, which is what a
/// delivered package always carries — the re-render states it as a gap.
const MANIFEST: &str = "[report]\ntitle = \"Acme\"\n\n[[repositories]]\n\
                        name = \"acme/api\"\npath = \"/nowhere/acme-api\"\n";

/// The version the stub renderer answers, standing in for the stale binary the
/// live run picked up off `PATH` (#6080).
const STUB_VERSION: &str = "0.18.0";

/// A `trusty-review` that writes one report into `--out`, like the real one,
/// and answers `--version` like the real one too — which is what the run has to
/// report back when nothing but `PATH` chose it.
const WRITES_A_REPORT: &str = "#!/bin/sh\n\
    case \"$1\" in --version) echo 'trusty-review 0.18.0'; exit 0;; esac\n\
    out=\"\"\nwhile [ $# -gt 0 ]; do\n  \
    case \"$1\" in --out) out=\"$2\"; shift;; esac\n  shift\ndone\n\
    mkdir -p \"$out\"\nprintf '# report\\n' > \"$out/report.md\"\nexit 0\n";

/// A machine holding an unzipped audit package and nothing else.
struct Recipient {
    tmp: tempfile::TempDir,
    /// The directory that came out of the zip — where the recipient stands.
    package: PathBuf,
    /// Prepended to the child's `PATH`, holding the stub `trusty-review`.
    bin: PathBuf,
    /// Stands in for the recipient's home, so `HOME` is never the real one.
    home: PathBuf,
}

impl Recipient {
    /// An unzipped package with `stems.len()` reports and a config at its root.
    fn new(stems: &[&str]) -> Self {
        let tmp = tempfile::tempdir().expect("tempdir");
        let package = tmp.path().join("acme-audit");
        let bin = tmp.path().join("bin");
        let home = tmp.path().join("home");
        for dir in [&bin, &home] {
            std::fs::create_dir_all(dir).expect("mkdir");
        }
        std::fs::create_dir_all(&package).expect("mkdir package");
        std::fs::write(package.join("engagement.toml"), CONFIG).expect("write config");
        let recipient = Self {
            tmp,
            package,
            bin,
            home,
        };
        for stem in stems {
            recipient.report(&recipient.package, stem);
        }
        write_script(&recipient.bin.join("trusty-review"), WRITES_A_REPORT);
        recipient
    }

    /// One `reports/<stem>/manifest.toml` under `root`, as the package ships it.
    fn report(&self, root: &Path, stem: &str) -> PathBuf {
        let dir = root.join("reports").join(stem);
        std::fs::create_dir_all(&dir).expect("mkdir report");
        std::fs::write(dir.join("manifest.toml"), MANIFEST).expect("write manifest");
        dir
    }

    /// One `out/<stem>/manifest.toml` under `root`, which is where a sweep run
    /// with `--work-dir <root>` leaves its reports (#6080).
    fn work_dir_report(&self, root: &Path, stem: &str) -> PathBuf {
        let dir = root.join("out").join(stem);
        std::fs::create_dir_all(&dir).expect("mkdir report");
        std::fs::write(dir.join("manifest.toml"), MANIFEST).expect("write manifest");
        dir
    }

    /// One `taudit` run INSIDE the unzipped package, with nothing exported.
    ///
    /// No `--work-dir`, no `--config`: whatever this run finds it has to find
    /// from the directory it was started in. `HOME` is the throwaway one and
    /// `OPENROUTER_API_KEY` is removed, so the key can only come from the
    /// config beside the package.
    fn render(&self, args: &[&str]) -> (String, String, Option<i32>) {
        self.render_with_key(args, None)
    }

    /// The same run, with `OPENROUTER_API_KEY` exported.
    fn render_with_key(&self, args: &[&str], key: Option<&str>) -> (String, String, Option<i32>) {
        let path = match std::env::var("PATH") {
            Ok(existing) => format!("{}:{existing}", self.bin.display()),
            Err(_) => self.bin.display().to_string(),
        };
        let mut command = Command::new(TAUDIT);
        command
            .arg("render")
            .args(args)
            .env("PATH", path)
            .env("HOME", &self.home)
            .env_remove("TRUSTY_AUDIT_WORKDIR")
            .current_dir(&self.package);
        match key {
            Some(value) => command.env("OPENROUTER_API_KEY", value),
            None => command.env_remove("OPENROUTER_API_KEY"),
        };
        let out = command.output().expect("taudit runs");
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code(),
        )
    }

    fn path(&self, relative: &str) -> PathBuf {
        self.tmp.path().join(relative)
    }
}

fn write_script(path: &Path, body: &str) {
    std::fs::write(path, body).expect("write script");
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
}

/// 🔴 The owner requirement, verbatim: run `trusty-audit render` from the
/// directory the config lives in, with just the render argument, and it works.
#[test]
fn render_with_no_arguments_renders_the_package_it_was_run_in() {
    let r = Recipient::new(&["01-acme-api", "02-acme-web"]);

    let (stdout, stderr, code) = r.render(&[]);

    assert_eq!(code, Some(0), "stdout: {stdout}\nstderr: {stderr}");
    for stem in ["01-acme-api", "02-acme-web"] {
        let report = r.package.join("rerendered").join(stem).join("report.md");
        assert!(report.is_file(), "{} missing\n{stdout}", report.display());
    }
    assert!(
        stdout.contains(&r.package.display().to_string()),
        "the run must name the package it read\n{stdout}"
    );
    assert!(
        stdout.contains("Re-rendered 2 reports"),
        "stdout: {stdout}\nstderr: {stderr}"
    );
}

/// 🔴 The owner requirement against the LIVE layout (#6080): the config is in
/// the directory the operator is standing in, every sweep ran with
/// `--work-dir <that directory>/work`, and `trusty-audit render` with nothing
/// after it must render those reports.
///
/// Against `397446da5` this exited 1 with
/// `no manifest.toml under /Users/masa/dogfood-audit — point --from …`: the cwd
/// itself carries no report, and the fallback went straight to the TOOL's
/// default work root, which on that machine held nothing.
#[test]
fn render_with_no_arguments_finds_the_engagements_own_work_dir() {
    let r = Recipient::new(&[]);
    let work = r.package.join("work");
    for stem in ["01-acme-api", "02-acme-web"] {
        r.work_dir_report(&work, stem);
    }

    let (stdout, stderr, code) = r.render(&[]);

    assert_eq!(code, Some(0), "stdout: {stdout}\nstderr: {stderr}");
    for stem in ["01-acme-api", "02-acme-web"] {
        let report = work.join("rerendered").join(stem).join("report.md");
        assert!(report.is_file(), "{} missing\n{stdout}", report.display());
    }
    assert!(
        stdout.contains(&work.display().to_string()),
        "the run must name the work dir it read\n{stdout}"
    );
    assert!(
        stdout.contains("Re-rendered 2 reports"),
        "stdout: {stdout}\nstderr: {stderr}"
    );
}

/// 🔴 A refusal names every location the run tried — the directory it was run
/// in, that directory's own `work/`, and the tool's default work root. Naming
/// only the first is what sent the live operator hunting through a directory
/// they could already see.
#[test]
fn a_refusal_names_every_location_it_looked_in() {
    let r = Recipient::new(&[]);

    let (stdout, stderr, code) = r.render(&[]);

    assert_ne!(code, Some(0), "stdout: {stdout}\nstderr: {stderr}");
    for expected in [
        r.package.clone(),
        r.package.join("work"),
        r.home.join(".trusty-tools/trusty-audit/work"),
    ] {
        assert!(
            stderr.contains(&expected.display().to_string()),
            "{} is missing from the refusal\n{stderr}",
            expected.display()
        );
    }
}

/// 🔴 #6080: a renderer resolved off `PATH` — nobody's choice, and subject to no
/// pin check — is disclosed with its path and its version, in the run's own
/// output. The live run silently used a `trusty-review` two minor versions
/// behind the engagement's pin and exited 0.
#[test]
fn a_renderer_taken_from_the_path_is_disclosed() {
    let r = Recipient::new(&["01-acme-api"]);

    let (stdout, stderr, code) = r.render(&[]);

    assert_eq!(code, Some(0), "stdout: {stdout}\nstderr: {stderr}");
    let disclosure = stdout
        .lines()
        .find(|line| line.contains("PATH"))
        .unwrap_or_else(|| panic!("no PATH disclosure in the run output\n{stdout}"));
    assert!(
        disclosure.contains(&r.bin.join("trusty-review").display().to_string()),
        "the disclosure must name the binary that ran: {disclosure}"
    );
    assert!(
        disclosure.contains(STUB_VERSION),
        "the disclosure must carry the version it resolved: {disclosure}"
    );
}

/// The other half of that: a renderer the operator named is theirs, and saying
/// so of every run would train them to skip the line that matters.
#[test]
fn a_named_renderer_is_not_disclosed() {
    let r = Recipient::new(&["01-acme-api"]);
    let named = r.bin.join("trusty-review");

    let (stdout, stderr, code) = r.render(&["--review-bin", &named.display().to_string()]);

    assert_eq!(code, Some(0), "stdout: {stdout}\nstderr: {stderr}");
    assert!(
        !stdout.contains("resolved on PATH"),
        "a renderer named with --review-bin must not be disclosed\n{stdout}"
    );
}

/// A recipient who was sent a key instead of a config still renders: the config
/// is one of two sources for the key, never a requirement of the verb.
#[test]
fn an_exported_key_renders_with_no_config_at_all() {
    let r = Recipient::new(&["01-acme-api"]);
    std::fs::remove_file(r.package.join("engagement.toml")).expect("remove config");

    let (stdout, stderr, code) = r.render_with_key(&[], Some("sk-or-v1-exported-not-real"));

    assert_eq!(code, Some(0), "stdout: {stdout}\nstderr: {stderr}");
    assert!(
        r.package.join("rerendered/01-acme-api/report.md").is_file(),
        "stdout: {stdout}"
    );
}

/// 🔴 No config in the directory and no exported key: the refusal names both
/// places that were looked in, because the recipient has to know which one to
/// fix.
#[test]
fn a_missing_config_names_what_was_looked_for() {
    let r = Recipient::new(&["01-acme-api"]);
    std::fs::remove_file(r.package.join("engagement.toml")).expect("remove config");

    let (stdout, stderr, code) = r.render(&[]);

    assert_ne!(code, Some(0), "stdout: {stdout}\nstderr: {stderr}");
    assert!(stderr.contains("OPENROUTER_API_KEY"), "{stderr}");
    assert!(
        stderr.contains(&r.package.join("engagement.toml").display().to_string()),
        "the refusal must name the config it looked for\n{stderr}"
    );
    assert!(
        !r.package.join("rerendered").exists(),
        "a refused run must write nothing"
    );
}

/// 🔴 Defaults never take a flag away: `--from` and `--out` still decide, and
/// the directory the command was run in is then left alone entirely.
#[test]
fn explicit_flags_still_win_over_the_defaults() {
    let r = Recipient::new(&["01-acme-api"]);
    let other = r.path("elsewhere");
    r.report(&other, "07-other-repo");
    let out = r.path("named-output");

    let (stdout, stderr, code) = r.render(&[
        "--from",
        &other.display().to_string(),
        "--out",
        &out.display().to_string(),
    ]);

    assert_eq!(code, Some(0), "stdout: {stdout}\nstderr: {stderr}");
    assert!(
        out.join("07-other-repo/report.md").is_file(),
        "stdout: {stdout}"
    );
    assert!(
        !r.package.join("rerendered").exists(),
        "the cwd must not be rendered when --from names another directory"
    );
}
