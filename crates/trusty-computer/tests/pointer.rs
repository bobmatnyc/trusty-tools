//! Coordinate input bounded to the granted window (issue #11696 Phase A):
//! the argument checks, the frame and bounds helpers, the grant flag, and the
//! broker pipeline against the fake daemon, including the delegation-authority
//! refusal of a point outside the window as it is at event time, and the
//! window-on-top gate of a drag (#11756).

mod common;

use common::rig::{Rig, WINDOW, consent, grant_req, pid};
use std::sync::atomic::Ordering;

use serde_json::{Value, json};
use trusty_computer::grants::DEFAULT_TOOLS;
use trusty_computer::pointer::{
    CLICK_AT_TOOL, DRAG_TOOL, Frame, GRANT_FLAG, disabled, frame, grant_name, within,
};
use trusty_computer::policy::{Scope, Verdict, check_call, rule};
use trusty_computer::window_order::{REFUSAL, UNAVAILABLE, WindowRecord};

fn scope() -> Scope {
    Scope {
        pids: [100].into_iter().collect(),
        ..Scope::default()
    }
}

fn code(tool: &str, args: Value) -> Option<&'static str> {
    check_call(tool, &args, &scope()).err().map(|e| e.code)
}

#[test]
fn coordinate_args_are_checked() {
    let click = json!({"pid": 100, "window_id": 7, "x": 1.5, "y": 0});
    assert_eq!(code(CLICK_AT_TOOL, click), None);
    let drag = json!({"pid": 100, "window_id": 7, "from_x": 0, "from_y": 1, "to_x": 2, "to_y": 3});
    assert_eq!(code(DRAG_TOOL, drag), None);
    for (tool, args, want) in [
        (
            CLICK_AT_TOOL,
            json!({"pid": 101, "window_id": 7, "x": 1, "y": 1}),
            "pid_out_of_scope",
        ),
        (
            CLICK_AT_TOOL,
            json!({"pid": 100, "x": 1, "y": 1}),
            "window_required",
        ),
        (
            CLICK_AT_TOOL,
            json!({"pid": 100, "window_id": 7, "x": 1}),
            "bad_coordinates",
        ),
        (
            CLICK_AT_TOOL,
            json!({"pid": 100, "window_id": 7, "x": -1, "y": 1}),
            "bad_coordinates",
        ),
        (
            CLICK_AT_TOOL,
            json!({"pid": 100, "window_id": 7, "x": "1", "y": 1}),
            "bad_coordinates",
        ),
        (
            CLICK_AT_TOOL,
            json!({"pid": 100, "window_id": 7, "x": 1, "y": 1, "modifier": ["cmd"]}),
            "coordinate_arg",
        ),
        (
            CLICK_AT_TOOL,
            json!({"pid": 100, "window_id": 7, "x": 1, "y": 1, "scope": "desktop"}),
            "coordinate_arg",
        ),
        (
            CLICK_AT_TOOL,
            json!({"pid": 100, "window_id": 7, "x": 1, "y": 1, "from_zoom": true}),
            "coordinate_arg",
        ),
        (
            DRAG_TOOL,
            json!({"pid": 100, "window_id": 7, "from_x": 0, "from_y": 1, "to_x": 2}),
            "bad_coordinates",
        ),
        (
            DRAG_TOOL,
            json!({"pid": 100, "window_id": 7, "from_x": 0, "from_y": 1, "to_x": 2, "to_y": 3, "delivery_mode": "foreground"}),
            "coordinate_arg",
        ),
    ] {
        assert_eq!(code(tool, args.clone()), Some(want), "{tool} {args}");
    }
    for tool in [CLICK_AT_TOOL, DRAG_TOOL] {
        assert_eq!(rule(tool).map(|r| r.verdict), Some(Verdict::Filter));
    }
    // The driver's own pixel tool stays DENY; only the bounded broker tool reaches it.
    assert_eq!(rule("drag").map(|r| r.verdict), Some(Verdict::Deny));
}

/// A daemon capture of `window` reporting a `w` x `h` frame.
fn capture(window: u64, w: f64, h: f64) -> Value {
    json!({"structuredContent": {"window_id": window, "screenshot_frame_valid": true,
        "screenshot_width": w, "screenshot_height": h}})
}

#[test]
fn frame_needs_a_valid_capture_of_the_window() {
    assert_eq!(
        frame(&capture(7, 420.0, 252.0), 7),
        Ok(Frame {
            width: 420.0,
            height: 252.0
        })
    );
    let mut invalid = capture(7, 420.0, 252.0);
    invalid["structuredContent"]["screenshot_frame_valid"] = json!(false);
    let mut errored = capture(7, 420.0, 252.0);
    errored["structuredContent"]["screenshot_error"] = json!("px_frame_mismatch");
    let mut sizeless = capture(7, 420.0, 252.0);
    sizeless["structuredContent"]
        .as_object_mut()
        .map(|o| o.remove("screenshot_height"));
    for bad in [
        capture(8, 420.0, 252.0),
        capture(7, 0.0, 252.0),
        invalid,
        errored,
        sizeless,
        json!({"content": []}),
    ] {
        let code = frame(&bad, 7).err().map(|(c, _)| c);
        assert_eq!(code, Some("frame_unavailable"), "{bad}");
    }
}

#[test]
fn within_admits_inside_and_refuses_edges_and_beyond() {
    let f = Frame {
        width: 100.0,
        height: 50.0,
    };
    assert!(within(f, &[(0.0, 0.0), (99.9, 49.9)]).is_ok());
    for p in [(100.0, 10.0), (10.0, 50.0), (f64::NAN, 1.0), (1e9, 1.0)] {
        let code = within(f, &[(1.0, 1.0), p]).err().map(|(c, _)| c);
        assert_eq!(code, Some("coordinates_out_of_bounds"), "{p:?}");
    }
}

#[test]
fn coordinate_tools_need_the_grant_flag() {
    assert_eq!(grant_name(CLICK_AT_TOOL), GRANT_FLAG);
    assert_eq!(grant_name(DRAG_TOOL), GRANT_FLAG);
    assert_eq!(grant_name("click"), "click");
    assert!(!DEFAULT_TOOLS.contains(&GRANT_FLAG));
    // A default grant may not use either tool.
    let mut rig = Rig::new();
    let reply = rig.call(
        CLICK_AT_TOOL,
        "c1",
        json!({"pid": pid(), "window_id": WINDOW, "x": 0, "y": 0}),
    );
    assert_eq!(reply["code"], "tool_not_granted", "{reply}");
    // Neither tool is grantable by its own name; only the flag grants them.
    for tool in [CLICK_AT_TOOL, DRAG_TOOL] {
        let reply = consent(
            &rig.server.operator,
            &rig.dialog,
            &grant_req(&[tool], WINDOW),
        );
        assert_eq!(reply["code"], "bad_tool", "{tool}: {reply}");
    }
    assert!(driver_calls(&rig, "click").is_empty());
}

/// The daemon calls named `name`, each without the broker's `session` label.
fn driver_calls(rig: &Rig, name: &str) -> Vec<Value> {
    rig.daemon(|d| d.calls())
        .into_iter()
        .filter(|(n, _)| n == name)
        .map(|(_, mut args)| {
            args.as_object_mut().expect("args").remove("session");
            args
        })
        .collect()
}

fn pointer_rig(w: f64, h: f64) -> Rig {
    let rig = Rig::with(&[GRANT_FLAG], WINDOW);
    rig.daemon(|d| d.set_frame(w, h));
    rig
}

fn click_at(rig: &mut Rig, rid: &str, x: f64, y: f64) -> Value {
    rig.call(
        CLICK_AT_TOOL,
        rid,
        json!({"pid": pid(), "window_id": WINDOW, "x": x, "y": y}),
    )
}

#[test]
fn click_at_reaches_driver_inside_the_frame() {
    let mut rig = pointer_rig(420.0, 252.0);
    let reply = click_at(&mut rig, "c1", 10.0, 20.0);
    assert_eq!(reply["ok"], true, "{reply}");
    assert!(reply["confirmation"].is_object(), "{reply}");
    assert!(!reply.to_string().contains(common::FAKE_PNG), "{reply}");
    assert_eq!(
        driver_calls(&rig, "click"),
        [
            json!({"pid": pid(), "window_id": WINDOW, "x": 10.0, "y": 20.0,
            "delivery_mode": "background"})
        ]
    );
    // The frame is read with a capture just before the click.
    let reads = driver_calls(&rig, "get_window_state");
    assert_eq!(reads[0]["include_screenshot"], true, "{reads:?}");
    let forward = rig
        .audit()
        .into_iter()
        .find(|r| r["phase"] == "forward" && r["tool"] == CLICK_AT_TOOL)
        .expect("forward line");
    assert_eq!(forward["frame"], json!({"width": 420.0, "height": 252.0}));
}

/// Df8: the delegation-authority refusal. A point outside the granted window
/// never reaches the driver.
#[test]
fn click_at_outside_the_current_frame_is_refused() {
    let mut rig = pointer_rig(420.0, 252.0);
    for (i, (x, y)) in [(420.0, 10.0), (10.0, 252.0), (5000.0, 5000.0)]
        .into_iter()
        .enumerate()
    {
        let reply = click_at(&mut rig, &format!("o{i}"), x, y);
        assert_eq!(reply["code"], "coordinates_out_of_bounds", "{reply}");
    }
    assert!(driver_calls(&rig, "click").is_empty());
    let forwards = rig
        .audit()
        .into_iter()
        .filter(|r| r["phase"] == "forward")
        .count();
    assert_eq!(forwards, 0, "a refused point is never forwarded");
}

/// The agent's view of the window is stale: the point was inside when it
/// looked, the window shrank before the call, and the call is refused.
#[test]
fn shrunk_window_refuses_a_point_valid_in_the_agents_view() {
    let mut rig = pointer_rig(420.0, 252.0);
    let reply = click_at(&mut rig, "c1", 300.0, 200.0);
    assert_eq!(reply["ok"], true, "{reply}");
    rig.daemon(|d| d.set_frame(200.0, 150.0));
    let reply = click_at(&mut rig, "c2", 300.0, 200.0);
    assert_eq!(reply["code"], "coordinates_out_of_bounds", "{reply}");
    assert_eq!(driver_calls(&rig, "click").len(), 1);
}

/// The granted window on screen at (100, 100), the rig's 420 x 252 frame at 1x.
fn granted_on_screen() -> WindowRecord {
    WindowRecord {
        window_id: WINDOW,
        pid: pid(),
        layer: 0,
        x: 100.0,
        y: 100.0,
        width: 420.0,
        height: 252.0,
    }
}

fn drag(rig: &mut Rig, rid: &str) -> Value {
    let args = json!({"pid": pid(), "window_id": WINDOW, "from_x": 5, "from_y": 6, "to_x": 50, "to_y": 60});
    rig.call(DRAG_TOOL, rid, args)
}

/// #11756: a drag on top along its path reaches the driver, in the foreground.
#[test]
fn drag_at_reaches_driver_when_on_top() {
    assert_eq!(disabled(DRAG_TOOL), None);
    assert!(trusty_computer::manifest::TOOLS.contains(&"drag"));
    let mut rig = pointer_rig(420.0, 252.0);
    rig.windows.set(Ok(vec![granted_on_screen()]));
    let reply = drag(&mut rig, "d1");
    assert_eq!(reply["ok"], true, "{reply}");
    assert_eq!(
        driver_calls(&rig, "drag"),
        [
            json!({"pid": pid(), "window_id": WINDOW, "from_x": 5, "from_y": 6,
            "to_x": 50, "to_y": 60, "delivery_mode": "foreground"})
        ]
    );
    let forward = rig
        .audit()
        .into_iter()
        .find(|r| r["phase"] == "forward" && r["tool"] == DRAG_TOOL)
        .expect("forward line");
    assert_eq!(forward["activates_app"], true);
    assert_eq!(forward["frame"]["on_top"]["windows_in_front"], 0);
}

/// #11756 (Df8): a drag whose path another window covers, or whose window is
/// not on screen, or whose window order cannot be read, never reaches the
/// driver and is never forwarded. A click reads no window order.
#[test]
fn covered_drag_is_refused_before_the_driver() {
    let mut rig = pointer_rig(420.0, 252.0);
    let cover = WindowRecord {
        window_id: 99,
        pid: 4242,
        ..granted_on_screen()
    };
    for (i, (list, want)) in [
        (Ok(vec![cover, granted_on_screen()]), REFUSAL),
        (Ok(vec![]), REFUSAL),
        (Err("no window server".to_owned()), UNAVAILABLE),
    ]
    .into_iter()
    .enumerate()
    {
        rig.windows.set(list);
        let reply = drag(&mut rig, &format!("d{i}"));
        assert_eq!(reply["code"], want, "{reply}");
    }
    assert!(driver_calls(&rig, "drag").is_empty());
    let forwards = rig
        .audit()
        .into_iter()
        .filter(|r| r["phase"] == "forward")
        .count();
    assert_eq!(forwards, 0, "a refused drag is never forwarded");
    let reads = rig.windows.reads.load(Ordering::SeqCst);
    assert_eq!(reads, 3);
    let reply = click_at(&mut rig, "c1", 10.0, 20.0);
    assert_eq!(reply["ok"], true, "{reply}");
    assert_eq!(rig.windows.reads.load(Ordering::SeqCst), reads);
}

#[test]
fn frame_read_failure_refuses_the_input() {
    let mut rig = pointer_rig(420.0, 252.0);
    rig.daemon(|d| d.break_readback());
    let reply = click_at(&mut rig, "c1", 1.0, 1.0);
    assert_eq!(reply["code"], "frame_unavailable", "{reply}");
    let mut rig = pointer_rig(420.0, 252.0);
    rig.daemon(|d| d.set_snapshot_mode("text_only"));
    let reply = click_at(&mut rig, "c1", 1.0, 1.0);
    assert_eq!(reply["code"], "frame_unavailable", "{reply}");
    let reply = rig.call(
        CLICK_AT_TOOL,
        "c2",
        json!({"pid": pid(), "window_id": WINDOW + 1, "x": 1, "y": 1}),
    );
    assert_eq!(reply["code"], "window_out_of_scope", "{reply}");
    assert!(driver_calls(&rig, "click").is_empty());
}
