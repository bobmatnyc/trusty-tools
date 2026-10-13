//! Route-change audit arms of the daemon snapshot (#9832, #8433 P3 D5).
//!
//! Why: no route set may go live without a durable record naming its commit
//! and author, and a failed record must keep the routes already in effect.
//! Each test fails an implementation that swaps first, logs every refresh,
//! reuses an author, or reads the branch tip's author.
//! What: real `LoaderSource` loads over temp homes and temp git repos, with
//! the snapshot helpers of `snapshot_tests.rs`.
//! Test: this is the test module.

use std::sync::Mutex;

use trusty_channels::audit::AuditRecord;
use trusty_channels::policy::{GateError, commit_author};

use super::*;
use crate::channels::audit_log::{
    AuditAppend, AuthorLookup, RouteAudit, append, audit_path, change_records,
};

const SENTINEL: &str = "SENTINEL-9832-do-not-log";

/// The keys a route-change line may carry.
const ROUTE_KEYS: [&str; 8] = [
    "ts",
    "event",
    "reason",
    "project",
    "commit",
    "author",
    "committed_at",
    "routes",
];

/// Run git in `dir`; its trimmed stdout.
fn git_out(dir: &Path, args: &[&str]) -> String {
    let mut cmd = trusty_common::git::command();
    cmd.current_dir(dir);
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            cmd.env_remove(key);
        }
    }
    let out = cmd.args(args).output().expect("run git");
    assert!(out.status.success(), "git {args:?}: {out:?}");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// Commit the routes file as `who`.
fn commit_routes_as(dir: &Path, text: &str, who: &str) {
    write_routes(dir, text);
    git(dir, &["add", ".trusty-channels/routes.toml"]);
    let name = format!("user.name={who}");
    git(dir, &["-c", &name, "commit", "-q", "-m", "routes"]);
}

/// Commit a README as `who`: the branch moves, the routes do not.
fn commit_readme_as(dir: &Path, who: &str) {
    let n = git_out(dir, &["rev-list", "--count", "HEAD"]);
    std::fs::write(dir.join("README"), n).expect("write");
    git(dir, &["add", "README"]);
    let name = format!("user.name={who}");
    git(dir, &["-c", &name, "commit", "-q", "-m", "readme"]);
}

/// Every line of the audit log under `home`; none when it does not exist.
fn audit_lines(home: &Home) -> Vec<serde_json::Value> {
    match std::fs::read_to_string(audit_path(&home.home)) {
        Ok(text) => text
            .lines()
            .map(|l| serde_json::from_str(l).expect("one JSON object per line"))
            .collect(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => panic!("read audit log: {e}"),
    }
}

/// Unix seconds as the log writes them.
fn rfc3339(secs: i64) -> String {
    chrono::DateTime::from_timestamp(secs, 0)
        .expect("time in range")
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// A listed repo whose routes Alice committed: bob's route.
fn alice_project() -> (Home, PathBuf) {
    let home = Home::new();
    let dir = repo(&home, "proj");
    commit_routes_as(&dir, &routes(&[("bob-dm", BOB)]), "Alice");
    home.write_host(&host(&[&dir]));
    (home, dir)
}

fn eve_allowed<S: PolicySource + 'static, C: Clock + Send + Sync + 'static>(
    snap: &PolicySnapshot<S, C>,
) -> bool {
    snap.check_egress(Channel::Slack, EVE, MessageKind::Question)
        .is_ok()
}

/// Put a directory where the log goes, so no append can open it.
fn break_log(home: &Home) {
    let path = audit_path(&home.home);
    let _ = std::fs::remove_file(&path);
    std::fs::create_dir_all(&path).expect("block the log path");
}

fn repair_log(home: &Home) {
    std::fs::remove_dir(audit_path(&home.home)).expect("unblock the log path");
}

#[test]
fn first_load_is_recorded() {
    let (home, dir) = alice_project();
    let tip = git_out(&dir, &["rev-parse", "HEAD"]);
    let snap = PolicySnapshot::new(LoaderSource::new(home.request()), TestClock::default());
    settle(&snap);
    assert_eq!(bob_verdicts(&snap), Ok(()));
    let lines = audit_lines(&home);
    assert_eq!(lines.len(), 1, "{lines:#?}");
    let line = &lines[0];
    assert_eq!(line["event"], "route_changed");
    assert_eq!(line["reason"], "first_load");
    assert_eq!(line["project"], dir.display().to_string());
    assert_eq!(line["commit"], tip);
    assert_eq!(line["author"], "Alice (inferred)");
    assert_eq!(line["routes"], 1);
}

#[test]
fn route_change_is_logged_with_author_and_time() {
    let (home, dir) = alice_project();
    let clock = TestClock::default();
    let snap = PolicySnapshot::new(LoaderSource::new(home.request()), clock.clone());
    settle(&snap);
    assert_eq!(bob_verdicts(&snap), Ok(()));
    commit_routes_as(&dir, &routes(&[("bob-dm", BOB), ("eve-dm", EVE)]), "Bob");
    let bob_time: i64 = git_out(&dir, &["log", "-1", "--format=%ct"])
        .parse()
        .expect("%ct");
    // Carol moves the branch after Bob; she did not change the routes.
    commit_readme_as(&dir, "Carol");
    let tip = git_out(&dir, &["rev-parse", "HEAD"]);
    clock.set(secs(5.0));
    settle(&snap);
    assert!(eve_allowed(&snap), "the reviewed change did not apply");
    let lines = audit_lines(&home);
    assert_eq!(lines.len(), 2, "{lines:#?}");
    let line = &lines[1];
    assert_eq!(line["reason"], "routes_changed");
    assert_eq!(line["commit"], tip, "not the verified commit");
    assert_eq!(line["author"], "Bob (inferred)", "not the routes' author");
    assert_eq!(line["committed_at"], rfc3339(bob_time));
    assert_eq!(line["routes"], 2);
}

#[test]
fn log_never_contains_message_text() {
    let home = Home::new();
    let dir = repo(&home, "proj");
    let text = format!("# {SENTINEL}\n{}", routes(&[("bob-dm", BOB)]));
    commit_routes_as(&dir, &text, "Alice");
    home.write_host(&host(&[&dir]));
    let snap = PolicySnapshot::new(LoaderSource::new(home.request()), TestClock::default());
    settle(&snap);
    assert_eq!(bob_verdicts(&snap), Ok(()));
    let log = std::fs::read_to_string(audit_path(&home.home)).expect("a log");
    for secret in [SENTINEL, BOB, "bob-dm"] {
        assert!(!log.contains(secret), "{secret} reached the log:\n{log}");
    }
    let allowed: Vec<&str> = ROUTE_KEYS.to_vec();
    for line in audit_lines(&home) {
        for key in line.as_object().expect("object").keys() {
            assert!(allowed.contains(&key.as_str()), "unexpected key {key}");
        }
    }
}

#[test]
fn audit_write_failure_keeps_old_loader_and_fingerprint() {
    let (home, dir) = alice_project();
    let clock = TestClock::default();
    let snap = PolicySnapshot::new(LoaderSource::new(home.request()), clock.clone());
    settle(&snap);
    assert_eq!(bob_verdicts(&snap), Ok(()));
    break_log(&home);
    commit_routes_as(&dir, &routes(&[("bob-dm", BOB), ("eve-dm", EVE)]), "Bob");
    clock.set(secs(5.0));
    settle(&snap);
    assert!(!eve_allowed(&snap), "an unrecorded change went live");
    assert_eq!(bob_verdicts(&snap), Ok(()), "old routes inside max age");
    clock.set(secs(61.0));
    settle(&snap);
    assert_snapshot_denies(
        &snap,
        |d| matches!(d, SnapshotDeny::Stale { .. }),
        "unrecorded change past max age",
    );
    // After the repair the next refresh records the change, then serves it.
    repair_log(&home);
    clock.set(secs(66.0));
    settle(&snap);
    assert!(eve_allowed(&snap), "repaired log: change not served");
    let lines = audit_lines(&home);
    assert_eq!(lines.len(), 1, "{lines:#?}");
    assert_eq!(lines[0]["reason"], "routes_changed");
    assert_eq!(lines[0]["author"], "Bob (inferred)");
}

#[test]
fn routes_not_served_until_record_appended() {
    let (home, dir) = alice_project();
    let (gate, held) = gate();
    let held = Mutex::new(held);
    let calls = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&calls);
    let slow: AuditAppend = Arc::new(move |path: &Path, records: &[AuditRecord]| {
        if counted.fetch_add(1, Ordering::SeqCst) == 1 {
            held.lock().expect("held").hold();
        }
        append(path, records)
    });
    let audit = RouteAudit::with(Some(audit_path(&home.home)), Arc::new(commit_author), slow);
    let clock = TestClock::default();
    let snap = PolicySnapshot::new(
        LoaderSource::with_audit(home.request(), audit),
        clock.clone(),
    );
    settle(&snap);
    assert_eq!(bob_verdicts(&snap), Ok(()));
    commit_routes_as(&dir, &routes(&[("bob-dm", BOB), ("eve-dm", EVE)]), "Bob");
    clock.set(secs(5.0));
    let _ = snap.current();
    assert!(gate.entered(), "the change was never appended");
    assert!(!eve_allowed(&snap), "served before its record was appended");
    gate.open();
    wait_idle(&snap);
    assert!(eve_allowed(&snap), "not served after its record");
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[test]
fn author_lookup_failure_records_unknown_inferred() {
    let (home, dir) = alice_project();
    let lookups = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&lookups);
    let author: AuthorLookup =
        Arc::new(
            move |dir: &Path, commit: &str| match counted.fetch_add(1, Ordering::SeqCst) {
                0 => commit_author(dir, commit),
                _ => Err(GateError::GitFailed { step: "log" }),
            },
        );
    let audit = RouteAudit::with(Some(audit_path(&home.home)), author, Arc::new(append));
    let clock = TestClock::default();
    let snap = PolicySnapshot::new(
        LoaderSource::with_audit(home.request(), audit),
        clock.clone(),
    );
    settle(&snap);
    commit_routes_as(&dir, &routes(&[("bob-dm", BOB), ("eve-dm", EVE)]), "Bob");
    clock.set(secs(5.0));
    settle(&snap);
    assert!(eve_allowed(&snap), "a failed lookup blocked the change");
    let lines = audit_lines(&home);
    assert_eq!(lines.len(), 2, "{lines:#?}");
    assert_eq!(lines[0]["author"], "Alice (inferred)");
    assert_eq!(lines[1]["author"], "unknown (inferred)", "author reused");
    assert!(lines[1]["commit"].is_string(), "{:#?}", lines[1]);
    assert!(lines[1].get("committed_at").is_none(), "{:#?}", lines[1]);
}

#[test]
fn missing_commit_records_unknown_author() {
    let (home, dir) = alice_project();
    let mut report = load_effective(&home.request());
    let absent = "1".repeat(40);
    report.per_file[0].verified_commit = Some(absent.clone());
    let lookup: AuthorLookup = Arc::new(commit_author);
    let records = change_records(None, &report, &lookup).expect("a first load is a change");
    assert_eq!(records.len(), 1);
    let line = serde_json::to_value(&records[0]).expect("encode");
    assert_eq!(line["author"], "unknown (inferred)");
    assert_eq!(line["commit"], absent);
    assert_eq!(line["project"], dir.display().to_string());
    assert!(line.get("committed_at").is_none(), "{line:#?}");
}

#[test]
fn unchanged_fingerprint_writes_no_record() {
    let (home, _dir) = alice_project();
    let clock = TestClock::default();
    let snap = PolicySnapshot::new(LoaderSource::new(home.request()), clock.clone());
    for at in [0.0, 5.0, 10.0] {
        clock.set(secs(at));
        settle(&snap);
        assert_eq!(bob_verdicts(&snap), Ok(()));
        assert_eq!(audit_lines(&home).len(), 1, "at {at} s");
    }
}

#[test]
fn unrelated_commit_writes_no_record() {
    let (home, dir) = alice_project();
    let clock = TestClock::default();
    let snap = PolicySnapshot::new(LoaderSource::new(home.request()), clock.clone());
    settle(&snap);
    assert_eq!(audit_lines(&home).len(), 1);
    // The default branch moves, so the fingerprint does; the routes do not.
    commit_readme_as(&dir, "Carol");
    clock.set(secs(5.0));
    settle(&snap);
    assert_eq!(bob_verdicts(&snap), Ok(()));
    assert_eq!(audit_lines(&home).len(), 1, "{:#?}", audit_lines(&home));
}

#[test]
fn host_only_change_records_one_unattributed_line() {
    let (home, dir) = alice_project();
    let clock = TestClock::default();
    let snap = PolicySnapshot::new(LoaderSource::new(home.request()), clock.clone());
    settle(&snap);
    // A lower host rate limit changes the routes; no project file moved.
    let lower = host(&[&dir]).replacen(
        "  version: 1\n",
        "  version: 1\n  rate_limit: { limit: 50, window_secs: 60 }\n",
        1,
    );
    home.write_host(&lower);
    clock.set(secs(5.0));
    settle(&snap);
    let lines = audit_lines(&home);
    assert_eq!(lines.len(), 2, "{lines:#?}");
    assert_eq!(lines[1]["reason"], "routes_changed");
    assert_eq!(lines[1]["author"], "unknown (inferred)");
    assert!(lines[1].get("project").is_none(), "{:#?}", lines[1]);
}

#[test]
fn first_load_audit_failure_stays_never_loaded() {
    let (home, _dir) = alice_project();
    break_log(&home);
    let clock = TestClock::default();
    let snap = PolicySnapshot::new(LoaderSource::new(home.request()), clock.clone());
    settle(&snap);
    assert_eq!(snap.current().err(), Some(SnapshotDeny::NeverLoaded));
    repair_log(&home);
    clock.set(secs(5.0));
    settle(&snap);
    assert_eq!(bob_verdicts(&snap), Ok(()));
    let lines = audit_lines(&home);
    assert_eq!(lines.len(), 1, "{lines:#?}");
    assert_eq!(lines[0]["reason"], "first_load");
}
