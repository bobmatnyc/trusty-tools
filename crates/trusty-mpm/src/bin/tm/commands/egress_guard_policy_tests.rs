//! #9741 (#8454 G3 S4): the policy readiness gate ahead of the taint deny.
//!
//! Why: ruling (a), 2026-10-12: for a tainted session's gated call the policy
//! readiness check runs before the taint deny. Each failure branch must deny
//! as `egress-policy-not-ready` and name itself; a ready policy falls through
//! to the `egress-channel-tainted` deny. No verdict loosens.
//! What: drives `with_taint` over a tainted temp home. The ready closure is
//! the real `check` over a temp `config.yaml` (and temp git repos for the
//! ready case), or `check_with` over an in-memory report or an injected
//! loader that hangs, panics or records its deadline.
//! Test: this is the test module.

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use trusty_channels::policy::{
    Channel, FileState, LoadReport, LoadRequest, MAX_FILE_BYTES, ProjectInput, merge_for,
    parse_host, parse_project_file,
};
use trusty_mpm::channels::readiness::{check, check_with};
use trusty_mpm::channels::snapshot::daemon_request;
use trusty_mpm::core::channels_readiness::{HOOK_LOAD_LIMIT, NotReady};
use trusty_mpm::core::channels_taint::{TaintMark, taint_dir, taint_managed_session};

use super::TAINT_CHECK;
use crate::commands::egress_guard::{Verdict, with_taint};
use crate::commands::hook_stdin::HookStdinError;

const SESSION: &str = "s-tainted";
const BOB: &str = "U0ABCDEF1";
/// A wait no healthy test load reaches, so only the injected stalls time out.
const LIMIT: Duration = Duration::from_secs(30);

/// A canonical temp home with `SESSION` tainted under it.
struct Home {
    _tmp: tempfile::TempDir,
    home: PathBuf,
}

impl Home {
    fn tainted() -> Self {
        let tmp = tempfile::tempdir().expect("tempdir");
        // macOS tempdirs sit under a symlink; the host read wants the real path.
        let home = std::fs::canonicalize(tmp.path()).expect("canonical");
        let mark = TaintMark {
            channel: "slack".into(),
            route: "owner-dm".into(),
            question_id: "Q-1".into(),
            tainted_at: chrono::Utc::now(),
            workspace: PathBuf::from("/"),
        };
        taint_managed_session(&taint_dir(&home), "m-1", SESSION, &mark).expect("taint");
        Self { _tmp: tmp, home }
    }

    fn host_path(&self) -> PathBuf {
        trusty_common::crate_config::crate_config_path_at(&self.home, "trusty-mpm")
    }

    fn write_host(&self, bytes: &[u8]) {
        let path = self.host_path();
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(path, bytes).expect("write host");
    }

    fn request(&self) -> LoadRequest {
        daemon_request(Some(self.home.clone()))
    }
}

fn read_payload(session: Option<&str>) -> Result<Value, HookStdinError> {
    let mut payload = json!({
        "tool_name": "Read",
        "tool_input": { "file_path": "/etc/hosts" },
    });
    if let Some(session) = session {
        payload["session_id"] = json!(session);
    }
    Ok(payload)
}

/// The guard's verdict on a tainted session's `Read`, readiness from `ready`.
fn gated(home: &Home, ready: impl FnOnce() -> Result<(), NotReady>) -> Verdict {
    with_taint(
        Verdict::Allow,
        &read_payload(Some(SESSION)),
        Some(&home.home),
        ready,
    )
}

/// The verdict with the real readiness load over `home`.
fn real(home: &Home) -> Verdict {
    let at = home.home.clone();
    gated(home, move || check(Some(&at), LIMIT))
}

/// The verdict with readiness judged from `report`, loaded by no I/O.
fn from_report(home: &Home, report: LoadReport) -> Verdict {
    let req = home.request();
    gated(home, move || {
        check_with(move |_: &LoadRequest, _: Instant| report, req, LIMIT)
    })
}

/// Assert `verdict` is the not-ready deny and its reason names `branch`.
fn assert_not_ready(verdict: &Verdict, branch: &str, case: &str) {
    match verdict {
        Verdict::Deny { check, reason } => {
            assert_eq!(*check, "egress-policy-not-ready", "{case}: {reason}");
            assert!(
                reason.contains(branch),
                "{case}: the reason must name `{branch}`: {reason}"
            );
            assert!(reason.contains("#8454"), "{case}: {reason}");
        }
        Verdict::Allow => panic!("{case}: a not-ready policy allowed a tainted gated call"),
    }
}

/// A ceiling enabling Slack (and Telegram unless `slack_only`) for `dirs`.
fn host(dirs: &[&Path], slack_only: bool) -> String {
    let list: Vec<String> = dirs
        .iter()
        .map(|d| format!("\"{}\"", d.display()))
        .collect();
    let list = list.join(", ");
    let mut text = format!(
        "channels:\n  version: 1\n  slack:\n    enabled: true\n    connection: {{ bot_ref: slack, \
         app_ref: slack-app }}\n    projects: [{list}]\n"
    );
    if !slack_only {
        text.push_str(&format!(
            "  telegram:\n    enabled: true\n    connection: {{ bot_ref: telegram }}\n    \
             projects: [{list}]\n"
        ));
    }
    text
}

fn slack_route(name: &str, recipient: &str) -> String {
    format!(
        "\n[[slack.routes]]\nname = \"{name}\"\nrecipient = \"{recipient}\"\nkinds = [\"question\"]\n"
    )
}

/// An in-memory merge of `files` (dir name, route file text) under `home`.
fn merged(home: &Home, files: &[(&str, &str)], slack_only: bool) -> LoadReport {
    let dirs: Vec<PathBuf> = files.iter().map(|(d, _)| home.home.join(d)).collect();
    let refs: Vec<&Path> = dirs.iter().map(PathBuf::as_path).collect();
    let inputs = dirs
        .iter()
        .zip(files)
        .map(|(dir, (_, text))| ProjectInput {
            project_dir: dir.clone(),
            file: dir.join(".trusty-channels/routes.toml"),
            parsed: parse_project_file(text, Some(&home.home)),
        })
        .collect();
    merge_for(
        parse_host(&host(&refs, slack_only), Some(&home.home)),
        inputs,
        &[Channel::Slack, Channel::Telegram],
    )
}

/// Bob's one Slack route in effect: the report a ready load returns.
fn bob_report(home: &Home) -> LoadReport {
    let text = format!("version = 2\n{}", slack_route("bob-dm", BOB));
    let report = merged(home, &[("proj", &text)], false);
    assert!(
        !report.denied && report.findings.is_empty(),
        "fixture: {report:#?}"
    );
    assert_eq!(report.policy.routes().len(), 1, "fixture");
    report
}

/// 🔴 #9741 B1: no home means no `config.yaml` can be read.
#[test]
fn home_unknown_denies_a_gated_call() {
    let verdict = with_taint(Verdict::Allow, &read_payload(Some(SESSION)), None, || {
        check(None, LIMIT)
    });
    assert_not_ready(&verdict, "no home directory is known", "home unknown");
}

/// 🔴 #9741 B2 (A8): a tainted session with no `config.yaml` is denied as not
/// ready, and the taint reason is kept beside the branch.
#[test]
fn config_missing_denies_a_gated_call() {
    let home = Home::tainted();
    let verdict = real(&home);
    assert_not_ready(&verdict, "config.yaml is missing", "missing");
    let Verdict::Deny { reason, .. } = verdict else {
        unreachable!("asserted a deny")
    };
    assert!(reason.contains("channel-tainted"), "{reason}");
}

/// 🔴 #9741 B3 (A8): a `config.yaml` the load cannot read denies.
#[test]
fn config_unreadable_denies_a_gated_call() {
    let dir = Home::tainted();
    std::fs::create_dir_all(dir.host_path()).expect("a directory at config.yaml");
    assert_not_ready(&real(&dir), "config.yaml is unreadable", "a directory");

    let big = Home::tainted();
    let cap = usize::try_from(MAX_FILE_BYTES).expect("cap fits");
    big.write_host(&vec![b'#'; cap + 1]);
    assert_not_ready(&real(&big), "config.yaml is unreadable", "over the cap");

    let bytes = Home::tainted();
    bytes.write_host(&[0xff, 0xfe, b'\n']);
    assert_not_ready(&real(&bytes), "config.yaml is unreadable", "not UTF-8");
}

/// 🔴 #9741 B4: a `config.yaml` that does not parse, carries an unknown key,
/// names an unknown version or has no `channels:` section denies.
#[test]
fn config_unparseable_or_unknown_key_denies() {
    for (text, branch) in [
        (
            "channels: [unclosed\n",
            "config.yaml is unparseable or invalid",
        ),
        (
            "channels:\n  version: 1\n  pigeon: {}\n",
            "config.yaml is unparseable or invalid",
        ),
        (
            "channels:\n  version: 9\n",
            "config.yaml is unparseable or invalid",
        ),
        (
            "default_model: sonnet\n",
            "config.yaml has no channels section",
        ),
    ] {
        let home = Home::tainted();
        home.write_host(text.as_bytes());
        assert_not_ready(&real(&home), branch, text);
    }
}

/// 🔴 #9741 B5: a denied load (two files routing one recipient) denies.
#[test]
fn denied_report_denies() {
    let home = Home::tainted();
    let a = format!("version = 2\n{}", slack_route("bob-a", BOB));
    let b = format!("version = 2\n{}", slack_route("bob-b", BOB));
    let report = merged(&home, &[("a", &a), ("b", &b)], false);
    assert!(report.denied, "fixture: {report:#?}");
    assert_not_ready(
        &from_report(&home, report),
        "the policy load was denied",
        "overlap",
    );
}

/// 🔴 #9741 B6: a load that dropped one route denies, though another route
/// is in effect and every file is effective.
#[test]
fn finding_denies_a_gated_call() {
    let home = Home::tainted();
    let text = format!(
        "version = 2\n{}\n[[telegram.routes]]\nname = \"bob-tg\"\nrecipient = \"123456789\"\nkinds = [\"question\"]\n",
        slack_route("bob-dm", BOB)
    );
    let report = merged(&home, &[("proj", &text)], true);
    assert!(!report.denied && !report.findings.is_empty(), "{report:#?}");
    assert!(!report.policy.routes().is_empty(), "fixture: a route stays");
    assert!(
        report
            .per_file
            .iter()
            .all(|s| matches!(s.state, FileState::Effective { .. })),
        "fixture: {report:#?}"
    );
    assert_not_ready(
        &from_report(&home, report),
        "ChannelDisabled",
        "a dropped route",
    );
}

/// 🔴 #9741 B6: a refused route file denies, though another file loads.
#[test]
fn refused_project_file_denies() {
    let home = Home::tainted();
    let good = format!("version = 2\n{}", slack_route("bob-dm", BOB));
    let report = merged(&home, &[("good", &good), ("bad", "version = [")], false);
    assert!(!report.policy.routes().is_empty(), "fixture: {report:#?}");
    assert_not_ready(&from_report(&home, report), "FileRefused", "refused file");
}

/// 🔴 #9741 B6: a listed project whose route file is not in effect denies,
/// with no finding naming it.
#[test]
fn file_not_in_effect_denies_a_gated_call() {
    let home = Home::tainted();
    for state in [FileState::Missing, FileState::Withheld, FileState::Stale] {
        let mut report = bob_report(&home);
        let mut odd = report.per_file[0].clone();
        odd.state = state;
        report.per_file.push(odd);
        assert_not_ready(
            &from_report(&home, report),
            &format!("{state:?}"),
            "a file not in effect",
        );
    }
}

/// 🔴 #9741 B7: a load past its wait denies, and the deadline handed to the
/// load (which kills its git) falls before the wait ends.
#[test]
fn load_past_deadline_denies() {
    let home = Home::tainted();
    let (tx, rx) = mpsc::channel();
    let limit = Duration::from_millis(200);
    let started = Instant::now();
    let req = home.request();
    let report = bob_report(&home);
    let verdict = gated(&home, move || {
        check_with(
            move |_: &LoadRequest, deadline: Instant| {
                let _ = tx.send(deadline);
                std::thread::sleep(Duration::from_secs(2));
                report
            },
            req,
            limit,
        )
    });
    assert_not_ready(&verdict, "did not finish within", "hung load");
    let deadline = rx.recv().expect("the loader ran");
    assert!(
        deadline < started + limit,
        "git must be killed before the wait ends"
    );
    assert!(started.elapsed() < Duration::from_secs(2), "it waited");
}

/// 🔴 #9741 B7: a load that panics denies.
#[test]
fn load_panic_denies() {
    let home = Home::tainted();
    let req = home.request();
    let verdict = gated(&home, move || {
        check_with(
            |_: &LoadRequest, _: Instant| -> LoadReport { panic!("synthetic load panic") },
            req,
            LIMIT,
        )
    });
    assert_not_ready(&verdict, "stopped without a result", "panic");
}

/// 🔴 #9741 B8: an enabled channel with no route in effect denies, from a
/// file with no route and from a ceiling listing no project.
#[test]
fn empty_policy_with_enabled_channel_denies() {
    let home = Home::tainted();
    let report = merged(&home, &[("proj", "version = 2\n")], false);
    assert!(
        !report.denied && report.findings.is_empty(),
        "fixture: {report:#?}"
    );
    assert_not_ready(
        &from_report(&home, report),
        "no route is in effect",
        "empty file",
    );

    let none = Home::tainted();
    none.write_host(host(&[], false).as_bytes());
    assert_not_ready(&real(&none), "no route is in effect", "no project");
}

/// 🔴 #9741 (ruling a): a ready policy falls through to the taint deny. The
/// real load runs over a committed route file in a temp git repo.
#[test]
fn ready_policy_falls_through_to_taint_deny() {
    let home = Home::tainted();
    let dir = home.home.join("proj");
    std::fs::create_dir(&dir).expect("mkdir");
    git(&dir, &["init", "-q", "-b", "main"]);
    git(&dir, &["commit", "-q", "--allow-empty", "-m", "root"]);
    std::fs::create_dir_all(dir.join(".trusty-channels")).expect("mkdir");
    let routes = format!("version = 2\n{}", slack_route("bob-dm", BOB));
    std::fs::write(dir.join(".trusty-channels/routes.toml"), routes).expect("write");
    git(&dir, &["add", ".trusty-channels/routes.toml"]);
    git(&dir, &["commit", "-q", "-m", "routes"]);
    home.write_host(host(&[&dir], false).as_bytes());

    let started = Instant::now();
    let at = home.home.clone();
    let verdict = gated(&home, move || check(Some(&at), LIMIT));
    // #9741: the inline load's wall time, recorded by the release-build run.
    eprintln!("#9741 inline policy load: {:?}", started.elapsed());
    match verdict {
        Verdict::Deny { check, reason } => {
            assert_eq!(check, TAINT_CHECK, "{reason}");
            assert!(reason.contains("channel-tainted"), "{reason}");
        }
        Verdict::Allow => panic!("a ready policy must not allow a tainted gated call"),
    }
    assert_eq!(HOOK_LOAD_LIMIT, Duration::from_millis(1500));
}

/// #9741: an untainted session, a non-gated tool and an earlier deny never
/// consult the policy, so they pay no load.
#[test]
fn untainted_session_never_reads_the_policy() {
    let home = Home::tainted();
    let never = || -> Result<(), NotReady> { panic!("the policy was read") };
    let clean = with_taint(
        Verdict::Allow,
        &read_payload(Some("s-clean")),
        Some(&home.home),
        never,
    );
    assert_eq!(clean, Verdict::Allow);
    let todo = Ok(json!({
        "session_id": SESSION,
        "tool_name": "TodoWrite",
        "tool_input": {},
    }));
    assert_eq!(
        with_taint(Verdict::Allow, &todo, Some(&home.home), never),
        Verdict::Allow
    );
    let first = Verdict::Deny {
        check: "egress-family",
        reason: "x".into(),
    };
    let read = read_payload(Some(SESSION));
    assert_eq!(
        with_taint(first.clone(), &read, Some(&home.home), never),
        first
    );
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
