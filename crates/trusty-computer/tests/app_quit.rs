//! Graceful quit of the granted app (issue #11696 Phase D): the argument
//! checks, the `app_quit` grant flag, the refusal of every other app or pid
//! before any quit is sent, and the broker pipeline on a fake quitter.

mod common;

use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use common::rig::{BUNDLE, Rig, WINDOW, consent, grant_req, pid};
use serde_json::{Value, json};

use std::time::{Duration, Instant};

use trusty_computer::app_quit::{
    CHECKED_MARK, GRANT_FLAG, PENDING_HINT, QUIT_TOOL, QuitOutcome, Quitter, SystemQuitter,
};
use trusty_computer::grants::DEFAULT_TOOLS;
use trusty_computer::policy::{Scope, Verdict, check_call, rule};

fn scope() -> Scope {
    Scope {
        pids: [i64::from(pid())].into(),
        bundle_ids: [BUNDLE.to_owned()].into(),
    }
}

fn code(args: Value) -> Option<&'static str> {
    check_call(QUIT_TOOL, &args, &scope()).err().map(|e| e.code)
}

fn quit(rig: &mut Rig, rid: &str, bundle: &str) -> Value {
    rig.call(QUIT_TOOL, rid, json!({"pid": pid(), "bundle_id": bundle}))
}

#[test]
fn quit_args_are_checked() {
    assert_eq!(rule(QUIT_TOOL).expect("rule").verdict, Verdict::Filter);
    assert_eq!(code(json!({"pid": pid(), "bundle_id": BUNDLE})), None);
    for (args, want) in [
        (json!({"bundle_id": BUNDLE}), "pid_required"),
        (json!({"pid": 1, "bundle_id": BUNDLE}), "pid_out_of_scope"),
        (json!({"pid": pid()}), "app_out_of_scope"),
        (
            json!({"pid": pid(), "bundle_id": "com.apple.Safari"}),
            "app_out_of_scope",
        ),
        (json!({"pid": pid(), "bundle_id": 7}), "app_out_of_scope"),
        (
            json!({"pid": pid(), "bundle_id": BUNDLE, "force": true}),
            "quit_arg",
        ),
    ] {
        assert_eq!(code(args.clone()), Some(want), "{args}");
    }
    // The driver's kill -9 stays DENY.
    assert_eq!(rule("kill_app").expect("kill_app").verdict, Verdict::Deny);
}

/// Refusal tests: no flag, another app, another pid, `kill_app`, and the
/// tool's own name in a grant; no quit is sent for any of them.
#[test]
fn other_apps_are_refused_before_any_quit() {
    assert!(!DEFAULT_TOOLS.contains(&GRANT_FLAG));
    assert!(!DEFAULT_TOOLS.contains(&QUIT_TOOL));
    let mut bare = Rig::new();
    let reply = quit(&mut bare, "q0", BUNDLE);
    assert_eq!(reply["code"], "tool_not_granted", "{reply}");
    let mut rig = Rig::with(&[GRANT_FLAG], WINDOW);
    let reply = quit(&mut rig, "q1", "com.apple.finder");
    assert_eq!(reply["code"], "app_out_of_scope", "{reply}");
    let reply = rig.call(QUIT_TOOL, "q2", json!({"pid": 1, "bundle_id": BUNDLE}));
    assert_eq!(reply["code"], "pid_out_of_scope", "{reply}");
    let reply = rig.call("kill_app", "q3", json!({"pid": pid()}));
    assert_eq!(reply["code"], "denied_tool", "{reply}");
    let reply = consent(
        &rig.server.operator,
        &rig.dialog,
        &grant_req(&[QUIT_TOOL], WINDOW),
    );
    assert_eq!(reply["code"], "bad_tool", "{reply}");
    assert!(bare.quitter.sent().is_empty());
    assert!(rig.quitter.sent().is_empty());
}

/// The pid no longer runs the granted app: the grant dies, no quit is sent.
#[test]
fn replaced_app_is_not_quit() {
    let mut rig = Rig::with(&[GRANT_FLAG], WINDOW);
    rig.resolver.set(pid(), "com.apple.Safari", "Safari");
    let reply = quit(&mut rig, "q1", BUNDLE);
    assert_eq!(reply["code"], "app_changed", "{reply}");
    // The re-check revokes the grant; the audit line records it.
    let line = rig
        .audit()
        .into_iter()
        .rfind(|r| r["tool"] == QUIT_TOOL)
        .expect("audit line");
    assert_eq!(line["refused"], "app_changed", "{line}");
    assert_eq!(line["revoked"], true, "{line}");
    assert!(rig.quitter.sent().is_empty());
}

#[test]
fn quit_runs_only_for_the_granted_app() {
    let mut rig = Rig::with(&[GRANT_FLAG], WINDOW);
    let reply = quit(&mut rig, "q1", BUNDLE);
    assert_eq!(reply["ok"], true, "{reply}");
    assert_eq!(
        reply["result"]["structuredContent"],
        json!({"quit_requested": true, "status": "accepted", "pid": pid(), "bundle_id": BUNDLE})
    );
    assert_eq!(rig.quitter.sent(), [(pid(), BUNDLE.to_owned())]);
    // A retry of the same request is answered from the journal, not re-sent.
    let again = quit(&mut rig, "q1", BUNDLE);
    assert_eq!(again["ok"], true, "{again}");
    assert_eq!(rig.quitter.sent().len(), 1);
    // The daemon is never asked to quit or kill anything.
    let daemon = rig.daemon(|d| d.calls());
    assert!(
        !daemon
            .iter()
            .any(|(n, _)| n == "kill_app" || n == QUIT_TOOL),
        "{daemon:?}"
    );
    let done = rig
        .audit()
        .into_iter()
        .rfind(|r| r["tool"] == QUIT_TOOL && r["phase"] == "done")
        .expect("done line");
    assert_eq!(done["verdict"], "FILTER");
    assert_eq!(done["quit_bundle_id"], BUNDLE);
}

#[test]
fn quitter_failure_is_reported() {
    let mut rig = Rig::with(&[GRANT_FLAG], WINDOW);
    rig.quitter.broken.store(true, Ordering::SeqCst);
    let reply = quit(&mut rig, "q1", BUNDLE);
    assert_eq!(reply["code"], "quit_error", "{reply}");
}

/// A save prompt: the quit was delivered but the app has not answered.
/// It is `ok` with status `pending` and a hint, never `quit_error`.
#[test]
fn pending_quit_is_not_an_error() {
    let mut rig = Rig::with(&[GRANT_FLAG], WINDOW);
    rig.quitter.pending.store(true, Ordering::SeqCst);
    let reply = quit(&mut rig, "q1", BUNDLE);
    assert_eq!(reply["ok"], true, "{reply}");
    let sc = &reply["result"]["structuredContent"];
    assert_eq!(sc["quit_requested"], true, "{reply}");
    assert_eq!(sc["status"], "pending", "{reply}");
    assert_eq!(sc["hint"], PENDING_HINT, "{reply}");
    let done = rig
        .audit()
        .into_iter()
        .rfind(|r| r["tool"] == QUIT_TOOL && r["phase"] == "done")
        .expect("done line");
    assert_eq!(done["quit_status"], "pending");
}

/// A stand-in for `osascript`: a shell script that ignores its arguments.
fn fake_osascript(dir: &tempfile::TempDir, body: &str) -> SystemQuitter {
    let path = dir.path().join("osascript");
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).expect("write script");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    SystemQuitter {
        program: path.to_string_lossy().into_owned(),
        timeout: Duration::from_millis(300),
    }
}

/// The quit ended in a timeout error.
fn assert_timed_out(r: Result<QuitOutcome, String>) {
    assert!(r.expect_err("timeout").contains("timed out"));
}

/// #11990: a non-timeout error fails the check and is printed in the message.
#[test]
#[should_panic(expected = "got: osascript: Text file busy (os error 26)")]
fn a_non_timeout_error_is_printed() {
    assert_timed_out(Err("osascript: Text file busy (os error 26)".into()));
}

/// #11990: the check stays strict; a quit that did not fail is not a timeout.
#[test]
#[should_panic(expected = "timeout")]
fn a_success_is_not_a_timeout() {
    assert_timed_out(Ok(QuitOutcome::Accepted));
}

/// #11990: a fresh fake script execs while sibling threads fork and exec.
/// A fork that inherits the script's write fd makes the exec fail with
/// `ETXTBSY` on Linux, which is the race the issue saw.
#[test]
fn fake_osascript_spawns_while_siblings_fork() {
    let stop = Arc::new(AtomicBool::new(false));
    let forkers: Vec<_> = (0..4)
        .map(|_| {
            let stop = Arc::clone(&stop);
            std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    let _ = std::process::Command::new("/bin/true").status();
                }
            })
        })
        .collect();
    let mut failures = Vec::new();
    for run in 0..100 {
        let dir = tempfile::tempdir().expect("tmp");
        let mut q = fake_osascript(&dir, "exit 0");
        // A wide deadline: this test is about the spawn, not the timeout.
        q.timeout = Duration::from_secs(10);
        match q.quit(1, "x.y") {
            Ok(QuitOutcome::Accepted) => {}
            other => failures.push(format!("run {run}: {other:?}")),
        }
    }
    stop.store(true, Ordering::Relaxed);
    for f in forkers {
        f.join().expect("forker");
    }
    assert!(
        failures.is_empty(),
        "{} of 100 failed: {failures:?}",
        failures.len()
    );
}

/// The live save-prompt case: the bundle id was checked and quit sent, then
/// no answer before the deadline. The script is killed and the quit is `Pending`.
#[test]
fn system_quitter_timeout_after_check_is_pending() {
    let dir = tempfile::tempdir().expect("tmp");
    let q = fake_osascript(&dir, &format!("echo {CHECKED_MARK} >&2\nexec sleep 30"));
    let start = Instant::now();
    assert_eq!(q.quit(1, "x.y"), Ok(QuitOutcome::Pending));
    assert!(
        start.elapsed() < Duration::from_secs(10),
        "killed at the deadline"
    );
}

/// No mark before the deadline: the check never ran, so it is an error.
/// A clean exit is `Accepted`; a failed one (mismatch, declined) is an error.
#[test]
fn system_quitter_timeout_before_check_is_an_error() {
    let dir = tempfile::tempdir().expect("tmp");
    let hung = fake_osascript(&dir, "exec sleep 30");
    assert_eq!(hung.timeout, Duration::from_millis(300));
    assert_timed_out(hung.quit(1, "x.y"));
    let dir = tempfile::tempdir().expect("tmp");
    let ok = fake_osascript(&dir, &format!("echo {CHECKED_MARK} >&2"));
    assert_eq!(ok.quit(1, "x.y"), Ok(QuitOutcome::Accepted));
    let dir = tempfile::tempdir().expect("tmp");
    let failed = fake_osascript(&dir, &format!("echo {CHECKED_MARK} >&2\nexit 1"));
    assert!(failed.quit(1, "x.y").is_err());
}
