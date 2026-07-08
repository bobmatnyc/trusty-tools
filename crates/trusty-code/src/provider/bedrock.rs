//! AWS Bedrock provider (#1021 phase 1: real Converse-API implementation).
//!
//! Why: #1021 established the provider abstraction and the `bedrock/*`
//! routing seam with a stub that panicked on `map_tool_choice`/`extract_usage`
//! (the agent loop never called a Bedrock provider yet). This phase wires the
//! real Bedrock wire integration: the Converse API's own tool-choice JSON
//! shape and its `TokenUsage` envelope, plus (`crate::llm::dispatch`) the
//! transport that actually calls Bedrock via `crate::llm::BedrockChatClient`.
//! What: [`BedrockProvider`] implements [`Provider`]. `map_tool_choice`
//! returns Converse's own toolChoice JSON shape (`{"auto":{}}`, `{"any":{}}`,
//! `{"tool":{"name":...}}`, or the sentinel string `"none"` when tools should
//! be suppressed entirely) — `crate::llm::bedrock::convert::build_tool_config`
//! interprets exactly this shape (and, for now, the plain OpenAI strings
//! `agent_loop::build_request` still hardcodes) when building the Converse
//! request. `extract_usage` delegates to `ChatResponse::token_usage`, the
//! same conversion `OpenRouterProvider` uses — by the time a Bedrock response
//! reaches here it has already been mapped into `ChatResponse`'s `UsageBlock`
//! shape by `crate::llm::bedrock::convert::converse_output_to_chat_response`,
//! so no Bedrock-specific parsing is needed at this layer.
//! Test: `bedrock::tests::*` cover `map_tool_choice`'s Converse-shaped output
//! and `extract_usage`'s delegation; `crate::llm::bedrock::tests` covers the
//! actual Converse wire conversion this provider's mapping feeds.
//!
//! (#2260) `supports_prompt_caching` now returns `true`: the Converse
//! transport (`crate::llm::bedrock::cache`) translates the SAME
//! `agent_loop::build_request` cache markers (`ChatMessage.cache_control`,
//! `FunctionDefinition.cache_control`) that OpenRouter's `cache_control`
//! passthrough consumes into Bedrock's native `cachePoint` content blocks —
//! see that module's doc for the wire-shape translation and the minimum-size
//! guard.

use serde_json::{Value, json};

use super::traits::{Provider, ToolChoice};
use crate::llm::ChatResponse;
use crate::perf::TokenUsage;

/// Provider for AWS Bedrock-hosted models (`bedrock/*` slugs), backed by the
/// Converse API.
///
/// Why: Reserves the routing target and (#1021 phase 1) normalises
/// tool-choice/usage for the Converse wire shape, which differs from
/// OpenRouter's OpenAI-compatible schema on both axes.
/// What: Zero-sized marker type implementing [`Provider`].
/// Test: `bedrock::tests::*`.
#[derive(Debug, Clone, Default)]
pub struct BedrockProvider;

impl BedrockProvider {
    /// Construct a `BedrockProvider`.
    ///
    /// Why: Gives the factory a uniform constructor call site.
    /// What: Returns the zero-sized marker.
    /// Test: `bedrock::tests::name_is_bedrock`.
    pub fn new() -> Self {
        Self
    }
}

impl Provider for BedrockProvider {
    fn name(&self) -> &str {
        "bedrock"
    }

    /// Translate a neutral [`ToolChoice`] into Converse's own toolChoice JSON
    /// shape.
    ///
    /// Why: Converse's `toolChoice` is `{"auto":{}}` / `{"any":{}}` /
    /// `{"tool":{"name":...}}` — structurally different from OpenAI's bare
    /// strings and `{"type":"function","function":{"name":...}}` object.
    /// `crate::llm::bedrock::convert::build_tool_config`'s
    /// `interpret_tool_choice` accepts BOTH this shape and the OpenAI shape
    /// (since `agent_loop::build_request` currently sends the OpenAI
    /// `"auto"` string unconditionally — #1021 doesn't rewire that call
    /// site), so wiring this mapping in later is a drop-in change with no
    /// transport update required.
    /// What: `None` -> `"none"` (a sentinel string; Converse has no "don't
    /// call any tool" choice, so the transport omits `toolConfig` entirely
    /// when it sees this). `Auto` -> `{"auto":{}}`. `Required` -> `{"any":{}}`
    /// (Converse's "must call some tool" choice). `Function(name)` ->
    /// `{"tool":{"name":name}}`.
    /// Test: `bedrock::tests::map_tool_choice_*`.
    fn map_tool_choice(&self, choice: ToolChoice) -> Value {
        match choice {
            ToolChoice::None => json!("none"),
            ToolChoice::Auto => json!({"auto": {}}),
            ToolChoice::Required => json!({"any": {}}),
            ToolChoice::Function(name) => json!({"tool": {"name": name}}),
        }
    }

    /// Extract canonical token usage from a (Bedrock-originated) chat response.
    ///
    /// Why: By the time a Bedrock Converse response reaches `Provider`, the
    /// transport (`crate::llm::bedrock::convert::converse_output_to_chat_response`)
    /// has already mapped Converse's `TokenUsage` (`inputTokens`/
    /// `outputTokens`/`cacheReadInputTokens`/`cacheWriteInputTokens`) into
    /// `ChatResponse.usage: UsageBlock` — the same canonical shape every
    /// other provider's response is normalised into. No Bedrock-specific
    /// parsing is needed at this layer.
    /// What: Delegates to `ChatResponse::token_usage` (identical to
    /// `OpenRouterProvider::extract_usage`).
    /// Test: `bedrock::tests::extract_usage_maps_fields`.
    fn extract_usage(&self, response: &ChatResponse) -> TokenUsage {
        response.clone().token_usage()
    }

    fn supports_native_tools(&self) -> bool {
        // Bedrock-hosted Anthropic/other models support native tool use; the
        // strategy matrix (#1023) may refine this per-model later.
        true
    }

    fn supports_prompt_caching(&self) -> bool {
        // (#2260) Bedrock's Converse API supports prompt caching via its own
        // `cachePoint` block shape (not Anthropic's `cache_control` marker
        // used by the OpenRouter passthrough, #2156) — `crate::llm::bedrock`
        // now implements that translation, so a Bedrock route is eligible
        // for `agent_loop::build_request`'s cache markers exactly like an
        // `anthropic/*` OpenRouter slug is.
        true
    }

    fn wants_detailed_usage(&self) -> bool {
        // `RequestUsageConfig`/`usage: {"include": true}` is an
        // OpenRouter-specific OpenAI-compat wire directive; Bedrock's
        // Converse API always returns its `usage` envelope unconditionally,
        // so there is nothing to request.
        false
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// `name()` returns `"bedrock"`.
    ///
    /// Why: Routing and telemetry distinguish Bedrock from OpenRouter by name.
    /// What: Construct the provider, assert the name.
    /// Test: this test.
    #[test]
    fn name_is_bedrock() {
        assert_eq!(BedrockProvider::new().name(), "bedrock");
    }

    /// Bedrock reports native-tool support.
    ///
    /// Why: Bedrock-hosted Anthropic models accept the native `tools` array.
    /// What: Assert `supports_native_tools()` is `true`.
    /// Test: this test.
    #[test]
    fn bedrock_supports_native_tools() {
        assert!(BedrockProvider::new().supports_native_tools());
    }

    /// Bedrock reports prompt-caching support (#2260).
    ///
    /// Why: `crate::llm::bedrock::cache` translates `build_request`'s cache
    /// markers into Bedrock's native `cachePoint` blocks, so
    /// `agent_loop::prompt_cache_enabled` must now treat a Bedrock route the
    /// same as an `anthropic/*` OpenRouter slug.
    /// What: Assert `supports_prompt_caching()` is `true`.
    /// Test: this test.
    #[test]
    fn bedrock_supports_prompt_caching() {
        assert!(BedrockProvider::new().supports_prompt_caching());
    }

    /// Bedrock does NOT request OpenRouter-style detailed usage accounting.
    ///
    /// Why: `usage: {"include": true}` is an OpenRouter-only OpenAI-compat
    /// directive; Bedrock always returns its usage envelope unconditionally.
    /// What: Assert `wants_detailed_usage()` is `false`.
    /// Test: this test.
    #[test]
    fn bedrock_does_not_want_detailed_usage() {
        assert!(!BedrockProvider::new().wants_detailed_usage());
    }

    /// `map_tool_choice` maps the three scalar policies to Converse's own
    /// JSON shape (NOT the OpenAI shape `OpenRouterProvider` uses).
    ///
    /// Why: This is the exact JSON `crate::llm::bedrock::convert::build_tool_config`
    /// interprets; a wrong shape here would silently fail to force/suppress
    /// tool calls once this mapping is wired into `agent_loop::build_request`.
    /// What: Map each scalar variant, assert the JSON value.
    /// Test: this test.
    #[test]
    fn map_tool_choice_scalars() {
        let p = BedrockProvider::new();
        assert_eq!(p.map_tool_choice(ToolChoice::None), json!("none"));
        assert_eq!(p.map_tool_choice(ToolChoice::Auto), json!({"auto": {}}));
        assert_eq!(p.map_tool_choice(ToolChoice::Required), json!({"any": {}}));
    }

    /// `map_tool_choice(Function)` produces Converse's specific-tool selector
    /// object.
    ///
    /// Why: Forcing a specific tool requires Converse's
    /// `{"tool":{"name":...}}` shape, not OpenAI's
    /// `{"type":"function","function":{"name":...}}`.
    /// What: Map `Function("search_code")`, assert the object structure.
    /// Test: this test.
    #[test]
    fn map_tool_choice_function() {
        let p = BedrockProvider::new();
        let v = p.map_tool_choice(ToolChoice::Function("search_code".into()));
        assert_eq!(v, json!({"tool": {"name": "search_code"}}));
    }

    /// `extract_usage` maps a (pre-normalised) usage block into `TokenUsage`,
    /// identically to `OpenRouterProvider::extract_usage`.
    ///
    /// Why: By the time a response reaches `Provider::extract_usage`, the
    /// Bedrock transport has already normalised Converse's usage envelope
    /// into `ChatResponse.usage`; this test pins that `BedrockProvider`
    /// performs no extra (and potentially inconsistent) transformation.
    /// What: Deserialise a fixture shaped like the Bedrock/Anthropic-native
    /// flat usage fields, extract usage, assert each field.
    /// Test: this test.
    #[test]
    fn extract_usage_maps_fields() {
        let fixture = r#"{
          "id": "bedrock-1",
          "model": "us.anthropic.claude-sonnet-4-6",
          "choices": [{"message": {"role": "assistant", "content": "ok"}, "finish_reason": "end_turn"}],
          "usage": {"prompt_tokens": 100, "completion_tokens": 20, "total_tokens": 120,
                    "cache_read_input_tokens": 30, "cache_creation_input_tokens": 10}
        }"#;
        let resp: ChatResponse = serde_json::from_str(fixture).expect("deserialise");
        let p = BedrockProvider::new();
        let usage = p.extract_usage(&resp);
        assert_eq!(usage.prompt_tokens, 100);
        assert_eq!(usage.completion_tokens, 20);
        assert_eq!(usage.cache_read_tokens, 30);
        assert_eq!(usage.cache_creation_tokens, 10);
    }
}
