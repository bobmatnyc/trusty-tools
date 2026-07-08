//! Request-side wire types for the OpenRouter / OpenAI chat-completions API.
//!
//! Why: Keeping tool definitions, tool-call types, and the request body in one
//! module lets the serialisation surface evolve without touching response or
//! message logic.
//! What: Defines `ToolCall`, `FunctionCall`, `ToolDefinition`,
//! `FunctionDefinition`, and `ChatRequest` — everything needed to construct
//! and serialise a `POST /v1/chat/completions` payload.
//! Test: `chat_request_serialises_required_fields`,
//! `chat_request_with_tools_serialises`, `tool_definition_serialises`,
//! `function_definition_round_trip`.

use serde::{Deserialize, Serialize};

use super::message::ChatMessage;

// ── Prompt-caching types (#2156) ────────────────────────────────────────────────

/// An Anthropic-style ephemeral prompt-cache breakpoint.
///
/// Why: Anthropic (and OpenRouter's passthrough for `anthropic/*` slugs) prices
/// everything up to and including a `cache_control` marker as one cached
/// prefix — a cache HIT on a later turn bills at roughly 0.1x the normal input
/// price instead of re-billing the full prefix. `agent_loop::build_request`
/// places this marker on the byte-stable tools+system-prompt prefix it
/// resends every turn, which is the make-or-break cost lever for #2156.
/// What: `kind` is always `"ephemeral"` — the only breakpoint type Anthropic
/// currently supports. Serialises to `{"type":"ephemeral"}`.
/// Test: `cache_control_ephemeral_serialises_expected_shape`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CacheControl {
    /// Always `"ephemeral"`.
    #[serde(rename = "type")]
    pub kind: String,
}

impl CacheControl {
    /// Construct the (only) ephemeral cache-breakpoint marker.
    ///
    /// Why: Every call site wants the identical `{"type":"ephemeral"}` value;
    /// naming the constructor after the wire concept avoids a stringly-typed
    /// literal at each use site.
    /// What: Returns `CacheControl { kind: "ephemeral".into() }`.
    /// Test: `cache_control_ephemeral_serialises_expected_shape`.
    pub fn ephemeral() -> Self {
        Self {
            kind: "ephemeral".into(),
        }
    }
}

/// OpenRouter's request-side "return detailed usage" directive (response-side
/// cache-usage fix).
///
/// Why: Requesting detailed usage accounting is what makes OpenRouter return
/// its authoritative, already cache-discount-aware `usage.cost` field (and
/// the nested `prompt_tokens_details` cache counters) on the response —
/// without it, only the bare prompt/completion/total counts are guaranteed.
/// Sent only for the OpenRouter provider (`agent_loop::build_request`); other
/// providers (direct/Bedrock) never see this field.
/// What: `include: true` serialises to the wire shape `{"include":true}`
/// under the request's top-level `usage` key.
/// Test: `request_usage_config_detailed_serialises_expected_shape`,
/// `chat_request_with_detailed_usage_serialises`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RequestUsageConfig {
    /// Whether to request detailed (cost + cache-breakdown) usage accounting.
    pub include: bool,
}

impl RequestUsageConfig {
    /// Construct the "include detailed usage" directive.
    ///
    /// Why: Every call site wants the identical `{"include":true}` value;
    /// naming the constructor after the wire concept avoids a bare `true`
    /// literal scattered at call sites.
    /// What: Returns `RequestUsageConfig { include: true }`.
    /// Test: `request_usage_config_detailed_serialises_expected_shape`.
    pub fn detailed() -> Self {
        Self { include: true }
    }
}

// ── Tool-calling types ─────────────────────────────────────────────────────────

/// A tool call emitted by the model.
///
/// Why: The model may decide to invoke one or more functions before producing a
/// final text response; callers must handle these before the conversation can
/// continue.
/// What: `id` is the opaque call identifier used when submitting the result
/// back; `r#type` is always `"function"`; `function` carries the name and
/// JSON-encoded arguments.
/// Test: `tool_call_deserialises_from_fixture` (in `response.rs` tests).
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
/// Test: `tool_call_deserialises_from_fixture` (in `response.rs` tests).
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

    /// Prompt-cache breakpoint (#2156). When set, serialises as a sibling
    /// `"cache_control":{"type":"ephemeral"}` field alongside this function's
    /// schema — the shape `block/goose`'s (verified, production) OpenRouter
    /// provider uses to cache Claude tool definitions via the OpenAI-compat
    /// endpoint. `agent_loop::build_request` sets this on the LAST tool
    /// definition only: Anthropic caches everything up to and including a
    /// breakpoint, so marking just the last tool caches the entire
    /// (byte-stable) tools array as a single prefix.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_control: Option<CacheControl>,
}

// ── Request type ───────────────────────────────────────────────────────────────

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

    /// Request OpenRouter's detailed usage accounting (response-side
    /// cache-usage fix). `Some(RequestUsageConfig::detailed())` for the
    /// OpenRouter provider; `None` (omitted from the wire payload) for every
    /// other provider/path.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<RequestUsageConfig>,
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

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
            usage: None,
        };
        let v: serde_json::Value = serde_json::to_value(&req).expect("serialise");
        assert_eq!(v["model"], "anthropic/claude-haiku-4-5");
        assert_eq!(v["messages"][0]["role"], "user");
        assert_eq!(v["messages"][0]["content"], "hello");
        assert!(v.get("temperature").is_none() || v["temperature"].is_null());
        assert!(v.get("tools").is_none() || v["tools"].is_null());
        assert!(
            v.get("usage").is_none() || v["usage"].is_null(),
            "usage must be omitted when unset"
        );
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
            cache_control: None,
        });
        let req = ChatRequest {
            model: "openai/gpt-4o-mini".into(),
            messages: vec![ChatMessage::user("What's the weather?")],
            temperature: Some(0.0),
            max_tokens: Some(256),
            tools: Some(vec![tool]),
            tool_choice: Some(serde_json::json!("auto")),
            usage: None,
        };
        let v: serde_json::Value = serde_json::to_value(&req).expect("serialise");
        assert_eq!(v["tools"][0]["type"], "function");
        assert_eq!(v["tools"][0]["function"]["name"], "get_weather");
        assert_eq!(v["tool_choice"], "auto");
        assert_eq!(v["temperature"], 0.0_f32);
        assert_eq!(v["max_tokens"], 256);
    }

    /// `RequestUsageConfig::detailed` serialises to the exact
    /// `{"include":true}` shape OpenRouter expects.
    ///
    /// Why: A typo (wrong key, wrong value) would silently disable detailed
    /// usage accounting without any test noticing — the same failure mode
    /// `cache_control_ephemeral_serialises_expected_shape` guards against.
    /// What: Serialise `RequestUsageConfig::detailed()`, assert the exact
    /// object.
    /// Test: this test.
    #[test]
    fn request_usage_config_detailed_serialises_expected_shape() {
        let v = serde_json::to_value(RequestUsageConfig::detailed()).expect("serialise");
        assert_eq!(v, serde_json::json!({"include": true}));
    }

    /// A `ChatRequest` with `usage` set serialises the top-level `"usage"`
    /// key; a request with `usage: None` omits it entirely (response-side
    /// cache-usage fix — gated to the OpenRouter provider only).
    ///
    /// Why: `agent_loop::build_request` must only attach this directive for
    /// OpenRouter; verifying both the present and absent cases in one test
    /// pins the exact wire contract other providers must never see.
    /// What: Serialise a request with `usage: Some(...)`, assert the field;
    /// serialise one with `usage: None`, assert it's absent.
    /// Test: this test.
    #[test]
    fn chat_request_with_detailed_usage_serialises() {
        let with_usage = ChatRequest {
            model: "anthropic/claude-sonnet-4-5".into(),
            messages: vec![ChatMessage::user("hi")],
            temperature: None,
            max_tokens: None,
            tools: None,
            tool_choice: None,
            usage: Some(RequestUsageConfig::detailed()),
        };
        let v: serde_json::Value = serde_json::to_value(&with_usage).expect("serialise");
        assert_eq!(v["usage"], serde_json::json!({"include": true}));

        let without_usage = ChatRequest {
            usage: None,
            ..with_usage
        };
        let v: serde_json::Value = serde_json::to_value(&without_usage).expect("serialise");
        assert!(
            v.get("usage").is_none() || v["usage"].is_null(),
            "usage must be omitted when None"
        );
    }

    /// `ToolDefinition::function` sets `kind = "function"`.
    ///
    /// Why: The wire format requires `type: "function"`; verify the constructor
    /// bakes in that invariant.
    /// What: Build via `ToolDefinition::function`, serialise, assert `type` field.
    /// Test: this test.
    #[test]
    fn tool_definition_serialises() {
        let tool = ToolDefinition::function(FunctionDefinition {
            name: "ping".into(),
            description: None,
            parameters: None,
            cache_control: None,
        });
        let v: serde_json::Value = serde_json::to_value(&tool).expect("serialise");
        assert_eq!(v["type"], "function");
        assert_eq!(v["function"]["name"], "ping");
        assert!(
            v["function"].get("cache_control").is_none(),
            "cache_control must be omitted when unset"
        );
    }

    /// `FunctionDefinition` round-trips through JSON.
    ///
    /// Why: Parameter schemas are `serde_json::Value`; verify the round-trip
    /// preserves structure.
    /// What: Build a `FunctionDefinition` with a parameters schema, serialise
    /// and deserialise, assert fields match.
    /// Test: this test.
    #[test]
    fn function_definition_round_trip() {
        let params = serde_json::json!({"type": "object", "properties": {}});
        let def = FunctionDefinition {
            name: "my_fn".into(),
            description: Some("does stuff".into()),
            parameters: Some(params.clone()),
            cache_control: None,
        };
        let serialised = serde_json::to_string(&def).expect("serialise");
        let de: FunctionDefinition = serde_json::from_str(&serialised).expect("deserialise");
        assert_eq!(de.name, "my_fn");
        assert_eq!(de.description.as_deref(), Some("does stuff"));
        assert_eq!(de.parameters.as_ref(), Some(&params));
        assert!(de.cache_control.is_none());
    }

    /// `CacheControl::ephemeral` serialises to the exact Anthropic-ephemeral
    /// shape (#2156).
    ///
    /// Why: This is the literal JSON marker OpenRouter's Claude passthrough
    /// looks for; a typo (wrong key name, wrong value) would silently disable
    /// caching without any test noticing.
    /// What: Serialise `CacheControl::ephemeral()`, assert the exact object.
    /// Test: this test.
    #[test]
    fn cache_control_ephemeral_serialises_expected_shape() {
        let v = serde_json::to_value(CacheControl::ephemeral()).expect("serialise");
        assert_eq!(v, serde_json::json!({"type": "ephemeral"}));
    }

    /// A `FunctionDefinition` with `cache_control` set serialises the marker
    /// nested inside the function object (#2156), matching the shape
    /// `block/goose`'s verified OpenRouter provider uses.
    ///
    /// Why: The breakpoint must land on the tool's `function` object, not on
    /// the wrapping `ToolDefinition` — placing it at the wrong nesting level
    /// would mean OpenRouter's OpenAI→Anthropic conversion drops the marker
    /// entirely and caching silently never activates.
    /// What: Build a `ToolDefinition` whose `FunctionDefinition.cache_control`
    /// is `Some`, serialise, assert `tools[0].function.cache_control` matches
    /// the ephemeral shape.
    /// Test: this test.
    #[test]
    fn function_definition_with_cache_control_serialises_marker() {
        let tool = ToolDefinition::function(FunctionDefinition {
            name: "write_file".into(),
            description: Some("Write a file".into()),
            parameters: None,
            cache_control: Some(CacheControl::ephemeral()),
        });
        let v: serde_json::Value = serde_json::to_value(&tool).expect("serialise");
        assert_eq!(
            v["function"]["cache_control"],
            serde_json::json!({"type": "ephemeral"})
        );
    }
}
