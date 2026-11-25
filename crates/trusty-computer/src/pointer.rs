//! Coordinate input bounded to the granted window (#11696 Phase A).
//!
//! Why: some surfaces (canvas, custom-drawn views) have no AX element to
//! address, so an agent needs a click or drag at window coordinates. A
//! coordinate is the input an agent can most easily aim outside the scope it
//! was delegated, so every point is checked against the granted window's frame
//! as the daemon reports it immediately before the event, never against a
//! frame the agent saw earlier (delegation-authority refusal).
//! What: [`CLICK_AT_TOOL`] and [`DRAG_TOOL`] are broker tools with FILTER rows
//! ([`crate::policy::BROKER_RULES`]) checked by [`click_args`] and
//! [`drag_args`]. Both need the one grant flag [`GRANT_FLAG`]; neither is a
//! [`crate::grants::DEFAULT_TOOLS`] entry. The broker reads the frame with
//! [`frame_args`], reduces the reply with [`frame`], refuses any point
//! [`within`] rejects, and only then drives the daemon with [`driver_args`].
//! Coordinates are window-local screenshot pixels with a top-left origin, the
//! space the driver takes, so a window that moves keeps them valid (the
//! driver anchors them to the window at event time); a window that shrinks
//! between the agent's view and the call is caught by the fresh read.
//! A [`DRAG_TOOL`] call also needs the granted window on top along its path
//! ([`crate::window_order::on_top`], #11756); [`DRAG_ENABLED`] is the switch.
//! Test: `tests/pointer.rs`; live: `tests/live_pointer.rs`.

use serde_json::{Map, Value, json};

use crate::policy::{PolicyError, Scope};

/// The broker tool for one left click at a window coordinate.
pub const CLICK_AT_TOOL: &str = "click_at";
/// The broker tool for one left-button drag between two window coordinates.
pub const DRAG_TOOL: &str = "drag_at";
/// Both coordinate tools, as an operator never names them in a grant.
pub const TOOLS: [&str; 2] = [CLICK_AT_TOOL, DRAG_TOOL];
/// The one grant name that admits both coordinate tools.
pub const GRANT_FLAG: &str = "coordinate_input";
/// The switch of `drag_at`; while false every drag is refused [`DRAG_REFUSAL`].
///
/// #11696 (architect decision fc7ec9af2b6d): cua-driver 0.23.2 drags only in
/// the foreground, posted as global screen input, so a covered granted window
/// lets the drag reach another app. #11756 turns it on behind the
/// window-on-top gate ([`crate::window_order::on_top`]). A click is posted to
/// the pid and is unaffected.
pub const DRAG_ENABLED: bool = true;
/// The refusal code of a `drag_at` call while [`DRAG_ENABLED`] is false.
pub const DRAG_REFUSAL: &str = "drag_unavailable_needs_window_on_top_check";

/// The refusal of `tool` before any gate, when it is a disabled coordinate tool.
///
/// Test: `drag_at_reaches_driver_when_on_top`.
pub fn disabled(tool: &str) -> Option<(&'static str, &'static str)> {
    (tool == DRAG_TOOL && !DRAG_ENABLED).then_some((
        DRAG_REFUSAL,
        "a drag is posted as global screen input and could reach a window covering the granted one; \
         it is off until the broker can check the granted window is on top",
    ))
}

/// The daemon tool the frame is read with.
pub const FRAME_TOOL: &str = "get_window_state";

/// A refused coordinate call: a stable code and a detail.
pub type Refusal = (&'static str, String);

/// True for [`CLICK_AT_TOOL`] and [`DRAG_TOOL`].
pub fn is_pointer_tool(tool: &str) -> bool {
    TOOLS.contains(&tool)
}

/// The grant entry `tool` needs: [`GRANT_FLAG`] for a coordinate tool, else itself.
///
/// Test: `coordinate_tools_need_the_grant_flag`.
pub fn grant_name(tool: &str) -> &str {
    if is_pointer_tool(tool) {
        GRANT_FLAG
    } else {
        tool
    }
}

/// The coordinate argument names of `tool`, in (x, y) pairs.
fn coord_keys(tool: &str) -> &'static [(&'static str, &'static str)] {
    match tool {
        CLICK_AT_TOOL => &[("x", "y")],
        DRAG_TOOL => &[("from_x", "from_y"), ("to_x", "to_y")],
        _ => &[],
    }
}

fn bad(code: &'static str, detail: impl Into<String>) -> PolicyError {
    PolicyError {
        code,
        detail: detail.into(),
    }
}

/// `args` names an integer `pid` and `window_id` and `pairs`, each a number >= 0.
fn shape(args: &Map<String, Value>, tool: &str) -> Result<(), PolicyError> {
    if !args.get("window_id").is_some_and(Value::is_u64) {
        return Err(bad(
            "window_required",
            "a coordinate call must name an integer window_id",
        ));
    }
    let pairs = coord_keys(tool);
    let known =
        |k: &str| matches!(k, "pid" | "window_id") || pairs.iter().any(|(x, y)| k == *x || k == *y);
    if let Some(extra) = args.keys().find(|k| !known(k)) {
        return Err(bad("coordinate_arg", format!("{extra} is not allowed")));
    }
    for key in pairs.iter().flat_map(|(x, y)| [*x, *y]) {
        let ok = args
            .get(key)
            .and_then(Value::as_f64)
            .is_some_and(|v| v.is_finite() && v >= 0.0);
        if !ok {
            return Err(bad(
                "bad_coordinates",
                format!("{key} must be a number of at least 0"),
            ));
        }
    }
    Ok(())
}

/// FILTER check of [`CLICK_AT_TOOL`]: exactly `pid`, `window_id`, `x`, `y`.
///
/// Why: a `modifier`, `delivery_mode`, `scope`, `from_zoom` or `debug_image_out`
/// would change what the click does or where it lands; the broker sets them.
/// Test: `coordinate_args_are_checked`.
pub fn click_args(args: &Map<String, Value>, _: &Scope) -> Result<(), PolicyError> {
    shape(args, CLICK_AT_TOOL)
}

/// FILTER check of [`DRAG_TOOL`]: exactly `pid`, `window_id` and both endpoints.
///
/// Test: `coordinate_args_are_checked`.
pub fn drag_args(args: &Map<String, Value>, _: &Scope) -> Result<(), PolicyError> {
    shape(args, DRAG_TOOL)
}

/// The points a coordinate call touches; a drag's two endpoints.
///
/// A window frame is a rectangle, so a straight drag between two points
/// inside it never leaves it; the endpoints are enough.
pub fn points(tool: &str, args: &Map<String, Value>) -> Vec<(f64, f64)> {
    let num = |k: &str| args.get(k).and_then(Value::as_f64).unwrap_or(f64::NAN);
    coord_keys(tool)
        .iter()
        .map(|(x, y)| (num(x), num(y)))
        .collect()
}

/// The granted window's frame in the driver's coordinate space.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Frame {
    /// Width in window-local screenshot pixels.
    pub width: f64,
    /// Height in window-local screenshot pixels.
    pub height: f64,
}

/// The daemon arguments of the frame read of `pid`'s `window_id`.
///
/// Why: cua-driver 0.23.2 reports `screenshot_width`/`height`, the space its
/// coordinates are in, only with a capture; a tree-only read carries no frame.
/// The image is dropped by the broker and never reaches the agent.
pub fn frame_args(pid: u32, window_id: u64) -> Value {
    json!({"pid": pid, "window_id": window_id, "include_screenshot": true})
}

/// The frame of `window_id` in a daemon `get_window_state` result.
///
/// What: refuses `frame_unavailable` when the result has no structured part,
/// names another window, carries a `screenshot_error`, has
/// `screenshot_frame_valid` not true, or lacks a positive finite
/// `screenshot_width` and `screenshot_height`.
/// Postcondition: an `Ok` frame has a positive, finite width and height.
/// Test: `frame_needs_a_valid_capture_of_the_window`.
pub fn frame(result: &Value, window_id: u64) -> Result<Frame, Refusal> {
    let unavailable = |detail: &str| ("frame_unavailable", detail.to_owned());
    let sc = result
        .get("structuredContent")
        .and_then(Value::as_object)
        .ok_or_else(|| unavailable("the frame read has no structured part"))?;
    if sc.get("window_id").and_then(Value::as_u64) != Some(window_id) {
        return Err(unavailable("the frame read is not of the granted window"));
    }
    if sc.contains_key("screenshot_error") || sc.get("screenshot_frame_valid") != Some(&json!(true))
    {
        return Err(unavailable("the daemon could not prove the window frame"));
    }
    let size = |k: &str| {
        sc.get(k)
            .and_then(Value::as_f64)
            .filter(|v| v.is_finite() && *v > 0.0)
    };
    match (size("screenshot_width"), size("screenshot_height")) {
        (Some(width), Some(height)) => Ok(Frame { width, height }),
        _ => Err(unavailable("the frame read has no window size")),
    }
}

/// Refuse `coordinates_out_of_bounds` unless every point lies in `frame`.
///
/// What: a point is inside when `0 <= x < width` and `0 <= y < height`;
/// a non-finite coordinate is outside.
/// Postcondition: `Ok` means every point is inside the frame.
/// Test: `within_admits_inside_and_refuses_edges_and_beyond`.
pub fn within(frame: Frame, points: &[(f64, f64)]) -> Result<(), Refusal> {
    let inside =
        |(x, y): (f64, f64)| (0.0..frame.width).contains(&x) && (0.0..frame.height).contains(&y);
    match points.iter().find(|p| !inside(**p)) {
        None => Ok(()),
        Some((x, y)) => Err((
            "coordinates_out_of_bounds",
            format!(
                "({x}, {y}) is outside the granted window ({} x {}) as it is now",
                frame.width, frame.height
            ),
        )),
    }
}

/// The daemon tool and arguments of a checked coordinate call.
///
/// What: `click_at` becomes a left `click` at `x`, `y`; `drag_at` a left
/// `drag` from `from_x`, `from_y` to `to_x`, `to_y`. Both name the pid and
/// window and a `delivery_mode` from [`activates_app`]; no other daemon
/// argument is set.
/// Test: `click_at_reaches_driver_inside_the_frame`, `drag_at_reaches_driver_when_on_top`.
pub fn driver_args(tool: &str, args: &Map<String, Value>) -> (&'static str, Value) {
    let mut out = Map::new();
    for key in ["pid", "window_id"]
        .into_iter()
        .chain(coord_keys(tool).iter().flat_map(|(x, y)| [*x, *y]))
    {
        out.insert(key.into(), args.get(key).cloned().unwrap_or(Value::Null));
    }
    let mode = if activates_app(tool) {
        "foreground"
    } else {
        "background"
    };
    out.insert("delivery_mode".into(), json!(mode));
    let name = if tool == DRAG_TOOL { "drag" } else { "click" };
    (name, Value::Object(out))
}

/// True when `tool` brings the target app to the front for the event.
///
/// #11696: cua-driver 0.23.2 refuses a background drag
/// (`background_unavailable`), so a drag is delivered `foreground`: the
/// driver fronts the app, drags, then restores the prior front app. A click
/// stays background. The audit records `activates_app` as for `invoke_menu`.
/// Test: `drag_at_reaches_driver_when_on_top`.
pub fn activates_app(tool: &str) -> bool {
    tool == DRAG_TOOL
}
