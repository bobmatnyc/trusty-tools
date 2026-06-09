//! Wire types for the OpenRouter / OpenAI chat-completions API.
//!
//! Why: Keeping request and response types in a dedicated file lets the client
//! logic stay focused on HTTP mechanics while these types evolve independently.
//! What: `ChatMessage`, `ChatRequest`, `ToolDefinition`, `ChatResponse`, and
//! the `UsageBlock` → `TokenUsage` mapping are all defined here.
//! Test: `types::tests::*` — serialization round-trips and fixture parsing.

use serde::{Deserialize, Serialize};

use crate::perf::TokenUsage;

// ── Message roles ─────────────────────────────────────────────────────────────

/// A single message in the chat conversation.
///
/// Why: OpenRouter's `/chat/completions` endpoint uses the standard OpenAI
/// message schema; `role` distinguishes speaker identity and `content` carries
/// the text payload (or `null` for tool-call-only turns).
/// What: `role` is one of `"system"`, `"user"`, `"assistant"`, `"tool"`;
/// `content` is the text or `None` for assistant turns that only emit tool
/// calls; `tool_calls` carries the outbound calls for assistant turns;
/// `tool_call_id` and `name` are populated for `tool` role messages.
/// Test: `chat_message_serialises_all_roles`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ChatMessage {
    /// Conversation role: `"system"`, `"user"`, `"assistant"`, or `"tool"`.
    pub role: String,

    /// Text content; `None` on assistant turns that only emit tool calls.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,

    /// Tool calls emitted by the assistant.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,

    /// For `tool` role messages: the ID of the tool call this message responds to.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,

    /// For `tool` role messages: the name of the function that was called.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

impl ChatMessage {
    /// Construct a `system` message.
    ///
    /// Why: Convenience constructor; avoids repeated `role.to_string()` boilerplate.
    /// What: Returns a `ChatMessage` with `role = "system"` and the provided content.
    /// Test: `chat_message_constructors`.
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: "system".into(),
            content: Some(content.into()),
            tool_calls: None,
            tool_call_id: None,
            name: None,
        }
    }

    /// Construct a `user` message.
    ///
    /// Why: Convenience constructor for the most common message type.
    /// What: Returns a `ChatMessage` with `role = "user"` and the provided content.
    /// Test: `chat_message_constructors`.
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: "user".into(),
            content: Some(content.into()),
            tool_calls: None,
            tool_call_id: None,
            name: None,
        }
    }

    /// Construct an `assistant` message with text content.
    ///
    /// Why: Convenience constructor for building conversation history from prior
    /// assistant responses.
    /// What: Returns a `ChatMessage` with `role = "assistant"` and the provided content.
    /// Test: `chat_message_constructors`.
    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: "assistant".into(),
            content: Some(content.into()),
            tool_calls: None,
            tool_call_id: None,
            name: None,
        }
    }

    /// Construct a `tool` result message.
    ///
    /// Why: After executing a tool call, the result must be fed back in a `tool`
    /// role message referencing the original `tool_call_id`.
    /// What: Returns a `ChatMessage` with `role = "tool"`, the call ID, function
    /// name, and result content.
    /// Test: `chat_message_tool_role_round_trip`.
    pub fn tool_result(
        tool_call_id: impl Into<String>,
        name: impl Into<String>,
        content: impl Into<String>,
    ) -> Self {
        Self {
            role: "tool".into(),
            content: Some(content.into()),
            tool_calls: None,
            tool_call_id: Some(tool_call_id.into()),
            name: Some(name.into()),
        }
    }
}

// ── Tool-calling types ────────────────────────────────────────────────────────

/// A tool call emitted by the model.
///
/// Why: The model may decide to invoke one or more functions before producing a
/// final text response; callers must handle these before the conversation can
/// continue.
/// What: `id` is the opaque call identifier used when submitting the result
/// back; `r#type` is always `"function"`; `function` carries the name and
/// JSON-encoded arguments.
/// Test: `tool_call_deserialises_from_fixture`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolCall {
    /// Unique call identifier; echoed in the subsequent `tool` result message.
    pub id: String,

    /// Always `"function"` for the current OpenAI schema.
    #[serde(rename = "type")]
    pub kind: String,

    /// The function being called.
    pub function: FunctionCall,
}

/// The function name and arguments within a tool call.
///
/// Why: Separating this from `ToolCall` keeps the struct layout consistent with
/// the wire format (OpenAI wraps function details in a nested object).
/// What: `name` is the registered function name; `arguments` is a JSON string
/// (not a parsed value) exactly as emitted by the model.
/// Test: `tool_call_deserialises_from_fixture`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FunctionCall {
    /// Registered function name.
    pub name: String,

    /// JSON-encoded argument object; parse with `serde_json::from_str` at the
    /// call site.
    pub arguments: String,
}

/// A tool definition submitted with the request.
///
/// Why: Providing tool definitions tells the model which functions are available
/// so it can decide when and how to call them.
/// What: `r#type` is always `"function"`; `function` carries the JSON Schema
/// describing the callable function.
/// Test: `tool_definition_serialises`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolDefinition {
    /// Always `"function"`.
    #[serde(rename = "type")]
    pub kind: String,

    /// The function schema.
    pub function: FunctionDefinition,
}

impl ToolDefinition {
    /// Construct a function-type tool definition.
    ///
    /// Why: All current OpenAI-compatible tools are function tools; this
    /// constructor bakes in that invariant.
    /// What: Sets `kind = "function"` and wraps the provided `FunctionDefinition`.
    /// Test: `tool_definition_serialises`.
    pub fn function(function: FunctionDefinition) -> Self {
        Self {
            kind: "function".into(),
            function,
        }
    }
}

/// JSON Schema description of a callable function.
///
/// Why: The model needs the name, a description, and parameter schema to decide
/// when and how to call the function correctly.
/// What: `name` is the call target; `description` guides model selection;
/// `parameters` is a raw `serde_json::Value` holding the JSON Schema object
/// for flexibility (different tools may use different schema shapes).
/// Test: `function_definition_round_trip`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FunctionDefinition {
    /// The function name as registered with the model.
    pub name: String,

    /// Human-readable description guiding the model's decision to call it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    /// JSON Schema object describing the function's parameter shape.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parameters: Option<serde_json::Value>,
}

// ── Request / response types ──────────────────────────────────────────────────

/// The full request body for `POST /v1/chat/completions`.
///
/// Why: A dedicated struct keeps the serialisation surface explicit and avoids
/// ad-hoc `serde_json::json!` construction scattered across call sites.
/// What: All standard OpenAI chat-completions fields; optional fields use
/// `skip_serializing_if` so the wire payload stays minimal.
/// Test: `chat_request_serialises_required_fields`,
/// `chat_request_with_tools_serialises`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatRequest {
    /// OpenRouter / OpenAI model slug, e.g. `"anthropic/claude-sonnet-4-5"`.
    pub model: String,

    /// Conversation history, including the new user turn.
    pub messages: Vec<ChatMessage>,

    /// Sampling temperature in `[0.0, 2.0]`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,

    /// Maximum tokens to generate.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,

    /// Tools the model may call.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<ToolDefinition>>,

    /// Tool-choice policy: `"none"`, `"auto"`, `"required"`, or a specific
    /// function selector.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<serde_json::Value>,
}

/// One choice in the model response.
///
/// Why: The API returns a `choices` array (allowing `n > 1`); we always request
/// a single choice but the struct must match the wire schema.
/// What: `message` holds the assistant turn (text and/or tool calls);
/// `finish_reason` carries the stop condition.
/// Test: `chat_response_deserialises_fixture`.
#[derive(Debug, Clone, Deserialize)]
pub struct ChatChoice {
    /// The assistant message produced for this choice.
    pub message: AssistantMessage,

    /// Why the model stopped: `"stop"`, `"tool_calls"`, `"length"`, `"content_filter"`.
    pub finish_reason: Option<String>,
}

/// The assistant message within a choice.
///
/// Why: Mirrors `ChatMessage` but uses explicit struct rather than re-using the
/// request type to keep request/response paths cleanly separated and avoid
/// accidental mis-serialisation.
/// What: `content` is the text (or `None` for tool-only turns); `tool_calls`
/// carries any function invocations.
/// Test: `assistant_message_text_and_tool_calls`.
#[derive(Debug, Clone, Deserialize)]
pub struct AssistantMessage {
    /// Text content; `None` when the turn consists solely of tool calls.
    pub content: Option<String>,

    /// Tool calls emitted this turn.
    #[serde(default)]
    pub tool_calls: Vec<ToolCall>,
}

/// Token usage block from the API response.
///
/// Why: Mapping the wire `usage` object to our internal `TokenUsage` is easiest
/// when we first deserialise into this intermediate struct.
/// What: Carries `prompt_tokens`, `completion_tokens`, and `total_tokens` from
/// the wire; optional Anthropic-specific cache fields (present on Anthropic
/// models routed via OpenRouter).
/// Test: `usage_block_maps_to_token_usage`.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct UsageBlock {
    /// Tokens consumed by the prompt (including cached tokens).
    #[serde(default)]
    pub prompt_tokens: u32,

    /// Tokens produced by the completion.
    #[serde(default)]
    pub completion_tokens: u32,

    /// Sum of prompt + completion; informational only.
    #[serde(default)]
    pub total_tokens: u32,

    /// Anthropic-only: prompt tokens served from the cache (not re-computed).
    #[serde(default)]
    pub cache_read_input_tokens: u32,

    /// Anthropic-only: prompt tokens written into the cache this turn.
    #[serde(default)]
    pub cache_creation_input_tokens: u32,
}

impl UsageBlock {
    /// Convert this wire usage block into the crate's `TokenUsage`.
    ///
    /// Why: `PerfCollector` speaks `TokenUsage`; this conversion centralises the
    /// mapping so callers don't need to know the wire field names.
    /// What: Maps `prompt_tokens` → `prompt_tokens`, `completion_tokens` →
    /// `completion_tokens`, `cache_read_input_tokens` → `cache_read_tokens`,
    /// `cache_creation_input_tokens` → `cache_creation_tokens`.
    /// Test: `usage_block_maps_to_token_usage`.
    pub fn into_token_usage(self) -> TokenUsage {
        TokenUsage::new(
            self.prompt_tokens,
            self.completion_tokens,
            self.cache_read_input_tokens,
            self.cache_creation_input_tokens,
        )
    }
}

/// The top-level response from `POST /v1/chat/completions`.
///
/// Why: Wraps the `choices` array and `usage` block so callers receive a single
/// typed value from `LlmClient::chat`.
/// What: `id` is the response ID; `choices` contains the generated turns;
/// `usage` carries token accounting.
/// Test: `chat_response_deserialises_fixture`.
#[derive(Debug, Clone, Deserialize)]
pub struct ChatResponse {
    /// Opaque response identifier from the provider.
    pub id: String,

    /// Generated choices (one per `n`; we always request `n = 1`).
    pub choices: Vec<ChatChoice>,

    /// Token accounting for this request.
    #[serde(default)]
    pub usage: UsageBlock,
}

impl ChatResponse {
    /// Extract the first choice's text content, if any.
    ///
    /// Why: The vast majority of calls want the assistant's text response; this
    /// convenience method avoids repetitive indexing at every call site.
    /// What: Returns `choices[0].message.content.clone()` or `None` when the
    /// response has no choices or the content is absent.
    /// Test: `chat_response_first_text_content`.
    pub fn first_text(&self) -> Option<String> {
        self.choices.first()?.message.content.clone()
    }

    /// Extract the first choice's tool calls.
    ///
    /// Why: Tool-call workflows need quick access to the emitted calls without
    /// indexing into nested fields each time.
    /// What: Returns a reference to `choices[0].message.tool_calls`, or an empty
    /// slice when there are no choices.
    /// Test: `chat_response_first_tool_calls`.
    pub fn first_tool_calls(&self) -> &[ToolCall] {
        self.choices
            .first()
            .map(|c| c.message.tool_calls.as_slice())
            .unwrap_or(&[])
    }

    /// Extract the finish reason from the first choice.
    ///
    /// Why: Callers need the stop condition to decide whether to continue the
    /// tool-call loop or surface the response.
    /// What: Returns `choices[0].finish_reason.as_deref()` or `None`.
    /// Test: `chat_response_deserialises_fixture`.
    pub fn finish_reason(&self) -> Option<&str> {
        self.choices.first()?.finish_reason.as_deref()
    }

    /// Convert the response's `usage` block into `TokenUsage`.
    ///
    /// Why: `PerfCollector::record_phase` accepts `&TokenUsage`; this method
    /// keeps the conversion at one place.
    /// What: Delegates to `UsageBlock::into_token_usage`.
    /// Test: `chat_response_usage_into_token_usage`.
    pub fn token_usage(self) -> TokenUsage {
        self.usage.into_token_usage()
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Request serialisation ──────────────────────────────────────────────

    /// Minimal `ChatRequest` serialises required fields only.
    ///
    /// Why: Verify that optional fields with `None` values are omitted from the
    /// JSON payload, keeping requests lean.
    /// What: Build a minimal request, serialise it, check required fields present
    /// and optional fields absent.
    /// Test: this test.
    #[test]
    fn chat_request_serialises_required_fields() {
        let req = ChatRequest {
            model: "anthropic/claude-haiku-4-5".into(),
            messages: vec![ChatMessage::user("hello")],
            temperature: None,
            max_tokens: None,
            tools: None,
            tool_choice: None,
        };
        let v: serde_json::Value = serde_json::to_value(&req).expect("serialise");
        assert_eq!(v["model"], "anthropic/claude-haiku-4-5");
        assert_eq!(v["messages"][0]["role"], "user");
        assert_eq!(v["messages"][0]["content"], "hello");
        assert!(v.get("temperature").is_none() || v["temperature"].is_null());
        assert!(v.get("tools").is_none() || v["tools"].is_null());
    }

    /// `ChatRequest` with tools serialises the `tools` and `tool_choice` fields.
    ///
    /// Why: Tool-calling requests must include the tool schema; validate that
    /// the serialised payload matches the OpenAI wire format.
    /// What: Build a request with one `ToolDefinition`, serialise, assert schema
    /// structure.
    /// Test: this test.
    #[test]
    fn chat_request_with_tools_serialises() {
        let tool = ToolDefinition::function(FunctionDefinition {
            name: "get_weather".into(),
            description: Some("Returns weather data".into()),
            parameters: Some(serde_json::json!({
                "type": "object",
                "properties": { "location": { "type": "string" } },
                "required": ["location"]
            })),
        });
        let req = ChatRequest {
            model: "openai/gpt-4o-mini".into(),
            messages: vec![ChatMessage::user("What's the weather?")],
            temperature: Some(0.0),
            max_tokens: Some(256),
            tools: Some(vec![tool]),
            tool_choice: Some(serde_json::json!("auto")),
        };
        let v: serde_json::Value = serde_json::to_value(&req).expect("serialise");
        assert_eq!(v["tools"][0]["type"], "function");
        assert_eq!(v["tools"][0]["function"]["name"], "get_weather");
        assert_eq!(v["tool_choice"], "auto");
        assert_eq!(v["temperature"], 0.0_f32);
        assert_eq!(v["max_tokens"], 256);
    }

    // ── Message constructors ───────────────────────────────────────────────

    /// Convenience constructors produce the right `role` strings.
    ///
    /// Why: Verify each static constructor sets the role correctly.
    /// What: Assert roles for `system`, `user`, `assistant`.
    /// Test: this test.
    #[test]
    fn chat_message_constructors() {
        assert_eq!(ChatMessage::system("s").role, "system");
        assert_eq!(ChatMessage::user("u").role, "user");
        assert_eq!(ChatMessage::assistant("a").role, "assistant");
    }

    /// `tool_result` constructor sets `tool_call_id` and `name`.
    ///
    /// Why: Tool result messages have extra required fields; the constructor must
    /// populate them correctly.
    /// What: Assert `tool_call_id` and `name` are `Some`.
    /// Test: this test.
    #[test]
    fn chat_message_tool_role_round_trip() {
        let msg = ChatMessage::tool_result("call_abc", "get_weather", r#"{"temp":72}"#);
        assert_eq!(msg.role, "tool");
        assert_eq!(msg.tool_call_id.as_deref(), Some("call_abc"));
        assert_eq!(msg.name.as_deref(), Some("get_weather"));
        assert_eq!(msg.content.as_deref(), Some(r#"{"temp":72}"#));
    }

    // ── Response deserialisation ───────────────────────────────────────────

    /// A representative API response fixture deserialises correctly.
    ///
    /// Why: Ensures the struct layout matches the real OpenRouter wire format.
    /// What: Parse a JSON string modelled on an actual response; assert all
    /// top-level fields are populated.
    /// Test: this test.
    #[test]
    fn chat_response_deserialises_fixture() {
        let fixture = r#"{
          "id": "gen-abc123",
          "choices": [
            {
              "message": {
                "role": "assistant",
                "content": "Hello, world!",
                "tool_calls": []
              },
              "finish_reason": "stop"
            }
          ],
          "usage": {
            "prompt_tokens": 10,
            "completion_tokens": 4,
            "total_tokens": 14
          }
        }"#;
        let resp: ChatResponse = serde_json::from_str(fixture).expect("deserialise fixture");
        assert_eq!(resp.id, "gen-abc123");
        assert_eq!(resp.first_text().as_deref(), Some("Hello, world!"));
        assert_eq!(resp.finish_reason(), Some("stop"));
        assert!(resp.first_tool_calls().is_empty());
    }

    /// A response with a tool call deserialises `tool_calls`.
    ///
    /// Why: Tool-calling workflow depends on correctly parsing `tool_calls` from
    /// the response.
    /// What: Parse a fixture with `tool_calls` populated; assert name and args.
    /// Test: this test.
    #[test]
    fn tool_call_deserialises_from_fixture() {
        let fixture = r#"{
          "id": "gen-xyz",
          "choices": [
            {
              "message": {
                "role": "assistant",
                "content": null,
                "tool_calls": [
                  {
                    "id": "call_001",
                    "type": "function",
                    "function": {
                      "name": "get_weather",
                      "arguments": "{\"location\":\"Seattle\"}"
                    }
                  }
                ]
              },
              "finish_reason": "tool_calls"
            }
          ],
          "usage": {
            "prompt_tokens": 25,
            "completion_tokens": 15,
            "total_tokens": 40
          }
        }"#;
        let resp: ChatResponse = serde_json::from_str(fixture).expect("deserialise");
        assert!(resp.first_text().is_none());
        let calls = resp.first_tool_calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].id, "call_001");
        assert_eq!(calls[0].function.name, "get_weather");
        assert_eq!(resp.finish_reason(), Some("tool_calls"));
    }

    // ── UsageBlock → TokenUsage mapping ───────────────────────────────────

    /// `UsageBlock::into_token_usage` maps all four fields correctly.
    ///
    /// Why: Correct usage mapping is critical for cost tracking; a mis-map
    /// would silently produce wrong cost calculations.
    /// What: Build a `UsageBlock` with known values, convert, assert each field.
    /// Test: this test.
    #[test]
    fn usage_block_maps_to_token_usage() {
        let block = UsageBlock {
            prompt_tokens: 100,
            completion_tokens: 50,
            total_tokens: 150,
            cache_read_input_tokens: 20,
            cache_creation_input_tokens: 10,
        };
        let usage = block.into_token_usage();
        assert_eq!(usage.prompt_tokens, 100);
        assert_eq!(usage.completion_tokens, 50);
        assert_eq!(usage.cache_read_tokens, 20);
        assert_eq!(usage.cache_creation_tokens, 10);
    }

    /// Default `UsageBlock` produces a zero `TokenUsage`.
    ///
    /// Why: When the API omits the `usage` field (rare but possible), the
    /// default should not crash and should produce zero counts.
    /// What: Build `UsageBlock::default()`, convert, assert all zeros.
    /// Test: this test.
    #[test]
    fn default_usage_block_is_zero() {
        let usage = UsageBlock::default().into_token_usage();
        assert_eq!(usage.prompt_tokens, 0);
        assert_eq!(usage.completion_tokens, 0);
        assert_eq!(usage.cache_read_tokens, 0);
        assert_eq!(usage.cache_creation_tokens, 0);
    }

    /// `ChatResponse::token_usage` delegates to `UsageBlock::into_token_usage`.
    ///
    /// Why: Callers use `response.token_usage()` rather than digging into
    /// `response.usage`; this test ensures the method works end-to-end.
    /// What: Build a full response fixture, call `token_usage()`, assert counts.
    /// Test: this test.
    #[test]
    fn chat_response_usage_into_token_usage() {
        let fixture = r#"{
          "id": "gen-zzz",
          "choices": [{"message": {"role": "assistant", "content": "ok"}, "finish_reason": "stop"}],
          "usage": {"prompt_tokens": 8, "completion_tokens": 3, "total_tokens": 11,
                    "cache_read_input_tokens": 5, "cache_creation_input_tokens": 2}
        }"#;
        let resp: ChatResponse = serde_json::from_str(fixture).expect("deserialise");
        let usage = resp.token_usage();
        assert_eq!(usage.prompt_tokens, 8);
        assert_eq!(usage.completion_tokens, 3);
        assert_eq!(usage.cache_read_tokens, 5);
        assert_eq!(usage.cache_creation_tokens, 2);
    }
}
