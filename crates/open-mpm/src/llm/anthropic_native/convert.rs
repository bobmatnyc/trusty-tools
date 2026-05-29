//! Message/value conversion helpers for the Anthropic native wire format.
//!
//! Why: Building an Anthropic `/v1/messages` body requires several distinct
//! transformations (prefix stripping, assistant-message conversion, cache
//! breakpoint placement, text extraction) that would clutter the request
//! builder in `mod.rs`. Splitting them out keeps the builder readable and the
//! helpers individually testable.
//! What: Pure functions over `serde_json::Value` covering token estimation,
//! cache-control attachment, provider-prefix stripping, assistant-message
//! conversion, and best-effort text extraction.
//! Test: Covered by the parent module's `tests` (history estimate, cache
//! attachment, strip-prefix) which exercise these via the public builder.

use serde_json::{Value, json};

/// Cheap token estimate for an Anthropic-shaped messages array (#29).
///
/// Why: We need to decide whether the conversation has grown large enough to
/// warrant a second cache breakpoint without dragging in a full tokenizer.
/// Anthropic's tokenizer averages ~4 chars/token in English; a char-count /
/// 4 heuristic is plenty accurate to gate "history > 2000 tokens".
/// What: Walks the JSON, summing the byte length of every text string and
/// `tool_result` content found, returning `chars / 4` as a u32.
/// Test: `history_token_estimate_counts_text_blocks`.
pub(super) fn history_token_estimate(messages: &[Value]) -> u32 {
    let mut chars: usize = 0;
    for m in messages {
        match m.get("content") {
            Some(Value::String(s)) => chars += s.len(),
            Some(Value::Array(blocks)) => {
                for b in blocks {
                    if let Some(s) = b.get("text").and_then(|v| v.as_str()) {
                        chars += s.len();
                    }
                    if let Some(s) = b.get("content").and_then(|v| v.as_str()) {
                        chars += s.len();
                    }
                    if let Some(input) = b.get("input") {
                        chars += input.to_string().len();
                    }
                }
            }
            _ => {}
        }
    }
    (chars / 4) as u32
}

/// Attach `cache_control: {type:"ephemeral"}` to the last content block of
/// the most recent assistant message (#29).
///
/// Why: A second cache breakpoint placed at the most recent assistant turn
/// lets every subsequent request hit the prompt cache for the entire history
/// up to that point, dropping read costs by ~90% for long sessions.
/// What: Scans `messages` from the end for an assistant message; normalizes
/// its content to an array of blocks and inserts `cache_control` on the last
/// block. No-op when no assistant turn exists yet.
/// Test: `attach_cache_control_to_last_assistant_inserts_marker`.
pub(super) fn attach_cache_control_to_last_assistant(messages: &mut [Value]) {
    for m in messages.iter_mut().rev() {
        if m.get("role").and_then(|r| r.as_str()) != Some("assistant") {
            continue;
        }
        // Normalize content to an array shape so we can attach cache_control
        // to the last block uniformly.
        let content = m.get("content").cloned();
        let mut arr: Vec<Value> = match content {
            Some(Value::Array(a)) => a,
            Some(Value::String(s)) => vec![json!({"type": "text", "text": s})],
            _ => vec![],
        };
        if let Some(last) = arr.last_mut()
            && let Some(obj) = last.as_object_mut()
        {
            obj.insert("cache_control".to_string(), json!({"type": "ephemeral"}));
        }
        m["content"] = Value::Array(arr);
        return;
    }
}

/// Strip a single leading `vendor/` segment from a model name.
///
/// Why: Direct Anthropic API rejects names like `anthropic/claude-sonnet-4-5`;
/// it wants the bare model id. Callers' configs use the OpenRouter form.
/// What: If `model` contains `/`, returns the substring after the first `/`.
/// Test: `strip_provider_prefix_cases`.
pub(super) fn strip_provider_prefix(model: &str) -> String {
    match model.split_once('/') {
        Some((_, rest)) => rest.to_string(),
        None => model.to_string(),
    }
}

/// Convert a serialized assistant message (OpenAI shape) into Anthropic's
/// content-block array. Text content becomes a `text` block; any `tool_calls`
/// array becomes `tool_use` blocks.
pub(super) fn convert_assistant_message(v: &Value) -> Value {
    let mut blocks: Vec<Value> = Vec::new();

    // Text content (string or null).
    if let Some(text) = v.get("content").and_then(|c| c.as_str())
        && !text.is_empty()
    {
        blocks.push(json!({"type": "text", "text": text}));
    }

    if let Some(calls) = v.get("tool_calls").and_then(|c| c.as_array()) {
        for tc in calls {
            let id = tc
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let function = tc.get("function").cloned().unwrap_or(Value::Null);
            let name = function
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let args_str = function
                .get("arguments")
                .and_then(|v| v.as_str())
                .unwrap_or("{}");
            let input: Value = serde_json::from_str(args_str).unwrap_or_else(|_| json!({}));
            blocks.push(json!({
                "type": "tool_use",
                "id": id,
                "name": name,
                "input": input,
            }));
        }
    }

    if blocks.is_empty() {
        // Empty assistant turns would be rejected; emit an empty text block
        // so the request stays valid (Anthropic is lenient about empty text).
        blocks.push(json!({"type": "text", "text": ""}));
    }

    json!({"role": "assistant", "content": blocks})
}

/// Best-effort extraction of text content from either a bare string, an
/// array of content blocks, or null. Used for both system and tool-result
/// inputs.
pub(super) fn extract_text_content(content: Option<&Value>) -> String {
    match content {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(arr)) => {
            let mut buf = String::new();
            for block in arr {
                if let Some(s) = block.get("text").and_then(|v| v.as_str()) {
                    if !buf.is_empty() {
                        buf.push('\n');
                    }
                    buf.push_str(s);
                }
            }
            buf
        }
        Some(other) => other.to_string(),
    }
}
