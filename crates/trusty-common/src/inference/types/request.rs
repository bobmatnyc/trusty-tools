//! [`ChatRequest`] — the normalized inference request body.
//!
//! Why: a dedicated struct keeps the serialisation surface explicit and avoids
//! ad-hoc `serde_json::json!` construction scattered across call sites. It
//! matches tcode's OpenAI-compatible request shape so #2406 migrates cleanly,
//! and it is what [`super::super::adapter::InferenceAdapter::chat`] accepts.
//! What: [`ChatRequest`] carries the model slug, message history, and the
//! standard optional knobs (temperature, max_tokens, tools, tool_choice, stop,
//! and the OpenRouter detailed-usage directive). Optional fields use
//! `skip_serializing_if` so the wire payload stays minimal.
//! Test: inline `tests` — `minimal_request_omits_optionals`,
//! `request_with_tools_and_stop_serialises`, `detailed_usage_toggles`.

use serde::{Deserialize, Serialize};

use super::message::ChatMessage;
use super::tool::{RequestUsageConfig, ToolDefinition};
use serde_json::Value;

/// The full request body for a chat/completions call.
///
/// Why: every adapter accepts this one shape; provider-specific translation
/// (tool-choice dialect, usage directive) happens inside the adapter, not here.
/// What: `model` is the provider slug; `messages` is the conversation history;
/// the remaining fields are optional sampling/tool/stop knobs plus `usage`
/// (OpenRouter's detailed-accounting opt-in). `tool_choice` is a raw `Value`
/// because its wire spelling is provider-dialect-specific (produced by
/// [`super::tool::openai_tool_choice`] or an adapter's own mapper).
/// Test: `minimal_request_omits_optionals`, `request_with_tools_and_stop_serialises`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatRequest {
    /// Provider model slug, e.g. `"anthropic/claude-sonnet-4-5"`.
    pub model: String,
    /// Conversation history, including the new user turn.
    pub messages: Vec<ChatMessage>,
    /// Sampling temperature.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    /// Maximum tokens to generate.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    /// Tools the model may call.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<ToolDefinition>>,
    /// Tool-choice policy in the target provider's wire dialect.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<Value>,
    /// Stop sequences that end generation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop: Option<Vec<String>>,
    /// OpenRouter detailed-usage directive; `None` (omitted) for every other
    /// provider/path.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<RequestUsageConfig>,
}

impl ChatRequest {
    /// Construct a minimal request from a model slug and message history.
    ///
    /// Why: the overwhelming majority of call sites want "this model, these
    /// messages, defaults for everything else"; a builder-free constructor keeps
    /// them terse while the optional knobs stay directly assignable.
    /// What: sets `model` and `messages`; every optional field starts `None`.
    /// Test: `minimal_request_omits_optionals`.
    pub fn new(model: impl Into<String>, messages: Vec<ChatMessage>) -> Self {
        Self {
            model: model.into(),
            messages,
            temperature: None,
            max_tokens: None,
            tools: None,
            tool_choice: None,
            stop: None,
            usage: None,
        }
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::super::tool::{FunctionDefinition, ToolDefinition};
    use super::*;
    use serde_json::json;

    /// Why: `None` optionals must be omitted so the wire payload stays lean.
    /// Test: itself.
    #[test]
    fn minimal_request_omits_optionals() {
        let req = ChatRequest::new("openai/gpt-4o-mini", vec![ChatMessage::user("hi")]);
        let v: Value = serde_json::to_value(&req).expect("serialise");
        assert_eq!(v["model"], "openai/gpt-4o-mini");
        assert_eq!(v["messages"][0]["content"], "hi");
        assert!(v.get("temperature").is_none() || v["temperature"].is_null());
        assert!(v.get("tools").is_none() || v["tools"].is_null());
        assert!(v.get("stop").is_none() || v["stop"].is_null());
        assert!(v.get("usage").is_none() || v["usage"].is_null());
    }

    /// Why: tool + stop + sampling fields must serialise in the OpenAI shape.
    /// Test: itself.
    #[test]
    fn request_with_tools_and_stop_serialises() {
        let mut req = ChatRequest::new("openai/gpt-4o-mini", vec![ChatMessage::user("weather?")]);
        req.temperature = Some(0.0);
        req.max_tokens = Some(256);
        req.tools = Some(vec![ToolDefinition::function(FunctionDefinition {
            name: "get_weather".into(),
            description: Some("weather".into()),
            parameters: Some(json!({"type": "object"})),
            cache_control: None,
        })]);
        req.tool_choice = Some(json!("auto"));
        req.stop = Some(vec!["\n\n".into()]);
        let v: Value = serde_json::to_value(&req).expect("serialise");
        assert_eq!(v["tools"][0]["function"]["name"], "get_weather");
        assert_eq!(v["tool_choice"], "auto");
        assert_eq!(v["temperature"], 0.0_f32);
        assert_eq!(v["max_tokens"], 256);
        assert_eq!(v["stop"][0], "\n\n");
    }

    /// Why: the detailed-usage directive must appear only when set.
    /// Test: itself.
    #[test]
    fn detailed_usage_toggles() {
        let mut req = ChatRequest::new("x", vec![ChatMessage::user("hi")]);
        req.usage = Some(RequestUsageConfig::detailed());
        let v: Value = serde_json::to_value(&req).expect("serialise");
        assert_eq!(v["usage"], json!({"include": true}));

        req.usage = None;
        let v: Value = serde_json::to_value(&req).expect("serialise");
        assert!(v.get("usage").is_none() || v["usage"].is_null());
    }
}
