//! Unit tests for `deployment_probe` and the #10607 review fixes.
//!
//! Why: the code-critic BLOCK on #10607 named a hang, a wrong remedy, an
//! unprobeable daemon read as clean, and an untested record-and-audit step.
//! What: the hang tests run real shell scripts through the production runner
//! with a short deadline; the rest use a scripted runner.
//! Test: this is the test module.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::*;
use crate::commands::content_step::RunOutput;
use crate::commands::deployment_record::{
    audit, finish, finish_at, load, record_one, record_placements, DeploymentAudit, DriftKind,
    Placement, RecordLoad, RECORD_FILE,
};
use crate::commands::stable_set::stable_set;

/// Answers `--version` with `1.0.0` and every other verb with success.
struct Answers;

impl CommandRunner for Answers {
    fn run(&self, _program: &Path, args: &[&str]) -> std::io::Result<RunOutput> {
        let stdout = if args == ["--version"] {
            "member 1.0.0\n"
        } else {
            "ok\n"
        };
        Ok(RunOutput {
            success: true,
            code: Some(0),
            stdout: stdout.to_owned(),
            stderr: String::new(),
        })
    }
}

/// Writes an executable shell script `name` with `body` into `dir`.
#[cfg(unix)]
fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join(name);
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).expect("write script");
    let mut perms = std::fs::metadata(&path).expect("stat").permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&path, perms).expect("chmod");
    path
}

/// A record in `dir` holding one member placed at `bin`, version 1.0.0.
fn record_of(dir: &Path, member: &str, bin: &Path) -> RecordLoad {
    let record = dir.join(RECORD_FILE);
    record_placements(&record, &[Placement::new(member, bin, "1.0.0")]).expect("record");
    load(&record)
}

fn none_running(_: &str) -> RunningVersion {
    RunningVersion::NotApplicable
}

/// Why: #10607 review blocker 1 — `stack health` with a hung member wrote
/// nothing and was killed at 20s.
/// What: a binary that hangs on `--version`, and a trusty-review that hangs on
/// its content check, each finish inside the deadline as a check that could
/// not run, which blocks an install and never reads clean.
/// Test: This is the test.
#[cfg(unix)]
#[test]
fn a_hung_member_is_a_failed_check_not_a_stall_10607() {
    let runner = ProcessRunner::with_deadline(Duration::from_millis(500));
    let cases = [
        ("tool", "exec sleep 30", DriftKind::Unverified),
        (
            "trusty-review",
            "case \"$1\" in --version) echo \"trusty-review 1.0.0\";; *) sleep 30;; esac",
            DriftKind::ContentUnverified,
        ),
    ];
    for (member, body, kind) in cases {
        let dir = tempfile::tempdir().expect("tempdir");
        let bin = script(dir.path(), member, body);
        let loaded = record_of(dir.path(), member, &bin);
        let started = Instant::now();
        let result = audit(&loaded, &[], &runner, &none_running);
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "{member}: the audit waited {:?} on a hung binary",
            started.elapsed()
        );
        let kinds: Vec<DriftKind> = result.findings.iter().map(|f| f.kind).collect();
        assert_eq!(kinds, [kind], "{member}: {result:?}");
        assert!(
            result.findings[0].detail.contains("did not exit"),
            "{result:?}"
        );
        assert_ne!(result.state, "clean");
        assert!(result.blocks_install());
    }
}

/// Why: the deadline must not cost a probe that exits its output or its code.
/// What: the production runner returns a child's stdout, stderr and exit code;
/// [`read_version`] reads a placed binary's version through it.
/// Test: This is the test.
#[cfg(unix)]
#[test]
fn a_bounded_run_returns_what_the_child_printed_10607() {
    let dir = tempfile::tempdir().expect("tempdir");
    let noisy = script(dir.path(), "noisy", "echo out; echo err >&2; exit 3");
    let out = ProcessRunner::probe().run(&noisy, &[]).expect("ran");
    assert_eq!(
        (
            out.success,
            out.code,
            out.stdout.as_str(),
            out.stderr.as_str()
        ),
        (false, Some(3), "out\n", "err\n")
    );
    let versioned = script(dir.path(), "versioned", "echo \"versioned 1.2.3\"");
    assert_eq!(read_version(&versioned).as_deref(), Some("1.2.3"));
}

/// Why: #10607 review blocker 2 — the stale-daemon remedy named `tctl install`,
/// which never restarts a loaded daemon.
/// What: a daemon running another build names `tctl restart <member>`; a
/// missing binary still names `tctl install <member>`.
/// Test: This is the test.
#[test]
fn a_stale_daemon_names_tctl_restart_10607() {
    let dir = tempfile::tempdir().expect("tempdir");
    let bin = dir.path().join("tool");
    std::fs::write(&bin, b"build one").expect("write binary");
    let loaded = record_of(dir.path(), "tool", &bin);
    let old = |_: &str| RunningVersion::Reported("0.9.0".to_owned());
    let stale = audit(&loaded, &[], &Answers, &old);
    assert!(
        stale.findings[0]
            .detail
            .ends_with("run `tctl restart tool`"),
        "{stale:?}"
    );
    std::fs::remove_file(&bin).expect("remove binary");
    let missing = audit(&loaded, &[], &Answers, &none_running);
    assert!(
        missing.findings[0]
            .detail
            .ends_with("run `tctl install tool`"),
        "{missing:?}"
    );
}

/// Why: #10607 review blocker 5 — `tctl upgrade` read a daemon it could not
/// probe as clean.
/// What: through `finish_at` (the step upgrade runs), a timeout, an HTTP or RPC
/// error, an unreadable envelope and a probe that could not run each leave the
/// audit undetermined and blocking; nothing listening stays clean.
/// Test: This is the test.
#[test]
fn an_unprobeable_daemon_is_never_clean_10607() {
    let dir = tempfile::tempdir().expect("tempdir");
    let bin = dir.path().join("tool");
    std::fs::write(&bin, b"build one").expect("write binary");
    let path = dir.path().join(RECORD_FILE);
    let placed = [Placement::new("tool", &bin, "1.0.0")];
    let run = |outcome: ProbeOutcome| {
        let running = move |_: &str| classify_running(&outcome);
        finish_at(Ok(path.clone()), &placed, true, &Answers, &running).expect("audited")
    };
    let unprobeable = [
        ProbeOutcome::Timeout,
        ProbeOutcome::HttpError { status: 500 },
        ProbeOutcome::BadEnvelope {
            got: "<html>".to_owned(),
        },
        ProbeOutcome::RpcError {
            code: -32603,
            message: "boom".to_owned(),
        },
        ProbeOutcome::ProbeFailed {
            detail: "no runtime".to_owned(),
        },
        ProbeOutcome::Unprobeable,
    ];
    for outcome in unprobeable {
        let result: DeploymentAudit = run(outcome.clone());
        assert_eq!(result.state, "undetermined", "{outcome:?}: {result:?}");
        assert!(result.blocks_install(), "{outcome:?}");
        assert!(
            result.findings[0]
                .detail
                .contains("could not read its version"),
            "{result:?}"
        );
    }
    for outcome in [ProbeOutcome::Refused, ProbeOutcome::NoAddress] {
        assert_eq!(run(outcome.clone()).state, "clean", "{outcome:?}");
    }
}

/// Why: a member with no version contract must not be asked for one.
/// What: trusty-review (presence only) is not applicable; trusty-search's
/// served version is read.
/// Test: This is the test.
#[test]
fn running_of_skips_members_with_no_version_contract_10607() {
    let member = |binary: &str| {
        stable_set()
            .into_iter()
            .find(|m| m.binary == binary)
            .expect("a stable-set member")
    };
    let serving = ProbeOutcome::Serving {
        status: "ok".to_owned(),
        version: Some("1.0.0".to_owned()),
    };
    assert_eq!(
        running_of(&member("trusty-review"), &serving),
        RunningVersion::NotApplicable
    );
    assert_eq!(
        running_of(&member("trusty-search"), &serving),
        RunningVersion::Reported("1.0.0".to_owned())
    );
}

/// Why: #10607 review — `finish`, `record_one` and `read_version` are what
/// install, upgrade and self-update call, and no test reached them.
/// What: under a throwaway data dir, `finish` records a real placement and
/// audits it clean, skips the audit when asked, and `record_one` writes the
/// self-updated member.
/// Test: This is the test.
#[cfg(unix)]
#[test]
fn finish_records_under_the_data_dir_10607() {
    use crate::commands::test_support::{
        clear_data_dir_override, stub_empty_data_dir, ENV_TEST_LOCK,
    };
    let _guard = ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let data = stub_empty_data_dir("deployment-finish");
    let bin = script(&data, "tool", "echo \"tool 1.0.0\"");
    let record = data.join("trusty-installer").join(RECORD_FILE);
    let placed = [Placement::new("tool", &bin, "1.0.0")];

    let audited = finish(&placed, true);
    let skipped = finish(&placed, false);
    let one = record_one(&Placement::new("other", &bin, "1.0.0"));
    let loaded = load(&record);
    clear_data_dir_override(&data);

    assert_eq!(audited.map(|a| a.state), Some("clean"));
    assert_eq!(skipped, None);
    assert_eq!(one, Ok(()));
    let RecordLoad::Loaded(loaded) = loaded else {
        panic!("no record written: {loaded:?}");
    };
    let members: Vec<&String> = loaded.members.keys().collect();
    assert_eq!(members, ["other", "tool"]);
}
