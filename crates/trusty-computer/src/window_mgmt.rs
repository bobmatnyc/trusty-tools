//! Focus, move and resize of the granted app's window (#11696 Phase B).
//!
//! Why: an agent working in a granted window sometimes needs it in front, out
//! of the way, or larger. The driver's own `bring_to_front` and
//! `set_window_frame` take any pid and window, so they stay DENY; these broker
//! tools reach them only for the one window the operator granted, and only
//! after the daemon confirms that window still belongs to the granted pid.
//! What: [`FOCUS_TOOL`], [`MOVE_TOOL`] and [`RESIZE_TOOL`] are broker tools
//! with FILTER rows ([`crate::policy::BROKER_RULES`]) checked by
//! [`focus_args`], [`move_args`] and [`resize_args`]. All three need the one
//! grant flag [`GRANT_FLAG`]. Before any driver call the broker refuses a pid
//! outside the grant (`pid_out_of_scope`) and a window other than the granted
//! one (`window_out_of_scope`). It then reads the pid's windows with
//! [`owner_args`]; [`owned_bounds`] refuses `window_not_in_granted_app` unless
//! the daemon lists the window under the granted pid. Only then
//! [`driver_args`] builds the one daemon call, addressed by pid and
//! window_id. No call positions the pointer, posts an input event, or acts on
//! the desktop or any other window.
//! Test: `tests/window_mgmt.rs`; live: `tests/live_window_mgmt.rs`.

use serde_json::{Map, Value, json};

use crate::policy::{PolicyError, Scope};

/// Bring the granted window to the front.
pub const FOCUS_TOOL: &str = "focus_window";
/// Move the granted window, keeping its size.
pub const MOVE_TOOL: &str = "move_window";
/// Resize the granted window, keeping its origin.
pub const RESIZE_TOOL: &str = "resize_window";
/// The three window tools, as an operator never names them in a grant.
pub const TOOLS: [&str; 3] = [FOCUS_TOOL, MOVE_TOOL, RESIZE_TOOL];
/// The one grant name that admits all three window tools.
pub const GRANT_FLAG: &str = "window_management";
/// The daemon tool the window's owner and frame are read with.
pub const OWNER_TOOL: &str = "list_windows";
/// The largest absolute origin or size a call may name, in desktop points.
pub const MAX_EXTENT: f64 = 16384.0;

/// A refused window call: a stable code and a detail.
pub type Refusal = (&'static str, String);

/// True for the three window tools.
pub fn is_window_tool(tool: &str) -> bool {
    TOOLS.contains(&tool)
}

/// True when `tool` brings the target app to the front; the audit records it.
pub fn activates_app(tool: &str) -> bool {
    tool == FOCUS_TOOL
}

/// The numeric argument names of `tool`.
fn value_keys(tool: &str) -> &'static [&'static str] {
    match tool {
        MOVE_TOOL => &["x", "y"],
        RESIZE_TOOL => &["width", "height"],
        _ => &[],
    }
}

fn bad(code: &'static str, detail: impl Into<String>) -> PolicyError {
    PolicyError {
        code,
        detail: detail.into(),
    }
}

/// `args` names exactly `pid`, an integer `window_id`, and `tool`'s numbers.
///
/// What: an origin is finite with `|v| <= MAX_EXTENT`; a size is finite with
/// `1 <= v <= MAX_EXTENT`. Any other key is refused `window_arg`.
fn shape(args: &Map<String, Value>, tool: &str) -> Result<(), PolicyError> {
    if !args.get("window_id").is_some_and(Value::is_u64) {
        return Err(bad(
            "window_required",
            "a window call must name an integer window_id",
        ));
    }
    let keys = value_keys(tool);
    let known = |k: &str| matches!(k, "pid" | "window_id") || keys.contains(&k);
    if let Some(extra) = args.keys().find(|k| !known(k)) {
        return Err(bad("window_arg", format!("{extra} is not allowed")));
    }
    let size = tool == RESIZE_TOOL;
    for key in keys {
        let ok = args
            .get(*key)
            .and_then(Value::as_f64)
            .is_some_and(|v| v.is_finite() && v.abs() <= MAX_EXTENT && (!size || v >= 1.0));
        if !ok {
            let range = if size { "1" } else { "-16384" };
            return Err(bad(
                "bad_window_frame",
                format!("{key} must be a number from {range} to {MAX_EXTENT}"),
            ));
        }
    }
    Ok(())
}

/// FILTER check of [`FOCUS_TOOL`]: exactly `pid` and `window_id`.
///
/// Test: `window_args_are_checked`.
pub fn focus_args(args: &Map<String, Value>, _: &Scope) -> Result<(), PolicyError> {
    shape(args, FOCUS_TOOL)
}

/// FILTER check of [`MOVE_TOOL`]: exactly `pid`, `window_id`, `x`, `y`.
///
/// Test: `window_args_are_checked`.
pub fn move_args(args: &Map<String, Value>, _: &Scope) -> Result<(), PolicyError> {
    shape(args, MOVE_TOOL)
}

/// FILTER check of [`RESIZE_TOOL`]: exactly `pid`, `window_id`, `width`, `height`.
///
/// Test: `window_args_are_checked`.
pub fn resize_args(args: &Map<String, Value>, _: &Scope) -> Result<(), PolicyError> {
    shape(args, RESIZE_TOOL)
}

/// A window's frame in the desktop space `list_windows` reports.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Bounds {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// The daemon arguments of the owner read: the granted pid's windows only.
pub fn owner_args(pid: u32) -> Value {
    json!({"pid": pid})
}

/// The frame of `window_id` when the daemon lists it under `pid`.
///
/// What: reads `structuredContent.windows`. Refuses
/// `window_not_in_granted_app` when no record names `window_id`, or any
/// record naming it has another pid; refuses `window_frame_unavailable` when
/// the result has no window list or the record's bounds are not finite.
/// Postcondition: `Ok` means a record with this `window_id` and this `pid`
/// was listed, no record gave the window another pid, and the bounds are
/// finite.
/// Test: `owner_read_proves_the_window_is_the_granted_pids`.
pub fn owned_bounds(result: &Value, pid: u32, window_id: u64) -> Result<Bounds, Refusal> {
    let unavailable = |detail: &str| ("window_frame_unavailable", detail.to_owned());
    let not_owned = || {
        (
            "window_not_in_granted_app",
            format!("window {window_id} is not a window of the granted pid {pid}"),
        )
    };
    let windows = result
        .pointer("/structuredContent/windows")
        .and_then(Value::as_array)
        .ok_or_else(|| unavailable("the window list has no structured part"))?;
    let named: Vec<&Value> = windows
        .iter()
        .filter(|w| w.get("window_id").and_then(Value::as_u64) == Some(window_id))
        .collect();
    let owned = |w: &&Value| w.get("pid").and_then(Value::as_u64) == Some(u64::from(pid));
    if named.is_empty() || !named.iter().all(owned) {
        return Err(not_owned());
    }
    let b = &named[0]["bounds"];
    let num = |k: &str| b.get(k).and_then(Value::as_f64).filter(|v| v.is_finite());
    match (num("x"), num("y"), num("width"), num("height")) {
        (Some(x), Some(y), Some(width), Some(height)) => Ok(Bounds {
            x,
            y,
            width,
            height,
        }),
        _ => Err(unavailable("the window record has no finite bounds")),
    }
}

/// The daemon tool and arguments of a checked window call on `now`.
///
/// What: focus becomes `bring_to_front` of `pid` and `window_id`; move
/// becomes `set_window_frame` at the new `x`, `y` with `now`'s size; resize
/// becomes `set_window_frame` at `now`'s origin with the new size. The pid
/// and window come from `args`, which the broker has already matched to the
/// grant; no other daemon argument is set.
/// Test: `window_tools_drive_only_the_granted_window`.
pub fn driver_args(tool: &str, args: &Map<String, Value>, now: Bounds) -> (&'static str, Value) {
    let get = |k: &str| args.get(k).cloned().unwrap_or(Value::Null);
    let (pid, window_id) = (get("pid"), get("window_id"));
    match tool {
        FOCUS_TOOL => (
            "bring_to_front",
            json!({"pid": pid, "window_id": window_id}),
        ),
        MOVE_TOOL => (
            "set_window_frame",
            json!({"pid": pid, "window_id": window_id, "x": get("x"), "y": get("y"),
                "width": now.width, "height": now.height}),
        ),
        _ => (
            "set_window_frame",
            json!({"pid": pid, "window_id": window_id, "x": now.x, "y": now.y,
                "width": get("width"), "height": get("height")}),
        ),
    }
}
