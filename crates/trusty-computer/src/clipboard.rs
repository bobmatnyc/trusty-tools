//! Clipboard read and write behind two grant flags (#11696 Phase C).
//!
//! Why: an agent sometimes has to paste into, or copy out of, a granted app.
//! The driver's own `clipboard_read` and `clipboard_write` stay DENY: the
//! read returns whatever the user last copied, and the write can also place
//! any absolute local file on the clipboard. cua-driver 0.23.2's bounded
//! manifest has no clipboard resource, so the daemon refuses both
//! (`system_clipboard ... outside the capability manifest`); the broker runs
//! these tools itself through a [`Pasteboard`], plain text only, under a
//! grant flag each.
//! What: [`READ_TOOL`] and [`WRITE_TOOL`] are broker tools with FILTER rows
//! ([`crate::policy::BROKER_RULES`]) checked by [`read_args`] and
//! [`write_args`]. A write needs [`WRITE_FLAG`] and a read needs the separate
//! [`READ_FLAG`]; neither is a default tool or grantable by its tool name.
//! A write takes exactly `text`, at most [`MAX_BYTES`] bytes. A read
//! ([`run`]) refuses `clipboard_too_large` above [`MAX_BYTES`] before any
//! redaction, then returns only the text after
//! [`crate::clipboard_redact::redact_text`]. The audit lines record the byte
//! count and the redaction count, never the text.
//! Test: `tests/clipboard.rs`.

use serde_json::{Map, Value, json};

pub use crate::clipboard_io::{Pasteboard, SystemPasteboard};
use crate::clipboard_redact::redact_text;
use crate::policy::{PolicyError, Scope};

/// Read the clipboard's plain text, redacted.
pub const READ_TOOL: &str = "read_clipboard";
/// Replace the clipboard with plain text.
pub const WRITE_TOOL: &str = "write_clipboard";
/// The grant name that admits [`READ_TOOL`].
pub const READ_FLAG: &str = "clipboard_read";
/// The grant name that admits [`WRITE_TOOL`].
pub const WRITE_FLAG: &str = "clipboard_write";
/// The two grant flags.
pub const FLAGS: [&str; 2] = [READ_FLAG, WRITE_FLAG];
/// The largest clipboard text a read returns or a write sets, in UTF-8 bytes.
pub const MAX_BYTES: usize = 100 * 1024;

/// True for the two clipboard tools.
pub fn is_clipboard_tool(tool: &str) -> bool {
    tool == READ_TOOL || tool == WRITE_TOOL
}

/// The grant flag `tool` needs, when it is a clipboard tool.
///
/// Test: `clipboard_tools_need_their_own_flag`.
pub fn grant_flag(tool: &str) -> Option<&'static str> {
    match tool {
        READ_TOOL => Some(READ_FLAG),
        WRITE_TOOL => Some(WRITE_FLAG),
        _ => None,
    }
}

fn bad(code: &'static str, detail: impl Into<String>) -> PolicyError {
    PolicyError {
        code,
        detail: detail.into(),
    }
}

/// FILTER check of [`READ_TOOL`]: no arguments.
///
/// Test: `clipboard_args_are_checked`.
pub fn read_args(args: &Map<String, Value>, _: &Scope) -> Result<(), PolicyError> {
    match args.keys().next() {
        Some(extra) => Err(bad("clipboard_arg", format!("{extra} is not allowed"))),
        None => Ok(()),
    }
}

/// FILTER check of [`WRITE_TOOL`]: exactly a string `text` of at most [`MAX_BYTES`].
///
/// Why: `image_path` and `file_path` would let an agent put any local file
/// on the clipboard, so only `text` is admitted.
/// Test: `clipboard_args_are_checked`, `oversize_write_is_refused`.
pub fn write_args(args: &Map<String, Value>, _: &Scope) -> Result<(), PolicyError> {
    if let Some(extra) = args.keys().find(|k| *k != "text") {
        return Err(bad("clipboard_arg", format!("{extra} is not allowed")));
    }
    let Some(text) = args.get("text").and_then(Value::as_str) else {
        return Err(bad(
            "clipboard_text_required",
            "a write must name a string text",
        ));
    };
    if text.len() > MAX_BYTES {
        return Err(bad(
            "clipboard_too_large",
            format!("text is {} bytes; the cap is {MAX_BYTES}", text.len()),
        ));
    }
    Ok(())
}

/// The broker reply of a checked clipboard call, and its audit fields.
///
/// What: a write sets the checked `text` with [`Pasteboard::write`] and
/// answers `written_bytes`. A read takes at most [`MAX_BYTES`] + 1 bytes with
/// [`Pasteboard::read`]; more than [`MAX_BYTES`] is refused
/// `clipboard_too_large` with no redaction run, text that is not UTF-8 is
/// refused `clipboard_not_text`, else the reply carries only the text after
/// [`redact_text`]. A pasteboard failure is `clipboard_error`. The audit
/// fields are `clipboard_bytes` (and `redacted` for a read), never the text.
/// Postcondition: an `ok` read reply's text is `redact_text` of a clipboard
/// text of at most [`MAX_BYTES`] bytes.
/// Test: `read_returns_redacted_text_only`, `oversize_read_is_refused`,
/// `pasteboard_failure_returns_no_text`.
pub fn run(
    pb: &dyn Pasteboard,
    tool: &str,
    args: &Map<String, Value>,
) -> (Value, Map<String, Value>) {
    let mut audit = Map::new();
    let failed = |code: &str, detail: &str| json!({"ok": false, "code": code, "error": detail});
    if tool == WRITE_TOOL {
        let text = args.get("text").and_then(Value::as_str).unwrap_or_default();
        audit.insert("clipboard_bytes".into(), json!(text.len()));
        return match pb.write(text) {
            Ok(()) => (
                json!({"ok": true, "result": {"structuredContent": {"written_bytes": text.len()}}}),
                audit,
            ),
            Err(e) => (failed("clipboard_error", &e), audit),
        };
    }
    let raw = match pb.read(MAX_BYTES + 1) {
        Ok(raw) => raw,
        Err(e) => return (failed("clipboard_error", &e), audit),
    };
    audit.insert("clipboard_bytes".into(), json!(raw.len()));
    if raw.len() > MAX_BYTES {
        let detail = format!("the clipboard text is over the {MAX_BYTES}-byte cap");
        return (failed("clipboard_too_large", &detail), audit);
    }
    let Ok(text) = String::from_utf8(raw) else {
        return (
            failed("clipboard_not_text", "the clipboard text is not UTF-8"),
            audit,
        );
    };
    let red = redact_text(&text);
    audit.insert("redacted".into(), json!(red.removed));
    let result = json!({
        "content": [{"type": "text", "text": red.text}],
        "structuredContent": {"text": red.text, "redacted": red.removed},
    });
    (json!({"ok": true, "result": result}), audit)
}
