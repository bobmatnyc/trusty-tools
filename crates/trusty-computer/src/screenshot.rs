//! The bounded window screenshot: one granted window, image only, fail closed.
//!
//! Why: an agent that can read the AX tree still misreads some surfaces
//! (#11694). A screenshot is the riskiest read the broker serves: it shows
//! whatever the window draws. So it is a separate tool the operator must name
//! in the grant, it is taken only of the granted window, and it is refused
//! whenever the broker cannot rule out a password field in it.
//! What: [`SCREENSHOT_TOOL`] is a broker tool with its own FILTER row
//! ([`crate::policy::BROKER_RULES`]); it is never a [`crate::grants::DEFAULT_TOOLS`]
//! entry. The broker drives it as `get_window_state` with [`driver_args`],
//! which the bounded daemon also limits to the lease's one pid and window.
//! [`bound`] turns the daemon reply into an image-only result: no AX tree, no
//! markdown, so no menu-bar element reaches the agent, and the capture of one
//! window never holds the system menu bar's pixels. It refuses a degraded or
//! incomplete tree, a capture the daemon could not prove, a secure text field
//! anywhere in the tree, and an image over [`MAX_IMAGE_BASE64`].
//! [`strip_images`] drops image parts from the plain read tools, whose driver
//! default is to attach an unbounded screenshot.
//! Test: `tests/screenshot.rs`; live: `tests/live_screenshot.rs`.

use serde_json::{Map, Value, json};

/// The broker tool name an operator grants and an agent calls.
pub const SCREENSHOT_TOOL: &str = "screenshot_window";
/// The daemon tool a screenshot is taken with.
pub const DRIVER_TOOL: &str = "get_window_state";
/// Largest base64 image the broker forwards (8 MiB, about a 5K window PNG).
pub const MAX_IMAGE_BASE64: usize = 8 * 1024 * 1024;
/// Image types the broker forwards.
pub const IMAGE_TYPES: [&str; 2] = ["image/png", "image/jpeg"];
/// AX roles and subroles whose presence refuses a screenshot.
pub const SENSITIVE_ROLES: [&str; 1] = ["AXSecureTextField"];
/// The journal's stand-in for a served image; a replay never re-sends pixels.
pub const REPLAY_NOTE: &str =
    "image not kept for replay; take a new screenshot with a new request_id";

/// A refused screenshot: a stable code and a detail.
pub type Refusal = (&'static str, String);

/// The daemon arguments for a screenshot of `pid`'s `window_id`.
///
/// Why: the agent names only the window; every other daemon argument
/// (`query`, `max_elements`, `screenshot_out_file`) could hide a field from
/// [`bound`] or write a file, so the broker builds the request itself.
pub fn driver_args(args: &Map<String, Value>) -> Value {
    json!({"pid": args.get("pid"), "window_id": args.get("window_id"),
        "include_screenshot": true})
}

fn refuse(code: &'static str, detail: impl Into<String>) -> Refusal {
    (code, detail.into())
}

/// True when `element` is a password field.
fn sensitive(element: &Value) -> bool {
    ["role", "subrole"].iter().any(|k| {
        element
            .get(*k)
            .and_then(Value::as_str)
            .is_some_and(|r| SENSITIVE_ROLES.contains(&r))
    })
}

/// Reduce a daemon `get_window_state` result to one image of `window_id`.
///
/// Why: the agent gets pixels only when the broker can vouch for what they
/// hold (#11694).
/// What: refuses, in order: no structured part (`redaction_failed`); another
/// window (`window_mismatch`); a degraded tree or one not marked
/// `elements_complete` (`screenshot_unverified`), since a password field
/// could hide in the part not walked; a secure text field
/// (`sensitive_content`); a capture error or `screenshot_frame_valid` not
/// true (`screenshot_unavailable`); not exactly one PNG or JPEG image part
/// (`screenshot_unavailable`); one over [`MAX_IMAGE_BASE64`]
/// (`screenshot_too_large`).
/// Postcondition: an `Ok` value holds `window_id`, `pid`, `mime_type`,
/// `width`, `height` and `image`, and no element, token or markdown.
/// Test: `bound_returns_image_only`, `bound_refuses_secure_field`,
/// `bound_refuses_incomplete_tree`, `bound_refuses_unproven_capture`,
/// `bound_refuses_other_window`, `bound_refuses_oversized_image`.
pub fn bound(result: &Value, window_id: u64) -> Result<Value, Refusal> {
    let Some(sc) = result.get("structuredContent").and_then(Value::as_object) else {
        return Err(refuse(
            "redaction_failed",
            "the capture has no structured part",
        ));
    };
    if sc.get("window_id").and_then(Value::as_u64) != Some(window_id) {
        return Err(refuse(
            "window_mismatch",
            "the capture is not of the granted window",
        ));
    }
    if sc.get("degraded") == Some(&json!(true)) || sc.get("elements_complete") != Some(&json!(true))
    {
        return Err(refuse(
            "screenshot_unverified",
            "the window's AX tree is incomplete, so a password field cannot be ruled out",
        ));
    }
    let elements = sc
        .get("elements")
        .and_then(Value::as_array)
        .map_or(&[][..], Vec::as_slice);
    let secure = elements.iter().filter(|e| sensitive(e)).count();
    if secure > 0 {
        return Err(refuse(
            "sensitive_content",
            format!("the window shows {secure} password field(s)"),
        ));
    }
    if sc.contains_key("screenshot_error") || sc.get("screenshot_frame_valid") != Some(&json!(true))
    {
        return Err(refuse(
            "screenshot_unavailable",
            "the daemon could not prove the capture is this window",
        ));
    }
    let images: Vec<&Value> = result
        .get("content")
        .and_then(Value::as_array)
        .map_or(&[][..], Vec::as_slice)
        .iter()
        .filter(|p| p.get("type") != Some(&json!("text")))
        .collect();
    let [image] = images.as_slice() else {
        return Err(refuse(
            "screenshot_unavailable",
            format!("expected one image part, got {}", images.len()),
        ));
    };
    let mime = image.get("mimeType").and_then(Value::as_str);
    let data = image.get("data").and_then(Value::as_str);
    let (Some(mime), Some(data), Some("image")) =
        (mime, data, image.get("type").and_then(Value::as_str))
    else {
        return Err(refuse(
            "screenshot_unavailable",
            "the image part is malformed",
        ));
    };
    if !IMAGE_TYPES.contains(&mime) {
        return Err(refuse(
            "screenshot_unavailable",
            format!("image type {mime} is not forwarded"),
        ));
    }
    if data.len() > MAX_IMAGE_BASE64 {
        return Err(refuse(
            "screenshot_too_large",
            format!("{} bytes over {MAX_IMAGE_BASE64}", data.len()),
        ));
    }
    Ok(
        json!({"window_id": window_id, "pid": sc.get("pid"), "mime_type": mime,
        "width": sc.get("screenshot_width"), "height": sc.get("screenshot_height"),
        "image": data}),
    )
}

/// Drop every non-text content part of a read-tool result.
///
/// Why: cua-driver 0.23.2 attaches a screenshot to `get_window_state` and
/// `verify_state` by default; only [`bound`] may hand pixels to an agent.
/// Test: `read_results_lose_image_parts`.
pub fn strip_images(result: &mut Value) {
    if let Some(parts) = result.get_mut("content").and_then(Value::as_array_mut) {
        parts.retain(|p| p.get("type") == Some(&json!("text")));
    }
}
