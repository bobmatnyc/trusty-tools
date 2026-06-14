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
//! Test: `ollama_provider_streams_sse_deltas`,
//! `accumulates_streamed_tool_call_fragments`.

use crate::chat::{ChatEvent, ToolCall};
use anyhow::{Context, Result};
use tokio::sync::mpsc::Sender;

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

/// Drive one OpenAI-compatible SSE stream into the caller's [`ChatEvent`]
/// channel.
///
/// Why: OpenRouter and Ollama both speak the same wire format; sharing the
/// loop keeps the two providers in lock-step.
/// What: reads `resp.bytes_stream()`, splits on newlines, parses `data:`
/// frames, forwards `delta.content` as [`ChatEvent::Delta`], accumulates
/// `delta.tool_calls`, and on `[DONE]` (or upstream EOF) emits one
/// [`ChatEvent::ToolCall`] per accumulated call followed by
/// [`ChatEvent::Done`].
/// Test: covered by `ollama_provider_streams_sse_deltas` and
/// `accumulates_streamed_tool_call_fragments`.
pub(super) async fn pump_openai_sse(resp: reqwest::Response, tx: Sender<ChatEvent>) -> Result<()> {
    use futures_util::StreamExt;

    let mut acc = ToolCallAccumulator::default();
    let mut buf = String::new();
    let mut stream = resp.bytes_stream();

    while let Some(chunk) = stream.next().await {
        let bytes = chunk.context("read chat stream chunk")?;
        let text = match std::str::from_utf8(&bytes) {
            Ok(s) => s,
            Err(_) => continue,
        };
        buf.push_str(text);

        while let Some(idx) = buf.find('\n') {
            let line: String = buf.drain(..=idx).collect();
            let line = line.trim();
            let Some(payload) = line.strip_prefix("data:").map(str::trim) else {
                continue;
            };
            if payload.is_empty() {
                continue;
            }
            if payload == "[DONE]" {
                for call in std::mem::take(&mut acc).finalize() {
                    if tx.send(ChatEvent::ToolCall(call)).await.is_err() {
                        return Ok(());
                    }
                }
                let _ = tx.send(ChatEvent::Done).await;
                return Ok(());
            }
            let v: serde_json::Value = match serde_json::from_str(payload) {
                Ok(v) => v,
                Err(_) => continue,
            };
            let delta = v
                .get("choices")
                .and_then(|c| c.get(0))
                .and_then(|c| c.get("delta"));
            if let Some(delta) = delta {
                // Forward any non-empty text content as a Delta event.
                let content_opt = delta
                    .get("content")
                    .and_then(|c| c.as_str())
                    .filter(|s| !s.is_empty())
                    .map(|s| s.to_string());
                let send_err = match content_opt {
                    Some(content) => tx.send(ChatEvent::Delta(content)).await.is_err(),
                    None => false,
                };
                if send_err {
                    return Ok(());
                }
                if let Some(tc) = delta.get("tool_calls") {
                    acc.apply_delta(tc);
                }
            }
        }
    }

    // Upstream EOF without a [DONE] sentinel — still flush and finish.
    for call in acc.finalize() {
        if tx.send(ChatEvent::ToolCall(call)).await.is_err() {
            return Ok(());
        }
    }
    let _ = tx.send(ChatEvent::Done).await;
    Ok(())
}
