//! `GET /sessions/{id}/transcript.md` — the one route with no RPC twin (#6637).
//!
//! Why this exists at all: every other row in [`crate::bridge::map`] is one path
//! onto one JSON-RPC method, because the daemon's REST handler was. This one was
//! not — `trusty_code::serve::rest::sessions`'s handler made TWO calls
//! (`session.get_transcript` and `session.status`) and rendered Markdown from
//! both, so the format lives in the HTTP layer rather than behind a method. The
//! HTTP layer is what PR 2c deletes, so the rendering comes here with it: the
//! webview's "Download transcript" button is a real feature and #6637 is not
//! licence to drop it.
//!
//! **This is a copy for exactly one PR's width.** `render` reproduces the
//! daemon's `render_transcript_markdown` byte for byte, and
//! `matches_the_daemons_header_and_turn_shape` pins the shape. PR 2c deletes the
//! original; until it does, an edit to either belongs in both. The alternative
//! considered and rejected was a `session.get_transcript_markdown` RPC method,
//! which is the right end state but a change to trusty-code, and #6637's PR 2b
//! changes no crate but this one.
//!
//! What: two unary calls, then [`render`] over the two `serde_json::Value`s.
//! Rendering from the wire JSON rather than from `trusty_code`'s `Session` and
//! `TranscriptRecord` types is what keeps the daemon out of this crate's
//! dependency graph — a desktop shell that linked the whole harness to format a
//! header would be the larger mistake.
//!
//! Test: `matches_the_daemons_header_and_turn_shape`, `an_empty_transcript_says_so`,
//! `a_tool_only_turn_renders_its_tool_lines`, plus
//! `the_markdown_route_answers_text_markdown` in `tests/bridge_uds.rs`.

use std::path::Path;

use axum::http::header;
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

use super::{CALL_TIMEOUT, call};

/// Fetch both records and answer the rendered Markdown.
///
/// What: `404` for an unknown session from either call — the same status the
/// JSON transcript route answers, because it is the same refusal — and otherwise
/// `200 text/markdown; charset=utf-8`.
/// Test: `the_markdown_route_answers_text_markdown` and
/// `the_markdown_route_404s_an_unknown_session` in `tests/bridge_uds.rs`.
pub(crate) async fn respond(socket: &Path, session_id: &str) -> Response {
    let params = json!({ "session_id": session_id });
    let transcript = match call(
        socket,
        "session.get_transcript",
        params.clone(),
        CALL_TIMEOUT,
    )
    .await
    {
        Ok(v) => v,
        Err(e) => return e.into_response(),
    };
    let session = match call(socket, "session.status", params, CALL_TIMEOUT).await {
        Ok(v) => v,
        Err(e) => return e.into_response(),
    };

    let markdown = render(&session, &transcript, &chrono_now());
    (
        [(header::CONTENT_TYPE, "text/markdown; charset=utf-8")],
        markdown,
    )
        .into_response()
}

/// The export timestamp, as an RFC 3339 string.
///
/// Why its own function: [`render`] takes the timestamp rather than reading the
/// clock, so the format is testable against a fixed value — the same seam the
/// daemon's `generated_at` parameter is.
fn chrono_now() -> String {
    chrono::Utc::now().to_rfc3339()
}

/// Render a session's transcript as the Markdown document the daemon rendered.
///
/// What: a title, a metadata bullet list (session id, workstream, project, task,
/// mode, status, session-start ISO, export ISO, turn count, cost), a horizontal
/// rule, then one `##` section per turn — prose verbatim, each tool call as its
/// own ``- `ROLE` ran: <tool>`` bullet, a ``ran the test command`` note when
/// flagged, and `_(no output)_` for a turn with neither. The tool-run lines are
/// the point for the motivating case (#3526): a runaway loop reads as a visibly
/// repeated column, never a collapsed summary.
/// Test: `matches_the_daemons_header_and_turn_shape`.
fn render(session: &Value, record: &Value, generated_at: &str) -> String {
    let session_id = str_or(record.get("session_id"), "");
    let task = str_or(session.get("task"), "");
    let title = if task.trim().is_empty() {
        session_id.clone()
    } else {
        task.clone()
    };
    let turns = record
        .get("turns")
        .and_then(Value::as_array)
        .map_or(&[][..], Vec::as_slice);

    let mut out = String::new();
    out.push_str(&format!("# Workstream transcript — {title}\n\n"));
    out.push_str(&format!("- **Session:** `{session_id}`\n"));
    out.push_str(&format!(
        "- **Workstream:** {}\n",
        session
            .get("workstream_id")
            .and_then(Value::as_str)
            .map_or_else(|| "_(unbound)_".to_string(), |w| format!("`{w}`"))
    ));
    out.push_str(&format!(
        "- **Project:** {}\n",
        session
            .get("project")
            .and_then(Value::as_str)
            .map_or_else(|| "_(none)_".to_string(), ToString::to_string)
    ));
    out.push_str(&format!(
        "- **Task:** {}\n",
        if task.trim().is_empty() {
            "_(none)_".to_string()
        } else {
            task
        }
    ));
    out.push_str(&format!(
        "- **Mode:** {}\n",
        session
            .get("mode")
            .and_then(Value::as_str)
            .map_or_else(|| "_(default)_".to_string(), ToString::to_string)
    ));
    out.push_str(&format!(
        "- **Status:** {}\n",
        session
            .get("status")
            .and_then(Value::as_str)
            .map_or_else(|| "unknown".to_string(), ToString::to_string)
    ));
    out.push_str(&format!(
        "- **Session started:** {}\n",
        str_or(session.get("created_at"), "")
    ));
    out.push_str(&format!("- **Exported:** {generated_at}\n"));
    out.push_str(&format!("- **Turns:** {}\n", turns.len()));
    out.push_str(&format!(
        "- **Cost (USD):** {}\n",
        record
            .get("cost_usd")
            .and_then(Value::as_f64)
            .map_or_else(|| "_(n/a)_".to_string(), |c| format!("{c:.4}"))
    ));
    out.push_str("\n---\n\n");

    if turns.is_empty() {
        out.push_str("_No turns were recorded for this session._\n");
        return out;
    }

    for (i, turn) in turns.iter().enumerate() {
        render_turn(&mut out, i, turn);
    }
    out
}

/// One `##` turn section — split out to keep [`render`] under one screen.
fn render_turn(out: &mut String, index: usize, turn: &Value) {
    let role_upper = str_or(turn.get("role"), "").to_uppercase();
    let model = str_or(turn.get("model"), "");
    if model.is_empty() {
        out.push_str(&format!("## {}. {role_upper}\n\n", index + 1));
    } else {
        out.push_str(&format!("## {}. {role_upper} · {model}\n\n", index + 1));
    }

    let text = str_or(turn.get("text"), "");
    let mut had_activity = false;
    if !text.trim().is_empty() {
        out.push_str(&text);
        out.push_str("\n\n");
        had_activity = true;
    }
    let tools = turn
        .get("tool_calls")
        .and_then(Value::as_array)
        .map_or(&[][..], Vec::as_slice);
    for tool in tools {
        out.push_str(&format!(
            "- `{role_upper}` ran: {}\n",
            str_or(Some(tool), "")
        ));
        had_activity = true;
    }
    let ran_test = turn
        .get("ran_test_command")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if ran_test {
        out.push_str(&format!("- `{role_upper}` ran the test command\n"));
        had_activity = true;
    }
    if !tools.is_empty() || ran_test {
        out.push('\n');
    }
    if !had_activity {
        out.push_str("_(no output)_\n\n");
    }
}

/// A JSON string field, or `fallback` when it is absent or not a string.
fn str_or(value: Option<&Value>, fallback: &str) -> String {
    value
        .and_then(Value::as_str)
        .unwrap_or(fallback)
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session() -> Value {
        json!({
            "id": "s-1",
            "task": "ship the bridge",
            "project": "trusty-tools",
            "status": "running",
            "created_at": "2026-09-08T10:00:00+00:00",
            "mode": "harness",
            "workstream_id": "w-1",
        })
    }

    /// Why: the daemon's renderer is the format the download button has been
    /// producing, and a header line that moved would land in an operator's saved
    /// file with no way to tell which version wrote it.
    /// Test: this is the test.
    #[test]
    fn matches_the_daemons_header_and_turn_shape() {
        let record = json!({
            "session_id": "s-1",
            "cost_usd": 1.5,
            "turns": [
                { "role": "pm", "model": "opus", "text": "hello", "tool_calls": [], "ran_test_command": false },
            ],
        });
        let out = render(&session(), &record, "2026-09-08T11:00:00+00:00");
        assert!(out.starts_with("# Workstream transcript — ship the bridge\n\n"));
        for line in [
            "- **Session:** `s-1`\n",
            "- **Workstream:** `w-1`\n",
            "- **Project:** trusty-tools\n",
            "- **Task:** ship the bridge\n",
            "- **Mode:** harness\n",
            "- **Status:** running\n",
            "- **Session started:** 2026-09-08T10:00:00+00:00\n",
            "- **Exported:** 2026-09-08T11:00:00+00:00\n",
            "- **Turns:** 1\n",
            "- **Cost (USD):** 1.5000\n",
        ] {
            assert!(out.contains(line), "missing {line:?} in:\n{out}");
        }
        assert!(
            out.contains("\n---\n\n## 1. PM · opus\n\nhello\n\n"),
            "{out}"
        );
    }

    /// Why: a session with no turns must say so rather than render an empty
    /// document an operator would read as a failed export.
    /// Test: this is the test.
    #[test]
    fn an_empty_transcript_says_so() {
        let record = json!({ "session_id": "s-2", "turns": [] });
        let out = render(&json!({ "task": "" }), &record, "2026-09-08T11:00:00+00:00");
        assert!(
            out.starts_with("# Workstream transcript — s-2\n\n"),
            "{out}"
        );
        assert!(out.contains("- **Task:** _(none)_\n"), "{out}");
        assert!(out.contains("- **Workstream:** _(unbound)_\n"), "{out}");
        assert!(out.contains("- **Cost (USD):** _(n/a)_\n"), "{out}");
        assert!(
            out.ends_with("_No turns were recorded for this session._\n"),
            "{out}"
        );
    }

    /// Why (#3526): the tool-run lines are the whole point — a runaway loop has
    /// to read as a repeated column, and a turn with neither prose nor tools has
    /// to be visible rather than blank.
    /// Test: this is the test.
    #[test]
    fn a_tool_only_turn_renders_its_tool_lines() {
        let record = json!({
            "session_id": "s-3",
            "turns": [
                { "role": "engineer", "model": "", "text": "", "tool_calls": ["bash", "bash"], "ran_test_command": true },
                { "role": "engineer", "model": "", "text": "", "tool_calls": [], "ran_test_command": false },
            ],
        });
        let out = render(&json!({ "task": "t" }), &record, "now");
        assert!(out.contains("## 1. ENGINEER\n\n"), "{out}");
        assert_eq!(out.matches("- `ENGINEER` ran: bash\n").count(), 2, "{out}");
        assert!(out.contains("- `ENGINEER` ran the test command\n"), "{out}");
        assert!(out.contains("## 2. ENGINEER\n\n_(no output)_\n\n"), "{out}");
    }
}
