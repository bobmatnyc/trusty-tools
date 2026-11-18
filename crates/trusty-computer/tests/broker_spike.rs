//! Port of the spike's `tests/test_broker.py` (14 cases) on the fake daemon.
//!
//! Each case keeps the spike's name with a `broker_` prefix; the spike's
//! parametrized confirmation case is two tests. The rig adds what the spike
//! had not: a grant from the consent flow and a held lease.

mod common;

use std::os::unix::fs::PermissionsExt;

use common::rig::{Conn, Rig, WINDOW, pid};
use serde_json::json;

fn mode(path: &std::path::Path) -> u32 {
    std::fs::metadata(path).expect("stat").permissions().mode() & 0o777
}

#[test]
fn broker_click_and_type_drive_fixture() {
    let mut rig = Rig::new();
    rig.snap("snap");
    assert_eq!(rig.click("r1", "tok-button")["ok"], true);
    let typed = rig.call(
        "type_text",
        "r2",
        json!({"pid": pid(), "window_id": WINDOW, "element_token": "tok-field", "text": "hi"}),
    );
    assert_eq!(typed["ok"], true, "{typed}");
    let state = rig.fixture();
    assert_eq!(
        (state["clicks"].as_u64(), state["text"].as_str()),
        (Some(1), Some("hi"))
    );
}

#[test]
fn broker_denied_tool_never_reaches_driver() {
    let mut rig = Rig::new();
    let reply = rig.call("kill_app", "r1", json!({"pid": pid()}));
    assert_eq!(
        (&reply["ok"], &reply["code"]),
        (&json!(false), &json!("denied_tool"))
    );
    assert!(rig.agent_calls().is_empty());
}

#[test]
fn broker_filtered_args_never_reach_driver() {
    let mut rig = Rig::new();
    let reply = rig.call(
        "click",
        "r1",
        json!({"pid": 1, "element_token": "tok-button"}),
    );
    assert_eq!(reply["code"], "pid_out_of_scope");
    assert!(rig.agent_calls().is_empty());
}

#[test]
fn broker_request_id_retry_is_not_rerun() {
    let mut rig = Rig::new();
    rig.snap("snap");
    assert_eq!(rig.click("same", "tok-button")["ok"], true);
    assert_eq!(rig.click("same", "tok-button")["ok"], true);
    assert_eq!(rig.fixture()["clicks"], 1);
    assert_eq!(rig.audit().last().expect("line")["replayed"], true);
}

#[test]
fn broker_call_without_request_id_is_refused() {
    let mut rig = Rig::new();
    let reply = rig
        .conn
        .send(&json!({"op": "call", "grant_id": rig.grant.id, "tool": "list_apps", "args": {}}));
    assert_eq!(reply["code"], "request_id_required");
}

#[test]
fn broker_audit_line_per_call() {
    let mut rig = Rig::new();
    rig.snap("snap");
    let before = rig.audit().len();
    rig.call(
        "type_text",
        "r1",
        json!({"pid": pid(), "window_id": WINDOW, "element_token": "tok-field", "text": "secret"}),
    );
    rig.call("clipboard_read", "r2", json!({}));
    let lines = rig.audit()[before..].to_vec();
    let phases: Vec<&str> = lines
        .iter()
        .map(|l| l["phase"].as_str().unwrap_or("refused"))
        .collect();
    assert_eq!(phases, ["forward", "done", "refused"]);
    assert_eq!(
        lines[1]["args"],
        json!({"keys": ["element_token", "pid", "text", "window_id"], "text_len": 6})
    );
    let text = std::fs::read_to_string(rig.audit_path()).expect("audit");
    assert!(!text.contains("secret"));
    assert_eq!(mode(&rig.audit_path()), 0o600);
}

#[test]
fn broker_audit_write_failure_refuses_call() {
    let mut rig = Rig::new();
    rig.snap("snap");
    std::fs::remove_file(rig.audit_path()).expect("rm");
    std::fs::create_dir(rig.audit_path()).expect("mkdir");
    let reply = rig.click("r1", "tok-button");
    assert_eq!(reply["code"], "audit_failed");
    assert_eq!(rig.fixture()["clicks"], 0);
}

#[test]
fn broker_operator_restart_and_status() {
    let rig = Rig::new();
    let restarted = rig.op(&json!({"op": "restart"}));
    assert_eq!(
        restarted["result"]["driver_version"], "0.23.2",
        "{restarted}"
    );
    assert_eq!(
        rig.op(&json!({"op": "status"}))["result"],
        json!({"driver_version": "0.23.2"})
    );
    assert_eq!(rig.op(&json!({"op": "stop"}))["ok"], true);
    assert_eq!(
        rig.op(&json!({"op": "status"}))["result"],
        json!({"driver_version": null})
    );
}

#[test]
fn broker_socket_is_private() {
    let rig = Rig::new();
    assert_eq!(mode(&rig.server.agent), 0o600);
    assert_eq!(mode(&rig.server.operator), 0o600);
    assert_eq!(mode(&rig.state()), 0o700);
}

#[test]
fn broker_bad_json_is_refused() {
    let rig = Rig::new();
    let mut conn = Conn::open(&rig.server.agent);
    assert_eq!(conn.send_raw("not json")["code"], "bad_request");
}

#[test]
fn broker_snapshot_strips_menu_bar_from_structured_and_text() {
    let mut rig = Rig::new();
    let result = rig.snap("snap");
    let elements = result["structuredContent"]["elements"]
        .as_array()
        .expect("elements");
    assert!(
        elements
            .iter()
            .all(|e| e["role"] != "AXMenuBar" && e["role"] != "AXMenuItem")
    );
    let blob = result.to_string();
    for gone in ["Recent Items", "Shut Down", "AXMenuBar"] {
        assert!(!blob.contains(gone), "{gone}");
    }
    assert_eq!(result["structuredContent"]["redacted_menu_bar_elements"], 4);
    assert_eq!(rig.audit().last().expect("line")["redacted"], 4);
}

#[test]
fn broker_menu_token_and_unsnapshotted_click_are_refused() {
    let mut rig = Rig::new();
    assert_eq!(rig.click("r0", "tok-button")["code"], "token_not_exposed");
    rig.snap("snap");
    assert_eq!(rig.click("r1", "tok-shut")["code"], "token_not_exposed");
    assert_eq!(rig.agent_calls(), ["get_window_state"]);
}

fn confirmation_case(token: &str, confirmed: bool) {
    let mut rig = Rig::new();
    rig.snap("snap");
    let reply = rig.click("r1", token);
    assert_eq!(reply["ok"], true, "{reply}");
    assert_eq!(reply["confirmation"]["confirmed"], confirmed, "{reply}");
    assert_eq!(rig.audit().last().expect("line")["confirmed"], confirmed);
}

#[test]
fn broker_click_confirmation_by_readback_button() {
    confirmation_case("tok-button", true);
}

#[test]
fn broker_click_confirmation_by_readback_inert() {
    confirmation_case("tok-inert", false);
}
