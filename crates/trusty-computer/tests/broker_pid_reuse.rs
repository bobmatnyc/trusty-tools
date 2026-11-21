//! Pid reuse (issue #11460): a grant names one process, its bundle and start
//! time, and the broker re-checks both at `grant_confirm` and at every call.
//! An agent that quits the granted app and lands a new process on the same
//! pid gets `app_changed`, and the grant is revoked.

mod common;

use common::dialog::FAKE_START_US;
use common::rig::{BUNDLE, Conn, Rig, WINDOW, grant_req, pid};
use serde_json::{Value, json};

fn snap_args() -> Value {
    json!({"pid": pid(), "window_id": WINDOW})
}

fn grant_count(rig: &Rig) -> usize {
    let reply = rig.op(&json!({"op": "grants"}));
    reply["result"]["grants"].as_array().expect("grants").len()
}

#[test]
fn app_changed_between_request_and_confirm_refuses() {
    let rig = Rig::new();
    let before = grant_count(&rig);
    let mut conn = Conn::open(&rig.server.operator);
    let reply = conn.send(&grant_req(&[], WINDOW));
    let challenge = reply["result"]["challenge"].clone();
    assert!(challenge.is_string(), "{reply}");
    // The app quits; a new process with the same bundle takes the pid.
    rig.resolver
        .set_started(pid(), BUNDLE, "Fixture", FAKE_START_US + 7);
    let code = rig.dialog.last_code();
    let confirm = conn.send(&json!({"op": "grant_confirm", "challenge": challenge, "code": code}));
    assert_eq!(confirm["code"], "app_changed", "{confirm}");
    let line = rig.audit().last().cloned().expect("audit");
    assert_eq!(line["refused"], "app_changed", "{line}");
    assert_eq!(grant_count(&rig), before, "no grant minted");
    // The challenge is gone: the same code cannot be tried again.
    let again = conn.send(&json!({"op": "grant_confirm", "challenge": challenge, "code": code}));
    assert_eq!(again["code"], "bad_challenge", "{again}");
}

#[test]
fn app_changed_between_confirm_and_call_revokes() {
    let mut rig = Rig::new();
    rig.snap("s1");
    let calls = rig.daemon(|d| d.calls().len());
    rig.resolver
        .set_started(pid(), BUNDLE, "Fixture", FAKE_START_US + 7);
    let reply = rig.call("get_window_state", "s2", snap_args());
    assert_eq!(reply["code"], "app_changed", "{reply}");
    assert_eq!(rig.daemon(|d| d.calls().len()), calls, "driver not called");
    let id = rig.grant.id.clone();
    assert!(rig.inner().grant(&id).expect("grant").revoked, "revoked");
    let line = rig.audit().last().cloned().expect("audit");
    assert_eq!(
        (&line["refused"], &line["revoked"]),
        (&json!("app_changed"), &json!(true))
    );
    // The original process back on the pid does not revive the grant.
    rig.resolver
        .set_started(pid(), BUNDLE, "Fixture", FAKE_START_US);
    let reply = rig.call("get_window_state", "s3", snap_args());
    assert_ne!(reply["ok"], true, "{reply}");
}

#[test]
fn vanished_or_rebundled_process_revokes_at_call() {
    for change in ["gone", "bundle"] {
        let mut rig = Rig::new();
        if change == "gone" {
            let mut apps = rig.resolver.apps.lock().expect("apps");
            apps.insert(pid(), Err("no such process".into()));
        } else {
            rig.resolver.set(pid(), "com.example.other", "Other");
        }
        let reply = rig.call("get_window_state", "s1", snap_args());
        assert_eq!(reply["code"], "app_changed", "{change}: {reply}");
        let id = rig.grant.id.clone();
        assert!(rig.inner().grant(&id).expect("grant").revoked, "{change}");
    }
}

#[cfg(target_os = "macos")]
#[test]
fn start_time_reads_a_live_process_and_refuses_a_dead_one() {
    use trusty_computer::app_identity::start_time_us;
    let me = start_time_us(pid()).expect("own start time");
    assert!(me > 1_600_000_000_000_000, "{me}");
    assert_eq!(start_time_us(pid()), Ok(me), "stable for one process");
    let mut child = std::process::Command::new("/usr/bin/true")
        .spawn()
        .expect("spawn");
    let gone = child.id();
    child.wait().expect("reap");
    assert!(
        start_time_us(gone).is_err(),
        "a reaped pid has no start time"
    );
}
