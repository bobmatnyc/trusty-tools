//! Tests for the daemon policy snapshot (#8433 P1, #8454).
//!
//! Why: the snapshot is a Fail-Open Check site. Each test goes red if an arm
//! that must deny (stale, denied, refresh panic, refresh I/O fault, poisoned
//! lock) is changed to serve the last report or to allow.
//! What: a scripted source and a settable clock for the timing arms; real
//! [`LoaderSource`] loads over a temp home and temp git repos for the
//! acceptance arms (A5 overlap, A12 unreviewed edit, empty policy).
//! Test: this is the test module.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Barrier, mpsc};
use std::time::Duration;

use trusty_channels::policy::{
    Channel, Clock, DenyReason, DropReason, FileState, Finding, GateError, LoadReport, LoadRequest,
    MessageKind, ProjectFileError, ProjectInput, QuestionState, load_effective, merge_for,
    parse_host, parse_project_file,
};

use super::{
    LoaderSource, MAX_AGE, PolicySnapshot, PolicySource, REFRESH_INTERVAL, Refusal, SnapshotDeny,
};

const BOB: &str = "U0ABCDEF1";
const EVE: &str = "U0ABCDEF2";

/// A clock the test moves by hand.
#[derive(Debug, Clone, Default)]
struct TestClock(Arc<AtomicU64>);

impl TestClock {
    fn set(&self, at: Duration) {
        let ms = u64::try_from(at.as_millis()).expect("ms fit");
        self.0.store(ms, Ordering::SeqCst);
    }
}

impl Clock for TestClock {
    fn now(&self) -> Duration {
        Duration::from_millis(self.0.load(Ordering::SeqCst))
    }
}

type Script = Box<dyn FnMut(usize) -> LoadReport + Send>;

/// A source whose `n`th refresh runs `script(n)`; `calls` counts refreshes.
struct Scripted {
    calls: Arc<AtomicUsize>,
    script: Script,
}

impl PolicySource for Scripted {
    fn refresh(&mut self) -> LoadReport {
        let n = self.calls.fetch_add(1, Ordering::SeqCst);
        (self.script)(n)
    }
}

fn scripted(
    script: impl FnMut(usize) -> LoadReport + Send + 'static,
) -> (
    PolicySnapshot<Scripted, TestClock>,
    TestClock,
    Arc<AtomicUsize>,
) {
    scripted_on(TestClock::default(), script)
}

/// `scripted` over `clock`, so a script can move the clock it runs under.
fn scripted_on(
    clock: TestClock,
    script: impl FnMut(usize) -> LoadReport + Send + 'static,
) -> (
    PolicySnapshot<Scripted, TestClock>,
    TestClock,
    Arc<AtomicUsize>,
) {
    let calls = Arc::new(AtomicUsize::new(0));
    let source = Scripted {
        calls: Arc::clone(&calls),
        script: Box::new(script),
    };
    (PolicySnapshot::new(source, clock.clone()), clock, calls)
}

/// A canonical temp directory (macOS tempdirs sit under a symlink).
struct Home {
    _tmp: tempfile::TempDir,
    home: PathBuf,
}

impl Home {
    fn new() -> Self {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = std::fs::canonicalize(tmp.path()).expect("canonical");
        Self { _tmp: tmp, home }
    }

    fn write_host(&self, yaml: &str) {
        let path = trusty_common::crate_config::crate_config_path_at(&self.home, "trusty-mpm");
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(path, yaml).expect("write host");
    }

    fn request(&self) -> LoadRequest {
        super::daemon_request(Some(self.home.clone()))
    }
}

/// Run git in `dir` with every `GIT_*` variable cleared and a fixed identity.
fn git(dir: &Path, args: &[&str]) {
    // See #9710: the shared builder turns auto-maintenance off.
    let mut cmd = trusty_common::git::command();
    cmd.current_dir(dir).args([
        "-c",
        "user.name=test",
        "-c",
        "user.email=test@example.com",
        "-c",
        "commit.gpgsign=false",
        "-c",
        "core.hooksPath=/dev/null",
    ]);
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            cmd.env_remove(key);
        }
    }
    let out = cmd.args(args).output().expect("run git");
    assert!(out.status.success(), "git {args:?}: {out:?}");
}

/// A repo on `main` with a root commit, under `home`.
fn repo(home: &Home, name: &str) -> PathBuf {
    let dir = home.home.join(name);
    std::fs::create_dir(&dir).expect("mkdir");
    git(&dir, &["init", "-q", "-b", "main"]);
    git(&dir, &["commit", "-q", "--allow-empty", "-m", "root"]);
    dir
}

fn write_routes(dir: &Path, text: &str) {
    std::fs::create_dir_all(dir.join(".trusty-channels")).expect("mkdir");
    std::fs::write(dir.join(".trusty-channels/routes.toml"), text).expect("write");
}

fn commit_routes(dir: &Path, text: &str) {
    write_routes(dir, text);
    git(dir, &["add", ".trusty-channels/routes.toml"]);
    git(dir, &["commit", "-q", "-m", "routes"]);
}

fn slack_route(name: &str, recipient: &str) -> String {
    format!(
        "\n[[slack.routes]]\nname = \"{name}\"\nrecipient = \"{recipient}\"\nkinds = [\"question\"]\n"
    )
}

fn routes(entries: &[(&str, &str)]) -> String {
    let mut text = String::from("version = 2\n");
    for (name, recipient) in entries {
        text.push_str(&slack_route(name, recipient));
    }
    text
}

/// A ceiling enabling Slack and Telegram for `dirs`.
fn host(dirs: &[&Path]) -> String {
    let list: Vec<String> = dirs
        .iter()
        .map(|d| format!("\"{}\"", d.display()))
        .collect();
    let list = list.join(", ");
    format!(
        "channels:\n  version: 1\n  slack:\n    enabled: true\n    connection: {{ bot_ref: slack, \
         app_ref: slack-app }}\n    projects: [{list}]\n  telegram:\n    enabled: true\n    \
         connection: {{ bot_ref: telegram }}\n    projects: [{list}]\n"
    )
}

/// An in-memory report with bob's route in effect; no I/O, no git.
fn bob_report() -> LoadReport {
    let home = Home::new();
    let dir = home.home.join("proj");
    let file = dir.join(".trusty-channels/routes.toml");
    let parsed = parse_project_file(&routes(&[("bob-dm", BOB)]), Some(&home.home));
    let report = merge_for(
        parse_host(&host(&[&dir]), Some(&home.home)),
        vec![ProjectInput {
            project_dir: dir,
            file,
            parsed,
        }],
        &[Channel::Slack, Channel::Telegram],
    );
    assert!(!report.denied, "fixture: {:#?}", report.findings);
    assert_eq!(report.policy.routes().len(), 1, "fixture");
    report
}

/// `bob_report` as a reload leaves it when the file is refused for `error`
/// and the loader keeps its last-good routes.
fn stale_bob_report(error: ProjectFileError) -> LoadReport {
    let mut report = bob_report();
    let file = report.per_file[0].file.clone();
    report.per_file[0].state = FileState::Stale;
    report.findings.push(Finding::FileRefused {
        file: file.clone(),
        error,
    });
    report.findings.push(Finding::Stale { file });
    report
}

fn open_question() -> Vec<QuestionState> {
    vec![QuestionState {
        id: 1,
        channel: Channel::Slack,
        route: "bob-dm".into(),
        answered: false,
    }]
}

/// The egress and inbound refusals, when either check refused.
type Refusals = (Option<Refusal<DenyReason>>, Option<Refusal<DropReason>>);

/// Both checks for bob: `Ok(())` when both allow, else the two refusals.
fn bob_verdicts<S: PolicySource + 'static, C: Clock + Send + Sync + 'static>(
    snap: &PolicySnapshot<S, C>,
) -> Result<(), Refusals> {
    let egress = snap.check_egress(Channel::Slack, BOB, MessageKind::Question);
    let inbound = snap.check_inbound(Channel::Slack, BOB, &[1], &open_question());
    match (egress, inbound) {
        (Ok(route), Ok((iroute, q))) => {
            assert_eq!(route.name(), "bob-dm");
            assert_eq!((iroute.name(), q), ("bob-dm", 1));
            Ok(())
        }
        (e, i) => Err((e.err(), i.err())),
    }
}

/// Assert both checks are refused by the snapshot for `why`.
fn assert_snapshot_denies<S: PolicySource + 'static, C: Clock + Send + Sync + 'static>(
    snap: &PolicySnapshot<S, C>,
    why: impl Fn(&SnapshotDeny) -> bool,
    what: &str,
) {
    match bob_verdicts(snap) {
        Ok(()) => panic!("{what}: allowed"),
        Err((Some(Refusal::Snapshot(e)), Some(Refusal::Snapshot(i)))) => {
            assert!(why(&e) && why(&i), "{what}: wrong reason {e:?} / {i:?}");
        }
        Err(other) => panic!("{what}: not a snapshot deny: {other:?}"),
    }
}

fn secs(s: f64) -> Duration {
    Duration::from_secs_f64(s)
}

#[test]
fn refresh_within_interval_spawns_no_git() {
    // Only `PolicySource::refresh` runs git; the count is the git spawn count.
    let (snap, clock, calls) = scripted(|_| bob_report());
    assert_eq!(REFRESH_INTERVAL, Duration::from_secs(5));
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "construction loads nothing"
    );
    settle(&snap);
    assert_eq!(bob_verdicts(&snap), Ok(()));
    assert_eq!(calls.load(Ordering::SeqCst), 1, "the first check loads");
    for at in [0.5, 2.0, 4.999] {
        clock.set(secs(at));
        settle(&snap);
        assert_eq!(bob_verdicts(&snap), Ok(()));
        assert!(snap.current().is_ok());
    }
    assert_eq!(calls.load(Ordering::SeqCst), 1, "refreshed inside 5 s");
    clock.set(secs(5.0));
    settle(&snap);
    assert_eq!(bob_verdicts(&snap), Ok(()));
    assert_eq!(calls.load(Ordering::SeqCst), 2, "no refresh at 5 s");
    clock.set(secs(9.9));
    settle(&snap);
    assert_eq!(bob_verdicts(&snap), Ok(()));
    assert_eq!(calls.load(Ordering::SeqCst), 2, "refreshed inside 5 s");

    // The real loader: a reviewed widening committed inside the interval is
    // not seen until it ends, because no reload (and so no git) ran.
    let home = Home::new();
    let dir = repo(&home, "proj");
    commit_routes(&dir, &routes(&[("bob-dm", BOB)]));
    home.write_host(&host(&[&dir]));
    let clock = TestClock::default();
    let snap = PolicySnapshot::new(LoaderSource::new(home.request()), clock.clone());
    settle(&snap);
    assert_eq!(bob_verdicts(&snap), Ok(()));
    commit_routes(&dir, &routes(&[("bob-dm", BOB), ("eve-dm", EVE)]));
    clock.set(secs(4.9));
    settle(&snap);
    let eve = snap.check_egress(Channel::Slack, EVE, MessageKind::Question);
    assert_eq!(
        eve.err(),
        Some(Refusal::Policy(DenyReason::NoRoute)),
        "reloaded early"
    );
    clock.set(secs(5.0));
    settle(&snap);
    let eve = snap.check_egress(Channel::Slack, EVE, MessageKind::Question);
    assert_eq!(
        eve.map(|r| r.name().to_string()).ok().as_deref(),
        Some("eve-dm")
    );
}

#[test]
fn snapshot_older_than_max_age_denies() {
    // One good load, then every refresh fails, so the snapshot ages.
    let (snap, clock, _) = scripted(|n| match n {
        0 => bob_report(),
        _ => panic!("scripted refresh failure"),
    });
    assert_eq!(MAX_AGE, Duration::from_secs(60));
    settle(&snap);
    assert_eq!(bob_verdicts(&snap), Ok(()));
    clock.set(secs(60.0));
    settle(&snap);
    assert_eq!(
        bob_verdicts(&snap),
        Ok(()),
        "60 s is not older than max age"
    );
    clock.set(secs(60.001));
    settle(&snap);
    assert_snapshot_denies(
        &snap,
        |d| matches!(d, SnapshotDeny::Stale { .. }),
        "past max age",
    );
    match snap.current() {
        Err(SnapshotDeny::Stale { age, max }) => {
            assert_eq!(max, MAX_AGE);
            assert!(age > MAX_AGE, "{age:?}");
        }
        other => panic!("current() past max age: {other:?}"),
    }
}

#[test]
fn denied_report_denies_egress_and_inbound() {
    // A denied report that still carries a route: only the `denied` flag
    // stands between it and an allow.
    let (snap, _, _) = scripted(|_| {
        let mut report = bob_report();
        report.denied = true;
        report
    });
    settle(&snap);
    assert_snapshot_denies(&snap, |d| *d == SnapshotDeny::Denied, "denied report");
    assert_eq!(snap.current().err(), Some(SnapshotDeny::Denied));
    // Control: the same report without the flag allows.
    let (snap, _, _) = scripted(|_| bob_report());
    settle(&snap);
    assert_eq!(bob_verdicts(&snap), Ok(()));
}

#[test]
fn overlapping_routes_deny_daemon_snapshot() {
    // A5: two project files name one recipient.
    let home = Home::new();
    let a = repo(&home, "alpha");
    let b = repo(&home, "beta");
    commit_routes(&a, &routes(&[("bob-dm", BOB)]));
    commit_routes(&b, &routes(&[("bob-too", BOB), ("eve-dm", EVE)]));
    home.write_host(&host(&[&a, &b]));
    let snap = PolicySnapshot::new(LoaderSource::new(home.request()), TestClock::default());
    settle(&snap);
    assert_snapshot_denies(&snap, |d| *d == SnapshotDeny::Denied, "overlap");
    let eve = snap.check_egress(Channel::Slack, EVE, MessageKind::Question);
    assert_eq!(eve.err(), Some(Refusal::Snapshot(SnapshotDeny::Denied)));
    // The load names both conflicting entries.
    let report = load_effective(&home.request());
    let named = report.findings.iter().any(|f| match f {
        Finding::Overlap { first, second, .. } => {
            first.file.starts_with(&a) && second.file.starts_with(&b)
        }
        _ => false,
    });
    assert!(named, "{:#?}", report.findings);
}

#[test]
fn routes_edit_outside_config_yaml_has_no_effect_in_daemon() {
    // A12: only a project listed in config.yaml, at its reviewed commit,
    // reaches live routing.
    let home = Home::new();
    let dir = repo(&home, "proj");
    let unlisted = repo(&home, "unlisted");
    commit_routes(&dir, &routes(&[("bob-dm", BOB)]));
    commit_routes(&unlisted, &routes(&[("eve-dm", EVE)]));
    home.write_host(&host(&[&dir]));
    let clock = TestClock::default();
    let snap = PolicySnapshot::new(LoaderSource::new(home.request()), clock.clone());
    let eve_denied = |what: &str| {
        let eve = snap.check_egress(Channel::Slack, EVE, MessageKind::Question);
        assert!(eve.is_err(), "{what}: eve allowed");
        let inbound = snap.check_inbound(Channel::Slack, EVE, &[1], &[]);
        assert!(inbound.is_err(), "{what}: eve inbound accepted");
    };
    settle(&snap);
    assert_eq!(bob_verdicts(&snap), Ok(()));
    eve_denied("unlisted project");

    let widened = routes(&[("bob-dm", BOB), ("eve-dm", EVE)]);
    write_routes(&dir, &widened);
    clock.set(secs(5.0));
    settle(&snap);
    eve_denied("uncommitted edit");
    assert_eq!(bob_verdicts(&snap), Ok(()), "uncommitted edit");

    git(&dir, &["add", ".trusty-channels/routes.toml"]);
    clock.set(secs(10.0));
    settle(&snap);
    eve_denied("staged edit");
    assert_eq!(bob_verdicts(&snap), Ok(()), "staged edit");
    // An unreviewed edit is not a fault: the snapshot stays fresh.
    clock.set(secs(120.0));
    settle(&snap);
    eve_denied("staged edit, two minutes on");
    assert_eq!(bob_verdicts(&snap), Ok(()), "staged edit, two minutes on");

    // Control: the reviewed path does reach the daemon.
    git(&dir, &["commit", "-q", "-m", "routes"]);
    clock.set(secs(125.0));
    settle(&snap);
    let eve = snap.check_egress(Channel::Slack, EVE, MessageKind::Question);
    assert_eq!(
        eve.map(|r| r.name().to_string()).ok().as_deref(),
        Some("eve-dm")
    );
}

#[test]
fn empty_policy_with_enabled_channel_denies() {
    let home = Home::new();
    let dir = repo(&home, "proj");
    for listed in [vec![], vec![dir.as_path()]] {
        home.write_host(&host(&listed));
        let snap = PolicySnapshot::new(LoaderSource::new(home.request()), TestClock::default());
        settle(&snap);
        let report = snap.current().expect("an empty policy is a valid load");
        assert!(report.policy.is_empty(), "{listed:?}");
        let egress = snap.check_egress(Channel::Slack, BOB, MessageKind::Question);
        assert_eq!(
            egress.err(),
            Some(Refusal::Policy(DenyReason::NoPolicy)),
            "{listed:?}"
        );
        let inbound = snap.check_inbound(Channel::Slack, BOB, &[1], &open_question());
        assert_eq!(
            inbound.err(),
            Some(Refusal::Policy(DropReason::UnknownSender)),
            "{listed:?}"
        );
    }
}

#[test]
fn refresh_error_keeps_last_good_only_until_max_age() {
    let io_faults = [
        ProjectFileError::Read {
            reason: "permission denied".into(),
        },
        ProjectFileError::Gate {
            error: GateError::GitUnavailable {
                reason: "not found".into(),
            },
        },
        ProjectFileError::Gate {
            error: GateError::GitFailed { step: "rev-parse" },
        },
        ProjectFileError::Gate {
            error: GateError::GitTimedOut { step: "show" },
        },
        // r-078 H2: gate.rs reports a git fault under these names too.
        ProjectFileError::Gate {
            error: GateError::NotARepository,
        },
        ProjectFileError::Gate {
            error: GateError::NotTopLevel,
        },
        ProjectFileError::Gate {
            error: GateError::NoDefaultCommit {
                branch: "main".into(),
            },
        },
        ProjectFileError::Gate {
            error: GateError::NotCommitted { at: "main".into() },
        },
    ];
    let mut cases: Vec<(String, Script)> = vec![(
        "panic".into(),
        Box::new(|n| match n {
            0 => bob_report(),
            _ => panic!("scripted refresh failure"),
        }),
    )];
    for fault in io_faults {
        let what = format!("{fault:?}");
        cases.push((
            what,
            Box::new(move |n| match n {
                0 => bob_report(),
                _ => stale_bob_report(fault.clone()),
            }),
        ));
    }
    for (what, script) in cases {
        let (snap, clock, calls) = scripted(script);
        settle(&snap);
        assert_eq!(bob_verdicts(&snap), Ok(()), "{what}: first load");
        for at in [5.0, 30.0, 60.0] {
            clock.set(secs(at));
            settle(&snap);
            assert_eq!(bob_verdicts(&snap), Ok(()), "{what}: last good at {at} s");
        }
        assert!(
            calls.load(Ordering::SeqCst) >= 2,
            "{what}: no failed refresh ran"
        );
        clock.set(secs(61.0));
        settle(&snap);
        assert_snapshot_denies(
            &snap,
            |d| matches!(d, SnapshotDeny::Stale { .. }),
            &format!("{what}: past max age"),
        );
        clock.set(secs(600.0));
        settle(&snap);
        assert_snapshot_denies(
            &snap,
            |d| matches!(d, SnapshotDeny::Stale { .. }),
            &format!("{what}: ten minutes on"),
        );
    }

    // A first load that panics leaves nothing to serve.
    let (snap, _, _) = scripted(|_| panic!("scripted first-load failure"));
    settle(&snap);
    assert_snapshot_denies(
        &snap,
        |d| *d == SnapshotDeny::NeverLoaded,
        "first load panicked",
    );

    // A good refresh after the failures serves again.
    let (snap, clock, _) = scripted(|n| match n {
        0 => bob_report(),
        1 => panic!("scripted refresh failure"),
        _ => bob_report(),
    });
    settle(&snap);
    assert_eq!(bob_verdicts(&snap), Ok(()));
    clock.set(secs(5.0));
    settle(&snap);
    assert_eq!(bob_verdicts(&snap), Ok(()));
    clock.set(secs(64.0));
    settle(&snap);
    assert_eq!(
        bob_verdicts(&snap),
        Ok(()),
        "a good refresh at 64 s is fresh"
    );

    // A real I/O fault: the reviewed routes file becomes unreadable.
    if running_as_root() {
        return;
    }
    let home = Home::new();
    let dir = repo(&home, "proj");
    commit_routes(&dir, &routes(&[("bob-dm", BOB)]));
    home.write_host(&host(&[&dir]));
    let clock = TestClock::default();
    let snap = PolicySnapshot::new(LoaderSource::new(home.request()), clock.clone());
    settle(&snap);
    assert_eq!(bob_verdicts(&snap), Ok(()), "real: first load");
    let file = dir.join(".trusty-channels/routes.toml");
    set_mode(&file, 0o000);
    clock.set(secs(5.0));
    settle(&snap);
    assert_eq!(
        bob_verdicts(&snap),
        Ok(()),
        "real: last good inside max age"
    );
    clock.set(secs(61.0));
    settle(&snap);
    let denied = bob_verdicts(&snap);
    set_mode(&file, 0o644);
    match denied {
        Err((Some(Refusal::Snapshot(SnapshotDeny::Stale { .. })), Some(_))) => {}
        other => panic!("real: unreadable routes file past max age: {other:?}"),
    }
}

#[test]
fn refresh_that_outlasts_max_age_denies() {
    // r-078 H1: a refresh that starts at 5 s and returns at 61 s with a git
    // timeout must be judged at 61 s, not at 5 s.
    let clock = TestClock::default();
    let during = clock.clone();
    let (snap, clock, _) = scripted_on(clock, move |n| match n {
        0 => bob_report(),
        _ => {
            during.set(secs(61.0));
            stale_bob_report(ProjectFileError::Gate {
                error: GateError::GitTimedOut { step: "show" },
            })
        }
    });
    settle(&snap);
    assert_eq!(bob_verdicts(&snap), Ok(()));
    clock.set(secs(5.0));
    settle(&snap);
    match snap.current() {
        Err(SnapshotDeny::Stale { age, max }) => {
            assert_eq!(max, MAX_AGE);
            assert!(age > MAX_AGE, "{age:?}");
        }
        other => panic!("a refresh that returned at 61 s: {other:?}"),
    }
}

#[test]
fn git_fault_reported_as_a_verdict_ages_to_deny() {
    // r-078 H2: a broken `.git/config` makes `rev-parse --show-toplevel`
    // exit 128, which the gate reports as NotARepository.
    let home = Home::new();
    let dir = repo(&home, "proj");
    commit_routes(&dir, &routes(&[("bob-dm", BOB)]));
    home.write_host(&host(&[&dir]));
    let clock = TestClock::default();
    let snap = PolicySnapshot::new(LoaderSource::new(home.request()), clock.clone());
    settle(&snap);
    assert_eq!(bob_verdicts(&snap), Ok(()), "first load");
    std::fs::write(dir.join(".git/config"), "[core\nbroken").expect("corrupt config");
    clock.set(secs(5.0));
    settle(&snap);
    assert_eq!(bob_verdicts(&snap), Ok(()), "last good inside max age");
    clock.set(secs(61.0));
    settle(&snap);
    assert_snapshot_denies(
        &snap,
        |d| matches!(d, SnapshotDeny::Stale { .. }),
        "broken .git/config past max age",
    );

    // Control: a refusal that is a content verdict keeps the snapshot fresh.
    let verdicts = [
        ProjectFileError::Gate {
            error: GateError::ContentDiffers { at: "main".into() },
        },
        ProjectFileError::Gate {
            error: GateError::NotOnDefaultBranch {
                head: "feature".into(),
                default: "main".into(),
            },
        },
        ProjectFileError::Gate {
            error: GateError::DetachedHead,
        },
        ProjectFileError::Parse {
            reason: "bad toml".into(),
        },
        ProjectFileError::Version { found: 9 },
        ProjectFileError::NotUtf8,
        ProjectFileError::TooLarge { limit: 1 },
    ];
    for verdict in verdicts {
        let what = format!("{verdict:?}");
        let (snap, clock, _) = scripted(move |n| match n {
            0 => bob_report(),
            _ => stale_bob_report(verdict.clone()),
        });
        settle(&snap);
        assert_eq!(bob_verdicts(&snap), Ok(()), "{what}: first load");
        for at in [5.0, 61.0, 120.0] {
            clock.set(secs(at));
            settle(&snap);
            assert_eq!(bob_verdicts(&snap), Ok(()), "{what}: at {at} s");
        }
    }
}

#[test]
fn stale_file_with_no_refusal_finding_ages_to_deny() {
    // r-078 delta: a Stale file whose refusal is not recorded is unexplained,
    // so it ages like a fault.
    let (snap, clock, _) = scripted(|n| {
        let mut report = bob_report();
        if n > 0 {
            report.per_file[0].state = FileState::Stale;
        }
        report
    });
    settle(&snap);
    assert_eq!(bob_verdicts(&snap), Ok(()), "first load");
    clock.set(secs(60.0));
    settle(&snap);
    assert_eq!(bob_verdicts(&snap), Ok(()), "inside max age");
    clock.set(secs(61.0));
    settle(&snap);
    assert_snapshot_denies(
        &snap,
        |d| matches!(d, SnapshotDeny::Stale { .. }),
        "unexplained Stale file past max age",
    );
}

fn set_mode(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).expect("chmod");
}

/// True when the tests run as root, which reads a mode-000 file.
fn running_as_root() -> bool {
    use std::os::unix::fs::MetadataExt as _;
    let probe = tempfile::tempdir().expect("tempdir");
    let file = probe.path().join("probe");
    std::fs::write(&file, b"x").expect("write");
    set_mode(&file, 0o000);
    let readable = std::fs::read(&file).is_ok();
    let uid = std::fs::metadata(probe.path()).expect("meta").uid();
    set_mode(&file, 0o644);
    readable || uid == 0
}

#[test]
fn poisoned_snapshot_lock_denies() {
    let (snap, _, _) = scripted(|_| bob_report());
    settle(&snap);
    assert_eq!(bob_verdicts(&snap), Ok(()));
    let snap = Arc::new(snap);
    let held = Arc::clone(&snap);
    let poisoner = std::thread::spawn(move || {
        let _guard = held.inner.published.lock();
        panic!("poison the snapshot lock");
    });
    assert!(poisoner.join().is_err());
    assert_snapshot_denies(&snap, |d| *d == SnapshotDeny::Poisoned, "poisoned lock");
}

/// The longest a test waits on another thread. A hang guard only: a check
/// that deadlocks fails the test here instead of hanging the suite.
const HANG_GUARD: Duration = Duration::from_secs(10);

/// Start the refresh a check at the clock's time would start, then wait
/// until no refresh is in flight.
fn settle<S: PolicySource + 'static, C: Clock + Send + Sync + 'static>(
    snap: &PolicySnapshot<S, C>,
) {
    let _ = snap.current();
    snap.wait_idle();
}

/// The test's end of a gate that holds one refresh inside the source.
struct Gate {
    entered: mpsc::Receiver<()>,
    release: mpsc::Sender<()>,
}

/// The source's end. Dropping the [`Gate`] releases it, so a failed test
/// never leaves a refresh held.
struct Held {
    entered: mpsc::Sender<()>,
    release: mpsc::Receiver<()>,
}

fn gate() -> (Gate, Held) {
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let gate = Gate {
        entered: entered_rx,
        release: release_tx,
    };
    let held = Held {
        entered: entered_tx,
        release: release_rx,
    };
    (gate, held)
}

impl Held {
    /// Signal that the refresh is inside the source, then wait for release.
    fn hold(&self) {
        let _ = self.entered.send(());
        let _ = self.release.recv();
    }
}

impl Gate {
    /// True once the gated refresh is inside the source.
    fn entered(&self) -> bool {
        self.entered.recv_timeout(HANG_GUARD).is_ok()
    }

    /// Let the gated refresh return.
    fn open(&self) {
        let _ = self.release.send(());
    }
}

type Snap = PolicySnapshot<Scripted, TestClock>;
type Checked = Result<Arc<LoadReport>, SnapshotDeny>;

/// `snap.current()` on its own thread; the result arrives on the receiver.
fn check_on_thread(snap: &Arc<Snap>) -> mpsc::Receiver<Checked> {
    let (tx, rx) = mpsc::channel();
    let snap = Arc::clone(snap);
    std::thread::spawn(move || {
        let _ = tx.send(snap.current());
    });
    rx
}

#[test]
fn reader_not_blocked_by_slow_refresh() {
    // #9777: a check during a refresh gets the published report at once.
    let (gate, held) = gate();
    let (snap, clock, calls) = scripted(move |n| {
        if n == 1 {
            held.hold();
        }
        bob_report()
    });
    settle(&snap);
    let snap = Arc::new(snap);
    clock.set(secs(5.0));
    let first = check_on_thread(&snap);
    assert!(gate.entered(), "the due refresh never reached the source");
    let reader = check_on_thread(&snap).recv_timeout(HANG_GUARD);
    gate.open();
    let reader = reader.map(|r| r.map(|report| report.policy.routes().len()));
    assert_eq!(reader, Ok(Ok(1)), "a reader waited on the refresh");
    let first = first.recv_timeout(HANG_GUARD).map(|r| r.is_ok());
    assert_eq!(first, Ok(true), "the check that found the refresh due");
    snap.wait_idle();
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[test]
fn slow_refresh_does_not_start_a_second_refresh() {
    // #9777: a refresh that takes 7 s, longer than REFRESH_INTERVAL.
    let (gate, held) = gate();
    let clock = TestClock::default();
    let during = clock.clone();
    let (snap, clock, calls) = scripted_on(clock, move |n| {
        if n == 1 {
            during.set(secs(12.0));
            held.hold();
        }
        bob_report()
    });
    settle(&snap);
    let snap = Arc::new(snap);
    clock.set(secs(5.0));
    let first = check_on_thread(&snap);
    assert!(gate.entered(), "the due refresh never reached the source");
    let readers: Vec<_> = (0..4).map(|_| check_on_thread(&snap)).collect();
    gate.open();
    for rx in std::iter::once(first).chain(readers) {
        let checked = rx.recv_timeout(HANG_GUARD);
        assert!(matches!(checked, Ok(Ok(_))), "{checked:?}");
    }
    snap.wait_idle();
    assert_eq!(
        calls.load(Ordering::SeqCst),
        2,
        "a refresh slower than 5 s started another"
    );
}

#[test]
fn hung_refresh_past_max_age_denies_without_blocking() {
    // #9777: a refresh that never returns cannot keep the snapshot fresh.
    let (gate, held) = gate();
    let (snap, clock, _) = scripted(move |n| {
        if n == 1 {
            held.hold();
        }
        bob_report()
    });
    settle(&snap);
    let snap = Arc::new(snap);
    clock.set(secs(5.0));
    let first = check_on_thread(&snap);
    assert!(gate.entered(), "the due refresh never reached the source");
    clock.set(secs(66.0));
    let reader = check_on_thread(&snap).recv_timeout(HANG_GUARD);
    gate.open();
    match reader {
        Ok(Err(SnapshotDeny::Stale { age, max })) => {
            assert_eq!((age, max), (secs(66.0), MAX_AGE));
        }
        other => panic!("a check during a hung refresh at 66 s: {other:?}"),
    }
    let _ = first.recv_timeout(HANG_GUARD);
    snap.wait_idle();
    // The refresh started at 5 s, so its report is 61 s old at 66 s.
    match snap.current() {
        Err(SnapshotDeny::Stale { age, .. }) => assert_eq!(age, secs(61.0)),
        other => panic!("a refresh that started at 5 s, judged at 66 s: {other:?}"),
    }
}

#[test]
fn never_loaded_denies_while_first_refresh_in_flight() {
    // #9777 Q1: a cold check denies while the first load runs.
    let (gate, held) = gate();
    let (snap, _, calls) = scripted(move |n| {
        if n == 0 {
            held.hold();
        }
        bob_report()
    });
    let snap = Arc::new(snap);
    let first = check_on_thread(&snap);
    assert!(gate.entered(), "the first load never reached the source");
    let first = first.recv_timeout(HANG_GUARD).map(|r| r.err());
    let second = check_on_thread(&snap)
        .recv_timeout(HANG_GUARD)
        .map(|r| r.err());
    gate.open();
    let never = Ok(Some(SnapshotDeny::NeverLoaded));
    assert_eq!(first, never, "the cold check waited on the first load");
    assert_eq!(second, never, "a check during the first load");
    snap.wait_idle();
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "a check during the first load started another"
    );
    assert!(snap.current().is_ok(), "the first load was not published");
}

#[test]
fn unverified_refresh_keeps_previous_report_and_ages() {
    // #9777: unverified reports from the refresh thread never move the fresh
    // time, so checks that never wait see the snapshot age to deny.
    let (snap, clock, calls) = scripted(|n| match n {
        0 => bob_report(),
        _ => stale_bob_report(ProjectFileError::Gate {
            error: GateError::GitTimedOut { step: "show" },
        }),
    });
    settle(&snap);
    for at in [5.0, 30.0, 60.0] {
        clock.set(secs(at));
        assert_eq!(bob_verdicts(&snap), Ok(()), "last good at {at} s");
    }
    clock.set(secs(61.0));
    assert_snapshot_denies(
        &snap,
        |d| matches!(d, SnapshotDeny::Stale { .. }),
        "past max age, no wait",
    );
    snap.wait_idle();
    assert!(calls.load(Ordering::SeqCst) >= 2, "no failed refresh ran");
    assert_snapshot_denies(
        &snap,
        |d| matches!(d, SnapshotDeny::Stale { .. }),
        "past max age, every refresh returned",
    );
}

#[test]
fn refresh_panic_clears_in_flight_and_keeps_report() {
    // #9777: a refresh that panics on its own thread ends its flight.
    let (snap, clock, calls) = scripted(|n| match n {
        0 => bob_report(),
        _ => panic!("scripted refresh failure"),
    });
    settle(&snap);
    let before = snap.current().expect("first load");
    clock.set(secs(5.0));
    settle(&snap);
    assert_eq!(calls.load(Ordering::SeqCst), 2, "the refresh at 5 s ran");
    let after = snap.current().expect("last good inside max age");
    assert!(
        Arc::ptr_eq(&before, &after),
        "a panicked refresh replaced it"
    );
    clock.set(secs(10.0));
    settle(&snap);
    assert_eq!(
        calls.load(Ordering::SeqCst),
        3,
        "a panicked refresh wedged the snapshot"
    );
    clock.set(secs(61.0));
    match snap.current() {
        Err(SnapshotDeny::Stale { age, .. }) => assert_eq!(age, secs(61.0)),
        other => panic!("age counts from the clean load at 0 s: {other:?}"),
    }
}

#[test]
fn concurrent_due_checks_start_one_refresh() {
    // #9777: eight checks find a refresh due at once; one refresh runs.
    let (gate, held) = gate();
    let active = Arc::new(AtomicUsize::new(0));
    let most = Arc::new(AtomicUsize::new(0));
    let (now_active, most_active) = (Arc::clone(&active), Arc::clone(&most));
    let (snap, clock, calls) = scripted(move |n| {
        let running = now_active.fetch_add(1, Ordering::SeqCst) + 1;
        most_active.fetch_max(running, Ordering::SeqCst);
        if n == 1 {
            held.hold();
        }
        now_active.fetch_sub(1, Ordering::SeqCst);
        bob_report()
    });
    settle(&snap);
    let snap = Arc::new(snap);
    clock.set(secs(5.0));
    let barrier = Arc::new(Barrier::new(8));
    let checks: Vec<_> = (0..8)
        .map(|_| {
            let (tx, rx) = mpsc::channel();
            let (snap, barrier) = (Arc::clone(&snap), Arc::clone(&barrier));
            std::thread::spawn(move || {
                barrier.wait();
                let _ = tx.send(snap.current());
            });
            rx
        })
        .collect();
    assert!(gate.entered(), "the due refresh never reached the source");
    for rx in checks {
        let checked = rx.recv_timeout(HANG_GUARD);
        assert!(matches!(checked, Ok(Ok(_))), "{checked:?}");
    }
    gate.open();
    snap.wait_idle();
    assert_eq!(calls.load(Ordering::SeqCst), 2, "more than one refresh ran");
    assert_eq!(most.load(Ordering::SeqCst), 1, "two refreshes overlapped");
}

#[cfg(feature = "daemon")]
#[tokio::test]
async fn daemon_state_holds_snapshot_under_its_user_home() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = std::fs::canonicalize(tmp.path()).expect("canonical");
    let state = crate::daemon::DaemonState::with_root_isolated_managed(root.clone()).await;
    let home = state.user_home().expect("pinned home");
    assert!(home.starts_with(&root), "{home:?}");
    let snap = state.channel_policy();
    settle(snap);
    // No config.yaml under the pinned home: the host is missing, deny all.
    assert_eq!(snap.current().err(), Some(SnapshotDeny::Denied));
    assert!(
        std::ptr::eq(snap, state.channel_policy()),
        "one snapshot per daemon"
    );
}
