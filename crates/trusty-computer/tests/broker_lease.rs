//! The desktop lease across sessions, rescope on handoff, and the idle-timeout lockout.
//!
//! Every case runs over the real sockets on the fake daemon (`common::rig`).

mod common;

use std::time::{Duration, Instant};

use common::rig::{Conn, Rig, WINDOW, call, pid};
use serde_json::{Value, json};

fn connect(rig: &Rig, grant: &str) -> (Conn, Value) {
    let mut conn = Conn::open(&rig.server.agent);
    let reply = conn.send(&json!({"op": "connect", "code": rig.code(grant)}));
    (conn, reply)
}

fn lease_of(conn: &mut Conn) -> Value {
    conn.send(&json!({"op": "status"}))["result"]["lease"].clone()
}

fn wait_held(conn: &mut Conn) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while lease_of(conn)["lease"] != "held" {
        assert!(Instant::now() < deadline, "lease never moved");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn second_session_is_queued_fifo_while_first_holds() {
    let mut rig = Rig::new();
    let (mut two, r2) = connect(&rig, &rig.mint(WINDOW).id);
    let (mut three, r3) = connect(&rig, &rig.mint(WINDOW).id);
    assert_eq!(r2["result"]["position"], 1, "{r2}");
    assert_eq!(r3["result"]["position"], 2, "{r3}");
    assert_eq!(rig.conn.send(&json!({"op": "release"}))["ok"], true);
    assert_eq!(lease_of(&mut two)["lease"], "held");
    assert_eq!(
        lease_of(&mut three),
        json!({"lease": "queued", "position": 1})
    );
    assert_eq!(lease_of(&mut rig.conn)["lease"], "none");
}

#[test]
fn queued_session_call_is_refused() {
    let rig = Rig::new();
    let own = rig.mint(WINDOW).id;
    let (mut two, _) = connect(&rig, &own);
    let reply = two.send(&call(&own, "list_apps", "q1", json!({})));
    assert_eq!(reply["code"], "lease_not_held", "{reply}");
    assert!(rig.agent_calls().is_empty());
    assert_eq!(
        rig.audit().last().expect("line")["refused"],
        "lease_not_held"
    );
}

#[test]
fn idle_holder_loses_the_lease() {
    let mut rig = Rig::new();
    let (mut two, _) = connect(&rig, &rig.mint(WINDOW).id);
    rig.advance(119.0);
    assert_eq!(rig.call("list_apps", "a1", json!({}))["ok"], true);
    rig.advance(121.0);
    let reply = rig.call("list_apps", "a2", json!({}));
    assert_eq!(reply["code"], "lease_not_held", "{reply}");
    assert_eq!(lease_of(&mut two)["lease"], "held");
    assert!(rig.audit().iter().any(|l| l["op"] == "lease_expired"));
}

#[test]
fn disconnect_releases_the_lease() {
    let mut rig = Rig::new();
    let (mut two, _) = connect(&rig, &rig.mint(WINDOW).id);
    rig.conn = Conn::open(&rig.server.agent); // drops the holder's connection
    wait_held(&mut two);
    assert!(rig.audit().iter().any(|l| l["op"] == "disconnect"));
}

#[test]
fn handoff_rescopes_to_the_next_grant() {
    let mut rig = Rig::new();
    let other = rig.mint(2);
    let (mut two, _) = connect(&rig, &other.id);
    rig.snap("s1");
    rig.conn.send(&json!({"op": "release"}));
    wait_held(&mut two);
    {
        let inner = rig.inner();
        let sup = &inner.supervisor;
        assert_eq!(sup.scope().map(|s| s.window_id), Some(2));
        assert_eq!(sup.launcher().starts, 2);
        assert!(sup.launcher().manifests[1].contains("window_id: 2"));
    }
    // The old window's snapshot was dropped with the handoff.
    let snap = two.send(&call(
        &other.id,
        "get_window_state",
        "t1",
        json!({"pid": pid(), "window_id": 2}),
    ));
    assert_eq!(snap["ok"], true, "{snap}");
}

#[test]
fn same_scope_handoff_does_not_restart() {
    let mut rig = Rig::new();
    let same = rig.mint(WINDOW);
    let (mut two, _) = connect(&rig, &same.id);
    rig.conn.send(&json!({"op": "release"}));
    wait_held(&mut two);
    assert_eq!(rig.inner().supervisor.launcher().starts, 1);
}

#[test]
fn failed_rescope_refuses_the_call() {
    let mut rig = Rig::new();
    let other = rig.mint(2);
    let (mut two, _) = connect(&rig, &other.id);
    rig.inner().supervisor.launcher_mut().fail_start = true;
    rig.conn.send(&json!({"op": "release"}));
    wait_held(&mut two);
    let args = json!({"pid": pid(), "window_id": 2});
    let reply = two.send(&call(&other.id, "get_window_state", "f1", args.clone()));
    assert_eq!(reply["code"], "rescope_failed", "{reply}");
    rig.inner().supervisor.launcher_mut().fail_start = false;
    let reply = two.send(&call(&other.id, "get_window_state", "f2", args));
    assert_eq!(
        reply["ok"], true,
        "recovers once the daemon starts: {reply}"
    );
}

#[test]
fn idle_timeout_lockout_restarts_and_retries_read_only() {
    let mut rig = Rig::new();
    rig.snap("s1");
    rig.daemon(|d| d.lock_out());
    let result = rig.snap("s2");
    assert_eq!(result["structuredContent"]["redacted_menu_bar_elements"], 4);
    assert_eq!(rig.inner().supervisor.launcher().starts, 2);
}

#[test]
fn idle_timeout_lockout_does_not_rerun_mutating_call() {
    let mut rig = Rig::new();
    rig.snap("s1");
    rig.daemon(|d| d.lock_out());
    let reply = rig.click("c1", "tok-button");
    assert_eq!(reply["code"], "driver_restarted", "{reply}");
    assert_eq!(rig.fixture()["clicks"], 0);
    assert!(rig.agent_calls().is_empty(), "the new daemon saw no click");
    // The snapshot died with the old daemon: a fresh one is needed, then a new id runs.
    assert_eq!(rig.click("c2", "tok-button")["code"], "token_not_exposed");
    rig.snap("s2");
    assert_eq!(rig.click("c3", "tok-button")["ok"], true);
    assert_eq!(rig.fixture()["clicks"], 1);
}
