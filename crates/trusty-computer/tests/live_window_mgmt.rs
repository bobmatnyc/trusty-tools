//! Live check of focus, move and resize scoped to the granted pid (issue #11696 Phase B).
//!
//! Why: the fake daemon cannot show the shape cua-driver 0.23.2 gives
//! `list_windows`, nor that `set_window_frame` and `bring_to_front` act on the
//! window they name and on no other.
//! What: builds `tests/fixtures/signedapp` (ad-hoc) and opens two resizable
//! copies (`SIGNEDCHECK_RESIZABLE=1`). One
//! bounded daemon runs under a temp HOME with the broker's pid+window manifest
//! for the granted copy. Each step runs the broker's own sequence:
//! [`window_mgmt::owner_args`] read, [`window_mgmt::owned_bounds`], then
//! [`window_mgmt::driver_args`] to the daemon. It checks: a move lands at the
//! new origin with the size kept; a resize lands at the new size with the
//! origin kept; a focus succeeds; the second copy's window is refused by the
//! owner check (`window_not_in_granted_app`) and, sent straight to the daemon,
//! by the bounded manifest, and its frame does not change.
//! Run: `cargo test -p trusty-computer --test live_window_mgmt -- --ignored --nocapture`.
//! Test: this file.

mod common;

use common::live::{Live, wait_for};
use serde_json::{Value, json};
use trusty_computer::driver::{DriverClient, DriverError};
use trusty_computer::window_mgmt::{
    self, Bounds, FOCUS_TOOL, MOVE_TOOL, OWNER_TOOL, RESIZE_TOOL, owned_bounds,
};

/// The granted pid's windows as the broker reads them; `Err` names the refusal.
fn owner(client: &DriverClient, (pid, wid): (u64, u64)) -> Result<Bounds, &'static str> {
    let pid = u32::try_from(pid).expect("pid");
    let reply = client
        .call(OWNER_TOOL, &window_mgmt::owner_args(pid))
        .expect("owner read");
    owned_bounds(&reply, pid, wid).map_err(|(code, _)| code)
}

/// The broker's owner gate then the daemon call; `Err` names the broker refusal.
fn send(
    client: &DriverClient,
    granted: (u64, u64),
    tool: &str,
    args: Value,
) -> Result<Result<Value, DriverError>, &'static str> {
    let now = owner(client, granted)?;
    let (name, request) = window_mgmt::driver_args(tool, args.as_object().expect("object"), now);
    Ok(client.call(name, &request))
}

/// Close enough: the window server rounds to whole points.
fn near(a: f64, b: f64) -> bool {
    (a - b).abs() <= 1.0
}

#[test]
#[ignore = "live: builds fixture apps and runs CuaDriver.app bounded; needs an unlocked screen"]
fn live_window_management_is_scoped_to_the_granted_pid() {
    let mut live = Live::new("tcomp-win");
    let (pid, wid, _) = live.launch("granted", &["SIGNEDCHECK_RESIZABLE=1"]);
    let (other_pid, other_wid, _) = live.launch("other", &["SIGNEDCHECK_RESIZABLE=1"]);
    let client = live.start_daemon((pid, wid));
    let win = (pid, wid);

    let start = owner(&client, win).expect("granted window listed under its pid");
    println!("start {start:?}");

    // Move: new origin, size kept.
    let (x, y) = (start.x + 40.0, start.y + 30.0);
    send(
        &client,
        win,
        MOVE_TOOL,
        json!({"pid": pid, "window_id": wid, "x": x, "y": y}),
    )
    .expect("owned")
    .expect("move delivered");
    let moved = wait_for("moved frame", 10, || {
        owner(&client, win)
            .ok()
            .filter(|b| near(b.x, x) && near(b.y, y))
    });
    assert!(near(moved.width, start.width) && near(moved.height, start.height));

    // Resize: new size, origin kept.
    let (w, h) = (start.width + 60.0, start.height + 40.0);
    send(
        &client,
        win,
        RESIZE_TOOL,
        json!({"pid": pid, "window_id": wid, "width": w, "height": h}),
    )
    .expect("owned")
    .expect("resize delivered");
    let sized = wait_for("resized frame", 10, || {
        owner(&client, win)
            .ok()
            .filter(|b| near(b.width, w) && near(b.height, h))
    });
    assert!(near(sized.x, x) && near(sized.y, y), "{sized:?}");

    // Focus.
    let focused = send(
        &client,
        win,
        FOCUS_TOOL,
        json!({"pid": pid, "window_id": wid}),
    )
    .expect("owned");
    println!("focus: {focused:?}");
    assert!(focused.is_ok(), "{focused:?}");

    // The other copy's window is not the granted pid's: the owner gate refuses it.
    assert_eq!(
        owner(&client, (pid, other_wid)).err(),
        Some("window_not_in_granted_app")
    );
    // Sent straight to the daemon, the bounded manifest refuses it too.
    let other = json!({"pid": other_pid, "window_id": other_wid});
    let (name, request) = window_mgmt::driver_args(
        RESIZE_TOOL,
        json!({"pid": other_pid, "window_id": other_wid, "width": 50, "height": 50})
            .as_object()
            .expect("object"),
        Bounds {
            x: 0.0,
            y: 0.0,
            width: 1.0,
            height: 1.0,
        },
    );
    let refused = client.call(name, &request);
    let bounded = matches!(&refused, Err(DriverError::ToolError { result, .. })
        if result.pointer("/structuredContent/refusal/code")
            == Some(&json!("bounded_resource_outside_manifest")));
    assert!(bounded, "second app's window {other}: {refused:?}");
    drop(live);
}
