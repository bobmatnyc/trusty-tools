//! Unit tests for `deployment_record` (#10607).
//!
//! Why: each drift kind and each check that cannot run needs a test that reaches
//! its arm; a daemon or a real binary would not make those arms reachable on demand.
//! What: real files in a tempdir for digests and presence, a scripted runner for
//! `--version` and the content verbs. No test here spawns a process.
//! Test: this is the test module.

use std::cell::Cell;

use super::*;
use crate::commands::content_step::RunOutput;

/// Answers `--version` with `version` and the content verbs with `content`.
struct Scripted {
    version: Result<String, String>,
    content: fn() -> std::io::Result<RunOutput>,
    runs: Cell<u32>,
}

fn ok_out(stdout: &str) -> RunOutput {
    RunOutput {
        success: true,
        code: Some(0),
        stdout: stdout.to_owned(),
        stderr: String::new(),
    }
}

fn failed_out(code: i32, stderr: &str) -> RunOutput {
    RunOutput {
        success: false,
        code: Some(code),
        stdout: String::new(),
        stderr: stderr.to_owned(),
    }
}

fn content_ok() -> std::io::Result<RunOutput> {
    Ok(ok_out("content serves it\n"))
}

impl Scripted {
    fn reporting(version: &str) -> Self {
        Self {
            version: Ok(format!("member {version}\n")),
            content: content_ok,
            runs: Cell::new(0),
        }
    }

    fn with_content(mut self, content: fn() -> std::io::Result<RunOutput>) -> Self {
        self.content = content;
        self
    }
}

impl CommandRunner for Scripted {
    fn run(&self, _program: &Path, args: &[&str]) -> std::io::Result<RunOutput> {
        self.runs.set(self.runs.get() + 1);
        if args == ["--version"] {
            return self
                .version
                .clone()
                .map(|v| ok_out(&v))
                .map_err(std::io::Error::other);
        }
        (self.content)()
    }
}

fn not_applicable(_: &str) -> RunningVersion {
    RunningVersion::NotApplicable
}

/// A tempdir with a recorded `member` binary at version 1.0.0.
fn recorded(name: &str) -> (tempfile::TempDir, PathBuf, PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let bin = dir.path().join(name);
    std::fs::write(&bin, b"build one").expect("write binary");
    let record = dir.path().join(RECORD_FILE);
    record_placements(&record, &[Placement::new(name, &bin, "1.0.0")]).expect("record");
    (dir, bin, record)
}

fn kinds(audit: &DeploymentAudit) -> Vec<DriftKind> {
    audit.findings.iter().map(|f| f.kind).collect()
}

fn run_audit(record: &Path, runner: &Scripted) -> DeploymentAudit {
    audit(&load(record), &[], runner, &not_applicable)
}

/// Why: the passing case every drift test is measured against.
/// What: a matching binary audits `clean`, and the audit ran `--version`.
/// Test: this.
#[test]
fn a_matching_deployment_is_clean() {
    let (_dir, _bin, record) = recorded("tool");
    let runner = Scripted::reporting("1.0.0");
    let result = run_audit(&record, &runner);
    assert_eq!(result.state, "clean", "{result:?}");
    assert_eq!(runner.runs.get(), 1, "`--version` ran once");
}

/// Why: #10607 "a package is missing".
/// What: a recorded binary that is gone is `drifted`, naming its path.
/// Test: this.
#[test]
fn a_missing_binary_is_drift() {
    let (_dir, bin, record) = recorded("tool");
    std::fs::remove_file(&bin).expect("remove");
    let result = run_audit(&record, &Scripted::reporting("1.0.0"));
    assert_eq!(result.state, "drifted");
    assert_eq!(kinds(&result), [DriftKind::MissingBinary]);
    assert!(result.findings[0].detail.contains("tool"), "{result:?}");
}

/// Why: #10607 "the wrong version".
/// What: a binary reporting another version is `drifted`, naming both versions.
/// Test: this.
#[test]
fn a_wrong_version_is_drift() {
    let (_dir, _bin, record) = recorded("tool");
    let result = run_audit(&record, &Scripted::reporting("0.9.0"));
    assert_eq!(result.state, "drifted");
    assert_eq!(kinds(&result), [DriftKind::WrongVersion]);
    let detail = &result.findings[0].detail;
    assert!(
        detail.contains("0.9.0") && detail.contains("1.0.0"),
        "{detail}"
    );
}

/// Why: #10607 "the staged interim build": same version string, other bytes.
/// What: a changed file at the placed version is `drifted` as `build_changed`.
/// Test: this.
#[test]
fn a_changed_build_at_the_same_version_is_drift() {
    let (_dir, bin, record) = recorded("tool");
    std::fs::write(&bin, b"build two").expect("rewrite");
    let result = run_audit(&record, &Scripted::reporting("1.0.0"));
    assert_eq!(result.state, "drifted");
    assert_eq!(kinds(&result), [DriftKind::BuildChanged]);
}

/// Why: #10607 "mixed": a reinstall that left the old daemon running.
/// What: a daemon reporting another version than the placed binary is `drifted`;
/// the same version, with a leading `v`, is clean.
/// Test: this.
#[test]
fn a_daemon_running_another_version_is_drift() {
    let (_dir, _bin, record) = recorded("tool");
    let runner = Scripted::reporting("1.0.0");
    let old = |_: &str| RunningVersion::Reported("0.9.0".to_owned());
    let result = audit(&load(&record), &[], &runner, &old);
    assert_eq!(kinds(&result), [DriftKind::RunningVersion]);
    assert_eq!(result.state, "drifted");
    let same = |_: &str| RunningVersion::Reported("v1.0.0".to_owned());
    assert_eq!(audit(&load(&record), &[], &runner, &same).state, "clean");
}

/// Why: #10607 "stale": the daemon's own below-floor word.
/// What: `stale` and `version_below_floor` read as below floor, and that is
/// drift. `degraded` shares the rollup's `stale` row but says nothing about the
/// version, so it stays a version comparison.
/// Test: this.
#[test]
fn a_daemon_below_its_floor_is_drift() {
    let serving = |status: &str| ProbeOutcome::Serving {
        status: status.to_owned(),
        version: Some("1.0.0".to_owned()),
    };
    for word in ["stale", "version_below_floor"] {
        assert_eq!(
            classify_running(&serving(word)),
            RunningVersion::BelowFloor(word.to_owned())
        );
    }
    assert_eq!(
        classify_running(&serving("degraded")),
        RunningVersion::Reported("1.0.0".to_owned())
    );
    let (_dir, _bin, record) = recorded("tool");
    let below = |_: &str| RunningVersion::BelowFloor("stale".to_owned());
    let result = audit(&load(&record), &[], &Scripted::reporting("1.0.0"), &below);
    assert_eq!(kinds(&result), [DriftKind::RunningBelowFloor]);
}

/// Why: #10607 Fail-Open Check on the probe mapping.
/// What: a failed probe says nothing about the version; a healthy answer naming
/// no version is `unreported`, which the audit reads as undetermined.
/// Test: this.
#[test]
fn classify_running_reads_each_probe_outcome() {
    assert_eq!(
        classify_running(&ProbeOutcome::Refused),
        RunningVersion::NotServing
    );
    let unnamed = ProbeOutcome::Serving {
        status: "ok".to_owned(),
        version: None,
    };
    assert_eq!(classify_running(&unnamed), RunningVersion::Unreported);
    let (_dir, _bin, record) = recorded("tool");
    let unreported = |_: &str| RunningVersion::Unreported;
    let result = audit(
        &load(&record),
        &[],
        &Scripted::reporting("1.0.0"),
        &unreported,
    );
    assert_eq!(kinds(&result), [DriftKind::RunningUnreported]);
    assert_eq!(result.state, "undetermined");
}

/// Why: #10607 "content": the deployed content does not serve the placed binary.
/// What: a failing `content check` is `drifted`, carrying the last stderr line
/// that names the missing file.
/// Test: this.
#[test]
fn content_that_fails_its_check_is_drift() {
    let (_dir, _bin, record) = recorded("trusty-review");
    let runner = Scripted::reporting("1.0.0")
        .with_content(|| Ok(failed_out(1, "missing prompts/review.md\n")));
    let result = run_audit(&record, &runner);
    assert_eq!(result.state, "drifted", "{result:?}");
    assert_eq!(kinds(&result), [DriftKind::ContentNotServed]);
    assert!(result.findings[0].detail.contains("prompts/review.md"));
}

/// Why: #10607 "content": tm with no verified content cannot compose a session.
/// What: a failing `tm content status` on a recorded trusty-mpm is `drifted`.
/// Test: this.
#[test]
fn trusty_mpm_without_verified_content_is_drift() {
    let (_dir, _bin, record) = recorded("trusty-mpm");
    let runner =
        Scripted::reporting("1.0.0").with_content(|| Ok(failed_out(1, "no verified content\n")));
    assert_eq!(
        kinds(&run_audit(&record, &runner)),
        [DriftKind::ContentNotServed]
    );
}

/// Why: Fail-Open Check, and #10588 must not regress: a trusty-review older than
/// the verb passes install as "not checked".
/// What: exit 2 with clap's unknown-verb text reads undetermined in health and
/// does not block an install.
/// Test: this.
#[test]
fn an_older_review_blocks_no_install_but_health_is_undetermined() {
    let (_dir, _bin, record) = recorded("trusty-review");
    let runner = Scripted::reporting("1.0.0")
        .with_content(|| Ok(failed_out(2, "error: unrecognized subcommand 'content'\n")));
    let result = run_audit(&record, &runner);
    assert_eq!(kinds(&result), [DriftKind::ContentUncheckable]);
    assert_eq!(result.state, "undetermined");
    assert!(!result.blocks_install());
}

/// Why: Fail-Open Check: a content check that could not start is not a pass.
/// What: the runner's spawn error reads undetermined and blocks an install.
/// Test: this.
#[test]
fn a_content_check_that_cannot_run_is_undetermined() {
    let (_dir, _bin, record) = recorded("trusty-review");
    let runner = Scripted::reporting("1.0.0").with_content(|| Err(std::io::Error::other("nope")));
    let result = run_audit(&record, &runner);
    assert_eq!(kinds(&result), [DriftKind::ContentUnverified]);
    assert_eq!(result.state, "undetermined");
    assert!(result.blocks_install());
}

/// Why: Fail-Open Check: a `--version` that cannot run must not read as a match.
/// What: a spawn failure, a non-zero exit and output with no version each read
/// undetermined.
/// Test: this.
#[test]
fn a_version_that_cannot_be_read_is_undetermined() {
    let (_dir, _bin, record) = recorded("tool");
    let spawn_failed = Scripted {
        version: Err("cannot exec".to_owned()),
        content: content_ok,
        runs: Cell::new(0),
    };
    let no_version = Scripted {
        version: Ok("no version here\n".to_owned()),
        content: content_ok,
        runs: Cell::new(0),
    };
    for runner in [spawn_failed, no_version] {
        let result = run_audit(&record, &runner);
        assert_eq!(kinds(&result), [DriftKind::Unverified]);
        assert_eq!(result.state, "undetermined");
    }
    struct Exits1;
    impl CommandRunner for Exits1 {
        fn run(&self, _: &Path, _: &[&str]) -> std::io::Result<RunOutput> {
            Ok(failed_out(1, "boom"))
        }
    }
    let result = audit(&load(&record), &[], &Exits1, &not_applicable);
    assert_eq!(kinds(&result), [DriftKind::Unverified]);
}

/// Why: Fail-Open Check: a digest that was never recorded compares nothing.
/// What: a recorded binary with no digest reads undetermined.
/// Test: this.
#[test]
fn a_binary_with_no_recorded_digest_is_undetermined() {
    let (dir, bin, _record) = recorded("tool");
    let mut members = BTreeMap::new();
    members.insert(
        "tool".to_owned(),
        vec![RecordedBinary {
            name: "tool".to_owned(),
            path: bin,
            version: Some("1.0.0".to_owned()),
            sha256: None,
        }],
    );
    let record = dir.path().join("no-digest.json");
    write(
        &record,
        &DeploymentRecord {
            schema: SCHEMA,
            members,
        },
    )
    .expect("write");
    let result = run_audit(&record, &Scripted::reporting("1.0.0"));
    assert_eq!(kinds(&result), [DriftKind::Unverified]);
}

/// Why: Fail-Open Check: no record means nothing was compared.
/// What: an absent record and an empty one both read undetermined, not clean.
/// Test: this.
#[test]
fn no_record_is_undetermined() {
    let dir = tempfile::tempdir().expect("tempdir");
    let absent = dir.path().join("absent.json");
    let result = run_audit(&absent, &Scripted::reporting("1.0.0"));
    assert_eq!(result.state, "undetermined");
    assert_eq!(kinds(&result), [DriftKind::NoRecord]);

    let empty = dir.path().join("empty.json");
    record_placements(&empty, &[]).expect("write");
    assert_eq!(
        kinds(&run_audit(&empty, &Scripted::reporting("1.0.0"))),
        [DriftKind::NoRecord]
    );
}

/// Why: Fail-Open Check: a record that cannot be read must not read as empty.
/// What: invalid JSON reads undetermined, naming the file.
/// Test: this.
#[test]
fn an_unparseable_record_is_undetermined() {
    let dir = tempfile::tempdir().expect("tempdir");
    let record = dir.path().join(RECORD_FILE);
    std::fs::write(&record, "{not json").expect("write");
    let result = run_audit(&record, &Scripted::reporting("1.0.0"));
    assert_eq!(result.state, "undetermined");
    assert!(
        result.findings[0].detail.contains(RECORD_FILE),
        "{result:?}"
    );
}

/// Why: Fail-Open Check: a record a future tctl wrote is not understood.
/// What: another schema number loads as corrupt.
/// Test: this.
#[test]
fn a_record_from_another_schema_is_undetermined() {
    let dir = tempfile::tempdir().expect("tempdir");
    let record = dir.path().join(RECORD_FILE);
    std::fs::write(&record, r#"{"schema":99,"members":{}}"#).expect("write");
    assert!(matches!(load(&record), RecordLoad::Corrupt(why) if why.contains("99")));
}

/// Why: a member installed outside `tctl install` has no placed state to match.
/// What: an expected member not in the record reads undetermined, naming it.
/// Test: this.
#[test]
fn an_unrecorded_installed_member_is_undetermined() {
    let (_dir, _bin, record) = recorded("tool");
    let expected = ["trusty-search".to_owned()];
    let result = audit(
        &load(&record),
        &expected,
        &Scripted::reporting("1.0.0"),
        &not_applicable,
    );
    assert_eq!(kinds(&result), [DriftKind::Unrecorded]);
    assert_eq!(result.findings[0].member, "trusty-search");
}

/// Why: #10607 repeat-install safety.
/// What: placing the same build twice leaves a byte-identical record that audits
/// clean.
/// Test: this.
#[test]
fn a_repeat_install_writes_the_same_record() {
    let (_dir, bin, record) = recorded("tool");
    let first = std::fs::read(&record).expect("read");
    record_placements(&record, &[Placement::new("tool", &bin, "1.0.0")]).expect("record");
    assert_eq!(first, std::fs::read(&record).expect("read"));
    assert_eq!(
        run_audit(&record, &Scripted::reporting("1.0.0")).state,
        "clean"
    );
}

/// Why: #10607 rollback safety: an older build placed by install is the deployed
/// set, and an older build placed behind install's back is not.
/// What: replacing the file without recording reads drift; recording that older
/// placement reads clean.
/// Test: this.
#[test]
fn a_rollback_through_install_reads_clean() {
    let (_dir, bin, record) = recorded("tool");
    std::fs::write(&bin, b"build zero").expect("roll back");
    let older = Scripted::reporting("0.9.0");
    assert_eq!(
        kinds(&run_audit(&record, &older)),
        [DriftKind::BuildChanged, DriftKind::WrongVersion]
    );
    record_placements(&record, &[Placement::new("tool", &bin, "0.9.0")]).expect("record");
    assert_eq!(run_audit(&record, &older).state, "clean");
}

/// Why: #10607 rollback and repeat: a member whose placement failed, or was
/// refused under #10584, was not placed, so its entry must survive.
/// What: recording another member keeps the first member's entry, and the first
/// member's own drift still shows.
/// Test: this.
#[test]
fn a_failed_member_keeps_its_old_entry() {
    let (dir, bin, record) = recorded("tool");
    let other = dir.path().join("other");
    std::fs::write(&other, b"other").expect("write");
    record_placements(&record, &[Placement::new("other", &other, "1.0.0")]).expect("record");
    std::fs::write(&bin, b"half replaced").expect("rewrite");
    let result = run_audit(&record, &Scripted::reporting("1.0.0"));
    assert_eq!(kinds(&result), [DriftKind::BuildChanged]);
    assert_eq!(result.findings[0].member, "tool");
}

/// Why: a sibling this release does not ship must not read as missing, and one it
/// does ship has no `--version` contract to run.
/// What: `trusty-search` records the gated binary with its version and a present
/// sibling with none; the absent launcher is not recorded; a swapped sibling is
/// drift with only the gated binary's `--version` run.
/// Test: this.
#[test]
fn siblings_are_digest_checked_only_when_present() {
    let dir = tempfile::tempdir().expect("tempdir");
    let gated = dir.path().join("trusty-search");
    let sibling = dir.path().join("trusty-embedderd");
    std::fs::write(&gated, b"search").expect("write");
    std::fs::write(&sibling, b"embedder").expect("write");
    let record = dir.path().join(RECORD_FILE);
    let placement = Placement::new("trusty-search", &gated, "1.0.0");
    record_placements(&record, &[placement]).expect("record");

    let RecordLoad::Loaded(loaded) = load(&record) else {
        panic!("record loads");
    };
    let binaries = &loaded.members["trusty-search"];
    let names: Vec<&str> = binaries.iter().map(|b| b.name.as_str()).collect();
    assert_eq!(names, ["trusty-search", "trusty-embedderd"]);
    assert_eq!(binaries[1].version, None);

    std::fs::write(&sibling, b"swapped").expect("rewrite");
    let runner = Scripted::reporting("1.0.0");
    let result = run_audit(&record, &runner);
    assert_eq!(kinds(&result), [DriftKind::BuildChanged]);
    assert_eq!(runner.runs.get(), 1, "only the gated binary is executed");
}

/// Why: signing rewrites bytes at the operator's request, and must not launder
/// another member's swapped build.
/// What: refreshing one member clears its drift and leaves the other's.
/// Test: this.
#[test]
fn signing_refreshes_only_the_named_member() {
    let (dir, bin, record) = recorded("tool");
    let other = dir.path().join("other");
    std::fs::write(&other, b"other").expect("write");
    record_placements(&record, &[Placement::new("other", &other, "1.0.0")]).expect("record");
    std::fs::write(&bin, b"signed").expect("rewrite");
    std::fs::write(&other, b"swapped").expect("rewrite");
    refresh_digests_at(&record, "tool").expect("refresh");
    let result = run_audit(&record, &Scripted::reporting("1.0.0"));
    assert_eq!(kinds(&result), [DriftKind::BuildChanged]);
    assert_eq!(result.findings[0].member, "other");
}

/// Why: `stack doctor <member>` triages one member, and the record concerns all.
/// What: scoping keeps that member and the record findings, and recomputes state.
/// Test: this.
#[test]
fn scoping_keeps_the_member_and_record_findings() {
    let finding = |member: &str, kind| DriftFinding {
        member: member.to_owned(),
        kind,
        detail: String::new(),
    };
    let all = DeploymentAudit::from_findings(vec![
        finding("a", DriftKind::MissingBinary),
        finding(RECORD_MEMBER, DriftKind::NoRecord),
    ]);
    assert_eq!(all.state, "drifted");
    let scoped = all.scoped_to(&["b".to_owned()]);
    assert_eq!(kinds(&scoped), [DriftKind::NoRecord]);
    assert_eq!(scoped.state, "undetermined");
}

/// Why: the rollup's two non-green words must not mask each other.
/// What: drift outranks undetermined, and a clean audit leaves the rows' word.
/// Test: this.
#[test]
fn verdict_fold_is_worst_wins() {
    let audit_in = |state| DeploymentAudit {
        state,
        findings: Vec::new(),
    };
    assert_eq!(fold_verdict("ready", &audit_in("clean")), "ready");
    assert_eq!(fold_verdict("ok", &audit_in("clean")), "ok");
    assert_eq!(fold_verdict("ready", &audit_in("drifted")), "degraded");
    assert_eq!(
        fold_verdict("undetermined", &audit_in("drifted")),
        "degraded"
    );
    assert_eq!(
        fold_verdict("degraded", &audit_in("undetermined")),
        "degraded"
    );
    assert_eq!(
        fold_verdict("ready", &audit_in("undetermined")),
        "undetermined"
    );
}

/// Why: Fail-Open Check on the write path: an unrecorded placement leaves every
/// later health check comparing against the wrong build.
/// What: a record path under a file, and an unresolvable one, each yield a
/// finding that blocks the install, with `--no-verify` as well.
/// Test: this.
#[test]
fn an_unwritable_record_blocks_the_install() {
    let (dir, bin, _record) = recorded("tool");
    let placements = [Placement::new("tool", &bin, "1.0.0")];
    let runner = Scripted::reporting("1.0.0");
    let under_a_file = dir.path().join("tool").join(RECORD_FILE);
    for verify in [true, false] {
        let result = finish_at(
            Ok(under_a_file.clone()),
            &placements,
            verify,
            &runner,
            &not_applicable,
        )
        .expect("a finding");
        assert_eq!(kinds(&result), [DriftKind::RecordWriteFailed]);
        assert!(result.blocks_install());
    }
    let unresolved = finish_at(
        Err("no home".to_owned()),
        &placements,
        false,
        &runner,
        &not_applicable,
    )
    .expect("a finding");
    assert_eq!(kinds(&unresolved), [DriftKind::RecordWriteFailed]);
}

/// Why: the install path end to end, short of a real install.
/// What: a placement writes the record and audits it; `--no-verify` writes and
/// skips the audit; nothing placed with no record has nothing to report.
/// Test: this.
#[test]
fn finish_at_records_then_audits() {
    let (dir, bin, _record) = recorded("tool");
    let path = dir.path().join("fresh.json");
    let placements = [Placement::new("tool", &bin, "1.0.0")];
    let good = Scripted::reporting("1.0.0");
    let skipped = finish_at(Ok(path.clone()), &placements, false, &good, &not_applicable);
    assert_eq!(skipped, None);
    assert!(path.exists(), "--no-verify still records");

    let audited = finish_at(Ok(path.clone()), &placements, true, &good, &not_applicable);
    assert_eq!(audited.expect("audited").state, "clean");

    let bad = Scripted::reporting("0.9.0");
    let drifted = finish_at(Ok(path), &[], true, &bad, &not_applicable).expect("audited");
    assert_eq!(drifted.state, "drifted");
    assert!(drifted.blocks_install());

    let none = dir.path().join("none.json");
    assert_eq!(finish_at(Ok(none), &[], true, &good, &not_applicable), None);
}
