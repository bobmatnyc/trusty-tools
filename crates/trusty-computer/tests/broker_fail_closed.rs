//! Every failure branch of the call pipeline fails closed (issue #11460, Phase 3).
//!
//! Each case names the error arm it pins. Run against a fail-open variant of
//! that arm (the arm removed, or its value defaulted), the case goes red.

mod common;

use common::OUT_OF_MANIFEST_PID;
use common::rig::{Conn, Rig, WINDOW, call, pid};
use serde_json::{Value, json};
use trusty_computer::journal::Outcome;

fn refused(rig: &Rig) -> Value {
    rig.audit().last().expect("line")["refused"].clone()
}

#[test]
fn every_gate_refusal_never_reaches_driver() {
    let mut rig = Rig::new();
    let g = rig.grant.id.clone();
    let p = pid();
    let cases: Vec<(Value, &str)> = vec![
        (
            json!({"op": "call", "tool": "list_apps", "request_id": "a"}),
            "grant_required",
        ),
        (
            call("g-000000000000", "list_apps", "b", json!({})),
            "grant_not_bound",
        ),
        (
            json!({"op": "call", "grant_id": g, "tool": 7, "request_id": "c"}),
            "bad_request",
        ),
        (
            json!({"op": "call", "grant_id": g, "tool": "list_apps", "request_id": "d", "args": [1]}),
            "bad_args",
        ),
        (call(&g, "no_such_tool", "e", json!({})), "unknown_tool"),
        (call(&g, "kill_app", "f", json!({"pid": p})), "denied_tool"),
        (
            call(&g, "press_key", "h", json!({"pid": p, "key": "a"})),
            "tool_not_granted",
        ),
        (
            call(
                &g,
                "get_window_state",
                "i",
                json!({"pid": p, "window_id": 2}),
            ),
            "window_out_of_scope",
        ),
        (
            call(
                &g,
                "click",
                "j",
                json!({"pid": p, "window_id": WINDOW, "element_token": "tok-button"}),
            ),
            "token_not_exposed",
        ),
        (json!({"op": "mint", "bundle_id": "x"}), "unknown_op"),
        (json!({"op": "restart"}), "unknown_op"),
    ];
    for (req, code) in cases {
        let reply = rig.conn.send(&req);
        assert_eq!(reply["code"], code, "{req} -> {reply}");
        assert_eq!(refused(&rig), code, "{req} audited");
    }
    assert!(rig.agent_calls().is_empty(), "{:?}", rig.agent_calls());
}

#[test]
fn call_without_the_lease_is_refused() {
    let rig = Rig::new();
    let mut other = Conn::open(&rig.server.agent);
    let reply = other.send(&call(&rig.grant.id, "list_apps", "x", json!({})));
    assert_eq!(reply["code"], "grant_not_bound", "an unbound session");
    let queued_grant = rig.mint(2);
    let code = rig.code(&queued_grant.id);
    assert_eq!(
        other.send(&json!({"op": "connect", "code": code}))["ok"],
        true
    );
    let reply = other.send(&call(&queued_grant.id, "list_apps", "y", json!({})));
    assert_eq!(reply["code"], "lease_not_held", "a bound, queued session");
    assert!(rig.agent_calls().is_empty());
}

#[test]
fn agent_ops_are_refused_on_the_operator_socket() {
    let rig = Rig::new();
    let reply = rig.op(&call(&rig.grant.id, "list_apps", "x", json!({})));
    assert_eq!(reply["code"], "unknown_op");
    assert!(rig.agent_calls().is_empty());
}

#[test]
fn connect_refusals() {
    let mut rig = Rig::new();
    let other = rig.mint(2);
    let mut conn = Conn::open(&rig.server.agent);
    assert_eq!(
        conn.send(&json!({"op": "connect"}))["code"],
        "grant_required"
    );
    let unknown = conn.send(&json!({"op": "connect", "grant_id": "g-nope"}));
    assert_eq!(unknown["code"], "grant_not_bound");
    let unbound = conn.send(&json!({"op": "connect", "grant_id": rig.grant.id}));
    assert_eq!(
        unbound["code"], "grant_not_bound",
        "a grant id alone is not enough"
    );
    let code = rig.code(&other.id);
    let mismatch = rig.conn.send(&json!({"op": "connect", "code": code}));
    assert_eq!(mismatch["code"], "lease_grant_mismatch");
}

#[test]
fn revoked_grant_is_refused_and_its_lease_dropped() {
    let mut rig = Rig::new();
    rig.broker.operator_revoke(&rig.grant.id).expect("revoke");
    let reply = rig.call("list_apps", "r1", json!({}));
    assert_eq!(reply["code"], "grant_expired", "{reply}");
    assert_eq!(
        rig.conn.send(&json!({"op": "status"}))["result"]["lease"]["lease"],
        "none"
    );
    let again = rig
        .conn
        .send(&json!({"op": "connect", "grant_id": rig.grant.id}));
    assert_eq!(again["code"], "grant_expired");
}

#[test]
fn idle_grant_is_refused() {
    let mut rig = Rig::new();
    rig.advance(301.0);
    assert_eq!(
        rig.call("list_apps", "r1", json!({}))["code"],
        "grant_expired"
    );
    assert!(rig.agent_calls().is_empty());
}

#[test]
fn tool_error_is_driver_refused() {
    let mut rig = Rig::with(&[], OUT_OF_MANIFEST_PID);
    let reply = rig.call(
        "get_window_state",
        "r1",
        json!({"pid": pid(), "window_id": OUT_OF_MANIFEST_PID}),
    );
    assert_eq!(
        (&reply["ok"], &reply["code"]),
        (&json!(false), &json!("driver_refused"))
    );
    let done = rig.audit().last().expect("line").clone();
    assert_eq!(
        (&done["phase"], &done["ok"]),
        (&json!("done"), &json!(false))
    );
    let click = rig.call(
        "click",
        "r2",
        json!({"pid": pid(), "window_id": OUT_OF_MANIFEST_PID, "element_token": "tok-button"}),
    );
    assert_eq!(
        click["code"], "token_not_exposed",
        "a refused snapshot exposes nothing"
    );
}

#[test]
fn driver_io_failure_is_driver_error() {
    let mut rig = Rig::new();
    rig.snap("s1");
    rig.inner().supervisor.launcher_mut().daemon = None; // the daemon vanished
    let reply = rig.click("c1", "tok-button");
    assert_eq!(reply["code"], "driver_error", "{reply}");
    assert_eq!(rig.audit().last().expect("line")["ok"], false);
}

#[test]
fn mutating_call_cut_by_session_end_is_outcome_unknown() {
    let mut rig = Rig::new();
    rig.snap("s1");
    let session = rig.inner().supervisor.session().to_owned();
    rig.daemon(|d| d.end_session(&session));
    let reply = rig.click("c1", "tok-button");
    assert_eq!(reply["code"], "outcome_unknown", "{reply}");
    let entry = rig
        .inner()
        .journal(&rig.grant.id)
        .and_then(|j| j.replay("c1"))
        .cloned();
    assert_eq!(entry.map(|e| e.outcome), Some(Outcome::Unknown));
    let replay = rig.click("c1", "tok-button");
    assert_eq!(replay["code"], "outcome_unknown");
    assert_eq!(rig.fixture()["clicks"], 0);
    let clicks = rig.agent_calls().iter().filter(|c| *c == "click").count();
    assert_eq!(clicks, 1, "the click was sent once and never re-run");
}

#[test]
fn unverifiable_effect_is_not_confirmed() {
    let mut rig = Rig::new();
    rig.snap("s1");
    rig.daemon(|d| d.set_effect("unverifiable"));
    let reply = rig.click("c1", "tok-inert");
    assert_eq!(reply["ok"], true);
    assert_eq!(reply["confirmation"]["confirmed"], false, "{reply}");
    assert_eq!(reply["confirmation"]["driver_effect"], "unverifiable");
    assert!(
        reply["hint"]
            .as_str()
            .is_some_and(|h| h.contains("do not retry"))
    );
    assert_eq!(rig.audit().last().expect("line")["confirmed"], false);
}

#[test]
fn readback_failure_is_not_confirmed() {
    let mut rig = Rig::new();
    rig.snap("s1");
    rig.daemon(|d| {
        d.break_readback();
        d.set_effect("confirmed");
    });
    let reply = rig.click("c1", "tok-button");
    assert_eq!(reply["ok"], true, "the click ran: {reply}");
    assert_eq!(reply["confirmation"]["confirmed"], false, "{reply}");
    assert!(reply["confirmation"]["readback_error"].is_string());
    assert_eq!(rig.fixture()["clicks"], 1);
    let again = rig.click("c2", "tok-button");
    assert_eq!(
        again["code"], "token_not_exposed",
        "the stale snapshot was dropped"
    );
}

#[test]
fn unchanged_readback_is_not_confirmed_without_a_prior_fingerprint() {
    let mut rig = Rig::with(&["launch_app"], WINDOW);
    let reply = rig.call(
        "launch_app",
        "l1",
        json!({"bundle_id": "dev.trusty.computer.fixture"}),
    );
    assert_eq!(reply["ok"], true, "{reply}");
    assert_eq!(reply["confirmation"]["confirmed"], false);
}
