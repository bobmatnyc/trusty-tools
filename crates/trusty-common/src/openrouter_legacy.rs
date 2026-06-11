//! Deprecated single-shot OpenRouter helpers.
//!
//! Why: `openrouter_chat` and `openrouter_chat_stream` were the original
//! direct call helpers before the `chat::OpenRouterProvider` streaming API
//! was introduced. They are preserved here for backward compatibility but
//! callers should migrate to `chat::OpenRouterProvider::chat_stream`.
//! What: wraps the OpenRouter REST API with a non-streaming and a streaming
//! variant; exposes `ChatMessage` for composing conversations.
//! Test: `chat_message_round_trips` and `openrouter_chat_rejects_empty_key`.

use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};

const OPENROUTER_URL: &str = "https://openrouter.ai/api/v1/chat/completions";
const HTTP_REFERER: &str = "https://github.com/bobmatnyc/trusty-common";
const X_TITLE: &str = "trusty-common";
const OPENROUTER_CONNECT_TIMEOUT_SECS: u64 = 10;
const OPENROUTER_REQUEST_TIMEOUT_SECS: u64 = 120;

/// OpenAI-compatible chat message.
///
/// Why: Both trusty-memory's `chat` subcommand and trusty-search's `/chat`
/// endpoint speak the OpenRouter format. Sharing the struct keeps them in
/// step (and lets callers compose chat histories without re-defining types).
/// Tool-use additions (`tool_call_id`, `tool_calls`) follow the OpenAI
/// function-calling shape: assistant messages set `tool_calls` when the model
/// requests tool invocations; subsequent `role: "tool"` messages echo the
/// matching `tool_call_id` with the tool's result in `content`.
/// What: `role` is one of `"system" | "user" | "assistant" | "tool"`.
/// `content` is the message text. `tool_call_id` is the id of the tool call
/// this message is replying to (only set when `role == "tool"`). `tool_calls`
/// is the raw OpenAI `tool_calls` array on an assistant message that asked
/// to invoke tools — kept as `serde_json::Value` so we don't drop any fields
/// the upstream may add.
/// Test: serde round-trip in `chat_message_round_trips`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub tool_calls: Option<Vec<serde_json::Value>>,
}

#[derive(Debug, Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    messages: &'a [ChatMessage],
    stream: bool,
}

#[derive(Debug, Deserialize)]
struct ChatResponse {
    choices: Vec<Choice>,
}

#[derive(Debug, Deserialize)]
struct Choice {
    message: ResponseMessage,
}

#[derive(Debug, Deserialize)]
struct ResponseMessage {
    #[serde(default)]
    content: String,
}

/// Send a chat completion request to OpenRouter and return the assistant's
/// message content.
///
/// Why: A one-shot, non-streaming chat call is the common-case helper — used
/// by trusty-memory's `chat` CLI and trusty-search's `/chat` endpoint.
/// What: POSTs `{model, messages, stream: false}` to OpenRouter with bearer
/// auth, decodes the response, and returns `choices[0].message.content`.
/// Errors propagate as anyhow with HTTP status context.
/// Test: error paths covered by `openrouter_chat_rejects_empty_key`.
#[deprecated(since = "0.3.1", note = "Use OpenRouterProvider::chat_stream instead")]
pub async fn openrouter_chat(
    api_key: &str,
    model: &str,
    messages: Vec<ChatMessage>,
) -> Result<String> {
    if api_key.is_empty() {
        return Err(anyhow!("openrouter api key is empty"));
    }
    let client = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(
            OPENROUTER_CONNECT_TIMEOUT_SECS,
        ))
        .timeout(std::time::Duration::from_secs(
            OPENROUTER_REQUEST_TIMEOUT_SECS,
        ))
        .build()
        .context("build reqwest client for openrouter_chat")?;
    let body = ChatRequest {
        model,
        messages: &messages,
        stream: false,
    };
    let resp = client
        .post(OPENROUTER_URL)
        .bearer_auth(api_key)
        .header("HTTP-Referer", HTTP_REFERER)
        .header("X-Title", X_TITLE)
        .json(&body)
        .send()
        .await
        .context("POST openrouter chat completions")?;
    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        return Err(anyhow!("openrouter HTTP {status}: {text}"));
    }
    let payload: ChatResponse = resp.json().await.context("decode openrouter response")?;
    payload
        .choices
        .into_iter()
        .next()
        .map(|c| c.message.content)
        .ok_or_else(|| anyhow!("openrouter returned no choices"))
}

/// Stream chat-completion deltas from OpenRouter through a tokio mpsc channel.
///
/// Why: `chat` UIs want incremental tokens for a responsive feel; the
/// streaming endpoint emits SSE `data:` frames with delta content.
/// What: POSTs the request with `stream: true`, parses each SSE `data:` line
/// as a JSON object, extracts `choices[0].delta.content`, and sends each
/// non-empty chunk to `tx`. The function returns when the stream terminates
/// (either by `[DONE]` sentinel or by upstream EOF).
/// Test: integration-only (no offline mock); covered manually via the
/// trusty-search `/chat` endpoint that re-uses this helper.
#[deprecated(since = "0.3.1", note = "Use OpenRouterProvider::chat_stream instead")]
pub async fn openrouter_chat_stream(
    api_key: &str,
    model: &str,
    messages: Vec<ChatMessage>,
    tx: tokio::sync::mpsc::Sender<String>,
) -> Result<()> {
    use futures_util::StreamExt;

    if api_key.is_empty() {
        return Err(anyhow!("openrouter api key is empty"));
    }
    let client = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(
            OPENROUTER_CONNECT_TIMEOUT_SECS,
        ))
        .timeout(std::time::Duration::from_secs(
            OPENROUTER_REQUEST_TIMEOUT_SECS,
        ))
        .build()
        .context("build reqwest client for openrouter_chat_stream")?;
    let body = ChatRequest {
        model,
        messages: &messages,
        stream: true,
    };
    let resp = client
        .post(OPENROUTER_URL)
        .bearer_auth(api_key)
        .header("HTTP-Referer", HTTP_REFERER)
        .header("X-Title", X_TITLE)
        .json(&body)
        .send()
        .await
        .context("POST openrouter chat completions (stream)")?;
    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        return Err(anyhow!("openrouter HTTP {status}: {text}"));
    }

    let mut buf = String::new();
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let bytes = chunk.context("read openrouter stream chunk")?;
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
            if payload.is_empty() || payload == "[DONE]" {
                continue;
            }
            let v: serde_json::Value = match serde_json::from_str(payload) {
                Ok(v) => v,
                Err(_) => continue,
            };
            let delta = v
                .get("choices")
                .and_then(|c| c.get(0))
                .and_then(|c| c.get("delta"))
                .and_then(|d| d.get("content"))
                .and_then(|c| c.as_str())
                .unwrap_or("");
            if !delta.is_empty() && tx.send(delta.to_string()).await.is_err() {
                return Ok(());
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chat_message_round_trips() {
        let m = ChatMessage {
            role: "user".into(),
            content: "hello".into(),
            tool_call_id: None,
            tool_calls: None,
        };
        let s = serde_json::to_string(&m).unwrap();
        let back: ChatMessage = serde_json::from_str(&s).unwrap();
        assert_eq!(back.role, "user");
        assert_eq!(back.content, "hello");
    }

    #[tokio::test]
    #[allow(deprecated)]
    async fn openrouter_chat_rejects_empty_key() {
        let err = openrouter_chat("", "x", vec![]).await.unwrap_err();
        assert!(err.to_string().contains("api key"));
    }
}
