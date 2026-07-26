//! Shared OpenAI-compatible SSE stream pump.
//!
//! Why: OpenRouter and Ollama both speak the same SSE `data:` framing with
//! identical JSON delta shapes. Extracting the pump into its own file keeps
//! the two provider implementations in lock-step and makes the protocol logic
//! easy to test in isolation.
//! What: `ToolCallAccumulator` accumulates multi-frame tool-call fragments;
//! `pump_openai_sse` drives a `reqwest::Response` bytes stream, forwards
//! `delta.content` as `ChatEvent::Delta`, accumulates `delta.tool_calls`,
//! and on `[DONE]` (or upstream EOF) emits one `ChatEvent::ToolCall` per
//! accumulated call followed by `ChatEvent::Done`.
//! Failure detection (issue #3757): the pump never used to emit
//! [`ChatEvent::Error`] — a mid-stream `{"error": {...}}` frame, a stream cut
//! off mid-frame, and a 200 answered with a non-SSE body ALL arrived as a clean
//! [`ChatEvent::Done`], so a partial answer rendered as a complete one. This
//! file now mirrors the three guards `inference::streaming` shipped in PR
//! #3751: a string-or-numeric in-band error probe, incomplete-frame detection
//! at EOF, and a `Content-Type` guard that degrades a buffered body instead of
//! silently dropping it.
//! Test: `ollama_provider_streams_sse_deltas`,
//! `accumulates_streamed_tool_call_fragments`, and `sse_pump_tests.rs`.

use crate::chat::{ChatEvent, ToolCall};
use anyhow::{Context, Result, anyhow};
use tokio::sync::mpsc::Sender;

#[cfg(test)]
#[path = "sse_pump_tests.rs"]
mod tests;

/// Accumulator for streamed tool-call fragments.
///
/// Why: OpenAI-style streaming sends each tool call across multiple SSE
/// frames: the first frame at a given `index` carries `id` and
/// `function.name`; subsequent frames append to `function.arguments`. We
/// accumulate by `index` and emit fully-formed [`ToolCall`]s only after the
/// stream terminates (or we see `finish_reason: tool_calls`).
/// What: a vector slot per index, growing as needed; merge logic is in
/// `apply_delta`. `finalize` drops slots that never received an id (defensive
/// — shouldn't happen but avoids emitting half-baked calls).
/// Test: `accumulates_streamed_tool_call_fragments`.
#[derive(Debug, Default)]
pub(super) struct ToolCallAccumulator {
    // index -> (id, name, args)
    slots: Vec<Option<(String, String, String)>>,
}

impl ToolCallAccumulator {
    pub(super) fn apply_delta(&mut self, tool_calls: &serde_json::Value) {
        let Some(arr) = tool_calls.as_array() else {
            return;
        };
        for tc in arr {
            let idx = tc.get("index").and_then(|i| i.as_u64()).unwrap_or(0) as usize;
            while self.slots.len() <= idx {
                self.slots.push(None);
            }
            let slot = self.slots[idx]
                .get_or_insert_with(|| (String::new(), String::new(), String::new()));
            if let Some(id) = tc
                .get("id")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
            {
                slot.0 = id.to_string();
            }
            if let Some(func) = tc.get("function") {
                if let Some(name) = func
                    .get("name")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
                {
                    slot.1 = name.to_string();
                }
                if let Some(args) = func.get("arguments").and_then(|v| v.as_str()) {
                    slot.2.push_str(args);
                }
            }
        }
    }

    pub(super) fn finalize(self) -> Vec<ToolCall> {
        self.slots
            .into_iter()
            .filter_map(|opt| {
                opt.and_then(|(id, name, arguments)| {
                    if name.is_empty() {
                        None
                    } else {
                        Some(ToolCall {
                            id,
                            name,
                            arguments,
                        })
                    }
                })
            })
            .collect()
    }
}

/// What the frame loop should do after one decoded SSE line.
///
/// Why: [`handle_line`] is shared by the live byte-stream loop and the
/// buffered-body fallback, so it must report "a terminal event was already
/// dispatched" without either caller re-deriving that from the event itself.
/// Crucially it must distinguish a SUCCESSFUL stop from a FAILED one: an
/// earlier revision collapsed both into `Stop`, so a mid-stream error frame
/// emitted `ChatEvent::Error` but still returned `Ok(())` — contradicting this
/// module's own contract that a failure surfaces on both channels, and leaving
/// consumers that only join the pump task (trusty-agents, trusty-search)
/// believing the stream succeeded.
/// What: `Continue` keeps reading; `Stop` means a normal terminal (`Done`) was
/// sent or the receiver hung up — either way the caller returns `Ok`;
/// `Failed(message)` means [`ChatEvent::Error`] was already sent and the caller
/// MUST return `Err` carrying `message`.
/// Test: `error_frame_emits_error_event`, `error_frame_returns_err`.
#[derive(Debug, PartialEq, Eq)]
enum Flow {
    Continue,
    Stop,
    Failed(String),
}

/// Render a provider's in-band `error` payload as a human-readable message.
///
/// Why (issue #3757): OpenRouter and other gateways report a mid-stream failure
/// as a JSON `{"error": {...}}` frame rather than a non-2xx status — the
/// connection already succeeded — and the `code` is provider-dependent: a
/// NUMBER (`429`) on some, a STRING (`"insufficient_quota"`) on others. Probing
/// both shapes (and a bare string payload) keeps a real quota/rate-limit
/// failure from being silently dropped, exactly as
/// `inference::streaming::error_from_chunk` does for the other stack.
/// What: a bare string payload is used verbatim; otherwise `message` (with a
/// generic fallback) prefixed by `code` when present in either shape.
/// Test: `error_message_reads_numeric_code`, `error_message_reads_string_code`,
/// `error_message_falls_back_without_code`.
fn error_message_from_frame(err: &serde_json::Value) -> String {
    if let Some(s) = err.as_str().filter(|s| !s.is_empty()) {
        return s.to_string();
    }
    let message = err
        .get("message")
        .and_then(|m| m.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("provider streaming error");
    let code = err.get("code");
    let code_str = code
        .and_then(|c| c.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .or_else(|| code.and_then(|c| c.as_u64()).map(|n| n.to_string()));
    match code_str {
        Some(c) => format!("{c}: {message}"),
        None => message.to_string(),
    }
}

/// Whether a response body is an SSE stream according to its `Content-Type`.
///
/// Why (issue #3757): a conformant provider answers a `stream:true` request
/// with `text/event-stream`. Some gateways/load balancers strip streaming and
/// return a buffered JSON body at 200 — feeding THAT to the frame loop yields
/// zero deltas and a clean `Done`, silently dropping the entire answer.
/// What: case-insensitive substring match; a missing header is treated as
/// non-SSE, which is safe because [`pump_non_sse_body`] still decodes a body
/// that turns out to contain `data:` frames.
/// Test: `content_type_guard_matches_sse`.
fn is_event_stream(headers: &reqwest::header::HeaderMap) -> bool {
    headers
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.to_ascii_lowercase().contains("text/event-stream"))
        .unwrap_or(false)
}

/// Flush accumulated tool calls followed by the terminal [`ChatEvent::Done`].
async fn flush_terminal(acc: ToolCallAccumulator, tx: &Sender<ChatEvent>) -> Flow {
    for call in acc.finalize() {
        if tx.send(ChatEvent::ToolCall(call)).await.is_err() {
            return Flow::Stop;
        }
    }
    let _ = tx.send(ChatEvent::Done).await;
    Flow::Stop
}

/// Decode one `data:` line into events on `tx`.
///
/// Why: the single frame decoder, shared by the live stream and the
/// buffered-body fallback so both paths treat `[DONE]`, error frames, content
/// deltas, and tool-call fragments identically.
/// What: non-`data:` lines, blank payloads, and unparseable NON-error frames
/// are skipped (best effort, matching the pre-#3757 behaviour). `[DONE]` flushes
/// the terminal events. An `error` field emits [`ChatEvent::Error`] and stops —
/// checked on the raw `Value` BEFORE anything else so a malformed-but-error
/// payload is never skipped as "just a bad frame".
/// Test: `sse_pump_tests.rs`.
async fn handle_line(line: &str, acc: &mut ToolCallAccumulator, tx: &Sender<ChatEvent>) -> Flow {
    let line = line.trim();
    let Some(payload) = line.strip_prefix("data:").map(str::trim) else {
        return Flow::Continue;
    };
    if payload.is_empty() {
        return Flow::Continue;
    }
    if payload == "[DONE]" {
        return flush_terminal(std::mem::take(acc), tx).await;
    }
    let Ok(v) = serde_json::from_str::<serde_json::Value>(payload) else {
        return Flow::Continue;
    };
    // #3757: an in-band error frame is a FAILURE, not a delta — surface it
    // instead of letting the stream end as an apparently complete answer.
    // `"error": null` is a real wire shape (providers that always include the
    // key), so a PRESENT-but-null value must not be read as a failure.
    if let Some(err) = v.get("error").filter(|e| !e.is_null()) {
        let message = error_message_from_frame(err);
        let _ = tx.send(ChatEvent::Error(message.clone())).await;
        return Flow::Failed(message);
    }
    let Some(delta) = v
        .get("choices")
        .and_then(|c| c.get(0))
        .and_then(|c| c.get("delta"))
    else {
        return Flow::Continue;
    };
    let content = delta
        .get("content")
        .and_then(|c| c.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());
    if let Some(content) = content
        && tx.send(ChatEvent::Delta(content)).await.is_err()
    {
        return Flow::Stop;
    }
    if let Some(tc) = delta.get("tool_calls") {
        acc.apply_delta(tc);
    }
    Flow::Continue
}

/// Drive one OpenAI-compatible SSE stream into the caller's [`ChatEvent`]
/// channel.
///
/// Why: OpenRouter and Ollama both speak the same wire format; sharing the
/// loop keeps the two providers in lock-step.
/// What: on an SSE body, reads `resp.bytes_stream()`, splits on newlines, and
/// feeds each line to [`handle_line`] — forwarding `delta.content` as
/// [`ChatEvent::Delta`], accumulating `delta.tool_calls`, and on `[DONE]`
/// emitting one [`ChatEvent::ToolCall`] per accumulated call followed by
/// [`ChatEvent::Done`]. A non-SSE body is handed to [`pump_non_sse_body`].
///
/// Failure paths (issue #3757), each emitting [`ChatEvent::Error`] AND
/// returning `Err` so both channel-only consumers and those that join the pump
/// task observe the failure:
///   - a mid-stream `{"error": {...}}` frame ([`handle_line`]);
///   - a transport error part-way through the stream;
///   - EOF with bytes still buffered mid-frame — a TRUNCATED stream, which must
///     not be reported as a complete short answer. EOF on a clean frame
///     boundary WITHOUT `[DONE]` remains a normal finish: not every provider
///     emits the sentinel.
///
/// Test: `ollama_provider_streams_sse_deltas`,
/// `accumulates_streamed_tool_call_fragments`, and `sse_pump_tests.rs`.
pub(super) async fn pump_openai_sse(resp: reqwest::Response, tx: Sender<ChatEvent>) -> Result<()> {
    use futures_util::StreamExt;

    // #3757: a 200 that is not an SSE body would decode to zero deltas and a
    // clean Done, silently dropping the answer.
    if !is_event_stream(resp.headers()) {
        return pump_non_sse_body(resp, tx).await;
    }

    let mut acc = ToolCallAccumulator::default();
    // Bytes, not a String: a chunk boundary can split a multi-byte UTF-8
    // character, and the previous `from_utf8(..).is_err() => continue` dropped
    // that whole chunk — another silent-truncation path (#3757).
    let mut buf: Vec<u8> = Vec::new();
    let mut stream = resp.bytes_stream();

    while let Some(chunk) = stream.next().await {
        let bytes = match chunk {
            Ok(b) => b,
            Err(e) => {
                // #3757: consumers that only watch the channel never see the
                // returned Err, so report the failure both ways.
                let _ = tx
                    .send(ChatEvent::Error(format!(
                        "chat stream transport error: {e}"
                    )))
                    .await;
                return Err(e).context("read chat stream chunk");
            }
        };
        buf.extend_from_slice(&bytes);

        while let Some(idx) = buf.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = buf.drain(..=idx).collect();
            match handle_line(&String::from_utf8_lossy(&line), &mut acc, &tx).await {
                Flow::Continue => {}
                Flow::Stop => return Ok(()),
                // The Error event is already sent; the Err is the second half
                // of the contract.
                Flow::Failed(message) => return Err(anyhow!("{message}")),
            }
        }
    }

    // #3757: EOF. A clean socket close on a frame boundary is a normal finish,
    // but bytes left MID-frame mean the stream was CUT OFF — reporting that as
    // Done would render a truncated answer as a complete one.
    //
    // "Bytes remain" alone is too blunt a signal: a body whose final frame
    // simply lacks its trailing newline (including a bare `data: [DONE]`) is
    // COMPLETE, and failing it would flip a previously-successful stream to an
    // error — in trusty-agents, a second full blocking LLM call per turn. So
    // decode the residual first and only call it a truncation when it is not a
    // whole frame.
    let residual = String::from_utf8_lossy(&buf);
    let residual = residual.trim();
    if !residual.is_empty() {
        if !is_complete_frame(residual) {
            let message = "chat stream ended with an incomplete SSE frame (truncated response)";
            let _ = tx.send(ChatEvent::Error(message.to_string())).await;
            return Err(anyhow!("{message}"));
        }
        match handle_line(residual, &mut acc, &tx).await {
            Flow::Continue => {}
            Flow::Stop => return Ok(()),
            Flow::Failed(message) => return Err(anyhow!("{message}")),
        }
    }
    flush_terminal(acc, &tx).await;
    Ok(())
}

/// Handle a 2xx response whose body is not an SSE stream.
///
/// Why (issue #3757): a gateway/LB that strips streaming answers `stream:true`
/// with a buffered body. Running the frame loop over it produces zero deltas
/// and a clean `Done`, so the whole answer disappears with no error anywhere.
/// What: degrades instead of dropping —
///   1. a body that still carries `data:` frames (right wire, wrong
///      `Content-Type`) is decoded as SSE, so a mislabelled-but-valid stream
///      keeps working;
///   2. a buffered chat completion is replayed as the first choice's
///      `message.content` plus its `message.tool_calls`, then `Done`;
///   3. an `error` object, an unparseable body, or a completion carrying
///      neither content nor tool calls emits [`ChatEvent::Error`] and returns
///      `Err` — those are the cases with nothing to render.
///
/// Test: `non_sse_json_body_is_replayed`, `non_sse_body_without_content_errors`,
/// `mislabelled_sse_body_is_decoded`.
async fn pump_non_sse_body(resp: reqwest::Response, tx: Sender<ChatEvent>) -> Result<()> {
    let text = resp
        .text()
        .await
        .context("read non-SSE chat completion body")?;

    if text.lines().any(|l| l.trim_start().starts_with("data:")) {
        let mut acc = ToolCallAccumulator::default();
        for line in text.split_inclusive('\n') {
            match handle_line(line, &mut acc, &tx).await {
                Flow::Continue => {}
                Flow::Stop => return Ok(()),
                Flow::Failed(message) => return Err(anyhow!("{message}")),
            }
        }
        flush_terminal(acc, &tx).await;
        return Ok(());
    }

    let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else {
        return fail(
            &tx,
            format!(
                "chat stream: provider returned a non-SSE, non-JSON body ({} bytes): {}",
                text.len(),
                excerpt(&text)
            ),
        )
        .await;
    };
    // A present-but-null `error` key is not a failure (see `handle_line`).
    if let Some(err) = v.get("error").filter(|e| !e.is_null()) {
        return fail(&tx, error_message_from_frame(err)).await;
    }

    let message = v
        .get("choices")
        .and_then(|c| c.get(0))
        .and_then(|c| c.get("message"));
    let content = message
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_str())
        .filter(|s| !s.is_empty());
    let mut acc = ToolCallAccumulator::default();
    if let Some(tc) = message.and_then(|m| m.get("tool_calls")) {
        acc.apply_delta(tc);
    }
    let calls = acc.finalize();
    if content.is_none() && calls.is_empty() {
        return fail(
            &tx,
            format!(
                "chat stream: provider returned a non-SSE body with no content and no tool \
                 calls ({} bytes): {}",
                text.len(),
                excerpt(&text)
            ),
        )
        .await;
    }

    if let Some(content) = content
        && tx
            .send(ChatEvent::Delta(content.to_string()))
            .await
            .is_err()
    {
        return Ok(());
    }
    for call in calls {
        if tx.send(ChatEvent::ToolCall(call)).await.is_err() {
            return Ok(());
        }
    }
    let _ = tx.send(ChatEvent::Done).await;
    Ok(())
}

/// Whether a newline-less trailing line is a COMPLETE SSE frame rather than a
/// truncated one.
///
/// Why: at EOF the only thing distinguishing "the provider did not terminate
/// its last frame with a newline" from "the socket was cut mid-frame" is
/// whether the residual payload is self-consistent. Treating every residual as
/// a truncation makes a previously-successful stream fail — an expensive false
/// positive, since trusty-agents answers it with a second full blocking LLM
/// call.
/// What: `true` for an SSE comment/keep-alive (no payload to truncate), for
/// `[DONE]`, and for a `data:` payload that parses as complete JSON. `false`
/// for an unparseable payload (a cut-off JSON object) or a line that is not a
/// recognisable field at all (a severed `dat…` prefix).
/// Test: `unterminated_done_sentinel_completes_cleanly`,
/// `truncated_stream_errors_instead_of_done`.
fn is_complete_frame(line: &str) -> bool {
    let line = line.trim();
    if line.is_empty() || line.starts_with(':') {
        return true;
    }
    let Some(payload) = line.strip_prefix("data:").map(str::trim) else {
        return false;
    };
    payload.is_empty()
        || payload == "[DONE]"
        || serde_json::from_str::<serde_json::Value>(payload).is_ok()
}

/// Report an unrecoverable stream failure on BOTH channels.
///
/// Why (issue #3757): consumers are split — `trusty-agents`' streaming driver
/// and `trusty-search`'s UI join the pump task and inspect its `Result`, while
/// `trusty-memory` and `trusty-analyze` only watch the event channel. Sending
/// [`ChatEvent::Error`] *and* returning `Err` means neither group can mistake a
/// failed stream for a complete answer.
/// What: sends the message as an `Error` event (ignoring a hung-up receiver),
/// then returns it as an `Err`.
/// Test: `non_sse_body_without_content_errors`.
async fn fail(tx: &Sender<ChatEvent>, message: String) -> Result<()> {
    let _ = tx.send(ChatEvent::Error(message.clone())).await;
    Err(anyhow!("{message}"))
}

/// First 200 characters of a body, for error messages.
///
/// Why: an unusable body must be identifiable in a log without pasting a
/// multi-megabyte HTML error page into the error message.
/// What: truncates on a character boundary and appends an ellipsis marker.
/// Test: `non_sse_body_without_content_errors`.
fn excerpt(text: &str) -> String {
    const LIMIT: usize = 200;
    if text.chars().count() <= LIMIT {
        return text.to_string();
    }
    let head: String = text.chars().take(LIMIT).collect();
    format!("{head}…")
}
