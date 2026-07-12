//! Fireworks provider — request-shaping normalisation for `fireworks/*` slugs
//! (#2406, epic #2400).
//!
//! Why: enabling Fireworks in tcode (#2406) is not only a transport concern —
//! `AgentLoop::build_request` consults `crate::provider::provider_for` to decide
//! whether to request OpenRouter's detailed-usage directive, whether to attach
//! prompt-cache breakpoints, and whether to send a native `tools` array. Routing
//! a `fireworks/*` slug to the default `OpenRouterProvider` would (wrongly)
//! request `usage:{include:true}` — a directive Fireworks doesn't understand —
//! so Fireworks needs its own normalisation profile, mirroring the shared
//! capability registry's Fireworks seed (`detailed_usage_accounting = false`,
//! `prompt_caching = false`, native OpenAI-style function calling).
//! What: [`FireworksProvider`] implements [`Provider`] with the OpenAI-dialect
//! `tool_choice` mapping (identical to OpenRouter's), the standard usage
//! extraction, native tool support, NO prompt caching, and NO detailed-usage
//! request. The transport (`crate::llm::client::OpenAiCompatClient`) is what
//! actually reaches `api.fireworks.ai`; this type only shapes the request.
//! Test: `fireworks::tests::*`.

use serde_json::{Value, json};

use super::traits::{Provider, ToolChoice};
use crate::llm::ChatResponse;
use crate::perf::TokenUsage;

/// Provider for Fireworks-hosted models (`fireworks/*` slugs), served behind the
/// OpenAI-compatible `/chat/completions` schema.
///
/// Why: gives `build_request` a Fireworks-correct normalisation profile so the
/// wire payload matches what Fireworks accepts (no OpenRouter-only directives).
/// What: zero-sized marker type implementing [`Provider`].
/// Test: `fireworks::tests::*`.
#[derive(Debug, Clone, Default)]
pub struct FireworksProvider;

impl FireworksProvider {
    /// Construct a `FireworksProvider`.
    ///
    /// Why: gives the factory a uniform constructor call site.
    /// What: returns the zero-sized marker.
    /// Test: `fireworks::tests::name_is_fireworks`.
    pub fn new() -> Self {
        Self
    }
}

impl Provider for FireworksProvider {
    fn name(&self) -> &str {
        "fireworks"
    }

    /// Translate a neutral [`ToolChoice`] into OpenAI-dialect wire JSON.
    ///
    /// Why: Fireworks uses the OpenAI `tool_choice` spelling (string policies +
    /// `{type:function, function:{name}}` object), identical to OpenRouter.
    /// What: the OpenAI mapping.
    /// Test: `fireworks::tests::map_tool_choice_scalars`.
    fn map_tool_choice(&self, choice: ToolChoice) -> Value {
        match choice {
            ToolChoice::None => json!("none"),
            ToolChoice::Auto => json!("auto"),
            ToolChoice::Required => json!("required"),
            ToolChoice::Function(name) => json!({
                "type": "function",
                "function": { "name": name },
            }),
        }
    }

    /// Extract canonical token usage from a Fireworks response.
    ///
    /// Why: Fireworks returns the standard OpenAI `usage` block, already
    /// normalised into `ChatResponse.usage` by the shared adapter's parse.
    /// What: delegates to `ChatResponse::token_usage` (identical to the other
    /// OpenAI-dialect providers).
    /// Test: `fireworks::tests::extract_usage_maps_fields`.
    fn extract_usage(&self, response: &ChatResponse) -> TokenUsage {
        response.clone().token_usage()
    }

    fn supports_native_tools(&self) -> bool {
        // Fireworks' tool-capable models accept the native OpenAI `tools` array;
        // mirrors the shared registry's Fireworks `native_tool_calling = true`.
        true
    }

    fn supports_prompt_caching(&self) -> bool {
        // Fireworks does not honour Anthropic-style `cache_control` breakpoints;
        // mirrors the shared registry's Fireworks `prompt_caching = false`.
        false
    }

    fn wants_detailed_usage(&self) -> bool {
        // `usage:{include:true}` is an OpenRouter-only directive; Fireworks would
        // not understand it. Mirrors the shared registry's Fireworks
        // `detailed_usage_accounting = false`.
        false
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// `name()` returns `"fireworks"`.
    ///
    /// Why: routing and telemetry distinguish Fireworks by name; the transport's
    /// `provider_for_slug` and `dispatch`'s bedrock check both key off it.
    /// What: assert the name.
    /// Test: this test.
    #[test]
    fn name_is_fireworks() {
        assert_eq!(FireworksProvider::new().name(), "fireworks");
    }

    /// Fireworks reports native-tool support.
    ///
    /// Why: `build_request` sends the native `tools` array for tool-capable
    /// Fireworks models.
    /// What: assert `supports_native_tools()` is `true`.
    /// Test: this test.
    #[test]
    fn fireworks_supports_native_tools() {
        assert!(FireworksProvider::new().supports_native_tools());
    }

    /// Fireworks does NOT request OpenRouter's detailed usage accounting.
    ///
    /// Why: this is the exact gate that would otherwise attach the
    /// OpenRouter-only `usage:{include:true}` directive to a Fireworks request —
    /// the core reason a Fireworks-specific provider is needed.
    /// What: assert `wants_detailed_usage()` is `false`.
    /// Test: this test.
    #[test]
    fn fireworks_does_not_want_detailed_usage() {
        assert!(!FireworksProvider::new().wants_detailed_usage());
    }

    /// Fireworks does NOT advertise prompt caching.
    ///
    /// Why: `build_request` must never mark a Fireworks request with an
    /// Anthropic-style `cache_control` breakpoint it cannot honour.
    /// What: assert `supports_prompt_caching()` is `false`.
    /// Test: this test.
    #[test]
    fn fireworks_does_not_support_prompt_caching() {
        assert!(!FireworksProvider::new().supports_prompt_caching());
    }

    /// `map_tool_choice` maps the scalar policies to the OpenAI wire strings.
    ///
    /// Why: Fireworks expects the OpenAI spelling; a wrong shape breaks
    /// tool routing.
    /// What: map each scalar variant, assert the JSON value.
    /// Test: this test.
    #[test]
    fn map_tool_choice_scalars() {
        let p = FireworksProvider::new();
        assert_eq!(p.map_tool_choice(ToolChoice::None), json!("none"));
        assert_eq!(p.map_tool_choice(ToolChoice::Auto), json!("auto"));
        assert_eq!(p.map_tool_choice(ToolChoice::Required), json!("required"));
        assert_eq!(
            p.map_tool_choice(ToolChoice::Function("search".into())),
            json!({"type": "function", "function": {"name": "search"}})
        );
    }

    /// `extract_usage` maps a normalised usage block into `TokenUsage`.
    ///
    /// Why: pins that Fireworks performs no extra (inconsistent) usage transform.
    /// What: deserialise a response fixture, extract usage, assert fields.
    /// Test: this test.
    #[test]
    fn extract_usage_maps_fields() {
        let fixture = r#"{
          "id": "fw-1",
          "choices": [{"message": {"role": "assistant", "content": "ok"}, "finish_reason": "stop"}],
          "usage": {"prompt_tokens": 40, "completion_tokens": 8, "total_tokens": 48}
        }"#;
        let resp: ChatResponse = serde_json::from_str(fixture).expect("deserialise");
        let usage = FireworksProvider::new().extract_usage(&resp);
        assert_eq!(usage.prompt_tokens, 40);
        assert_eq!(usage.completion_tokens, 8);
    }
}
