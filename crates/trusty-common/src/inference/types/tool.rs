//! Tool-calling + prompt-cache wire types and the neutral [`ToolChoice`] policy.
//!
//! Why: tool definitions, tool calls, and the tool-choice policy are shared by
//! every consumer that lets a model call functions. Isolating them here (rather
//! than in `request.rs`) keeps the request body free of tool-schema
//! boilerplate and lets the tool surface evolve independently. These types
//! mirror tcode's proven OpenAI-compatible shapes so #2406 migrates without a
//! wire-format change.
//! What: [`ToolCall`]/[`FunctionCall`] (model-emitted), [`ToolDefinition`]/
//! [`FunctionDefinition`] (caller-supplied schema), the [`ToolChoice`] neutral
//! policy enum with its [`openai_tool_choice`] wire mapper, [`CacheControl`]
//! (Anthropic ephemeral breakpoint), and [`RequestUsageConfig`] (OpenRouter's
//! detailed-usage directive).
//! Test: inline `tests` — `tool_definition_serialises`,
//! `function_definition_round_trip`, `openai_tool_choice_maps_every_variant`,
//! `cache_control_ephemeral_shape`, `request_usage_detailed_shape`.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// An Anthropic-style ephemeral prompt-cache breakpoint.
///
/// Why: Anthropic (and OpenRouter's passthrough for `anthropic/*` slugs) prices
/// everything up to and including a `cache_control` marker as one cached prefix,
/// so a cache hit on a later turn bills the prefix at ~0.1x the normal input
/// rate. Callers place this on the byte-stable tools+system prefix they resend
/// every turn.
/// What: `kind` is always `"ephemeral"`; serialises to `{"type":"ephemeral"}`.
/// Test: `cache_control_ephemeral_shape`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CacheControl {
    /// Always `"ephemeral"`.
    #[serde(rename = "type")]
    pub kind: String,
}

impl CacheControl {
    /// Construct the (only) ephemeral cache-breakpoint marker.
    ///
    /// Why: every call site wants the identical value; naming the constructor
    /// after the wire concept avoids a stringly-typed literal at each use site.
    /// What: returns `CacheControl { kind: "ephemeral" }`.
    /// Test: `cache_control_ephemeral_shape`.
    pub fn ephemeral() -> Self {
        Self {
            kind: "ephemeral".into(),
        }
    }
}

/// OpenRouter's request-side "return detailed usage" directive.
///
/// Why: requesting detailed usage accounting is what makes OpenRouter return its
/// authoritative, cache-discount-aware `usage.cost` and nested cache counters —
/// without it only the bare token counts are guaranteed. Sent only for the
/// OpenRouter provider (gated by [`super::super::adapter::InferenceAdapter::wants_detailed_usage`]).
/// What: `include: true` serialises to `{"include":true}` under the request's
/// top-level `usage` key.
/// Test: `request_usage_detailed_shape`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestUsageConfig {
    /// Whether to request detailed (cost + cache-breakdown) usage accounting.
    pub include: bool,
}

impl RequestUsageConfig {
    /// Construct the "include detailed usage" directive.
    ///
    /// Why: every call site wants the identical `{"include":true}` value.
    /// What: returns `RequestUsageConfig { include: true }`.
    /// Test: `request_usage_detailed_shape`.
    pub fn detailed() -> Self {
        Self { include: true }
    }
}

/// A tool call emitted by the model.
///
/// Why: the model may invoke one or more functions before producing final text;
/// callers must execute these and feed results back before the conversation can
/// continue.
/// What: `id` is the opaque call identifier echoed in the subsequent `tool`
/// result message; `kind` is always `"function"`; `function` carries the name
/// and JSON-encoded arguments.
/// Test: exercised via `super::response` deserialisation tests.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCall {
    /// Unique call identifier.
    pub id: String,
    /// Always `"function"` for the current OpenAI schema.
    #[serde(rename = "type")]
    pub kind: String,
    /// The function being called.
    pub function: FunctionCall,
}

/// The function name and arguments within a [`ToolCall`].
///
/// Why: separating this from [`ToolCall`] matches the wire format, which wraps
/// function details in a nested object.
/// What: `name` is the registered function; `arguments` is a JSON string exactly
/// as emitted by the model (parse with `serde_json::from_str` at the call site).
/// Test: exercised via `super::response` deserialisation tests.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FunctionCall {
    /// Registered function name.
    pub name: String,
    /// JSON-encoded argument object.
    pub arguments: String,
}

/// A tool definition submitted with a request.
///
/// Why: providing tool definitions tells the model which functions exist so it
/// can decide when and how to call them.
/// What: `kind` is always `"function"`; `function` carries the JSON-Schema
/// description.
/// Test: `tool_definition_serialises`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
    /// Why: all current OpenAI-compatible tools are function tools; this
    /// constructor bakes in that invariant.
    /// What: sets `kind = "function"` and wraps the provided schema.
    /// Test: `tool_definition_serialises`.
    pub fn function(function: FunctionDefinition) -> Self {
        Self {
            kind: "function".into(),
            function,
        }
    }
}

/// JSON-Schema description of a callable function.
///
/// Why: the model needs the name, a description, and parameter schema to decide
/// when and how to call the function correctly.
/// What: `name` is the call target; `description` guides selection; `parameters`
/// is a raw `serde_json::Value` JSON-Schema object; `cache_control` optionally
/// marks this (typically the last) tool as a cache breakpoint.
/// Test: `function_definition_round_trip`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FunctionDefinition {
    /// The function name as registered with the model.
    pub name: String,
    /// Human-readable description guiding the model's decision to call it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// JSON-Schema object describing the parameter shape.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parameters: Option<Value>,
    /// Prompt-cache breakpoint; when set, serialises as a sibling
    /// `"cache_control":{"type":"ephemeral"}` field alongside the schema.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_control: Option<CacheControl>,
}

/// Provider-neutral tool-choice policy.
///
/// Why: callers express intent — "don't call tools", "you may", "you must", or
/// "call this specific function" — without knowing how each backend spells it on
/// the wire. The adapter translates this via
/// [`super::super::adapter::InferenceAdapter::map_tool_choice`].
/// What: `None` disables tools, `Auto` lets the model decide, `Required` forces
/// some tool call, `Function(name)` forces a specific named function.
/// Test: `openai_tool_choice_maps_every_variant`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolChoice {
    /// The model must not call any tool.
    None,
    /// The model may call a tool if it decides to.
    Auto,
    /// The model must call some tool this turn.
    Required,
    /// The model must call the named function.
    Function(String),
}

/// Map a neutral [`ToolChoice`] into the OpenAI-dialect wire value.
///
/// Why: OpenAI-compatible backends (OpenRouter, Fireworks, OpenAI-direct) share
/// one spelling — string policies plus a `{type,function}` object for a specific
/// call. Centralising it here is the default the [`ToolDialect::OpenAiFunctions`]
/// adapters reuse rather than re-deriving at each site.
/// What: returns the `serde_json::Value` to place in `ChatRequest.tool_choice`.
/// Test: `openai_tool_choice_maps_every_variant`.
///
/// [`ToolDialect::OpenAiFunctions`]: super::super::registry::ToolDialect::OpenAiFunctions
pub fn openai_tool_choice(choice: ToolChoice) -> Value {
    match choice {
        ToolChoice::None => json!("none"),
        ToolChoice::Auto => json!("auto"),
        ToolChoice::Required => json!("required"),
        ToolChoice::Function(name) => json!({"type": "function", "function": {"name": name}}),
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: the wire format requires `type: "function"`.
    /// Test: itself.
    #[test]
    fn tool_definition_serialises() {
        let tool = ToolDefinition::function(FunctionDefinition {
            name: "ping".into(),
            description: None,
            parameters: None,
            cache_control: None,
        });
        let v: Value = serde_json::to_value(&tool).expect("serialise");
        assert_eq!(v["type"], "function");
        assert_eq!(v["function"]["name"], "ping");
        assert!(v["function"].get("cache_control").is_none());
    }

    /// Why: parameter schemas are raw JSON; verify the round-trip preserves them.
    /// Test: itself.
    #[test]
    fn function_definition_round_trip() {
        let params = json!({"type": "object", "properties": {}});
        let def = FunctionDefinition {
            name: "my_fn".into(),
            description: Some("does stuff".into()),
            parameters: Some(params.clone()),
            cache_control: Some(CacheControl::ephemeral()),
        };
        let s = serde_json::to_string(&def).expect("serialise");
        let de: FunctionDefinition = serde_json::from_str(&s).expect("deserialise");
        assert_eq!(de.name, "my_fn");
        assert_eq!(de.parameters.as_ref(), Some(&params));
        assert_eq!(de.cache_control, Some(CacheControl::ephemeral()));
    }

    /// Why: a typo in any wire spelling would silently break tool routing.
    /// Test: itself.
    #[test]
    fn openai_tool_choice_maps_every_variant() {
        assert_eq!(openai_tool_choice(ToolChoice::None), json!("none"));
        assert_eq!(openai_tool_choice(ToolChoice::Auto), json!("auto"));
        assert_eq!(openai_tool_choice(ToolChoice::Required), json!("required"));
        assert_eq!(
            openai_tool_choice(ToolChoice::Function("get_weather".into())),
            json!({"type": "function", "function": {"name": "get_weather"}})
        );
    }

    /// Why: this is the literal marker the caching passthrough looks for.
    /// Test: itself.
    #[test]
    fn cache_control_ephemeral_shape() {
        let v = serde_json::to_value(CacheControl::ephemeral()).expect("serialise");
        assert_eq!(v, json!({"type": "ephemeral"}));
    }

    /// Why: a typo would silently disable detailed usage accounting.
    /// Test: itself.
    #[test]
    fn request_usage_detailed_shape() {
        let v = serde_json::to_value(RequestUsageConfig::detailed()).expect("serialise");
        assert_eq!(v, json!({"include": true}));
    }
}
