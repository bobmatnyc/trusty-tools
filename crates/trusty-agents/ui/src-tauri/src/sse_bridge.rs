//! Desktop-shell Server-Sent Events bridge (token-streaming).
//!
//! Why: In browser mode `App.svelte` opens an `EventSource` against
//! `/api/events` and fans `agent_message_delta` onto the `task-delta` web-bus
//! so the chat bubble grows token-by-token. But the Tauri desktop shell — the
//! actual demo vehicle — never runs that browser bridge (`startEventStream`
//! early-returns on `isDesktop()`), and `task_commands::send_message` only
//! POST-then-polls, so streaming deltas never reach the desktop UI. This
//! module gives the Rust side its OWN thin SSE client that connects to the
//! sidecar's `/api/events`, parses frames, and re-emits each streaming delta
//! as a Tauri `task-delta` event — exactly the payload the browser bridge
//! emits, so `ChatView` handles both transports with identical code.
//! What: [`start_event_bridge`] spawns a supervised background task that
//! (re)connects to `{api_base}/api/events`, incrementally parses the SSE
//! `event:`/`data:` framing off `Response::chunk()` (no extra deps — reqwest's
//! chunk reader is available without the `stream` feature), and for every
//! `agent_message_delta` frame emits `app.emit("task-delta", DeltaPayload)`.
//! #3752: it ALSO forwards the two Slack-mirror kinds
//! (`slack_message_received` / `slack_reply_sent`) as a `slack-event` Tauri
//! event carrying the raw event object — otherwise the SlackMirror pane would
//! populate only in browser mode (App.svelte's browser SSE bridge is skipped
//! on `isDesktop()`), leaving the packaged demo app blank. Ping/lag/other
//! events are ignored (the poll loop still drives progress/complete). The
//! connection auto-reconnects with a short backoff so a sidecar restart or idle
//! reap doesn't permanently kill streaming.
//! Test: `parse_dispatches_agent_message_delta`,
//! `parse_ignores_non_delta_frames`, `parse_handles_split_frames`,
//! `parse_slack_frame_matches_both_kinds`, `parse_slack_frame_ignores_others`.

use serde::Serialize;
use tauri::{AppHandle, Emitter};

use crate::sidecar::api_base;

/// Reconnect backoff after the SSE stream ends or errors.
const RECONNECT_BACKOFF: std::time::Duration = std::time::Duration::from_secs(2);

/// Payload emitted on the `task-delta` Tauri event — mirrors the browser
/// bridge's `DeltaPayload` (see `ui/src/lib/chatStream.ts`) so `ChatView`
/// consumes both transports identically.
#[derive(Debug, Clone, Serialize, PartialEq)]
struct DeltaPayload {
    task_id: String,
    text: String,
    agent: String,
    done: bool,
}

/// Start the desktop SSE→Tauri event bridge (idempotent responsibility of the
/// caller — spawn exactly once; see `sidecar::ensure_api_server`).
///
/// Why: A long-lived background listener is the only way the desktop shell can
/// receive push events without the browser `EventSource`. Spawned detached
/// because it must outlive any single `send_message` call and serve every task.
/// What: Loops forever — connect, pump frames until the stream ends, sleep
/// `RECONNECT_BACKOFF`, repeat. Errors are logged, never fatal.
pub fn start_event_bridge(app: AppHandle, port: u16) {
    tauri::async_runtime::spawn(async move {
        let url = format!("{}/api/events", api_base(port));
        loop {
            if let Err(e) = pump_stream(&app, &url).await {
                tracing::debug!(?e, "event bridge stream ended; will reconnect");
            }
            tokio::time::sleep(RECONNECT_BACKOFF).await;
        }
    });
}

/// Connect to `url` and pump SSE frames until the stream ends.
async fn pump_stream(app: &AppHandle, url: &str) -> Result<(), String> {
    let client = reqwest::Client::new();
    let mut resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| format!("connect: {e}"))?
        .error_for_status()
        .map_err(|e| format!("status: {e}"))?;

    let mut parser = SseParser::default();
    while let Some(chunk) = resp.chunk().await.map_err(|e| format!("read: {e}"))? {
        for data in parser.push(&chunk) {
            dispatch_frame(app, &data);
        }
    }
    Ok(())
}

/// Route one completed SSE `data:` payload to the matching Tauri event.
///
/// Why: The desktop shell forwards two disjoint event families — streaming
/// `agent_message_delta` (→ `task-delta`, browser-parity) and the Slack-mirror
/// kinds (→ `slack-event`, #3752). Everything else stays on the poll loop.
/// What: Emits `task-delta` for a delta frame, else `slack-event` (carrying the
/// raw event object) for a Slack frame, else nothing.
/// Test: `parse_dispatches_agent_message_delta`,
/// `parse_slack_frame_matches_both_kinds`.
fn dispatch_frame(app: &AppHandle, data: &str) {
    if let Some(payload) = parse_delta_frame(data) {
        let _ = app.emit("task-delta", payload);
    } else if let Some(value) = parse_slack_frame(data) {
        let _ = app.emit("slack-event", value);
    }
}

/// Incremental SSE frame parser.
///
/// Why: `Response::chunk()` yields arbitrary byte boundaries — a single SSE
/// frame (`event: event\ndata: {…}\n\n`) can span two chunks, and one chunk
/// can carry several frames. This buffers across chunks and yields the
/// `data:` payload of each COMPLETE `event: event` frame (the wire name the
/// server uses for real events; `ping`/`lag` frames are dropped).
/// What: Accumulates bytes, splits on `\n`, tracks the current frame's
/// `event`/`data` fields, and flushes on the blank-line frame terminator.
/// Test: `parse_handles_split_frames`, `parse_ignores_non_delta_frames`.
#[derive(Default)]
struct SseParser {
    buf: Vec<u8>,
    event: String,
    data: String,
}

impl SseParser {
    /// Feed a chunk; return the `data` payloads of any frames completed by it.
    fn push(&mut self, chunk: &[u8]) -> Vec<String> {
        self.buf.extend_from_slice(chunk);
        let mut out = Vec::new();
        while let Some(pos) = self.buf.iter().position(|&b| b == b'\n') {
            let line_bytes: Vec<u8> = self.buf.drain(..=pos).collect();
            let line = String::from_utf8_lossy(&line_bytes);
            let line = line.trim_end_matches(['\n', '\r']);
            if line.is_empty() {
                // Frame terminator: flush a completed `event: event` frame.
                if self.event == "event" && !self.data.is_empty() {
                    out.push(std::mem::take(&mut self.data));
                }
                self.event.clear();
                self.data.clear();
            } else if let Some(v) = line.strip_prefix("event:") {
                self.event = v.trim().to_string();
            } else if let Some(v) = line.strip_prefix("data:") {
                // SSE allows one optional space after the colon.
                self.data.push_str(v.strip_prefix(' ').unwrap_or(v));
            }
        }
        out
    }
}

/// Parse an SSE `data:` JSON payload into a `DeltaPayload` iff it is an
/// `agent_message_delta` event; otherwise `None`.
///
/// Why: Only streaming deltas cross this bridge; every other event type still
/// flows through the poll loop, so we filter here rather than re-emitting the
/// whole event vocabulary onto the desktop bus.
/// What: JSON-decodes `data`, checks `type == "agent_message_delta"`, and maps
/// the fields (defaulting missing ones) to the browser-parity payload.
/// Test: `parse_dispatches_agent_message_delta`, `parse_ignores_non_delta_frames`.
fn parse_delta_frame(data: &str) -> Option<DeltaPayload> {
    let value: serde_json::Value = serde_json::from_str(data).ok()?;
    if value.get("type").and_then(|t| t.as_str()) != Some("agent_message_delta") {
        return None;
    }
    Some(DeltaPayload {
        task_id: field_str(&value, "session_id"),
        text: field_str(&value, "text"),
        agent: field_str(&value, "agent"),
        done: value.get("done").and_then(|v| v.as_bool()).unwrap_or(false),
    })
}

/// Parse an SSE `data:` JSON payload into the raw event `Value` iff it is one
/// of the two Slack-mirror kinds; otherwise `None`. (#3752)
///
/// Why: The desktop SlackMirror pane consumes the SAME event shape the browser
/// bridge feeds `pushSlackEvent`, so we forward the whole object unchanged
/// rather than reshaping it into a bespoke payload — the frontend listener
/// calls `pushSlackEvent` on it identically in both transports.
/// What: JSON-decodes `data`, returns it iff `type` is `slack_message_received`
/// or `slack_reply_sent`.
/// Test: `parse_slack_frame_matches_both_kinds`, `parse_slack_frame_ignores_others`.
fn parse_slack_frame(data: &str) -> Option<serde_json::Value> {
    let value: serde_json::Value = serde_json::from_str(data).ok()?;
    match value.get("type").and_then(|t| t.as_str()) {
        Some("slack_message_received") | Some("slack_reply_sent") => Some(value),
        _ => None,
    }
}

fn field_str(value: &serde_json::Value, key: &str) -> String {
    value
        .get(key)
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_dispatches_agent_message_delta() {
        let data = r#"{"type":"agent_message_delta","session_id":"t1","agent":"Izzie","text":"Hel","done":false}"#;
        let got = parse_delta_frame(data).expect("should parse");
        assert_eq!(
            got,
            DeltaPayload {
                task_id: "t1".into(),
                text: "Hel".into(),
                agent: "Izzie".into(),
                done: false,
            }
        );
    }

    #[test]
    fn parse_ignores_non_delta_frames() {
        assert!(parse_delta_frame(r#"{"type":"pm_thinking","text":"hm"}"#).is_none());
        assert!(parse_delta_frame(r#"{"type":"session_done","status":"ok"}"#).is_none());
        assert!(parse_delta_frame("not json").is_none());
    }

    #[test]
    fn parse_handles_split_frames() {
        let mut p = SseParser::default();
        // A delta frame split across three chunks, followed by a ping.
        assert!(p.push(b"event: event\n").is_empty());
        assert!(p.push(b"data: {\"type\":\"agent_message").is_empty());
        let frames = p.push(b"_delta\",\"session_id\":\"t1\",\"text\":\"hi\",\"done\":false}\n\n");
        assert_eq!(frames.len(), 1);
        assert_eq!(parse_delta_frame(&frames[0]).unwrap().text, "hi");
        // Ping frame yields nothing.
        assert!(p.push(b"event: ping\ndata: {}\n\n").is_empty());
    }

    #[test]
    fn parse_slack_frame_matches_both_kinds() {
        // #3752: both Slack-mirror kinds are forwarded verbatim (raw object).
        let inbound = r#"{"type":"slack_message_received","channel":"C0","user_display":"Masa","text":"hi","tier":"all"}"#;
        let v = parse_slack_frame(inbound).expect("inbound should match");
        assert_eq!(v.get("type").unwrap(), "slack_message_received");
        assert_eq!(v.get("tier").unwrap(), "all");

        let reply = r#"{"type":"slack_reply_sent","channel":"C0","text":"ok","identity":"CTO Bot (as itself)"}"#;
        let v = parse_slack_frame(reply).expect("reply should match");
        assert_eq!(v.get("type").unwrap(), "slack_reply_sent");
        assert_eq!(v.get("identity").unwrap(), "CTO Bot (as itself)");
    }

    #[test]
    fn parse_slack_frame_ignores_others() {
        // A streaming delta is NOT a slack frame (it goes to `task-delta`).
        assert!(parse_slack_frame(r#"{"type":"agent_message_delta","text":"x"}"#).is_none());
        assert!(parse_slack_frame(r#"{"type":"pm_thinking","text":"x"}"#).is_none());
        assert!(parse_slack_frame("not json").is_none());
    }

    #[test]
    fn parse_flushes_multiple_frames_in_one_chunk() {
        let mut p = SseParser::default();
        let frames = p.push(
            b"event: event\ndata: {\"type\":\"agent_message_delta\",\"session_id\":\"a\",\"text\":\"1\"}\n\nevent: event\ndata: {\"type\":\"agent_message_delta\",\"session_id\":\"a\",\"text\":\"2\"}\n\n",
        );
        assert_eq!(frames.len(), 2);
        assert_eq!(parse_delta_frame(&frames[0]).unwrap().text, "1");
        assert_eq!(parse_delta_frame(&frames[1]).unwrap().text, "2");
    }
}
