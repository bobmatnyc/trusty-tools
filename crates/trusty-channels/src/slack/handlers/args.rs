//! Argument extraction, validation, and pagination helpers shared by the
//! Slack tool handlers.
//!
//! Why: every handler needs the same small set of primitives — pull a
//! required/optional argument out of the caller's JSON, clamp a page size into
//! Slack's accepted range, and (for the two `conversations.*`-backed read
//! tools) forward/consume Slack's cursor-pagination fields. Centralising them
//! here keeps each handler in [`super::read`], [`super::search`], etc. focused
//! on its own request shape.
//! What: [`require_str`]/[`opt_str`]/[`opt_i64`] read typed arguments;
//! [`require_ts`] additionally validates a Slack timestamp's
//! `seconds.microseconds` shape; [`clamp_page_size`] bounds a page size;
//! [`apply_pagination_args`]/[`next_cursor`]/[`has_more`] round-trip Slack's
//! `cursor`/`oldest`/`latest` request fields and `response_metadata.next_cursor`
//! / `has_more` response fields.
//! Test: unit-tested inline below.

use serde_json::{json, Value};

use crate::slack::server::ToolCallError;

/// Lower bound on a caller-supplied page size (`limit`/`count`).
pub(super) const MIN_PAGE_SIZE: i64 = 1;

/// Upper bound on a caller-supplied page size (`limit`/`count`); matches the
/// widest window Slack's `search.messages` accepts.
pub(super) const MAX_PAGE_SIZE: i64 = 1000;

/// Upper bound on `limit` for `conversations.history` / `conversations.replies`
/// specifically — Slack documents a hard maximum of 999 for these two methods
/// (distinct from [`MAX_PAGE_SIZE`], which governs `search.messages`).
pub(super) const MAX_CONVERSATION_PAGE_SIZE: i64 = 999;

/// Require a string argument, erroring before any network call if absent.
///
/// Why: a missing required argument is a caller error (`INVALID_PARAMS`), never
/// a Slack API error — surface it distinctly and early.
/// What: returns the owned string, or [`ToolCallError::InvalidArgs`].
/// Test: `require_str_errors_when_missing`.
pub(super) fn require_str(args: &Value, key: &str) -> Result<String, ToolCallError> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| {
            ToolCallError::InvalidArgs(format!("missing required string argument '{key}'"))
        })
}

/// Read an optional string argument (absent or wrong-typed → `None`).
pub(super) fn opt_str(args: &Value, key: &str) -> Option<String> {
    args.get(key).and_then(Value::as_str).map(str::to_string)
}

/// Read an optional integer argument (absent or wrong-typed → `None`).
pub(super) fn opt_i64(args: &Value, key: &str) -> Option<i64> {
    args.get(key).and_then(Value::as_i64)
}

/// Require a Slack timestamp ("ts") string argument, validating its shape.
///
/// Why: Slack ts values are exact-match strings of the form
/// `"<seconds>.<microseconds>"` (e.g. `"1616703000.216300"`). A caller (or an
/// LLM formatting a tool call) treating the value as a number and round-
/// tripping it through a float silently drops trailing-zero precision,
/// producing a syntactically-plausible but wrong string that Slack rejects
/// with an opaque `invalid_arguments` — hard to root-cause from the caller's
/// side. Validating the shape here fails fast with a message that names the
/// expected format *before* any network call, rather than forwarding a
/// suspect value to Slack (issue #2996).
/// What: returns the owned string when it matches `\d+\.\d+`; otherwise
/// [`ToolCallError::InvalidArgs`] naming the expected shape.
/// Test: `require_ts_accepts_valid_shape`, `require_ts_rejects_malformed`.
pub(super) fn require_ts(args: &Value, key: &str) -> Result<String, ToolCallError> {
    let raw = require_str(args, key)?;
    if is_valid_slack_ts(&raw) {
        Ok(raw)
    } else {
        Err(ToolCallError::InvalidArgs(format!(
            "'{key}' must be a Slack timestamp string 'seconds.microseconds' \
             (e.g. '1616703000.216300'); got '{raw}'. If this value came from \
             re-formatting a number, precision may have been lost — pass the \
             original ts string unchanged."
        )))
    }
}

/// Whether `s` has the Slack ts shape: one or more digits, a literal `.`, then
/// one or more digits.
///
/// Why: shared by [`require_ts`]'s validation and its unit tests.
/// What: a strict digit-dot-digit check; not a numeric-range or precision
/// check (Slack alone knows whether a given ts identifies a real message).
/// Test: `is_valid_slack_ts_matches_expected_shape`.
pub(super) fn is_valid_slack_ts(s: &str) -> bool {
    let mut parts = s.splitn(2, '.');
    let secs = parts.next().unwrap_or("");
    let micros = parts.next();
    !secs.is_empty()
        && secs.bytes().all(|b| b.is_ascii_digit())
        && micros.is_some_and(|m| !m.is_empty() && m.bytes().all(|b| b.is_ascii_digit()))
}

/// Copy the shared pagination arguments (`cursor`, `oldest`, `latest`) from
/// the caller's `args` into an outgoing Slack request `body`, when present.
///
/// Why: `read::read_channel` and `read::read_thread` both page through
/// `conversations.history` / `conversations.replies`, which accept the same
/// three optional pagination parameters; centralising avoids the two handlers
/// drifting on which params they forward.
/// What: mutates `body` in place, setting `cursor`/`oldest`/`latest` only when
/// the corresponding string argument is present in `args`.
/// Test: `apply_pagination_args_forwards_present_fields`,
/// `apply_pagination_args_omits_absent_fields`.
pub(super) fn apply_pagination_args(body: &mut Value, args: &Value) {
    if let Some(cursor) = opt_str(args, "cursor") {
        body["cursor"] = json!(cursor);
    }
    if let Some(oldest) = opt_str(args, "oldest") {
        body["oldest"] = json!(oldest);
    }
    if let Some(latest) = opt_str(args, "latest") {
        body["latest"] = json!(latest);
    }
}

/// Extract Slack's next-page cursor from a paged response, if any.
///
/// Why: Slack signals "no further pages" with an empty-string
/// `response_metadata.next_cursor` rather than omitting the field, so a naive
/// passthrough would surface a useless `Some("")` to the caller instead of a
/// clean `None`.
/// What: returns `Some(cursor)` when `response_metadata.next_cursor` is a
/// non-empty string; `None` otherwise (missing, wrong-typed, or empty).
/// Test: `next_cursor_extracts_and_treats_empty_as_none`.
pub(super) fn next_cursor(resp: &Value) -> Option<String> {
    resp.get("response_metadata")
        .and_then(|m| m.get("next_cursor"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// Extract Slack's `has_more` flag from a paged response, defaulting to
/// `false` when absent.
///
/// Test: `has_more_reads_flag_and_defaults_false`.
pub(super) fn has_more(resp: &Value) -> bool {
    resp.get("has_more")
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

/// Clamp a caller-supplied page size into Slack's accepted range.
///
/// Why: Slack validates page size server-side, but forwarding an out-of-range
/// value (a negative, zero, or absurdly large `limit`/`count`) is sloppy —
/// clamp it to `[MIN_PAGE_SIZE, max]` as defense-in-depth so the body we build
/// is always well-formed before it ever reaches the network. `max` is caller-
/// supplied because Slack's per-method ceilings differ (999 for the
/// `conversations.*` family vs 1000 for `search.messages`).
/// What: returns `n` clamped to `MIN_PAGE_SIZE..=max`.
/// Test: `clamp_page_size_bounds_hostile_values`.
pub(super) fn clamp_page_size(n: i64, max: i64) -> i64 {
    n.clamp(MIN_PAGE_SIZE, max)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn require_str_errors_when_missing() {
        let args = json!({ "channel": "C1" });
        assert!(require_str(&args, "channel").is_ok());
        let err = require_str(&args, "text").expect_err("missing text");
        assert!(matches!(err, ToolCallError::InvalidArgs(_)));
    }

    #[test]
    fn opt_helpers_read_present_and_absent() {
        let args = json!({ "types": "public_channel", "limit": 7 });
        assert_eq!(opt_str(&args, "types").as_deref(), Some("public_channel"));
        assert_eq!(opt_str(&args, "missing"), None);
        assert_eq!(opt_i64(&args, "limit"), Some(7));
        assert_eq!(opt_i64(&args, "missing"), None);
    }

    #[test]
    fn clamp_page_size_bounds_hostile_values() {
        // Hostile / degenerate inputs are pulled into the valid window before a
        // request body is ever built.
        assert_eq!(clamp_page_size(i64::MIN, MAX_PAGE_SIZE), MIN_PAGE_SIZE);
        assert_eq!(clamp_page_size(-100, MAX_PAGE_SIZE), MIN_PAGE_SIZE);
        assert_eq!(clamp_page_size(0, MAX_PAGE_SIZE), MIN_PAGE_SIZE);
        // In-range values pass through untouched.
        assert_eq!(clamp_page_size(1, MAX_PAGE_SIZE), 1);
        assert_eq!(clamp_page_size(MAX_PAGE_SIZE, MAX_PAGE_SIZE), MAX_PAGE_SIZE);
        // Absurdly large values collapse to the upper bound.
        assert_eq!(clamp_page_size(1_000_000, MAX_PAGE_SIZE), MAX_PAGE_SIZE);
        assert_eq!(clamp_page_size(i64::MAX, MAX_PAGE_SIZE), MAX_PAGE_SIZE);
        // The conversations.* family uses a distinct (lower) ceiling.
        assert_eq!(
            clamp_page_size(i64::MAX, MAX_CONVERSATION_PAGE_SIZE),
            MAX_CONVERSATION_PAGE_SIZE
        );
    }

    #[test]
    fn read_channel_clamps_limit_into_body() {
        // A hostile `limit` is clamped before it reaches the request body.
        let huge = clamp_page_size(
            opt_i64(&json!({ "limit": 9_999_999 }), "limit").unwrap(),
            MAX_CONVERSATION_PAGE_SIZE,
        );
        assert_eq!(huge, MAX_CONVERSATION_PAGE_SIZE);
        let negative = clamp_page_size(
            opt_i64(&json!({ "limit": -5 }), "limit").unwrap(),
            MAX_CONVERSATION_PAGE_SIZE,
        );
        assert_eq!(negative, MIN_PAGE_SIZE);
        // Absent `limit` falls back to the default (itself in range).
        let default = clamp_page_size(
            opt_i64(&json!({}), "limit").unwrap_or(super::super::DEFAULT_READ_LIMIT),
            MAX_CONVERSATION_PAGE_SIZE,
        );
        assert_eq!(default, super::super::DEFAULT_READ_LIMIT);
    }

    #[test]
    fn require_ts_accepts_valid_shape() {
        let args = json!({ "thread_ts": "1616703000.216300" });
        assert_eq!(
            require_ts(&args, "thread_ts").expect("valid ts"),
            "1616703000.216300"
        );
    }

    #[test]
    fn require_ts_rejects_malformed() {
        // Missing decimal component, non-digit characters, and empty string
        // all fail fast with an actionable InvalidArgs message before any
        // network call — never forwarded to Slack as a suspect value.
        for bad in ["1616703000", "not-a-ts", "1616703000.", ".216300", ""] {
            let args = json!({ "thread_ts": bad });
            let err = require_ts(&args, "thread_ts").expect_err("malformed ts must error");
            match err {
                ToolCallError::InvalidArgs(msg) => {
                    assert!(msg.contains("thread_ts"), "names the field: {msg}");
                }
                other => panic!("expected InvalidArgs, got {other:?}"),
            }
        }
    }

    #[test]
    fn is_valid_slack_ts_matches_expected_shape() {
        assert!(is_valid_slack_ts("1616703000.216300"));
        assert!(is_valid_slack_ts("0.0"));
        assert!(!is_valid_slack_ts("1616703000"));
        assert!(!is_valid_slack_ts("1616703000."));
        assert!(!is_valid_slack_ts(".216300"));
        assert!(!is_valid_slack_ts(""));
        assert!(!is_valid_slack_ts("16e9.216300"));
    }

    #[test]
    fn apply_pagination_args_forwards_present_fields() {
        let mut body = json!({ "channel": "C1" });
        apply_pagination_args(
            &mut body,
            &json!({ "cursor": "dXNlcjpVMDYxTkZUVDI=", "oldest": "1.0", "latest": "2.0" }),
        );
        assert_eq!(body["cursor"], "dXNlcjpVMDYxTkZUVDI=");
        assert_eq!(body["oldest"], "1.0");
        assert_eq!(body["latest"], "2.0");
    }

    #[test]
    fn apply_pagination_args_omits_absent_fields() {
        let mut body = json!({ "channel": "C1" });
        apply_pagination_args(&mut body, &json!({}));
        assert!(body.get("cursor").is_none());
        assert!(body.get("oldest").is_none());
        assert!(body.get("latest").is_none());
    }

    #[test]
    fn next_cursor_extracts_and_treats_empty_as_none() {
        let with_cursor = json!({
            "response_metadata": { "next_cursor": "dXNlcjpVMDYxTkZUVDI=" }
        });
        assert_eq!(
            next_cursor(&with_cursor).as_deref(),
            Some("dXNlcjpVMDYxTkZUVDI=")
        );

        let empty_cursor = json!({ "response_metadata": { "next_cursor": "" } });
        assert_eq!(next_cursor(&empty_cursor), None);

        let missing = json!({ "ok": true });
        assert_eq!(next_cursor(&missing), None);
    }

    #[test]
    fn has_more_reads_flag_and_defaults_false() {
        assert!(has_more(&json!({ "has_more": true })));
        assert!(!has_more(&json!({ "has_more": false })));
        assert!(!has_more(&json!({})));
    }
}
