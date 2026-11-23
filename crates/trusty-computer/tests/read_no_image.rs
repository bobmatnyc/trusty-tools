//! `computer_read` never forwards a screenshot: cua-driver attaches an
//! unredacted image to `get_window_state` by default, so the broker asks for
//! none and drops any image part that arrives anyway.

mod common;

use common::FAKE_PNG;
use common::rig::{Rig, WINDOW, pid};
use serde_json::{Value, json};

fn snap_args() -> Value {
    json!({"pid": pid(), "window_id": WINDOW})
}

/// The daemon arguments of each `get_window_state` call.
fn snapshot_args(rig: &Rig) -> Vec<Value> {
    rig.daemon(|d| d.calls())
        .into_iter()
        .filter(|(name, _)| name == "get_window_state")
        .map(|(_, args)| args)
        .collect()
}

#[test]
fn read_asks_driver_for_no_image() {
    let mut rig = Rig::new();
    let reply = rig.call("get_window_state", "r1", snap_args());
    assert_eq!(reply["ok"], true, "{reply}");
    assert!(!reply.to_string().contains(FAKE_PNG), "{reply}");
    assert_eq!(snapshot_args(&rig)[0]["include_screenshot"], false);
}

#[test]
fn read_drops_image_the_driver_sends_anyway() {
    let mut rig = Rig::new();
    rig.daemon(|d| d.set_snapshot_mode("image_always"));
    let reply = rig.call("get_window_state", "r1", snap_args());
    assert_eq!(reply["ok"], true, "{reply}");
    assert!(!reply.to_string().contains(FAKE_PNG), "{reply}");
    let parts = reply["result"]["content"].as_array().expect("content");
    assert!(parts.iter().all(|p| p["type"] == "text"), "{reply}");
}

#[test]
fn refused_read_drops_image() {
    let mut rig = Rig::new();
    rig.daemon(|d| d.set_snapshot_mode("refused"));
    let reply = rig.call("get_window_state", "r1", snap_args());
    assert_eq!(reply["code"], "driver_refused", "{reply}");
    assert!(!reply.to_string().contains(FAKE_PNG), "{reply}");
}

#[test]
fn click_drops_image_the_driver_sends() {
    let mut rig = Rig::new();
    rig.snap("r1");
    rig.daemon(|d| d.set_snapshot_mode("image_always"));
    let reply = rig.click("r2", "tok-button");
    assert_eq!(reply["ok"], true, "{reply}");
    assert!(!reply.to_string().contains(FAKE_PNG), "{reply}");
}
