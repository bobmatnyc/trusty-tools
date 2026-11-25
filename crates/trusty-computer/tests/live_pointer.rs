//! Live check of coordinate input bounded to the granted window (issue #11696 Phase A).
//!
//! Why: the fake daemon cannot show which coordinate space cua-driver 0.23.2
//! really uses, nor that a click and a drag land in the window they name.
//! What: builds `tests/fixtures/signedapp` (ad-hoc) and opens two copies with
//! `SIGNEDCHECK_POINTER=1` (a drag pad; SIGUSR1 shrinks the window, SIGUSR2
//! moves it). One bounded daemon runs under a temp HOME with the broker's
//! pid+window manifest for the granted copy. Each step runs the broker's own
//! sequence: [`pointer::frame_args`] read, [`pointer::frame`],
//! [`pointer::within`], then [`pointer::driver_args`] to the daemon. It checks:
//! a click at the "Press Me" button lands (the fixture logs it); a drag
//! endpoint past the frame is refused; a drag is refused
//! `drag_blocked_window_not_on_top` while the second copy (opened at the same
//! place) covers the granted window, and lands on the drag pad after the
//! daemon brings the granted window to the front (#11756, the broker's
//! [`window_order::on_top`] on the real window server list); a point past the frame is refused before the daemon; after the
//! window moves, the same window coordinate still presses the button; after
//! the window shrinks, a point inside the old frame is refused
//! (delegation-authority refusal at event time); and the second copy's window
//! is refused by the bounded daemon.
//! Run: `cargo test -p trusty-computer --test live_pointer -- --ignored --nocapture`.
//! Test: this file.

mod common;

use common::live::{Live, logged, wait_for};
use serde_json::{Value, json};
use trusty_computer::driver::{DriverClient, DriverError};
use trusty_computer::pointer::{self, CLICK_AT_TOOL, DRAG_TOOL, Frame};
use trusty_computer::window_order::{self, SystemWindowLister, WindowLister};

/// The frame the broker would read now, with its capture scale.
fn read_frame(client: &DriverClient, (pid, wid): (u64, u64)) -> (Frame, f64) {
    let scope = u32::try_from(pid).expect("pid");
    let reply = client
        .call(pointer::FRAME_TOOL, &pointer::frame_args(scope, wid))
        .expect("frame read");
    let frame = pointer::frame(&reply, wid).expect("a proven frame");
    let scale = reply["structuredContent"]["screenshot_scale"]
        .as_f64()
        .unwrap_or(1.0);
    println!("frame of {wid}: {frame:?} scale={scale}");
    (frame, scale)
}

/// The broker's gate then the daemon call; `Err` names the broker refusal.
fn send(
    client: &DriverClient,
    window: (u64, u64),
    tool: &str,
    args: Value,
) -> Result<Result<Value, DriverError>, &'static str> {
    let args = args.as_object().expect("object").clone();
    let (frame, _) = read_frame(client, window);
    let points = pointer::points(tool, &args);
    pointer::within(frame, &points).map_err(|(code, _)| code)?;
    if tool == DRAG_TOOL {
        // #11756: the broker's window-on-top gate, on the real window server list.
        let windows = SystemWindowLister.on_screen().expect("window order");
        let pid = u32::try_from(window.0).expect("pid");
        let order = window_order::on_top(&windows, pid, window.1, frame, &points);
        println!("on_top: {order:?}");
        order.map_err(|(code, _)| code)?;
    }
    let (name, request) = pointer::driver_args(tool, &args);
    Ok(client.call(name, &request))
}

#[test]
#[ignore = "live: builds fixture apps and runs CuaDriver.app bounded; needs an unlocked screen"]
fn live_coordinate_input_is_bounded_to_the_granted_window() {
    let mut live = Live::new("tcomp-ptr");
    let (pid, wid, state) = live.launch("granted", &["SIGNEDCHECK_POINTER=1"]);
    let (other_pid, other_wid, _) = live.launch("other", &["SIGNEDCHECK_POINTER=1"]);
    let client = live.start_daemon((pid, wid));
    let win = (pid, wid);

    // Content is 420x220 points; the button's center is 210 from the left and
    // 66 above the bottom edge; the pad spans x 10..150, 10..110 above the bottom.
    let (frame, s) = read_frame(&client, win);
    let bottom = |up: f64| frame.height - up * s;
    let button = json!({"pid": pid, "window_id": wid, "x": 210.0 * s, "y": bottom(66.0)});
    send(&client, win, CLICK_AT_TOOL, button.clone())
        .expect("inside the frame")
        .expect("click delivered");
    logged(&state, "click 1");

    // Df8: a point past the frame never reaches the daemon.
    let past = json!({"pid": pid, "window_id": wid, "x": frame.width + 5.0, "y": 10.0});
    assert_eq!(
        send(&client, win, CLICK_AT_TOOL, past).err(),
        Some("coordinates_out_of_bounds")
    );
    let past = json!({"pid": pid, "window_id": wid, "from_x": 30.0 * s, "from_y": bottom(30.0),
        "to_x": 30.0 * s, "to_y": frame.height + 1.0});
    assert_eq!(
        send(&client, win, DRAG_TOOL, past).err(),
        Some("coordinates_out_of_bounds")
    );

    // #11756: the second copy opened over the granted one; the drag is refused.
    let pad = json!({"pid": pid, "window_id": wid, "from_x": 30.0 * s, "from_y": bottom(60.0),
        "to_x": 120.0 * s, "to_y": bottom(40.0)});
    assert_eq!(
        send(&client, win, DRAG_TOOL, pad.clone()).err(),
        Some(window_order::REFUSAL),
        "the second copy must cover the granted window"
    );
    // In front, the same drag passes the gate and lands on the pad.
    client
        .call("bring_to_front", &json!({"pid": pid, "window_id": wid}))
        .expect("bring_to_front");
    wait_for("granted window in front", 10, || {
        let windows = SystemWindowLister.on_screen().ok()?;
        let p = u32::try_from(pid).ok()?;
        window_order::on_top(
            &windows,
            p,
            wid,
            frame,
            &pointer::points(DRAG_TOOL, pad.as_object()?),
        )
        .ok()
    });
    send(&client, win, DRAG_TOOL, pad)
        .expect("on top along the path")
        .expect("drag delivered");
    logged(&state, "drag 1");

    // A moved window keeps its window coordinates: the same point presses again.
    // SAFETY: a pid this test launched.
    unsafe { libc::kill(pid as i32, libc::SIGUSR2) };
    logged(&state, "moved");
    send(&client, win, CLICK_AT_TOOL, button)
        .expect("inside the moved frame")
        .expect("click delivered");
    logged(&state, "click 2");

    // Valid in the agent's last view, invalid at event time: the window shrank.
    let corner = json!({"pid": pid, "window_id": wid, "x": 400.0 * s, "y": 10.0 * s});
    let args = corner.as_object().expect("object").clone();
    assert!(pointer::within(frame, &pointer::points(CLICK_AT_TOOL, &args)).is_ok());
    // SAFETY: a pid this test launched.
    unsafe { libc::kill(pid as i32, libc::SIGUSR1) };
    logged(&state, "shrunk");
    wait_for("shrunk frame", 10, || {
        (read_frame(&client, win).0.width < frame.width).then_some(())
    });
    assert_eq!(
        send(&client, win, CLICK_AT_TOOL, corner).err(),
        Some("coordinates_out_of_bounds")
    );

    // The second copy's window is outside the daemon's manifest.
    let (name, request) = pointer::driver_args(
        CLICK_AT_TOOL,
        json!({"pid": other_pid, "window_id": other_wid, "x": 10, "y": 10})
            .as_object()
            .expect("object"),
    );
    let refused = client.call(name, &request);
    let bounded = matches!(&refused, Err(DriverError::ToolError { result, .. })
        if result.pointer("/structuredContent/refusal/code")
            == Some(&json!("bounded_resource_outside_manifest")));
    assert!(bounded, "second app's window: {refused:?}");
    let text = std::fs::read_to_string(&state).expect("state");
    println!("fixture log: {text:?}");
    assert!(!text.contains("click 3"), "{text}");
    drop(live);
}
