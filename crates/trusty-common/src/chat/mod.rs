//! Provider-agnostic streaming chat abstraction with tool-use support.
//!
//! Why: trusty-memory and trusty-search both want to support more than one
//! upstream LLM (OpenRouter for cloud, Ollama / LM Studio for local). Rather
//! than each crate re-implementing the dispatch, we expose a small
//! [`ChatProvider`] trait plus concrete implementations and an auto-detector
//! for a running local model server. The trait also surfaces OpenAI-style
//! tool/function calling so downstream agents can let the model invoke tools
//! (search, memory recall, shell, etc.).
//!
//! What: defines the [`ChatProvider`] trait, [`ToolDef`] / [`ToolCall`] /
//! [`ChatEvent`] tool-use types, an [`OpenRouterProvider`] and an
//! [`OllamaProvider`] that both speak OpenAI-compatible
//! `/v1/chat/completions` with SSE streaming (including the streamed
//! `tool_calls` shape), a [`BedrockProvider`] that uses the AWS Bedrock
//! `Converse` API (behind the `bedrock` feature flag), and
//! [`auto_detect_local_provider`] which probes `{base_url}/v1/models` with a
//! 1-second timeout.
//!
//! Test: `cargo test -p trusty-common` covers default config values, the
//! unreachable-server path of `auto_detect_local_provider`, SSE delta
//! streaming, and accumulation of streamed tool-call fragments.

mod openai_compat;

#[cfg(feature = "bedrock")]
mod bedrock_impl;
#[cfg(not(feature = "bedrock"))]
mod bedrock_stub;

pub use openai_compat::{OllamaProvider, OpenRouterProvider, auto_detect_local_provider};

#[cfg(feature = "bedrock")]
pub use bedrock_impl::{
    BedrockProvider, DEFAULT_BEDROCK_MODEL, DEFAULT_BEDROCK_REGION, ENV_REGION_AWS,
    ENV_REGION_TRUSTY,
};

// Re-expose the bedrock_impl module as `bedrock_provider` so downstream
// crates can access constants (e.g. `DEFAULT_BEDROCK_MODEL`) without needing
// to depend on the bedrock feature themselves.
#[cfg(feature = "bedrock")]
pub mod bedrock_provider {
    pub use super::bedrock_impl::*;
}

#[cfg(not(feature = "bedrock"))]
pub use bedrock_stub::BedrockProvider;

// Stub constant so code that references DEFAULT_BEDROCK_MODEL compiles without
// the bedrock feature. Must stay in sync with bedrock_impl::DEFAULT_BEDROCK_MODEL.
// Claude Sonnet 4.6 drops the date stamp and -v1:0 suffix (verified vs AWS docs).
#[cfg(not(feature = "bedrock"))]
pub const DEFAULT_BEDROCK_MODEL: &str = "us.anthropic.claude-sonnet-4-6";

use crate::ChatMessage;
use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc::Sender;

// ── Public re-exports so callers get the full surface from `chat::*` ──────────

/// Configuration for a local OpenAI-compatible model server (Ollama, LM
/// Studio, llama.cpp's server, etc.).
///
/// Why: callers want a single struct they can deserialize from config files
/// and pass to [`auto_detect_local_provider`] without juggling defaults.
/// What: holds an enable flag, the server's base URL (no trailing slash),
/// and the default model to request. Defaults target Ollama's standard
/// localhost binding.
/// Test: `local_model_config_defaults` asserts the default values.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalModelConfig {
    pub enabled: bool,
    pub base_url: String,
    pub model: String,
}

impl Default for LocalModelConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            base_url: "http://localhost:11434".to_string(),
            model: "qwen3:30b".to_string(),
        }
    }
}

// ─── Tool-use types ───────────────────────────────────────────────────────────

/// JSON-Schema description of a callable tool, in OpenAI function-calling
/// shape.
///
/// Why: downstream agents (trusty-memory, trusty-search) expose tools like
/// `memory_recall` or `web_search` to the LLM. The OpenAI tool format is the
/// de-facto common denominator across OpenRouter, Ollama, LM Studio, and
/// most cloud providers.
/// What: `name` and `description` are passed verbatim; `parameters` is a
/// JSON Schema object (typically `{"type":"object","properties":{...}}`).
/// Test: `tool_def_serializes_as_function` checks the wire shape.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

/// A tool invocation the model wants the host to perform.
///
/// Why: the streaming chat API emits `tool_calls` in fragments — first an
/// `id` + `function.name`, then a string of `function.arguments` deltas.
/// We accumulate fragments and surface one fully-formed [`ToolCall`] per
/// invocation to the caller.
/// What: `id` is the upstream's call id (echoed back in subsequent
/// `role:"tool"` messages); `name` is the function name; `arguments` is a
/// JSON string (NOT a parsed value — many models emit malformed JSON and
/// callers want the raw text for error reporting / repair).
/// Test: `accumulates_streamed_tool_call_fragments`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

/// Streaming chat event.
///
/// Why: replaces the previous "string-only" channel so callers can
/// distinguish text deltas from tool invocations and from terminal
/// success/error without parsing magic markers out of the text stream.
/// What: `Delta` is a content chunk; `ToolCall` is a fully-accumulated tool
/// invocation; `Done` signals the upstream stream terminated normally;
/// `Error` carries a human-readable message for stream-mid failures (the
/// provider also returns `Err` from `chat_stream`, but `Error` lets the
/// caller display partial-stream failures inline).
/// Test: `ollama_provider_streams_sse_deltas`.
#[derive(Debug, Clone)]
pub enum ChatEvent {
    Delta(String),
    ToolCall(ToolCall),
    Done,
    Error(String),
}

/// Streaming chat provider abstraction.
///
/// Why: downstream crates (trusty-memory, trusty-search) want to support
/// multiple LLM backends without hard-coding which one to call. Providers
/// expose a uniform streaming interface so the caller can swap them at
/// runtime based on configuration / availability.
/// What: implementors stream [`ChatEvent`]s into `tx`. Pass an empty
/// `tools` vec to disable tool use entirely (the provider MUST then omit
/// the `tools` field from the upstream request — some models error on an
/// empty array). Returning `Ok(())` means the stream completed normally;
/// the caller should also expect a final [`ChatEvent::Done`].
/// Test: implementations are covered by their own unit tests in this
/// module plus integration tests in downstream crates.
#[async_trait]
pub trait ChatProvider: Send + Sync {
    /// Human-readable provider name (e.g. `"openrouter"`, `"ollama"`).
    fn name(&self) -> &str;
    /// Model identifier sent on every request.
    fn model(&self) -> &str;
    /// Stream chat events into `tx`. `tools` empty disables tool use.
    async fn chat_stream(
        &self,
        messages: Vec<ChatMessage>,
        tools: Vec<ToolDef>,
        tx: Sender<ChatEvent>,
    ) -> Result<()>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_model_config_defaults() {
        let cfg = LocalModelConfig::default();
        assert!(cfg.enabled);
        assert_eq!(cfg.base_url, "http://localhost:11434");
        assert_eq!(cfg.model, "qwen3:30b");
    }

    #[test]
    fn local_model_config_deserializes_from_toml() {
        let toml_src = r#"
            enabled = true
            base_url = "http://localhost:1234"
            model = "qwen2.5-coder"
        "#;
        let cfg: LocalModelConfig = toml::from_str(toml_src).expect("parse TOML");
        assert!(cfg.enabled);
        assert_eq!(cfg.base_url, "http://localhost:1234");
        assert_eq!(cfg.model, "qwen2.5-coder");
    }
}
